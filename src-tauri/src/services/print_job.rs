use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub const FORMATS: &[&str] = &[
    "application/pdf",
    "image/jpeg",
    "image/urf",
    "image/pwg-raster",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrintOptions {
    pub job_name: String,
    pub page_ranges: Vec<(i32, i32)>,
    pub scaling: String,
    pub fidelity: bool,
    pub copies: i32,
    pub quality: i32,
    pub sides: String,
    pub color_mode: String,
    pub media: String,
    pub orientation: i32,
    pub paper_kind: i32,
    pub paper_width: i32,
    pub paper_height: i32,
}

impl Default for PrintOptions {
    fn default() -> Self {
        Self {
            job_name: "AirPrint document".into(),
            page_ranges: Vec::new(),
            scaling: "fit".into(),
            fidelity: false,
            copies: 1,
            quality: 4,
            sides: "one-sided".into(),
            color_mode: "auto".into(),
            media: "iso_a4_210x297mm".into(),
            orientation: 3,
            paper_kind: 9,
            paper_width: 827,
            paper_height: 1169,
        }
    }
}

pub fn validate_document(format: &str, data: &[u8]) -> bool {
    match format {
        "application/pdf" => data.starts_with(b"%PDF-"),
        "image/jpeg" => data.starts_with(&[0xff, 0xd8, 0xff]),
        "image/urf" => data.starts_with(b"UNIRAST\0"),
        "image/pwg-raster" => data.starts_with(b"RaS2"),
        _ => false,
    }
}

#[cfg(all(test, target_os = "windows"))]
pub(crate) static TEST_OUTPUT: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

#[cfg(all(test, target_os = "windows"))]
pub(crate) fn submit_test_pdf(
    printer: &str,
    data: &[u8],
    format: &str,
    options: &PrintOptions,
    canceled: &AtomicBool,
) -> Result<i32, String> {
    if printer != "Microsoft Print to PDF" {
        return Err("Simulation only permits Microsoft Print to PDF".into());
    }
    submit_windows(
        printer,
        data,
        format,
        options,
        canceled,
        Some(TEST_OUTPUT.get().ok_or("Missing test output")?),
    )
}

/// Submit through the Windows driver, without changing the default printer or
/// launching a file association. Success means Windows accepted the spool job.
#[cfg(target_os = "windows")]
pub fn submit(
    printer: &str,
    data: &[u8],
    format: &str,
    options: &PrintOptions,
    canceled: &AtomicBool,
) -> Result<i32, String> {
    submit_windows(printer, data, format, options, canceled, None)
}

#[cfg(not(target_os = "windows"))]
pub fn submit(_: &str, _: &[u8], _: &str, _: &PrintOptions, _: &AtomicBool) -> Result<i32, String> {
    Err("Native print bridge requires Windows".into())
}

#[cfg(target_os = "windows")]
fn submit_windows(
    printer: &str,
    data: &[u8],
    format: &str,
    options: &PrintOptions,
    canceled: &AtomicBool,
    output: Option<&Path>,
) -> Result<i32, String> {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    if !validate_document(format, data) {
        return Err("Document does not match its declared format".into());
    }
    let directory = tempfile::tempdir().map_err(|e| e.to_string())?;
    let input = directory.path().join("document");
    std::fs::write(&input, data).map_err(|e| e.to_string())?;
    let pages = match format {
        "image/urf" | "image/pwg-raster" => {
            super::raster::decode(data, directory.path()).map_err(|e| e.to_string())?
        }
        "image/jpeg" => vec![input.clone()],
        _ => vec![],
    };
    let cancel_path = directory.path().join("cancel");
    let id_path = directory.path().join("spool-id");
    let manifest = directory.path().join("job.json");
    let script = directory.path().join("print.ps1");
    let error_path = directory.path().join("stderr.txt");
    let stdout = directory.path().join("stdout.txt");
    let body = serde_json::json!({
        "printer": printer, "format": format, "input": input, "directory": directory.path(),
        "pages": pages, "cancel": cancel_path, "spool_id": id_path, "output": output,
        "name": options.job_name, "page_ranges": options.page_ranges,
        "scaling": options.scaling, "fidelity": options.fidelity,
        "copies": options.copies, "quality": options.quality, "sides": options.sides, "color_mode": options.color_mode, "media": options.media,
        "orientation": options.orientation,
        "paper_kind": options.paper_kind, "paper_width": options.paper_width, "paper_height": options.paper_height,
    });
    std::fs::write(
        &manifest,
        serde_json::to_vec(&body).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    std::fs::write(&script, include_str!("windows_print.ps1")).map_err(|e| e.to_string())?;
    let mut child = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(&script)
        .env("AIRPRINTER_MANIFEST", &manifest)
        .stdin(Stdio::null())
        .stdout(std::fs::File::create(&stdout).map_err(|e| e.to_string())?)
        .stderr(std::fs::File::create(&error_path).map_err(|e| e.to_string())?)
        .creation_flags(0x08000000)
        .spawn()
        .map_err(|e| e.to_string())?;
    let started = Instant::now();
    loop {
        let cancel = canceled.load(Ordering::Acquire);
        if cancel {
            let _ = std::fs::write(&cancel_path, b"cancel");
        }
        match child.try_wait().map_err(|e| e.to_string())? {
            Some(status) => {
                if cancel {
                    if let Ok(id) = std::fs::read_to_string(&id_path)
                        .unwrap_or_default()
                        .parse()
                    {
                        let _ = cancel_spool_job(printer, id);
                    }
                    return Err("Print job canceled".into());
                }
                if !status.success() {
                    return Err(std::fs::read_to_string(&error_path)
                        .unwrap_or_else(|_| "Windows print helper failed".into()));
                }
                return std::fs::read_to_string(&id_path)
                    .map_err(|e| e.to_string())?
                    .parse::<i32>()
                    .map_err(|e| e.to_string());
            }
            None if started.elapsed() > Duration::from_secs(120) => {
                let _ = child.kill();
                let _ = child.wait();
                if let Ok(id) = std::fs::read_to_string(&id_path)
                    .unwrap_or_default()
                    .parse()
                {
                    let _ = cancel_spool_job(printer, id);
                }
                return Err("Windows printing timed out".into());
            }
            None => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}

#[cfg(target_os = "windows")]
pub fn cancel_spool_job(printer: &str, id: i32) -> Result<(), String> {
    spooler::cancel(printer, id)
}

#[cfg(not(target_os = "windows"))]
pub fn cancel_spool_job(_: &str, _: i32) -> Result<(), String> {
    Err("Requires Windows".into())
}

#[cfg(all(test, target_os = "windows"))]
mod tests {
    use super::*;
    #[test]
    #[ignore = "Uses the Windows Microsoft Print to PDF queue, creates a local PDF only"]
    fn windows_raster_spool_smoke() {
        let mut data = b"UNIRAST\0".to_vec();
        data.extend(1u32.to_be_bytes());
        let mut header = [0; 32];
        header[0] = 8;
        header[12..16].copy_from_slice(&100u32.to_be_bytes());
        header[16..20].copy_from_slice(&100u32.to_be_bytes());
        header[20..24].copy_from_slice(&300u32.to_be_bytes());
        data.extend(header);
        data.extend([99, 99, 80]);
        let temp = tempfile::tempdir().unwrap();
        let output = temp.path().join("output.pdf");
        let id = submit_windows(
            "Microsoft Print to PDF",
            &data,
            "image/urf",
            &PrintOptions::default(),
            &AtomicBool::new(false),
            Some(&output),
        )
        .unwrap();
        assert!(id > 0);
        let pdf = std::fs::read(&output).unwrap();
        assert!(pdf.starts_with(b"%PDF-"));
        let second = temp.path().join("roundtrip.pdf");
        submit_windows(
            "Microsoft Print to PDF",
            &pdf,
            "application/pdf",
            &PrintOptions::default(),
            &AtomicBool::new(false),
            Some(&second),
        )
        .unwrap();
        assert!(std::fs::read(second).unwrap().starts_with(b"%PDF-"));
        let jpeg_output = temp.path().join("jpeg.pdf");
        submit_windows(
            "Microsoft Print to PDF",
            include_bytes!("../../tests/fixtures/quadrants.jpg"),
            "image/jpeg",
            &PrintOptions::default(),
            &AtomicBool::new(false),
            Some(&jpeg_output),
        )
        .unwrap();
        assert!(std::fs::read(jpeg_output).unwrap().starts_with(b"%PDF-"));
        let mut pwg = b"RaS2".to_vec();
        let mut header = vec![0; 1796];
        for (offset, value) in [
            (276, 300u32),
            (280, 300),
            (372, 100),
            (376, 100),
            (384, 8),
            (388, 8),
            (392, 100),
            (400, 18),
        ] {
            header[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
        }
        pwg.extend(header);
        pwg.extend([99, 99, 80]);
        let pwg_output = temp.path().join("pwg.pdf");
        submit_windows(
            "Microsoft Print to PDF",
            &pwg,
            "image/pwg-raster",
            &PrintOptions::default(),
            &AtomicBool::new(false),
            Some(&pwg_output),
        )
        .unwrap();
        assert!(std::fs::read(pwg_output).unwrap().starts_with(b"%PDF-"));
    }
}
#[cfg(target_os = "windows")]
mod spooler {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "winspool")]
    unsafe extern "system" {
        fn OpenPrinterW(name: *const u16, handle: *mut *mut c_void, defaults: *const c_void)
            -> i32;
        fn ClosePrinter(handle: *mut c_void) -> i32;
        fn GetJobW(
            handle: *mut c_void,
            id: u32,
            level: u32,
            data: *mut u8,
            size: u32,
            needed: *mut u32,
        ) -> i32;
        fn SetJobW(handle: *mut c_void, id: u32, level: u32, data: *const u8, command: u32) -> i32;
    }
    #[repr(C)]
    struct JobInfo1 {
        id: u32,
        printer: *mut u16,
        machine: *mut u16,
        user: *mut u16,
        document: *mut u16,
        datatype: *mut u16,
        status_text: *mut u16,
        status: u32,
        priority: u32,
        position: u32,
        total_pages: u32,
        pages_printed: u32,
        submitted: [u16; 8],
    }
    struct Handle(*mut c_void);
    impl Drop for Handle {
        fn drop(&mut self) {
            unsafe {
                ClosePrinter(self.0);
            }
        }
    }
    fn open(name: &str) -> Result<Handle, String> {
        if name.contains('\0') {
            return Err("Invalid printer name".into());
        }
        let name: Vec<_> = std::ffi::OsStr::new(name)
            .encode_wide()
            .chain(Some(0))
            .collect();
        let mut handle = std::ptr::null_mut();
        if unsafe { OpenPrinterW(name.as_ptr(), &mut handle, std::ptr::null()) } == 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(Handle(handle))
    }
    pub fn cancel(name: &str, id: i32) -> Result<(), String> {
        let handle = open(name)?;
        if unsafe { SetJobW(handle.0, id as u32, 0, std::ptr::null(), 3) } == 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(())
    }
    /// None means the job has left the Windows spool queue, not proof of physical output.
    pub fn state(name: &str, id: i32) -> Result<Option<u32>, String> {
        let handle = open(name)?;
        let mut needed = 0;
        unsafe {
            GetJobW(handle.0, id as u32, 1, std::ptr::null_mut(), 0, &mut needed);
        }
        if needed == 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(87) {
                return Ok(None);
            }
            return Err(error.to_string());
        }
        // Pointer-sized allocation gives JOB_INFO_1 its required alignment.
        let mut buffer = vec![0usize; (needed as usize).div_ceil(std::mem::size_of::<usize>())];
        if unsafe {
            GetJobW(
                handle.0,
                id as u32,
                1,
                buffer.as_mut_ptr().cast(),
                needed,
                &mut needed,
            )
        } == 0
        {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(87) {
                return Ok(None);
            }
            return Err(error.to_string());
        }
        Ok(Some(unsafe {
            (*(buffer.as_ptr() as *const JobInfo1)).status
        }))
    }
}

#[cfg(target_os = "windows")]
pub fn spool_state(printer: &str, id: i32) -> Result<Option<u32>, String> {
    spooler::state(printer, id)
}
#[cfg(not(target_os = "windows"))]
pub fn spool_state(_: &str, _: i32) -> Result<Option<u32>, String> {
    Err("Requires Windows".into())
}
