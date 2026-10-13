use crate::models::printer::{PrinterAlert, PrinterCapabilities};
use crate::models::{Printer, PrinterStatus};

/// PRINTER_STATUS_* bits (winspool.h) surfaced as display-only warnings.
/// Windows does not expose ink/toner percentages via the spooler; these bits
/// are the only supply information available without vendor protocols.
const ALERT_BITS: &[(u32, PrinterAlert)] = &[
    (0x0000_0001, PrinterAlert::Paused),
    (0x0000_0008, PrinterAlert::PaperJam),
    (0x0000_0010, PrinterAlert::PaperOut),
    (0x0000_0020, PrinterAlert::ManualFeed),
    (0x0000_0040, PrinterAlert::PaperProblem),
    (0x0000_0800, PrinterAlert::OutputBinFull),
    (0x0001_0000, PrinterAlert::WarmingUp),
    (0x0002_0000, PrinterAlert::TonerLow),
    (0x0004_0000, PrinterAlert::NoToner),
    (0x0010_0000, PrinterAlert::UserIntervention),
    (0x0040_0000, PrinterAlert::DoorOpen),
    (0x0100_0000, PrinterAlert::PowerSave),
];

pub struct PrinterDetector;

impl PrinterDetector {
    pub fn new() -> Self {
        Self
    }

    #[cfg(target_os = "windows")]
    pub fn detect(&self) -> Result<Vec<Printer>, String> {
        use std::os::windows::process::CommandExt;
        let output = std::process::Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                include_str!("windows_printers.ps1"),
            ])
            .creation_flags(0x08000000)
            .output()
            .map_err(|e| e.to_string())?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).into_owned());
        }
        parse_printers(&output.stdout)
    }

    #[cfg(not(target_os = "windows"))]
    pub fn detect(&self) -> Result<Vec<Printer>, String> {
        Err("Printer bridge requires Windows".into())
    }

    pub fn detect_one(&self, id: &str) -> Result<Option<Printer>, String> {
        Ok(self.detect()?.into_iter().find(|printer| printer.id == id))
    }
}

fn parse_printers(data: &[u8]) -> Result<Vec<Printer>, String> {
    #[derive(serde::Deserialize)]
    struct Queue {
        name: String,
        status: u32,
        #[serde(default)]
        capabilities: PrinterCapabilities,
    }
    let queues: Vec<Queue> = serde_json::from_slice(data).map_err(|e| e.to_string())?;
    Ok(queues
        .into_iter()
        .map(|queue| {
            // The name itself is the stable, lossless queue ID; resource_path handles URI encoding.
            let status = if queue.status & (128 | 4096) != 0 {
                PrinterStatus::Offline
            } else if queue.status & (1 | 2 | 8 | 16 | 64 | 1048576 | 4194304) != 0 {
                PrinterStatus::Error(format!("Windows printer status: {}", queue.status))
            } else if queue.status & (512 | 1024 | 16384) != 0 {
                PrinterStatus::Busy
            } else {
                PrinterStatus::Online
            };
            Printer {
                id: queue.name.clone(),
                name: queue.name,
                status,
                capabilities: queue.capabilities,
                alerts: ALERT_BITS
                    .iter()
                    .filter(|(bit, _)| queue.status & bit != 0)
                    .map(|(_, alert)| *alert)
                    .collect(),
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unicode_queues_and_status_flags() {
        let printers = parse_printers(r#"[{"name":"办公室打印机共享队列","status":128,"capabilities":{"color":false,"duplex":true,"max_copies":99,"media":["iso_a4_210x297mm"],"default_media":"iso_a4_210x297mm"}}]"#.as_bytes()).unwrap();
        assert_eq!(printers[0].id, "办公室打印机共享队列");
        assert!(matches!(printers[0].status, PrinterStatus::Offline));
        // "capabilities" omitted entirely: the detector tolerates partial rows.
        let low_toner = serde_json::json!([{"name":"粉量低", "status":131072}]);
        let parsed = parse_printers(&serde_json::to_vec(&low_toner).unwrap()).unwrap();
        assert!(
            matches!(parsed[0].status, PrinterStatus::Online),
            "toner-low alone must not take the queue offline"
        );
        assert_eq!(parsed[0].alerts, vec![PrinterAlert::TonerLow]);
        let mut encoded = serde_json::to_value(&printers[0].capabilities).unwrap();
        encoded["color"] = serde_json::json!(true);
        let printing =
            serde_json::json!([{"name":"打印中", "status":1024, "capabilities":encoded}]);
        assert!(matches!(
            parse_printers(&serde_json::to_vec(&printing).unwrap()).unwrap()[0].status,
            PrinterStatus::Busy
        ));
        assert!(parse_printers(b"not JSON").is_err());
        assert!(parse_printers(b"[]").unwrap().is_empty());
    }
}
