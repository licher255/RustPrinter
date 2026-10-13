//! Local protocol fixture: a simulated queue that accepts discovery and IPP
//! attribute requests. No Print-Job is submitted by the JS client.
use airprinter::models::{Printer, PrinterStatus};
use airprinter::services::AirPrintServer;
use std::io::{self, BufRead};
use std::net::IpAddr;

fn main() -> Result<(), String> {
    let address: IpAddr = std::env::args()
        .nth(1)
        .ok_or("Pass an isolated virtual adapter IPv4 address")?
        .parse()
        .map_err(|e| format!("Invalid simulation address: {e}"))?;
    let isolated = local_ip_address::list_afinet_netifas()
        .map_err(|e| e.to_string())?
        .into_iter()
        .any(|(name, ip)| {
            ip == address
                && (name.to_ascii_lowercase().contains("vethernet")
                    || name.to_ascii_lowercase().contains("vmnet1")
                    || name.to_ascii_lowercase().contains("host-only"))
        });
    if !isolated {
        return Err("Use a host-only virtual adapter for the isolated simulation".into());
    }
    let name = format!("AirPrinter JS simulation {}", std::process::id());
    let printer = Printer {
        id: name.clone(),
        name: name.clone(),
        status: PrinterStatus::Online,
        capabilities: Default::default(),
        alerts: Vec::new(),
    };
    let public_printer = Printer::gateway_from(&printer);
    let mut server = AirPrintServer::isolated_for_test(address);
    server.share(printer.clone())?;
    let port = server.ipp_port().ok_or("IPP listener is missing")?;
    println!(
        "SIM_READY {}",
        serde_json::json!({ "name": public_printer.name, "id": public_printer.id, "port": port, "rp": public_printer.resource_path(), "address": address })
    );
    let mut stop = String::new();
    let _ = io::stdin().lock().read_line(&mut stop);
    server.stop(&printer.id)?;
    println!("SIM_STOPPED");
    Ok(())
}
