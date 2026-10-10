use serde::{Deserialize, Serialize};

// Bump when an incompatible discovery/IPP profile change requires clients to refresh cached state.
pub const DISCOVERY_REVISION: u8 = 7;
pub const GATEWAY_PRINTER_ID: &str = "airprinter-gateway";

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Printer {
    pub name: String,
    pub id: String,
    pub status: PrinterStatus,
    #[serde(default)]
    pub capabilities: PrinterCapabilities,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct PrinterCapabilities {
    pub color: bool,
    pub duplex: bool,
    pub max_copies: i32,
    pub media: Vec<String>,
    pub default_media: String,
    #[serde(default = "default_media_sizes")]
    pub media_sizes: Vec<MediaSize>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct MediaSize {
    pub name: String,
    /// IPP dimensions, in hundredths of a millimetre.
    pub width: i32,
    pub height: i32,
    pub windows_kind: i32,
}

fn default_media_sizes() -> Vec<MediaSize> {
    vec![MediaSize {
        name: "iso_a4_210x297mm".into(),
        width: 21000,
        height: 29700,
        windows_kind: 9,
    }]
}

impl Default for PrinterCapabilities {
    fn default() -> Self {
        Self {
            color: false,
            duplex: false,
            max_copies: 1,
            media: vec!["iso_a4_210x297mm".into()],
            default_media: "iso_a4_210x297mm".into(),
            media_sizes: default_media_sizes(),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum PrinterStatus {
    Online,
    Offline,
    Busy,
    Error(String),
}

impl PrinterStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            PrinterStatus::Online => "online",
            PrinterStatus::Offline => "offline",
            PrinterStatus::Busy => "busy",
            PrinterStatus::Error(_) => "error",
        }
    }
}

impl Printer {
    pub fn gateway_from(target: &Self) -> Self {
        Self {
            name: "AirPrinter".into(),
            id: GATEWAY_PRINTER_ID.into(),
            status: target.status.clone(),
            capabilities: target.capabilities.clone(),
        }
    }

    /// A bounded, stable URI even when a Windows queue has a long Unicode name.
    pub fn resource_path(&self) -> String {
        if self.id == GATEWAY_PRINTER_ID {
            return "ipp/print".into();
        }
        format!(
            "ipp/print/{}",
            uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, self.id.as_bytes()).simple()
        )
    }

    pub fn uuid(&self, hostname: &str) -> String {
        uuid::Uuid::new_v5(
            &uuid::Uuid::NAMESPACE_DNS,
            format!("{hostname}/{}:{DISCOVERY_REVISION}", self.id).as_bytes(),
        )
        .to_string()
    }
}

pub fn truncate_utf8(value: &str, max_bytes: usize) -> &str {
    let mut end = max_bytes.min(value.len());
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_names_and_resource_paths_are_safe() {
        assert_eq!(truncate_utf8("办公室打印机", 8), "办公");
        let printer = Printer {
            name: "办公室".into(),
            id: "a/b %中".into(),
            status: PrinterStatus::Online,
            capabilities: PrinterCapabilities::default(),
        };
        assert_eq!(printer.resource_path().len(), 42);
        assert!(printer.resource_path().is_ascii());
        assert_eq!(printer.clone().resource_path(), printer.resource_path());
    }

    #[test]
    fn gateway_identity_and_uri_are_stable_while_capabilities_follow_target() {
        let mut first = Printer {
            name: "Office A".into(),
            id: "queue-a".into(),
            status: PrinterStatus::Online,
            capabilities: PrinterCapabilities::default(),
        };
        first.capabilities.color = true;
        let mut second = Printer {
            name: "Office B".into(),
            id: "queue-b".into(),
            status: PrinterStatus::Busy,
            capabilities: PrinterCapabilities::default(),
        };
        second.capabilities.duplex = true;

        let gateway_a = Printer::gateway_from(&first);
        let gateway_b = Printer::gateway_from(&second);
        assert_eq!(gateway_a.id, GATEWAY_PRINTER_ID);
        assert_eq!(gateway_a.name, "AirPrinter");
        assert_eq!(gateway_a.resource_path(), "ipp/print");
        assert_eq!(gateway_a.resource_path(), gateway_b.resource_path());
        assert_eq!(gateway_a.uuid("pc.local."), gateway_b.uuid("pc.local."));
        assert!(gateway_a.capabilities.color);
        assert!(!gateway_a.capabilities.duplex);
        assert_eq!(gateway_b.status, PrinterStatus::Busy);
        assert!(gateway_b.capabilities.duplex);
        assert!(!gateway_b.capabilities.color);
    }
}
