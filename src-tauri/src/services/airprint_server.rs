use crate::models::{Printer, PrinterStatus};
use crate::services::{ipp::IppServer, MdnsBroadcaster};
use rust_i18n::t;
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::net::IpAddr;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Consecutive bad polls required before a shared queue is withdrawn.
/// Sleeping USB printers and transient detection gaps must not flap the
/// announcement on every reconcile cycle.
const OFFLINE_WITHDRAW_POLLS: u32 = 2;
/// Consecutive identical capability readings required before re-announcing.
/// A flapping driver report must not re-register the mDNS services.
const CAPABILITY_CONFIRM_POLLS: u32 = 2;
const SHARE_RETRY_BASE_DELAY: Duration = Duration::from_secs(30);
const SHARE_RETRY_MAX_DELAY: Duration = Duration::from_secs(600);

#[derive(Clone, Copy)]
struct PendingCapability {
    color: bool,
    duplex: bool,
    polls: u32,
}

struct RetryState {
    attempts: u32,
    next_retry: Instant,
}

/// Exponential backoff between automatic re-share attempts:
/// 30s, 60s, 120s ... capped at 10 minutes.
fn retry_delay(attempts: u32) -> Duration {
    let shift = attempts.saturating_sub(1).min(10);
    let secs = SHARE_RETRY_BASE_DELAY
        .as_secs()
        .saturating_mul(1u64 << shift);
    Duration::from_secs(secs.min(SHARE_RETRY_MAX_DELAY.as_secs()))
}

pub struct AirPrintServer {
    shared_printers: HashMap<String, Printer>,
    selected: HashSet<String>,
    preferences: Option<PathBuf>,
    bind_address: String,
    mdns_interface: Option<IpAddr>,
    mdns: Option<MdnsBroadcaster>,
    ipp_server: Option<IppServer>,
    receive_directory: Option<PathBuf>,
    offline_streaks: HashMap<String, u32>,
    pending_capabilities: HashMap<String, PendingCapability>,
    share_retries: HashMap<String, RetryState>,
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
            offline_streaks: HashMap::new(),
            pending_capabilities: HashMap::new(),
            share_retries: HashMap::new(),
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
        crate::services::diagnostics::record(&format!(
            "[AirPrint] share requested: id={} name={:?} status={}",
            printer.id,
            printer.name,
            printer.status.as_str()
        ));
        crate::services::diagnostics::record(&format!(
            "[AirPrint] capabilities: id={} color={} duplex={} max_copies={} default_media={} media=[{}] alerts={:?}",
            printer.id,
            printer.capabilities.color,
            printer.capabilities.duplex,
            printer.capabilities.max_copies,
            printer.capabilities.default_media,
            printer.capabilities.media.join(", "),
            printer.alerts
        ));
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
                let ipp = self.ipp_server.as_ref().unwrap();
                crate::services::diagnostics::record(&format!(
                    "[AirPrint] listeners ready: ipp_port={} ipps_port={:?} mdns_hostname={}",
                    ipp.port().unwrap_or(0),
                    ipp.secure_port(),
                    self.mdns.as_ref().unwrap().hostname()
                ));
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
        match &result {
            Ok(()) => crate::services::diagnostics::record(&format!(
                "[AirPrint] share succeeded: id={}",
                printer.id
            )),
            Err(error) => crate::services::diagnostics::record(&format!(
                "[AirPrint] share failed: id={} error={error}",
                printer.id
            )),
        }
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
        self.offline_streaks.remove(id);
        self.pending_capabilities.remove(id);
        self.share_retries.remove(id);
        if self.shared_printers.is_empty() {
            self.mdns = None;
            self.ipp_server = None;
        }
        crate::services::diagnostics::record(&format!("[AirPrint] share stopped: id={id}"));
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

    /// Whether an automatic re-share attempt is due for this queue.
    fn retry_ready(&self, id: &str) -> bool {
        match self.share_retries.get(id) {
            None => true,
            Some(state) => Instant::now() >= state.next_retry,
        }
    }

