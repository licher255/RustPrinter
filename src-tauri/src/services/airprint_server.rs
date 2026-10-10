use crate::models::{Printer, PrinterStatus};
use crate::services::{ipp::IppServer, MdnsBroadcaster};
use rust_i18n::t;
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::net::IpAddr;
use std::path::PathBuf;

pub struct AirPrintServer {
    shared_printers: HashMap<String, Printer>,
    selected: HashSet<String>,
    preferences: Option<PathBuf>,
    bind_address: String,
    mdns_interface: Option<IpAddr>,
    mdns: Option<MdnsBroadcaster>,
    ipp_server: Option<IppServer>,
    receive_directory: Option<PathBuf>,
}

impl AirPrintServer {
    pub fn new() -> Self {
        Self {
            shared_printers: HashMap::new(),
            selected: HashSet::new(),
            preferences: None,
            bind_address: "0.0.0.0".into(),
            mdns_interface: None,
            mdns: None,
            ipp_server: None,
            receive_directory: None,
        }
    }

    pub fn with_preferences(path: PathBuf) -> Result<Self, String> {
        let selected = match std::fs::read(&path) {
            Ok(data) => serde_json::from_slice::<Vec<String>>(&data)
                .map_err(|e| format!("Invalid saved printer selection: {e}"))?
                .into_iter()
                .collect(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => HashSet::new(),
            Err(error) => return Err(error.to_string()),
        };
        Ok(Self {
            selected,
            preferences: Some(path),
            ..Self::new()
        })
    }

    /// A local simulation endpoint bound to one virtual adapter.
    pub fn isolated_for_test(address: IpAddr) -> Self {
        Self {
            bind_address: address.to_string(),
            mdns_interface: Some(address),
            ..Self::new()
        }
    }

    pub fn set_receive_directory(&mut self, directory: Option<PathBuf>) -> Result<(), String> {
        if directory.as_ref().is_some_and(|path| !path.is_absolute()) {
            return Err("Receive-only directory must be absolute".into());
        }
        if let Some(ipp) = &mut self.ipp_server {
            ipp.set_receive_directory(directory.clone())?;
        }
        self.receive_directory = directory;
        Ok(())
    }

    fn save_selected(&self, selected: &HashSet<String>) -> Result<(), String> {
        let Some(path) = &self.preferences else {
            return Ok(());
        };
        let parent = path.parent().ok_or("Invalid preferences path")?;
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        let mut names: Vec<_> = selected.iter().collect();
        names.sort_unstable();
        let data = serde_json::to_vec(&names).map_err(|e| e.to_string())?;
        let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
        temp.write_all(&data).map_err(|e| e.to_string())?;
        temp.as_file().sync_all().map_err(|e| e.to_string())?;
        temp.persist(path).map_err(|e| e.error.to_string())?;
        Ok(())
    }

    fn share_runtime(&mut self, printer: Printer) -> Result<(), String> {
        let id = printer.id.clone();
        if self.shared_printers.contains_key(&id) {
            return Err(t!("messages.printer_already_shared", id = id).to_string());
        }
        if !matches!(printer.status, PrinterStatus::Online | PrinterStatus::Busy) {
            return Err(format!("Windows printer is not ready: {}", printer.name));
        }
        let result = (|| {
            if self.mdns.is_none() {
                let mdns = match self.mdns_interface {
                    Some(address) => MdnsBroadcaster::new_on_interface(address),
                    None => MdnsBroadcaster::new(),
                };
                self.mdns = Some(mdns.map_err(|e| format!("mDNS: {e}"))?);
            }
            if self.ipp_server.is_none() {
                let mut ipp = IppServer::new(&self.bind_address, 631);
                ipp.set_receive_directory(self.receive_directory.clone())?;
                ipp.start().map_err(|e| format!("IPP listener: {e}"))?;
                if let Some(directory) = self.preferences.as_deref().and_then(|p| p.parent()) {
                    ipp.start_tls(self.mdns.as_ref().unwrap().hostname(), directory)
                        .map_err(|e| format!("IPPS listener: {e}"))?;
                }
                self.ipp_server = Some(ipp);
            }
            let mdns = self.mdns.as_mut().unwrap();
            let ipp = self.ipp_server.as_ref().unwrap();
            let port = ipp.port()?;
            ipp.add_printer(printer.clone(), mdns.hostname())?;
            if let Err(error) = mdns.broadcast_with_tls(&printer, port, ipp.secure_port()) {
                ipp.remove_printer(&printer)?;
                return Err(error);
            }
            Ok::<(), String>(())
        })();
        if let Err(error) = result {
            if self.shared_printers.is_empty() {
                self.mdns = None;
                self.ipp_server = None;
            }
            return Err(error);
        }
        self.shared_printers.insert(id, printer);
        Ok(())
    }

    fn stop_runtime(&mut self, id: &str) -> Result<(), String> {
        let printer = self
            .shared_printers
            .get(id)
            .ok_or_else(|| t!("messages.printer_not_shared", id = id).to_string())?;
        if let Some(mdns) = &mut self.mdns {
            mdns.stop(id)?;
        }
        if let Some(ipp) = &self.ipp_server {
            ipp.remove_printer(printer)?;
        }
        self.shared_printers.remove(id);
        if self.shared_printers.is_empty() {
            self.mdns = None;
            self.ipp_server = None;
        }
        Ok(())
    }

    pub fn share(&mut self, printer: Printer) -> Result<String, String> {
        let id = printer.id.clone();
        self.share_runtime(printer)?;
        let mut selected = self.selected.clone();
        selected.insert(id.clone());
        if let Err(error) = self.save_selected(&selected) {
            let _ = self.stop_runtime(&id);
            return Err(error);
        }
        self.selected = selected;
        Ok(t!("messages.share_success", id = id).to_string())
    }

    pub fn stop(&mut self, id: &str) -> Result<(), String> {
        if self.shared_printers.contains_key(id) {
            self.stop_runtime(id)?;
        }
        if !self.selected.contains(id) {
            return Err(t!("messages.printer_not_shared", id = id).to_string());
        }
        let mut selected = self.selected.clone();
        selected.remove(id);
        self.save_selected(&selected)?;
        self.selected = selected;
        Ok(())
    }

    /// Reconcile the remembered choice with what Windows currently exposes.
    /// A missing/offline queue is withdrawn and kept in preferences for retry.
    pub fn reconcile(&mut self, detected: &[Printer]) -> Vec<String> {
        let available: HashMap<_, _> = detected.iter().map(|p| (p.id.as_str(), p)).collect();
        let running: Vec<_> = self.shared_printers.keys().cloned().collect();
        let mut errors = Vec::new();
        for id in running {
            match available.get(id.as_str()) {
                None
                | Some(Printer {
                    status: PrinterStatus::Offline | PrinterStatus::Error(_),
                    ..
                }) => {
                    if let Err(error) = self.stop_runtime(&id) {
                        errors.push(format!("{id}: {error}"));
                    }
                }
                Some(printer) => {
                    let previous = &self.shared_printers[&id];
                    if previous != *printer {
                        let old_color = previous.capabilities.color;
                        let old_duplex = previous.capabilities.duplex;
                        if old_color != printer.capabilities.color
                            || old_duplex != printer.capabilities.duplex
                        {
                            if let Some(mdns) = &self.mdns {
                                let port = self
                                    .ipp_server
                                    .as_ref()
                                    .and_then(|ipp| ipp.port().ok())
                                    .unwrap_or(631);
                                let secure_port =
                                    self.ipp_server.as_ref().and_then(|ipp| ipp.secure_port());
                                if let Err(error) =
                                    mdns.refresh_airprint(printer, port, secure_port)
                                {
                                    errors.push(format!("{id}: {error}"));
                                    continue;
                                }
                            }
                        }
                        if let Some(ipp) = &self.ipp_server {
                            if let Err(error) = ipp.update_printer((*printer).clone()) {
                                errors.push(format!("{id}: {error}"));
                                continue;
                            }
                        }
                        self.shared_printers.insert(id, (*printer).clone());
                    }
                }
            }
        }
        let to_restore: Vec<_> = self
            .selected
            .iter()
            .filter(|id| !self.shared_printers.contains_key(*id))
            .filter_map(|id| available.get(id.as_str()))
            .filter(|printer| matches!(printer.status, PrinterStatus::Online | PrinterStatus::Busy))
            .cloned()
            .cloned()
            .collect();
        for printer in to_restore {
            let id = printer.id.clone();
            if let Err(error) = self.share_runtime(printer) {
                errors.push(format!("{id}: {error}"));
            }
        }
        errors
    }

    pub fn is_shared(&self, id: &str) -> bool {
        self.shared_printers.contains_key(id)
    }
    pub fn get_shared_printers(&self) -> Vec<&Printer> {
        self.shared_printers.values().collect()
    }

    pub fn ipp_port(&self) -> Option<u16> {
        self.ipp_server.as_ref().and_then(|ipp| ipp.port().ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "Publishes a short-lived simulated queue and binds TCP 631"]
    fn selection_survives_restart_and_device_reconnection() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shared-printers.json");
        let mut printer = Printer {
            id: format!("simulated-queue-{}", std::process::id()),
            name: "AirPrinter reconnect simulation".into(),
            status: PrinterStatus::Online,
            capabilities: Default::default(),
        };
        let id = printer.id.clone();

        let mut first = AirPrintServer::with_preferences(path.clone()).unwrap();
        first.share(printer.clone()).unwrap();
        assert!(first.is_shared(&id));
        let saved: Vec<String> = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved, vec![id.clone()]);
        drop(first);

        let mut restarted = AirPrintServer::with_preferences(path.clone()).unwrap();
        assert!(!restarted.is_shared(&id));
        let errors = restarted.reconcile(&[printer.clone()]);
        assert!(errors.is_empty(), "{errors:?}");
        assert!(restarted.is_shared(&id));
        assert!((631..=8699).contains(&restarted.ipp_server.as_ref().unwrap().port().unwrap()));

        printer.capabilities.color = true;
        assert!(restarted.reconcile(&[printer.clone()]).is_empty());
        assert!(restarted.get_shared_printers()[0].capabilities.color);

        let mut offline = printer.clone();
        offline.status = PrinterStatus::Offline;
        assert!(restarted.reconcile(&[offline]).is_empty());
        assert!(!restarted.is_shared(&id));
        assert_eq!(
            serde_json::from_slice::<Vec<String>>(&std::fs::read(&path).unwrap()).unwrap(),
            vec![id.clone()]
        );
        assert!(restarted.reconcile(&[printer]).is_empty());
        assert!(restarted.is_shared(&id));

        restarted.stop(&id).unwrap();
        assert!(!restarted.is_shared(&id));
        assert!(
            serde_json::from_slice::<Vec<String>>(&std::fs::read(&path).unwrap())
                .unwrap()
                .is_empty()
        );
        drop(restarted);
        assert!(AirPrintServer::with_preferences(path)
            .unwrap()
            .selected
            .is_empty());
    }
}
