use crate::models::printer::GATEWAY_PRINTER_ID;
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
        if self.shared_printers.contains_key(GATEWAY_PRINTER_ID) {
            return Err("AirPrinter gateway is already shared".into());
        }
        if !matches!(printer.status, PrinterStatus::Online | PrinterStatus::Busy) {
            return Err(format!("Windows printer is not ready: {}", printer.name));
        }
        let public_printer = Printer::gateway_from(&printer);
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
            ipp.add_routed_printer(public_printer.clone(), &printer.name, mdns.hostname())?;
            if let Err(error) = mdns.broadcast_with_tls(&public_printer, port, ipp.secure_port()) {
                ipp.remove_printer(&public_printer)?;
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
        self.shared_printers
            .insert(GATEWAY_PRINTER_ID.into(), printer);
        Ok(())
    }

    fn stop_runtime(&mut self, id: &str) -> Result<(), String> {
        if id != GATEWAY_PRINTER_ID {
            return Err("Invalid AirPrinter runtime ID".into());
        }
        let target = self
            .shared_printers
            .get(GATEWAY_PRINTER_ID)
            .ok_or_else(|| t!("messages.printer_not_shared", id = id).to_string())?;
        let public_printer = Printer::gateway_from(target);
        if let Some(mdns) = &mut self.mdns {
            mdns.stop(GATEWAY_PRINTER_ID)?;
        }
        if let Some(ipp) = &self.ipp_server {
            ipp.remove_printer(&public_printer)?;
        }
        self.shared_printers.remove(GATEWAY_PRINTER_ID);
        if self.shared_printers.is_empty() {
            self.mdns = None;
            self.ipp_server = None;
        }
        Ok(())
    }

    pub fn share(&mut self, printer: Printer) -> Result<String, String> {
        let id = printer.id.clone();
        if !matches!(printer.status, PrinterStatus::Online | PrinterStatus::Busy) {
            return Err(format!("Windows printer is not ready: {}", printer.name));
        }
        if let Some(current) = self.shared_printers.get(GATEWAY_PRINTER_ID).cloned() {
            if current.id == printer.id {
                return Err(t!("messages.printer_already_shared", id = id).to_string());
            }
            let selected = [id.clone()].into_iter().collect();
            let previous_selected = self.selected.clone();
            self.save_selected(&selected)?;
            if let Err(error) = self.update_runtime_target(&printer) {
                return match self.save_selected(&previous_selected) {
                    Ok(()) => Err(error),
                    Err(rollback_error) => Err(format!(
                        "Target switch failed ({error}); saved selection rollback also failed ({rollback_error})"
                    )),
                };
            }
            self.selected = selected;
            return Ok(t!("messages.share_success", id = id).to_string());
        }
        let selected = [id.clone()].into_iter().collect();
        let previous_selected = self.selected.clone();
        self.save_selected(&selected)?;
        if let Err(error) = self.share_runtime(printer) {
            return match self.save_selected(&previous_selected) {
                Ok(()) => Err(error),
                Err(rollback_error) => Err(format!(
                    "AirPrinter start failed ({error}); saved selection rollback also failed ({rollback_error})"
                )),
            };
        }
        self.selected = selected;
        Ok(t!("messages.share_success", id = id).to_string())
    }

    fn update_runtime_target(&mut self, target: &Printer) -> Result<(), String> {
        let previous = self
            .shared_printers
            .get(GATEWAY_PRINTER_ID)
            .cloned()
            .ok_or("AirPrinter gateway is not shared")?;
        let public_printer = Printer::gateway_from(target);
        let previous_public = Printer::gateway_from(&previous);
        let ipp = self
            .ipp_server
            .as_ref()
            .ok_or("IPP server is unavailable")?;
        let port = ipp.port()?;
        let secure_port = ipp.secure_port();
        let mdns = self
            .mdns
            .as_ref()
            .ok_or("mDNS broadcaster is unavailable")?;
        if let Err(error) = mdns.refresh_airprint(&public_printer, port, secure_port) {
            let mdns_rollback = mdns.refresh_airprint(&previous_public, port, secure_port);
            return match mdns_rollback {
                Ok(()) => Err(error),
                Err(rollback_error) => Err(format!(
                    "mDNS target refresh failed ({error}); restore failed ({rollback_error})"
                )),
            };
        }
        if let Err(error) = ipp.update_routed_printer(public_printer, &target.name) {
            let rollback = mdns.refresh_airprint(&previous_public, port, secure_port);
            return match rollback {
                Ok(()) => Err(error),
                Err(rollback_error) => Err(format!(
                    "IPP route update failed ({error}); mDNS restore failed ({rollback_error})"
                )),
            };
        }
        self.shared_printers
            .insert(GATEWAY_PRINTER_ID.into(), target.clone());
        Ok(())
    }

    pub fn stop(&mut self, id: &str) -> Result<(), String> {
        let stop_running = self
            .shared_printers
            .get(GATEWAY_PRINTER_ID)
            .is_some_and(|printer| printer.id == id);
        if !self.selected.contains(id) {
            return Err(t!("messages.printer_not_shared", id = id).to_string());
        }
        let mut selected = self.selected.clone();
        selected.remove(id);
        self.save_selected(&selected)?;
        if stop_running {
            if let Err(error) = self.stop_runtime(GATEWAY_PRINTER_ID) {
                return match self.save_selected(&self.selected) {
                    Ok(()) => Err(error),
                    Err(rollback_error) => Err(format!(
                        "AirPrinter stop failed ({error}); saved selection rollback also failed ({rollback_error})"
                    )),
                };
            }
        }
        self.selected = selected;
        Ok(())
    }

    /// Reconcile the remembered choice with what Windows currently exposes.
    /// A missing/offline queue is withdrawn and kept in preferences for retry.
    pub fn reconcile(&mut self, detected: &[Printer]) -> Vec<String> {
        let available: HashMap<_, _> = detected.iter().map(|p| (p.id.as_str(), p)).collect();
        let mut errors = Vec::new();
        if self.selected.len() > 1 {
            errors.push(
                "Multiple saved printer selections found; select one printer before restoring AirPrinter."
                    .into(),
            );
            return errors;
        }

        let selected_id = self.selected.iter().next().cloned();
        if let Some(current) = self.shared_printers.get(GATEWAY_PRINTER_ID).cloned() {
            if selected_id.as_deref() != Some(current.id.as_str()) {
                errors.push("AirPrinter runtime does not match the saved physical target.".into());
                return errors;
            }
            match available.get(current.id.as_str()) {
                None
                | Some(Printer {
                    status: PrinterStatus::Offline | PrinterStatus::Error(_),
                    ..
                }) => {
                    if let Err(error) = self.stop_runtime(GATEWAY_PRINTER_ID) {
                        errors.push(format!("{}: {error}", current.id));
                    }
                    return errors;
                }
                Some(target) if *target != &current => {
                    if let Err(error) = self.update_runtime_target(target) {
                        errors.push(format!("{}: {error}", target.id));
                    }
                }
                Some(_) => (),
            }
            return errors;
        }

        if let Some(id) = selected_id {
            if let Some(target) = available.get(id.as_str()) {
                if matches!(target.status, PrinterStatus::Online | PrinterStatus::Busy) {
                    if let Err(error) = self.share_runtime((**target).clone()) {
                        errors.push(format!("{id}: {error}"));
                    }
                }
            }
        }
        errors
    }

    pub fn is_shared(&self, id: &str) -> bool {
        self.shared_printers
            .get(GATEWAY_PRINTER_ID)
            .is_some_and(|printer| printer.id == id)
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
    fn multiple_legacy_preferences_are_not_restored_automatically() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shared-printers.json");
        let first = Printer {
            id: "legacy-a".into(),
            name: "Legacy A".into(),
            status: PrinterStatus::Online,
            capabilities: Default::default(),
        };
        let second = Printer {
            id: "legacy-b".into(),
            name: "Legacy B".into(),
            status: PrinterStatus::Online,
            capabilities: Default::default(),
        };
        std::fs::write(
            &path,
            serde_json::to_vec(&vec![first.id.clone(), second.id.clone()]).unwrap(),
        )
        .unwrap();
        let mut server = AirPrintServer::with_preferences(path.clone()).unwrap();
        let errors = server.reconcile(&[first, second]);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("Multiple saved printer selections"));
        assert!(server.get_shared_printers().is_empty());
        assert_eq!(server.selected.len(), 2);
        assert!(server.ipp_server.is_none());
    }

    #[test]
    fn failed_preference_save_does_not_start_or_select_a_target() {
        let dir = tempfile::tempdir().unwrap();
        let mut server = AirPrintServer::new();
        server.preferences = Some(dir.path().to_path_buf());
        server.selected.insert("previous-target".into());
        let target = Printer {
            id: "new-target".into(),
            name: "New target".into(),
            status: PrinterStatus::Online,
            capabilities: Default::default(),
        };

        assert!(server.share(target).is_err());
        assert_eq!(server.selected, HashSet::from(["previous-target".into()]));
        assert!(server.shared_printers.is_empty());
        assert!(server.mdns.is_none());
        assert!(server.ipp_server.is_none());

        let previous = Printer {
            id: "previous-target".into(),
            name: "Previous target".into(),
            status: PrinterStatus::Online,
            capabilities: Default::default(),
        };
        server
            .shared_printers
            .insert(GATEWAY_PRINTER_ID.into(), previous.clone());
        assert!(server.stop(&previous.id).is_err());
        assert!(server.is_shared(&previous.id));
        assert!(server.selected.contains(&previous.id));
    }

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