    /// Reconcile the remembered choice with what Windows currently exposes.
    /// A missing/offline queue is withdrawn only after consecutive bad polls,
    /// and a failed queue is retried with exponential backoff; both stay in
    /// preferences so a recovering printer comes back on its own.
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
                    // Debounce: withdraw only after consecutive bad polls, so a
                    // sleeping printer or one flaky detection does not flap the
                    // announcement every cycle.
                    let streak = {
                        let streak = self.offline_streaks.entry(id.clone()).or_insert(0);
                        *streak += 1;
                        *streak
                    };
                    if streak < OFFLINE_WITHDRAW_POLLS {
                        crate::services::diagnostics::record(&format!(
                            "[AirPrint] offline debounce: id={id} streak={streak}/{OFFLINE_WITHDRAW_POLLS}; announcement kept"
                        ));
                        continue;
                    }
                    if let Err(error) = self.stop_runtime(&id) {
                        errors.push(format!("{id}: {error}"));
                    }
                }
                Some(printer) => {
                    self.offline_streaks.remove(&id);
                    let previous = self.shared_printers[&id].clone();
                    if previous == **printer {
                        // A poll matching the shared profile breaks the streak:
                        // the staged capability reading was not seen again.
                        self.pending_capabilities.remove(&id);
                        continue;
                    }
                    let capability_changed = previous.capabilities.color
                        != printer.capabilities.color
                        || previous.capabilities.duplex != printer.capabilities.duplex;
                    if capability_changed {
                        // Require the same reading on consecutive polls before
                        // re-announcing; a flapping driver report never confirms.
                        let polls = match self.pending_capabilities.get(&id) {
                            Some(pending)
                                if pending.color == printer.capabilities.color
                                    && pending.duplex == printer.capabilities.duplex =>
                            {
                                pending.polls + 1
                            }
                            _ => 1,
                        };
                        if polls < CAPABILITY_CONFIRM_POLLS {
                            self.pending_capabilities.insert(
                                id.clone(),
                                PendingCapability {
                                    color: printer.capabilities.color,
                                    duplex: printer.capabilities.duplex,
                                    polls,
                                },
                            );
                            crate::services::diagnostics::record(&format!(
                                "[AirPrint] capability change pending: id={id} color={} duplex={} poll={polls}/{CAPABILITY_CONFIRM_POLLS}",
                                printer.capabilities.color,
                                printer.capabilities.duplex
                            ));
                            continue;
                        }
                        self.pending_capabilities.remove(&id);
                        if let Some(mdns) = &self.mdns {
                            let port = self
                                .ipp_server
                                .as_ref()
                                .and_then(|ipp| ipp.port().ok())
                                .unwrap_or(631);
                            let secure_port =
                                self.ipp_server.as_ref().and_then(|ipp| ipp.secure_port());
                            if let Err(error) = mdns.refresh_airprint(printer, port, secure_port)
                            {
                                errors.push(format!("{id}: {error}"));
                                continue;
                            }
                        }
                        crate::services::diagnostics::record(&format!(
                            "[AirPrint] capability change confirmed: id={id}; announcement refreshed"
                        ));
                    } else {
                        self.pending_capabilities.remove(&id);
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
        let to_restore: Vec<_> = self
            .selected
            .iter()
            .filter(|id| !self.shared_printers.contains_key(*id))
            .filter(|id| self.retry_ready(id))
            .filter_map(|id| available.get(id.as_str()))
            .filter(|printer| matches!(printer.status, PrinterStatus::Online | PrinterStatus::Busy))
            .cloned()
            .cloned()
            .collect();
        for printer in to_restore {
            let id = printer.id.clone();
            match self.share_runtime(printer) {
                Ok(()) => {
                    self.share_retries.remove(&id);
                }
                Err(error) => {
                    let attempts = {
                        let retry = self.share_retries.entry(id.clone()).or_insert(RetryState {
                            attempts: 0,
                            next_retry: Instant::now(),
                        });
                        retry.attempts += 1;
                        retry.next_retry = Instant::now() + retry_delay(retry.attempts);
                        retry.attempts
                    };
                    crate::services::diagnostics::record(&format!(
                        "[AirPrint] share retry #{attempts} failed: id={id}; next attempt in {}s: {error}",
                        retry_delay(attempts).as_secs()
                    ));
                    errors.push(format!("{id}: {error}"));
                }
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
            alerts: Vec::new(),
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
        assert!(
            !restarted.get_shared_printers()[0].capabilities.color,
            "first poll only stages a capability change"
        );
        assert!(restarted.reconcile(&[printer.clone()]).is_empty());
        assert!(restarted.get_shared_printers()[0].capabilities.color);

        let mut offline = printer.clone();
        offline.status = PrinterStatus::Offline;
        assert!(restarted.reconcile(&[offline.clone()]).is_empty());
        assert!(
            restarted.is_shared(&id),
            "a single offline poll must not withdraw the announcement"
        );
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

    fn shared_offline_server(printer: &Printer) -> AirPrintServer {
        // No mDNS/IPP runtime: reconcile's debounce bookkeeping is testable
        // without touching the network because refresh/update skip None.
        let mut server = AirPrintServer::new();
        server
            .shared_printers
            .insert(printer.id.clone(), printer.clone());
        server.selected.insert(printer.id.clone());
        server
    }

    #[test]
    fn offline_withdrawal_requires_consecutive_polls_and_flapping_resets() {
        let printer = Printer {
            id: "queue-debounce".into(),
            name: "Debounce".into(),
            status: PrinterStatus::Online,
            capabilities: Default::default(),
            alerts: Vec::new(),
        };
        let mut server = shared_offline_server(&printer);
        let mut offline = printer.clone();
        offline.status = PrinterStatus::Offline;

        assert!(server.reconcile(&[offline.clone()]).is_empty());
        assert!(
            server.is_shared("queue-debounce"),
            "single offline poll must keep the announcement"
        );

        // Flap back to online once: the streak resets instead of accumulating.
        assert!(server.reconcile(&[printer.clone()]).is_empty());
        assert!(server.reconcile(&[offline.clone()]).is_empty());
        assert!(
            server.is_shared("queue-debounce"),
            "a flap must reset the offline streak"
        );

        // Missing from detection also counts; two consecutive bad polls withdraw.
        assert!(server.reconcile(&[offline]).is_empty());
        assert!(!server.is_shared("queue-debounce"));

        // A queue missing from detection is debounced the same way.
        let mut server = shared_offline_server(&printer);
        assert!(server.reconcile(&[]).is_empty());
        assert!(server.is_shared("queue-debounce"));
        assert!(server.reconcile(&[]).is_empty());
        assert!(!server.is_shared("queue-debounce"));
    }

    #[test]
    fn capability_change_requires_two_consistent_polls() {
        let printer = Printer {
            id: "queue-caps".into(),
            name: "Caps".into(),
            status: PrinterStatus::Online,
            capabilities: Default::default(),
            alerts: Vec::new(),
        };
        let mut server = shared_offline_server(&printer);
        let mut color = printer.clone();
        color.capabilities.color = true;

        assert!(server.reconcile(&[color.clone()]).is_empty());
        assert!(
            !server.get_shared_printers()[0].capabilities.color,
            "first poll stages the change without re-announcing"
        );
        assert!(server.reconcile(&[color.clone()]).is_empty());
        assert!(server.get_shared_printers()[0].capabilities.color);

        // A flapping reading never reaches confirmation.
        let mut plain = color.clone();
        plain.capabilities.color = false;
        assert!(server.reconcile(&[plain.clone()]).is_empty());
        assert!(server.reconcile(&[color.clone()]).is_empty());
        assert!(server.reconcile(&[plain]).is_empty());
        assert!(
            server.get_shared_printers()[0].capabilities.color,
            "flapping must never confirm a capability change"
        );
    }

    #[test]
    fn non_capability_changes_apply_immediately() {
        let printer = Printer {
            id: "queue-status".into(),
            name: "Status".into(),
            status: PrinterStatus::Online,
            capabilities: Default::default(),
            alerts: Vec::new(),
        };
        let mut server = shared_offline_server(&printer);
        let mut busy = printer.clone();
        busy.status = PrinterStatus::Busy;
        assert!(server.reconcile(&[busy]).is_empty());
        assert!(matches!(
            server.get_shared_printers()[0].status,
            PrinterStatus::Busy
        ));
    }

    #[test]
    fn retry_delay_doubles_and_caps() {
        assert_eq!(retry_delay(1), Duration::from_secs(30));
        assert_eq!(retry_delay(2), Duration::from_secs(60));
        assert_eq!(retry_delay(3), Duration::from_secs(120));
        assert_eq!(retry_delay(4), Duration::from_secs(240));
        assert_eq!(retry_delay(5), Duration::from_secs(480));
        assert_eq!(retry_delay(6), Duration::from_secs(600));
        assert_eq!(retry_delay(20), Duration::from_secs(600));
    }

    #[test]
    fn failed_restore_waits_for_backoff_before_retrying() {
        let mut server = AirPrintServer::new();
        server.selected.insert("queue-retry".to_string());
        assert!(server.retry_ready("queue-retry"));

        server.share_retries.insert(
            "queue-retry".to_string(),
            RetryState {
                attempts: 3,
                next_retry: Instant::now() + Duration::from_secs(3600),
            },
        );
        assert!(!server.retry_ready("queue-retry"));
        let printer = Printer {
            id: "queue-retry".into(),
            name: "Retry".into(),
            status: PrinterStatus::Online,
            capabilities: Default::default(),
            alerts: Vec::new(),
        };
        let errors = server.reconcile(&[printer]);
        assert!(
            errors.is_empty(),
            "backoff suppresses the attempt without an error"
        );
        assert!(
            !server.is_shared("queue-retry"),
            "no share attempt may happen during backoff"
        );

        server.share_retries.insert(
            "queue-retry".to_string(),
            RetryState {
                attempts: 3,
                next_retry: Instant::now() - Duration::from_secs(1),
            },
        );
        assert!(server.retry_ready("queue-retry"));
    }
}
