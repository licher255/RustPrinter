use crate::models::{printer::truncate_utf8, Printer};
use crate::models::printer::MAKE_AND_MODEL;
use mdns_sd::{DaemonEvent, IfKind, ServiceDaemon, ServiceInfo};
use std::collections::{hash_map::DefaultHasher, HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::net::IpAddr;
use std::time::Duration;

pub struct MdnsBroadcaster {
    daemon: ServiceDaemon,
    registrations: HashMap<String, Vec<String>>,
    hostname: String,
}

impl MdnsBroadcaster {
    pub fn new() -> Result<Self, String> {
        let daemon = ServiceDaemon::new().map_err(|e| e.to_string())?;
        // The HTTP listener is currently IPv4-only; never publish unreachable AAAA records.
        daemon
            .disable_interface(IfKind::IPv6)
            .map_err(|e| e.to_string())?;
        let machine = std::env::var("COMPUTERNAME")
            .or_else(|_| std::env::var("HOSTNAME"))
            .unwrap_or_else(|_| "airprinter".into());
        let mut hasher = DefaultHasher::new();
        machine.hash(&mut hasher);
        let hostname = format!("airprinter-{:016x}.local.", hasher.finish());
        Ok(Self {
            daemon,
            registrations: HashMap::new(),
            hostname,
        })
    }

    pub fn hostname(&self) -> &str {
        &self.hostname
    }

    pub fn new_on_interface(address: IpAddr) -> Result<Self, String> {
        let mut broadcaster = Self::new()?;
        let interfaces = local_ip_address::list_afinet_netifas().map_err(|e| e.to_string())?;
        if !interfaces.iter().any(|(_, ip)| *ip == address) {
            return Err(format!(
                "Simulation interface {address} is not configured locally"
            ));
        }
        let mut hasher = DefaultHasher::new();
        address.hash(&mut hasher);
        let interface_hash = hasher.finish() as u32;
        broadcaster.hostname = format!(
            "airprinter-sim-{:x}-{interface_hash:08x}.local.",
            std::process::id()
        );
        for (_, ip) in interfaces {
            if ip != address {
                broadcaster
                    .daemon
                    .disable_interface(IfKind::Addr(ip))
                    .map_err(|e| e.to_string())?;
            }
        }
        Ok(broadcaster)
    }

    fn service_info(
        printer: &Printer,
        hostname: &str,
        port: u16,
        service_type: &str,
    ) -> Result<ServiceInfo, String> {
        let name = printer.dns_sd_name(hostname);
        let mut properties = vec![
            ("txtvers", "1".to_string()),
            ("rp", printer.resource_path()),
            ("qtotal", "1".to_string()),
            ("UUID", printer.uuid(hostname)),
            ("air", "none".to_string()),
            // 64 bytes keeps the whole TXT record within the 400-octet mDNS
            // budget (PWG 5100.14 section 4.2.4) once "adminurl" is included.
            ("ty", truncate_utf8(MAKE_AND_MODEL, 64).to_string()),
            ("product", format!("({})", truncate_utf8(MAKE_AND_MODEL, 32))),
            ("kind", "document".to_string()),
            ("priority", "0".to_string()),
            (
                "pdl",
                "application/pdf,image/urf,image/jpeg,image/pwg-raster".to_string(),
            ),
            ("URF", printer.capabilities.urf_features().join(",")),
            (
                "Color",
                if printer.capabilities.color { "T" } else { "F" }.to_string(),
            ),
            (
                "Duplex",
                if printer.capabilities.duplex {
                    "T"
                } else {
                    "F"
                }
                .to_string(),
            ),
            (
                "Copies",
                if printer.capabilities.max_copies > 1 {
                    "T"
                } else {
                    "F"
                }
                .to_string(),
            ),
        ];
        if service_type.contains("_ipps.") {
            properties.push(("TLS", "1.2".into()));
        }
        // PWG 5100.14 Table 3: "adminurl" mirrors the IPP "printer-more-info"
        // attribute. The "_printer._tcp" flagship service uses port 0 (no LPD),
        // so a usable URL can only be advertised on real IPP/IPPS services.
        if port != 0 {
            properties.push((
                "adminurl",
                format!(
                    "http://{}:{}/",
                    hostname.trim_end_matches('.'),
                    port
                ),
            ));
        }
        ServiceInfo::new(
            service_type,
            &name,
            hostname,
            "",
            port,
            properties.as_slice(),
        )
        .map(|info| info.enable_addr_auto())
        .map_err(|e| e.to_string())
    }

    fn service_infos(
        printer: &Printer,
        hostname: &str,
        port: u16,
        secure_port: Option<u16>,
    ) -> Result<Vec<ServiceInfo>, String> {
        let ipp = Self::service_info(printer, hostname, port, "_universal._sub._ipp._tcp.local.")?;
        let flagship = Self::service_info(printer, hostname, 0, "_printer._tcp.local.")?;
        let mut services = vec![ipp, flagship];
        if let Some(port) = secure_port {
            services.push(Self::service_info(
                printer,
                hostname,
                port,
                "_universal._sub._ipps._tcp.local.",
            )?);
        }
        Ok(services)
    }

    fn fullname_conflicts(
        registrations: &HashMap<String, Vec<String>>,
        services: &[ServiceInfo],
    ) -> bool {
        services.iter().any(|service| {
            registrations
                .values()
                .any(|fullnames| {
                    fullnames
                        .iter()
                        .any(|name| name.eq_ignore_ascii_case(service.get_fullname()))
                })
        })
    }

    pub fn broadcast_airprint(&mut self, printer: &Printer, port: u16) -> Result<(), String> {
        self.broadcast_with_tls(printer, port, None)
    }

    pub fn broadcast_with_tls(
        &mut self,
        printer: &Printer,
        port: u16,
        secure_port: Option<u16>,
    ) -> Result<(), String> {
        if self.registrations.contains_key(&printer.id) {
            return Err("Printer already advertised".into());
        }
        // A subtype's PTR targets the base _ipp instance. Register it ONCE, not
        // as a second service with a different SRV target (which overwrites it).
        let services = Self::service_infos(printer, &self.hostname, port, secure_port)?;
        if Self::fullname_conflicts(&self.registrations, &services) {
            return Err("Another shared printer already uses this DNS-SD instance name".into());
        }
        let mut fullnames: Vec<String> = Vec::new();
        let events = self.daemon.monitor().map_err(|e| e.to_string())?;
        for info in services {
            let fullname = info.get_fullname().to_string();
            if let Err(error) = self.daemon.register(info) {
                for registered in &fullnames {
                    let _ = self.daemon.unregister(registered);
                }
                return Err(error.to_string());
            }
            fullnames.push(fullname);
        }
        let mut announced = HashSet::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while announced.len() < fullnames.len() {
            match events.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
            {
                Ok(DaemonEvent::Announce(name, _)) if fullnames.contains(&name) => {
                    announced.insert(name);
                }
                Ok(DaemonEvent::Error(error)) => {
                    for name in &fullnames {
                        let _ = self.daemon.unregister(name);
                    }
                    return Err(error.to_string());
                }
                Ok(_) => (),
                Err(_) => {
                    for name in &fullnames {
                        let _ = self.daemon.unregister(name);
                    }
                    return Err("No mDNS announcement on an available IPv4 interface".into());
                }
            }
        }
        self.registrations.insert(printer.id.clone(), fullnames);
        Ok(())
    }

    pub fn stop(&mut self, printer_id: &str) -> Result<(), String> {
        if let Some(fullnames) = self.registrations.get(printer_id) {
            for fullname in fullnames {
                self.daemon
                    .unregister(fullname)
                    .map_err(|e| e.to_string())?
                    .recv_timeout(Duration::from_secs(2))
                    .map_err(|e| e.to_string())?;
            }
        }
        self.registrations.remove(printer_id);
        Ok(())
    }

    pub fn refresh_airprint(
        &self,
        printer: &Printer,
        port: u16,
        secure_port: Option<u16>,
    ) -> Result<(), String> {
        if !self.registrations.contains_key(&printer.id) {
            return Err("Printer is not advertised".into());
        }
        for service in Self::service_infos(printer, &self.hostname, port, secure_port)? {
            self.daemon.register(service).map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}

impl Drop for MdnsBroadcaster {
    fn drop(&mut self) {
        let ids: Vec<_> = self.registrations.keys().cloned().collect();
        for id in ids {
            let _ = self.stop(&id);
        }
        if let Ok(done) = self.daemon.shutdown() {
            let _ = done.recv_timeout(Duration::from_secs(2));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::PrinterStatus;

    #[test]
    fn subtype_uses_base_instance_and_queue_specific_resource() {
        let printer = Printer {
            id: "queue-a".into(),
            name: "办公室.彩色打印机".repeat(10),
            status: PrinterStatus::Online,
            capabilities: Default::default(),
            alerts: Vec::new(),
        };
        let info = MdnsBroadcaster::service_info(
            &printer,
            "test-pc.local.",
            631,
            "_universal._sub._ipp._tcp.local.",
        )
        .unwrap();
        assert_eq!(info.get_type(), "_ipp._tcp.local.");
        assert_eq!(
            info.get_subtype().as_deref(),
            Some("_universal._sub._ipp._tcp.local.")
        );
        assert!(info.get_fullname().ends_with("._ipp._tcp.local."));
        assert!(info.get_fullname().split('.').next().unwrap().len() <= 63);
        assert_eq!(info.get_hostname(), "test-pc.local.");
        assert_eq!(
            info.get_property_val_str("rp"),
            Some(printer.resource_path().as_str())
        );
        assert_eq!(
            info.get_property_val_str("product"),
            Some(format!("({})", truncate_utf8(MAKE_AND_MODEL, 32)).as_str())
        );
        let txt_bytes: usize = info
            .get_properties()
            .iter()
            .map(|entry| 2 + entry.key().len() + entry.val().map_or(0, |value| value.len()))
            .sum();
        assert!(txt_bytes <= 400, "TXT record grew to {txt_bytes} octets");
        assert!(info.is_addr_auto());
        let secure = MdnsBroadcaster::service_info(
            &printer,
            "test-pc.local.",
            8631,
            "_universal._sub._ipps._tcp.local.",
        )
        .unwrap();
        assert_eq!(secure.get_type(), "_ipps._tcp.local.");
        assert_eq!(
            secure.get_subtype().as_deref(),
            Some("_universal._sub._ipps._tcp.local.")
        );
        assert_eq!(
            secure.get_property_val_str("UUID"),
            info.get_property_val_str("UUID")
        );
        assert_eq!(secure.get_property_val_str("TLS"), Some("1.2"));
        assert_eq!(info.get_property_val_str("TLS"), None);
    }

    #[test]
    fn duplicate_instance_names_are_rejected_before_registration() {
        let first = Printer {
            id: "queue-one".into(),
            name: "Office Printer".into(),
            status: PrinterStatus::Online,
            capabilities: Default::default(),
            alerts: Vec::new(),
        };
        let second = Printer {
            id: "queue-two".into(),
            name: "office printer".into(),
            ..first.clone()
        };
        let existing = MdnsBroadcaster::service_infos(&first, "pc.local.", 631, None).unwrap();
        let candidate = MdnsBroadcaster::service_infos(&second, "pc.local.", 632, None).unwrap();
        let registrations = HashMap::from([(
            first.id,
            existing
                .iter()
                .map(|service| service.get_fullname().to_string())
                .collect(),
        )]);
        assert!(MdnsBroadcaster::fullname_conflicts(&registrations, &candidate));
    }

    #[test]
    fn gateway_target_switch_keeps_one_stable_discovery_identity() {
        let mut target_a = Printer {
            id: "physical-a".into(),
            name: "Office A".into(),
            status: PrinterStatus::Online,
            capabilities: Default::default(),
            alerts: Vec::new(),
        };
        target_a.capabilities.color = true;
        let mut target_b = Printer {
            id: "physical-b".into(),
            name: "Office B".into(),
            status: PrinterStatus::Online,
            capabilities: Default::default(),
            alerts: Vec::new(),
        };
        target_b.capabilities.duplex = true;
        let gateway_a = Printer::gateway_from(&target_a);
        let gateway_b = Printer::gateway_from(&target_b);
        let services_a =
            MdnsBroadcaster::service_infos(&gateway_a, "pc.local.", 631, Some(8631)).unwrap();
        let services_b =
            MdnsBroadcaster::service_infos(&gateway_b, "pc.local.", 631, Some(8631)).unwrap();
        assert_eq!(services_a.len(), 3);
        assert_eq!(services_b.len(), 3);
        let labels_a: HashSet<_> = services_a
            .iter()
            .map(|info| info.get_fullname().split('.').next().unwrap().to_string())
            .collect();
        let labels_b: HashSet<_> = services_b
            .iter()
            .map(|info| info.get_fullname().split('.').next().unwrap().to_string())
            .collect();
        assert_eq!(labels_a.len(), 1);
        assert_eq!(labels_a, labels_b);
        for (a, b) in services_a.iter().zip(&services_b) {
            assert_eq!(a.get_fullname(), b.get_fullname());
            assert_eq!(a.get_property_val_str("rp"), Some("ipp/print"));
            assert_eq!(
                a.get_property_val_str("UUID"),
                b.get_property_val_str("UUID")
            );
            assert_eq!(a.get_property_val_str("ty"), Some(MAKE_AND_MODEL));
        }
        assert_eq!(services_a[0].get_property_val_str("Color"), Some("T"));
        assert_eq!(services_b[0].get_property_val_str("Color"), Some("F"));
        assert_eq!(services_b[0].get_property_val_str("Duplex"), Some("T"));
    }

    #[test]
    #[ignore = "Publishes two short-lived simulated printers over local mDNS"]
    fn mdns_two_queues_resolve_and_withdraw() {
        use mdns_sd::ServiceEvent;
        let mut broadcaster = MdnsBroadcaster::new().unwrap();
        let browser = ServiceDaemon::new().unwrap();
        let events = browser.browse("_universal._sub._ipp._tcp.local.").unwrap();
        let a = Printer {
            id: format!("simulation-a-{}", std::process::id()),
            name: "AirPrinter simulation A".into(),
            status: PrinterStatus::Online,
            capabilities: Default::default(),
            alerts: Vec::new(),
        };
        let b = Printer {
            id: format!("simulation-b-{}", std::process::id()),
            name: "AirPrinter simulation B".into(),
            status: PrinterStatus::Online,
            capabilities: Default::default(),
            alerts: Vec::new(),
        };
        broadcaster.broadcast_airprint(&a, 18631).unwrap();
        broadcaster.broadcast_airprint(&b, 18632).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut resolved = HashMap::new();
        while resolved.len() < 2 {
            let event = events
                .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                .expect("mDNS resolution timed out");
            if let ServiceEvent::ServiceResolved(info) = event {
                let resource = info.get_property_val_str("rp").unwrap_or("").to_string();
                if resource == a.resource_path() || resource == b.resource_path() {
                    assert_eq!(info.get_hostname(), broadcaster.hostname());
                    assert!(!info.get_addresses().is_empty());
                    assert_eq!(
                        info.get_port(),
                        if resource == a.resource_path() {
                            18631
                        } else {
                            18632
                        }
                    );
                    resolved.insert(resource, info.get_fullname().to_string());
                }
            }
        }
        let name_a = resolved.get(&a.resource_path()).unwrap();
        broadcaster.stop(&a.id).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if let ServiceEvent::ServiceRemoved(_, name) = events
                .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                .expect("mDNS goodbye timed out")
            {
                if &name == name_a {
                    break;
                }
            }
        }
        assert!(broadcaster.registrations.contains_key(&b.id));
        broadcaster.stop(&b.id).unwrap();
        browser
            .shutdown()
            .unwrap()
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
    }
}
