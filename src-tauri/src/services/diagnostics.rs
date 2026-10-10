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
}
