use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

static LOG: OnceLock<Mutex<PathBuf>> = OnceLock::new();
const MAX_LOG_BYTES: u64 = 2 * 1024 * 1024;

pub fn init(directory: &Path) -> io::Result<()> {
    fs::create_dir_all(directory)?;
    let path = directory.join("print-diagnostics.jsonl");
    OpenOptions::new().create(true).append(true).open(&path)?;
    let _ = LOG.set(Mutex::new(path));
    record(&format!(
        "[AirPrinter] diagnostics started pid={} version={}",
        std::process::id(),
        env!("CARGO_PKG_VERSION")
    ));
    Ok(())
}

/// Save request metadata and errors, never document bytes or TLS keys.
pub fn record(message: &str) {
    eprintln!("{message}");
    let Some(log) = LOG.get() else { return };
    let path = log.lock().unwrap_or_else(|error| error.into_inner());
    if let Err(error) = append(&path, message, MAX_LOG_BYTES) {
        eprintln!("[AirPrinter] could not save print diagnostics: {error}");
    }
}

/// Absolute path of the active diagnostics log file.
pub fn file_path() -> Option<PathBuf> {
    LOG.get()
        .map(|log| log.lock().unwrap_or_else(|error| error.into_inner()).clone())
}

/// Read the newest log content, bounded so the frontend can display/copy it.
pub fn tail(max_bytes: usize, max_lines: usize) -> Result<String, String> {
    let Some(path) = file_path() else {
        return Err("Diagnostics logging is not initialized".into());
    };
    tail_from(&path, max_bytes, max_lines)
}

fn tail_from(path: &Path, max_bytes: usize, max_lines: usize) -> Result<String, String> {
    let data = fs::read(path).map_err(|error| error.to_string())?;
    let mut start = data.len().saturating_sub(max_bytes);
    // Never split a multi-byte UTF-8 character at the cut point.
    while start < data.len() && (data[start] & 0b1100_0000) == 0b1000_0000 {
        start += 1;
    }
    let text = String::from_utf8_lossy(&data[start..]);
    let lines: Vec<&str> = text.lines().collect();
    let kept = &lines[lines.len().saturating_sub(max_lines)..];
    Ok(kept.join("\n"))
}

/// Record machine and network facts once at startup: exactly the data needed
/// when a phone discovers the printer but cannot fetch its attributes.
pub fn log_environment(config_dir: &Path) {
    let exe = std::env::current_exe()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| "unknown".into());
    let computer = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "unknown".into());
    record(&format!(
        "[AirPrinter] environment version={} os={} arch={} computer={computer}",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
    ));
    record(&format!("[AirPrinter] exe={exe}"));
    record(&format!("[AirPrinter] config_dir={}", config_dir.display()));
    match local_ip_address::list_afinet_netifas() {
        Ok(interfaces) => {
            let ipv4: Vec<String> = interfaces
                .iter()
                .filter(|(_, ip)| ip.is_ipv4())
                .map(|(name, ip)| format!("{name}={ip}"))
                .collect();
            record(&format!(
                "[AirPrinter] ipv4_interfaces={}",
                ipv4.join(", ")
            ));
        }
        Err(error) => record(&format!(
            "[AirPrinter] could not list network interfaces: {error}"
        )),
    }
}

fn append(path: &Path, message: &str, limit: u64) -> io::Result<()> {
    let mut line = serde_json::to_vec(&serde_json::json!({
        "timestamp_ms": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis(),
        "message": message.chars().take(4096).collect::<String>(),
    }))?;
    line.push(b'\n');
    if fs::metadata(path).is_ok_and(|metadata| metadata.len() + line.len() as u64 > limit) {
        let previous = path.with_extension("jsonl.previous");
        match fs::remove_file(&previous) {
            Ok(()) => (),
            Err(error) if error.kind() == io::ErrorKind::NotFound => (),
            Err(error) => return Err(error),
        }
        fs::rename(path, previous)?;
    }
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?
        .write_all(&line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_is_readable_and_bounded_across_rotation() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("print-diagnostics.jsonl");
        append(&path, "[IPP] options rejected: 中文\n0x040b", 200).unwrap();
        let entry: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert!(entry["message"].as_str().unwrap().contains("中文\n0x040b"));
        assert!(entry["timestamp_ms"].as_u64().unwrap() > 0);
        for index in 0..10 {
            append(
                &path,
                &format!("[IPP] request_id={index} status=0x0000"),
                200,
            )
            .unwrap();
        }
        assert!(fs::metadata(&path).unwrap().len() <= 200);
        assert!(path.with_extension("jsonl.previous").exists());
        assert!(fs::read_to_string(path).unwrap().contains("request_id=9"));
    }

    #[test]
    fn tail_is_bounded_by_bytes_and_lines_without_splitting_utf8() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("print-diagnostics.jsonl");
        for index in 0..20 {
            append(
                &path,
                &format!("[IPP] 第{index}行 中文内容 padding-padding-padding"),
                u64::MAX,
            )
            .unwrap();
        }

        let last_two = tail_from(&path, usize::MAX, 2).unwrap();
        assert_eq!(last_two.lines().count(), 2);
        assert!(last_two.contains("第19行"));

        let tail = tail_from(&path, 120, usize::MAX).unwrap();
        assert!(tail.len() <= 160, "tail should stay near the byte budget");
        assert!(tail.contains('行'), "tail must not corrupt UTF-8: {tail}");
        assert!(tail_from(&directory.path().join("missing.jsonl"), 10, 10).is_err());
    }
}
