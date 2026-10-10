use super::transport::{self, OriginMap};
use crate::models::{Printer, PrinterStatus};
use crate::services::print_job::{self, PrintOptions, FORMATS};
use ipp::attribute::{IppAttribute, IppAttributeGroup};
use ipp::model::{DelimiterTag, IppVersion, StatusCode};
use ipp::parser::IppParser;
use ipp::reader::IppReader;
use ipp::request::IppRequestResponse;
use ipp::value::IppValue;
use std::collections::HashMap;
use std::io::{Cursor, Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener};
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering},
    Arc, Mutex,
};
use std::thread;
use std::time::{Duration, Instant};
use tiny_http::{Header, Response, Server};

macro_rules! trace {
    ($($arg:tt)*) => { crate::services::diagnostics::record(&format!($($arg)*)) };
}

const DEFAULT_PRINT_PATH: &str = "/ipp/print";
static NEXT_JOB_ID: AtomicI32 = AtomicI32::new(1);
const MAX_UPLOAD: u64 = 64 * 1024 * 1024;
type Backend = fn(&str, &[u8], &str, &PrintOptions, &AtomicBool) -> Result<i32, String>;

#[derive(Clone)]
struct Job {
    id: i32,
    path: String,
    profile: Printer,
    printer: String,
    owner: String,
    name: String,
    options: PrintOptions,
    state: i32,
    reason: String,
    message: String,
    canceled: Arc<AtomicBool>,
    spool_id: Option<i32>,
    created: Instant,
    uuid: String,
    processed: Option<Instant>,
    ended: Option<Instant>,
}
struct State {
    printers: HashMap<String, (Printer, String)>,
    routes: HashMap<String, String>,
    jobs: HashMap<i32, Job>,
    started: Instant,
    backend: Backend,
    receive_directory: Option<PathBuf>,
}
type Shared = Arc<Mutex<State>>;

pub struct IppServer {
    address: String,
    shared: Shared,
    running: Arc<AtomicBool>,
    listener: Option<thread::JoinHandle<()>>,
    transport_listener: Option<thread::JoinHandle<()>>,
    secure_listener: Option<thread::JoinHandle<()>>,
    secure_port: Option<u16>,
    origins: OriginMap,
    tls_target: Arc<Mutex<Option<SocketAddr>>>,
}

impl IppServer {
    pub fn new(bind_address: &str, port: u16) -> Self {
        Self {
            address: format!("{bind_address}:{port}"),
            shared: Arc::new(Mutex::new(State {
                printers: HashMap::new(),
                routes: HashMap::new(),
                jobs: HashMap::new(),
                started: Instant::now(),
                backend: print_job::submit,
                receive_directory: None,
            })),
            running: Arc::new(AtomicBool::new(false)),
            listener: None,
            transport_listener: None,
            secure_listener: None,
            secure_port: None,
            origins: Arc::new(Mutex::new(HashMap::new())),
            tls_target: Arc::new(Mutex::new(None)),
        }
    }

    pub fn set_receive_directory(&mut self, directory: Option<PathBuf>) -> Result<(), String> {
        if directory.as_ref().is_some_and(|path| !path.is_absolute()) {
            return Err("Receive-only directory must be absolute".into());
        }
        self.shared
            .lock()
            .map_err(|error| error.to_string())?
            .receive_directory = directory;
        Ok(())
    }

    pub fn add_printer(&self, printer: Printer, hostname: &str) -> Result<(), String> {
        let target_name = printer.name.clone();
        self.add_routed_printer(printer, &target_name, hostname)
    }

    pub fn add_routed_printer(
        &self,
        printer: Printer,
        target_name: &str,
        hostname: &str,
    ) -> Result<(), String> {
        let path = format!("/{}", printer.resource_path());
        let mut state = self.shared.lock().map_err(|e| e.to_string())?;
        if state.printers.contains_key(&path) {
            return Err("Duplicate printer resource".into());
        }
        state
            .printers
            .insert(path.clone(), (printer, hostname.to_string()));
        state.routes.insert(path, target_name.to_string());
        Ok(())
    }
    pub fn remove_printer(&self, printer: &Printer) -> Result<(), String> {
        let path = format!("/{}", printer.resource_path());
        let mut state = self.shared.lock().map_err(|e| e.to_string())?;
        state.printers.remove(&path);
        state.routes.remove(&path);
        Ok(())
    }
    pub fn update_printer(&self, printer: Printer) -> Result<(), String> {
        let path = format!("/{}", printer.resource_path());
        let mut state = self.shared.lock().map_err(|e| e.to_string())?;
        let current = state
            .printers
            .get_mut(&path)
            .ok_or("Printer is not shared")?;
        let target = printer.name.clone();
        current.0 = printer;
        state.routes.insert(path, target);
        Ok(())
    }

    pub fn update_routed_printer(&self, printer: Printer, target_name: &str) -> Result<(), String> {
        let path = format!("/{}", printer.resource_path());
        let mut state = self.shared.lock().map_err(|e| e.to_string())?;
        let current = state
            .printers
            .get_mut(&path)
            .ok_or("Printer is not shared")?;
        current.0 = printer;
        state.routes.insert(path, target_name.to_string());
        Ok(())
    }
    pub fn start(&mut self) -> Result<(), String> {
        if self.listener.is_some() {
            return Err("IPP server already running".into());
        }
        let listener = match TcpListener::bind(&self.address) {
            Ok(listener) => listener,
            Err(error)
                if self.address.ends_with(":631")
                    && error.kind() == std::io::ErrorKind::AddrInUse =>
            {
                // Windows can retain TCP 631 after tiny_http closes a connection.
                // DNS-SD publishes the actual SRV port, so use a reserved fallback
                // range instead of forcibly sharing a port with another process.
                let bind_host = self
                    .address
                    .rsplit_once(':')
                    .map(|(host, _)| host)
                    .unwrap_or("0.0.0.0");
                (8631..=8699)
                    .find_map(|port| TcpListener::bind(format!("{bind_host}:{port}")).ok())
                    .ok_or("No free IPP port in 8631-8699")?
            }
            Err(error) => return Err(error.to_string()),
        };
        listener
            .set_nonblocking(true)
            .map_err(|error| error.to_string())?;
        let public_address = listener.local_addr().map_err(|error| error.to_string())?;
        self.address = public_address.to_string();
        let port = public_address.port();
        let server = Server::http("127.0.0.1:0").map_err(|error| error.to_string())?;
        let http_target = server
            .server_addr()
            .to_ip()
            .ok_or("Missing HTTP backend address")?;
        self.running.store(true, Ordering::Release);
        self.listener = Some(self.serve(server, port, self.origins.clone()));
        self.transport_listener = Some(transport::spawn_mux(
            listener,
            http_target,
            self.tls_target.clone(),
            self.origins.clone(),
            self.running.clone(),
            port,
        ));
        trace!("[IPP] listening on {}", self.address);
        Ok(())
    }

    pub fn start_tls(&mut self, hostname: &str, directory: &std::path::Path) -> Result<(), String> {
        if self.secure_listener.is_some() {
            return Ok(());
        }
        let identity = super::tls::identity(directory, hostname)?;
        let host = self
            .address
            .rsplit_once(':')
            .map(|(h, _)| h)
            .ok_or("Invalid IPP address")?;
        // This range is already covered by the application's LAN firewall rule.
        let mut last_error = String::from("No free IPPS port");
        for port in 8631..=8699 {
            let listener = match std::net::TcpListener::bind(format!("{host}:{port}")) {
                Ok(listener) => listener,
                Err(error) => {
                    last_error = error.to_string();
                    continue;
                }
            };
            let server =
                Server::from_listener(listener, Some(identity)).map_err(|e| e.to_string())?;
            let actual = server
                .server_addr()
                .to_ip()
                .ok_or("Missing TLS listener address")?;
            let proxy_ip = match host.parse::<IpAddr>() {
                Ok(ip) if ip.is_unspecified() => match ip {
                    IpAddr::V4(_) => IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
                    IpAddr::V6(_) => IpAddr::V6(std::net::Ipv6Addr::LOCALHOST),
                },
                _ => actual.ip(),
            };
            *self
                .tls_target
                .lock()
                .unwrap_or_else(|error| error.into_inner()) =
                Some(SocketAddr::new(proxy_ip, actual.port()));
            self.secure_listener = Some(self.serve(server, port, self.origins.clone()));
            self.secure_port = Some(port);
            trace!("[IPPS] TLS listening on {host}:{port}");
            return Ok(());
        }
        Err(last_error)
    }

    pub fn secure_port(&self) -> Option<u16> {
        self.secure_port
    }

    fn serve(&self, server: Server, port: u16, origins: OriginMap) -> thread::JoinHandle<()> {
        let shared = self.shared.clone();
        let running = self.running.clone();
        running.store(true, Ordering::Release);
        let active = Arc::new(AtomicUsize::new(0));
        thread::spawn(move || {
            while running.load(Ordering::Acquire) {
                match server.recv_timeout(Duration::from_millis(100)) {
                    Ok(Some(request)) => {
                        // ponytail: bounded handlers, replace with a worker pool only if needed.
                        if active.load(Ordering::Acquire) >= 8 {
                            trace!("[IPP] rejecting request: handler limit reached");
                            let _ = request.respond(Response::empty(503));
                            continue;
                        }
                        active.fetch_add(1, Ordering::AcqRel);
                        let active = active.clone();
                        let shared = shared.clone();
                        let origins = origins.clone();
                        thread::spawn(move || {
                            struct Guard(Arc<AtomicUsize>);
                            impl Drop for Guard {
                                fn drop(&mut self) {
                                    self.0.fetch_sub(1, Ordering::AcqRel);
                                }
                            }
                            let _guard = Guard(active);
                            Self::handle_request(request, &shared, &origins, port);
                        });
                    }
                    Ok(None) => (),
                    Err(error) => {
                        trace!("[IPP] listener port={port} failed: {error}");
                        break;
                    }
                }
            }
        })
    }

    pub fn port(&self) -> Result<u16, String> {
        self.address
            .rsplit_once(':')
            .and_then(|(_, port)| port.parse().ok())
            .ok_or("Invalid IPP listener address".into())
    }

    fn handle_request(
        mut request: tiny_http::Request,
        shared: &Shared,
        origins: &OriginMap,
        listener_port: u16,
    ) {
        let mapped = request.remote_addr().and_then(|addr| {
            origins
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .get(addr)
                .copied()
        });
        let remote_addr = mapped
            .map(|origin| origin.peer)
            .or_else(|| request.remote_addr().copied());
        let port = mapped
            .map(|origin| origin.public_port)
            .unwrap_or(listener_port);
        trace!(
            "[IPP] {} {} from {:?} secure={}",
            request.method(),
            request.url(),
            remote_addr,
            request.secure()
        );
        let mut has_receive_header = false;
        let mut invalid_receive_header = false;
        for header in request.headers() {
            if header.field.equiv("X-AirPrinter-Receive-Only") {
                has_receive_header = true;
                invalid_receive_header |= header.value.as_str() != "1";
            }
        }
        if has_receive_header {
            let receive_enabled = shared
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .receive_directory
                .is_some();
            if !receive_enabled || invalid_receive_header {
                trace!("[IPP] rejecting receive-only web request: mode disabled or invalid marker");
                let _ = request.respond(Response::empty(409));
                return;
            }
        }
        #[cfg(debug_assertions)]
        if request.method() == &tiny_http::Method::Get && request.url() == "/debug/test.jpg" {
            let _ = request.respond(
                Response::from_data(
                    include_bytes!("../../../tests/fixtures/quadrants.jpg").to_vec(),
                )
                .with_header(Header::from_bytes("Content-Type", "image/jpeg").unwrap()),
            );
            return;
        }
        #[cfg(debug_assertions)]
        if request.method() == &tiny_http::Method::Get && request.url() == "/debug/jobs" {
            let state = shared.lock().unwrap_or_else(|e| e.into_inner());
            let job = state.jobs.values().max_by_key(|job| job.id).map(|job| {
                serde_json::json!({
                    "id": job.id,
                    "state": job.state,
                    "reason": job.reason,
                    "message": job.message,
                    "spool_id": job.spool_id,
                })
            });
            let _ = request.respond(
                Response::from_string(serde_json::json!({"job": job}).to_string())
                    .with_header(Header::from_bytes("Content-Type", "application/json").unwrap()),
            );
            return;
        }
        #[cfg(debug_assertions)]
        if request.method() == &tiny_http::Method::Get && request.url() == "/debug" {
            let path = shared
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .printers
                .keys()
                .next()
                .cloned();
            let Some(path) = path else {
                let _ = request.respond(Response::from_string("No shared printer"));
                return;
            };
            let page = include_str!("debug.html").replace("__PATH__", &path);
            let _ = request.respond(Response::from_string(page).with_header(
                Header::from_bytes("Content-Type", "text/html; charset=utf-8").unwrap(),
            ));
            return;
        }
        if request.method() == &tiny_http::Method::Get && request.url() == "/receive" {
            let Some((hostname, page)) = receive_page(shared) else {
                let _ = request.respond(Response::empty(404));
                return;
            };
            let host = request
                .headers()
                .iter()
                .find(|header| header.field.equiv("Host"))
                .map(|header| header.value.as_str())
                .unwrap_or("");
            if !valid_host(host, &hostname, port) {
                trace!("[IPP] rejecting receive page: invalid Host header");
                let _ = request.respond(Response::empty(400));
                return;
            }
            let _ = request.respond(
                Response::from_string(page)
                    .with_header(
                        Header::from_bytes("Content-Type", "text/html; charset=utf-8").unwrap(),
                    )
                    .with_header(Header::from_bytes("Cache-Control", "no-store").unwrap()),
            );
            return;
        }
        // PWG 5100.14 Table 5 note 6: serve the absolute "printer-icons" URIs.
        if request.method() == &tiny_http::Method::Get {
            if let Some(icon) = request.url().strip_prefix("/icons/") {
                let bytes: Option<&[u8]> = match icon {
                    "32.png" => Some(include_bytes!("../../../icons/32x32.png").as_slice()),
                    "64.png" => Some(include_bytes!("../../../icons/64x64.png").as_slice()),
                    "128.png" => Some(include_bytes!("../../../icons/128x128.png").as_slice()),
                    "512.png" => Some(include_bytes!("../../../icons/icon.png").as_slice()),
                    _ => None,
                };
                let Some(bytes) = bytes else {
                    let _ = request.respond(Response::empty(404));
                    return;
                };
                // Cacheable per PWG 5100.14 section 5.1.3.
                let _ = request.respond(
                    Response::from_data(bytes.to_vec())
                        .with_header(Header::from_bytes("Content-Type", "image/png").unwrap())
                        .with_header(Header::from_bytes("Cache-Control", "max-age=86400").unwrap()),
                );
                return;
            }
            // Landing page for the "printer-more-info"/"adminurl" references.
            if request.url() == "/" {
                let state = shared.lock().unwrap_or_else(|e| e.into_inner());
                let mut rows = String::new();
                for (path, (printer, _)) in state.printers.iter() {
                    rows.push_str(&format!(
                        "<li><strong>{}</strong> — {} — <code>{}</code></li>",
                        escape_html_attribute(&printer.name),
                        printer.status.as_str(),
                        escape_html_attribute(path),
                    ));
                }
                drop(state);
                let page = format!(
                    "<!doctype html><html><head><meta charset=\"utf-8\"><title>AirPrinter</title></head><body><h1>AirPrinter</h1><ul>{rows}</ul></body></html>"
                );
                let _ = request.respond(
                    Response::from_string(page)
                        .with_header(
                            Header::from_bytes("Content-Type", "text/html; charset=utf-8").unwrap(),
                        )
                        .with_header(Header::from_bytes("Cache-Control", "no-store").unwrap()),
                );
                return;
            }
        }
        let requested_path = request.url().to_string();
        let request_base = requested_path
            .split_once("/jobs/")
            .map(|(path, _)| path)
            .unwrap_or(&requested_path)
            .to_string();
        let (selected, shared_count) = {
            let state = shared.lock().unwrap_or_else(|e| e.into_inner());
            let count = state.printers.len();
            let selected = resolve_printer_path(&request_base, &state.printers, &state.routes)
                .and_then(|(path, is_alias)| {
                    state
                        .printers
                        .get(&path)
                        .map(|(_, hostname)| (path, hostname.clone(), is_alias))
                });
            (selected, count)
        };
        let Some((path, hostname, is_alias)) = selected else {
            trace!("[IPP] rejecting request: unknown path actual_path={requested_path:?} shared_printers={shared_count}");
            let _ = request.respond(Response::empty(404));
            return;
        };
        let alias_path = is_alias.then_some(request_base.as_str());
        let path_job_id = requested_path
            .strip_prefix(&format!("{request_base}/jobs/"))
            .and_then(|id| id.parse::<i32>().ok());
        let is_job_path = requested_path != request_base;
        if request.method() != &tiny_http::Method::Post {
            trace!("[IPP] rejecting request: method is not POST");
            let _ = request.respond(Response::empty(405));
            return;
        }
        let host = request
            .headers()
            .iter()
            .find(|h| h.field.equiv("Host"))
            .map(|h| h.value.as_str())
            .unwrap_or("")
            .to_string();
        trace!(
            "[IPP] host={host:?} body_length={:?} content_type={:?} transfer_encoding={:?} expect={:?}",
            request.body_length(),
            request.headers().iter().find(|h| h.field.equiv("Content-Type")).map(|h| h.value.as_str()),
            request.headers().iter().find(|h| h.field.equiv("Transfer-Encoding")).map(|h| h.value.as_str()),
            request.headers().iter().find(|h| h.field.equiv("Expect")).map(|h| h.value.as_str()),
        );
        if !valid_host(&host, &hostname, port) {
            trace!("[IPP] rejecting request: invalid Host header");
            let _ = request.respond(Response::empty(400));
            return;
        }
        let content_type = request
            .headers()
            .iter()
            .find(|h| h.field.equiv("Content-Type"))
            .map(|h| h.value.as_str().split(';').next().unwrap_or("").trim());
        if !content_type.is_some_and(|c| c.eq_ignore_ascii_case("application/ipp")) {
            trace!("[IPP] rejecting request: unsupported Content-Type");
            let _ = request.respond(Response::empty(415));
            return;
        }
        if request
            .body_length()
            .is_some_and(|size| size as u64 > MAX_UPLOAD)
        {
            trace!("[IPP] rejecting request: Content-Length exceeds limit");
            let _ = request.respond(Response::empty(413));
            return;
        }
        let mut body = Vec::new();
        if request
            .as_reader()
            .take(MAX_UPLOAD + 1)
            .read_to_end(&mut body)
            .is_err()
        {
            trace!("[IPP] rejecting request: failed to read body");
            let _ = request.respond(Response::empty(400));
            return;
        }
        trace!("[IPP] body_bytes={}", body.len());
        if body.len() as u64 > MAX_UPLOAD {
            trace!("[IPP] rejecting request: body exceeds limit");
            let _ = request.respond(Response::empty(413));
            return;
        }
        if body.len() < 9 {
            trace!("[IPP] rejecting request: short body");
            let _ = request.respond(Response::empty(400));
            return;
        }
        let id = u32::from_be_bytes(body[4..8].try_into().unwrap());
        let parsed =
            std::panic::catch_unwind(|| IppParser::new(IppReader::new(Cursor::new(body))).parse());
        let response = match parsed {
            Ok(Ok(req)) => {
                trace!(
                    "[IPP] request_id={id} operation={:#06x} version={:?}",
                    req.header().operation_or_status,
                    req.header().version
                );
                trace!(
                    "[IPP] request_id={id} actual_path={requested_path:?} canonical_path={path:?} printer_uri={:?}",
                    string_attr(&req, "printer-uri")
                );
                let job_path_matches = if is_job_path {
                    path_job_id.is_some_and(|id| {
                        attr(&req, "job-id") == Some(&IppValue::Integer(id))
                            || string_attr(&req, "job-uri")
                                .is_some_and(|uri| uri.ends_with(&format!("/jobs/{id}")))
                    })
                } else {
                    true
                };
                if job_path_matches {
                    let authority = response_authority(remote_addr, &host, port, request.secure());
                    trace!("[IPP] request_id={id} response_authority={authority:?}");
                    process_request_at_port_with_alias(
                        shared,
                        &path,
                        alias_path,
                        &authority,
                        req,
                        request.secure(),
                        port,
                    )
                } else {
                    reply(req.header().version, id, StatusCode::ClientErrorNotFound)
                }
            }
            Ok(Err(error)) => {
                trace!("[IPP] request_id={id} parse failed: {error}");
                reply(IppVersion::v2_0(), id, StatusCode::ClientErrorBadRequest)
            }
            Err(_) => {
                trace!("[IPP] request_id={id} parser panicked");
                reply(IppVersion::v2_0(), id, StatusCode::ClientErrorBadRequest)
            }
        };
        let response_bytes = response.to_bytes();
        trace!(
            "[IPP] request_id={id} actual_path={requested_path:?} status={:#06x} response_bytes={}",
            response.header().operation_or_status,
            response_bytes.len()
        );
        if let Err(error) = request.respond(
            Response::from_data(response_bytes.to_vec())
                .with_header(Header::from_bytes("Content-Type", "application/ipp").unwrap())
                .with_header(Header::from_bytes("Cache-Control", "no-cache").unwrap()),
        ) {
            trace!("[IPP] request_id={id} response failed: {error}");
        }
    }
}

impl Drop for IppServer {
    fn drop(&mut self) {
        self.shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .printers
            .clear();
        self.running.store(false, Ordering::Release);
        if let Some(listener) = self.listener.take() {
            let _ = listener.join();
        }
        if let Some(listener) = self.secure_listener.take() {
            let _ = listener.join();
        }
        if let Some(listener) = self.transport_listener.take() {
            let _ = listener.join();
        }
    }
}

fn resolve_printer_path(
    request_path: &str,
    printers: &HashMap<String, (Printer, String)>,
    routes: &HashMap<String, String>,
) -> Option<(String, bool)> {
    if request_path == DEFAULT_PRINT_PATH {
        if printers.len() != 1 {
            return None;
        }
        return printers
            .keys()
            .next()
            .cloned()
            .map(|path| (path.clone(), path != request_path));
    }
    if printers.contains_key(request_path) {
        return Some((request_path.to_string(), false));
    }
    for (public_path, (printer, _)) in printers {
        if printer.id != crate::models::printer::GATEWAY_PRINTER_ID {
            continue;
        }
        let target_name = routes.get(public_path)?;
        if request_path == legacy_target_resource_path(target_name) {
            return Some((public_path.clone(), true));
        }
    }
    None
}

fn legacy_target_resource_path(target_name: &str) -> String {
    // PrinterDetector uses the Windows queue name as `Printer.id`.
    let id = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, target_name.as_bytes()).simple();
    format!("/ipp/print/{id}")
}

fn receive_page(shared: &Shared) -> Option<(String, String)> {
    let state = shared.lock().unwrap_or_else(|error| error.into_inner());
    if state.receive_directory.is_none() || state.printers.len() != 1 {
        return None;
    }
    let (path, (_, hostname)) = state.printers.iter().next()?;
    let path = escape_html_attribute(path);
    Some((
        hostname.clone(),
        include_str!("receive.html").replace("__PRINTER_PATH__", &path),
    ))
}

fn escape_html_attribute(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\'', "&#39;")
}

fn valid_host(authority: &str, hostname: &str, port: u16) -> bool {
    // IPP's default port is 631; some clients omit it from the HTTP Host field.
    let (host, supplied_port) = authority.rsplit_once(':').unwrap_or((authority, "631"));
    if supplied_port.parse::<u16>().ok() != Some(port) {
        return false;
    }
    let local_name = std::env::var("COMPUTERNAME")
        .ok()
        .filter(|name| !name.trim().is_empty())
        .or_else(|| std::env::var("HOSTNAME").ok());
    if is_local_hostname(host, hostname, local_name.as_deref()) {
        return true;
    }
    let Ok(ip) = host.parse::<std::net::Ipv4Addr>() else {
        return false;
    };
    if ip.is_loopback() {
        return true;
    }
    local_ip_address::list_afinet_netifas()
        .map(|interfaces| {
            interfaces
                .iter()
                .any(|(_, address)| *address == std::net::IpAddr::V4(ip))
        })
        .unwrap_or(false)
}

fn is_local_hostname(host: &str, configured: &str, computer_name: Option<&str>) -> bool {
    let host = host.trim_end_matches('.');
    if host.eq_ignore_ascii_case(configured.trim_end_matches('.')) {
        return true;
    }
    let Some(computer_name) = computer_name.map(|name| name.trim_end_matches('.')) else {
        return false;
    };
    if computer_name.is_empty() {
        return false;
    }
    if computer_name.to_ascii_lowercase().ends_with(".local") {
        host.eq_ignore_ascii_case(computer_name)
    } else {
        host.eq_ignore_ascii_case(&format!("{computer_name}.local"))
    }
}

fn response_authority(
    peer: Option<std::net::SocketAddr>,
    host: &str,
    port: u16,
    secure: bool,
) -> String {
    let hostname = host.rsplit_once(':').map(|(host, _)| host).unwrap_or(host);
    // IPPS keeps the DNS hostname that appears in the persistent TLS certificate.
    if !secure
        && hostname
            .trim_end_matches('.')
            .to_ascii_lowercase()
            .ends_with(".local")
    {
        if let Some(std::net::SocketAddr::V4(peer)) = peer {
            // A UDP connect selects the OS route without sending a datagram. Use
            // its source address, not the first NIC (which may be a virtual adapter).
            let local = std::net::UdpSocket::bind("0.0.0.0:0").and_then(|socket| {
                socket.connect(peer)?;
                socket.local_addr()
            });
            if let Ok(local) = local {
                if !local.ip().is_unspecified() {
                    return format!("{}:{port}", local.ip());
                }
            }
        }
    }
    format!("{hostname}:{port}")
}

fn valid_ipp_authority(authority: &str, hostname: &str, port: u16) -> bool {
    // RFC 3510: an ipp:// URI without an explicit port means TCP 631.
    // The HTTP Host header can also omit the default 631 port.
    if authority.rsplit_once(':').is_some() {
        valid_host(authority, hostname, port)
    } else {
        valid_host(&format!("{authority}:631"), hostname, port)
    }
}

fn job_id_from_uri(
    value: &str,
    path: &str,
    alias_path: Option<&str>,
    hostname: &str,
    port: u16,
    scheme: &str,
) -> Option<i32> {
    let uri = value.parse::<ipp::prelude::Uri>().ok()?;
    if uri.scheme_str() != Some(scheme) || uri.query().is_some() {
        return None;
    }
    let authority = uri.authority()?;
    if !valid_ipp_authority(authority.as_str(), hostname, port) {
        return None;
    }
    uri.path()
        .strip_prefix(&format!("{path}/jobs/"))
        .or_else(|| {
            alias_path.and_then(|alias| uri.path().strip_prefix(&format!("{alias}/jobs/")))
        })?
        .parse::<i32>()
        .ok()
}
fn attr<'a>(req: &'a IppRequestResponse, name: &str) -> Option<&'a IppValue> {
    req.attributes()
        .groups()
        .iter()
        .find_map(|g| g.attributes().get(name).map(|a| a.value()))
}
fn string_attr<'a>(req: &'a IppRequestResponse, name: &str) -> Option<&'a str> {
    match attr(req, name)? {
        IppValue::Keyword(v)
        | IppValue::Uri(v)
        | IppValue::NameWithoutLanguage(v)
        | IppValue::MimeMediaType(v)
        | IppValue::Charset(v) => Some(v),
        _ => None,
    }
}
fn reply(version: IppVersion, id: u32, status: StatusCode) -> IppRequestResponse {
    IppRequestResponse::new_response(version, status, id)
}
fn add(response: &mut IppRequestResponse, tag: DelimiterTag, name: &str, value: IppValue) {
    response
        .attributes_mut()
        .add(tag, IppAttribute::new(name, value));
}
fn keywords(values: &[&str]) -> IppValue {
    IppValue::Array(
        values
            .iter()
            .map(|v| IppValue::Keyword((*v).into()))
            .collect(),
    )
}

/// RFC 8011 dateTime in UTC (utc_dir '+', zero offset is always valid).
fn ipp_datetime(at: std::time::SystemTime) -> IppValue {
    let secs = at
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let days = (secs / 86400) as i64;
    let day_secs = secs % 86400;
    // Howard Hinnant's civil-from-days algorithm.
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u8;
    let year = (if month <= 2 { y + 1 } else { y }) as u16;
    IppValue::DateTime {
        year,
        month,
        day,
        hour: (day_secs / 3600) as u8,
        minutes: ((day_secs % 3600) / 60) as u8,
        seconds: (day_secs % 60) as u8,
        deci_seconds: 0,
        utc_dir: '+',
        utc_hours: 0,
        utc_mins: 0,
    }
}

/// PWG 5100.14 section 5.7.2: job URIs/UUIDs must not be reused; include the
/// wall clock so IDs survive a restart of this process.
fn job_uuid(path: &str, id: i32) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    uuid::Uuid::new_v5(
        &uuid::Uuid::NAMESPACE_OID,
        format!("airprinter-job:{path}:{id}:{nanos}").as_bytes(),
    )
    .to_string()
}

fn wants_printer_attribute(requested: Option<&IppValue>, name: &str) -> bool {
    // AirPrint's observed polling lists omit the printer URI. Include the
    // canonical submission endpoint and its matching security/authentication
    // values so the client can follow the IPv4 compatibility address.
    if [
        "printer-uri-supported",
        "uri-security-supported",
        "uri-authentication-supported",
    ]
    .contains(&name)
    {
        return true;
    }
    let matches = |value: &IppValue| matches!(value, IppValue::Keyword(key) if key == name || key == "all" || key == "printer-description" || key == "job-template");
    match requested {
        None => true,
        Some(IppValue::Array(values)) => values.iter().any(matches),
        Some(value) => matches(value),
    }
}

fn options(
    req: &IppRequestResponse,
    printer: &Printer,
    base: Option<&PrintOptions>,
) -> Result<PrintOptions, StatusCode> {
    let mut options = base.cloned().unwrap_or_else(|| PrintOptions {
        media: printer.capabilities.default_media.clone(),
        ..Default::default()
    });
    if let Some(value) = attr(req, "orientation-requested") {
        if let IppValue::Enum(value @ 3..=6) = value {
            options.orientation = *value;
        } else {
            return Err(StatusCode::ClientErrorAttributesOrValuesNotSupported);
        }
    }
    for (name, supported) in [("number-up", IppValue::Integer(1))] {
        if attr(req, name).is_some_and(|v| v != &supported) {
            return Err(StatusCode::ClientErrorAttributesOrValuesNotSupported);
        }
    }
    if attr(req, "output-bin").is_some_and(|value| value != &IppValue::Keyword("auto".into())) {
        return Err(StatusCode::ClientErrorAttributesOrValuesNotSupported);
    }
    if let Some(value) = attr(req, "print-quality") {
        if let IppValue::Enum(quality @ 3..=5) = value {
            options.quality = *quality;
        } else {
            return Err(StatusCode::ClientErrorAttributesOrValuesNotSupported);
        }
    }
    if let Some(value) = attr(req, "page-ranges") {
        let values = match value {
            IppValue::Array(values) => values.as_slice(),
            value => std::slice::from_ref(value),
        };
        let mut ranges = Vec::new();
        let mut previous = 0;
        for value in values {
            let IppValue::RangeOfInteger { min, max } = value else {
                return Err(StatusCode::ClientErrorAttributesOrValuesNotSupported);
            };
            // RFC 8011: page ranges must be positive, ascending and non-overlapping.
            if *min <= previous || max < min {
                return Err(StatusCode::ClientErrorBadRequest);
            }
            ranges.push((*min, *max));
            previous = *max;
        }
        if ranges.is_empty() {
            return Err(StatusCode::ClientErrorBadRequest);
        }
        options.page_ranges = ranges;
    }
    if let Some(value) = attr(req, "print-scaling") {
        match value {
            IppValue::Keyword(value)
                if ["auto", "auto-fit", "fit", "fill", "none"].contains(&value.as_str()) =>
            {
                options.scaling = value.clone();
            }
            _ => return Err(StatusCode::ClientErrorAttributesOrValuesNotSupported),
        }
    }
    if let Some(value) = attr(req, "ipp-attribute-fidelity") {
        if let IppValue::Boolean(value) = value {
            options.fidelity = *value;
        } else {
            return Err(StatusCode::ClientErrorBadRequest);
        }
    }
    let mut named = false;
    if let Some(value) = attr(req, "job-name") {
        options.job_name = match value {
            IppValue::NameWithoutLanguage(value) => value.clone(),
            IppValue::NameWithLanguage { name, .. } => name.clone(),
            _ => return Err(StatusCode::ClientErrorAttributesOrValuesNotSupported),
        };
        named = true;
    }
    // STD 92 Table 8: "document-name" names the document; use it when the
    // client did not supply a job name (iOS Send-Document carries it).
    if !named {
        if let Some(value) = attr(req, "document-name") {
            options.job_name = match value {
                IppValue::NameWithoutLanguage(value) => value.clone(),
                IppValue::NameWithLanguage { name, .. } => name.clone(),
                IppValue::TextWithoutLanguage(value) => value.clone(),
                IppValue::TextWithLanguage { text, .. } => text.clone(),
                _ => return Err(StatusCode::ClientErrorAttributesOrValuesNotSupported),
            };
        }
    }
    // STD 92 Table 12: validate against the single advertised resolution.
    if let Some(value) = attr(req, "printer-resolution") {
        let supported = matches!(
            value,
            IppValue::Resolution {
                cross_feed: 300,
                feed: 300,
                units: 3,
            }
        );
        if !supported {
            return Err(StatusCode::ClientErrorAttributesOrValuesNotSupported);
        }
    }
    // STD 92 Table 12 (conditional for PDF): only collated single-document jobs.
    if let Some(value) = attr(req, "multiple-document-handling") {
        if value != &IppValue::Keyword("separate-documents-collated-copies".into()) {
            return Err(StatusCode::ClientErrorAttributesOrValuesNotSupported);
        }
    }
    if let Some(value) = attr(req, "copies") {
        if let IppValue::Integer(value) = value {
            options.copies = *value;
        } else {
            return Err(StatusCode::ClientErrorAttributesOrValuesNotSupported);
        }
    }
    for (name, output) in [
        ("sides", &mut options.sides),
        ("media", &mut options.media),
        ("print-color-mode", &mut options.color_mode),
    ] {
        if let Some(value) = attr(req, name) {
            if let IppValue::Keyword(value) = value {
                *output = value.clone();
            } else {
                return Err(StatusCode::ClientErrorAttributesOrValuesNotSupported);
            }
        }
    }
    if let Some(value) = attr(req, "media-col") {
        let IppValue::Collection(media) = value else {
            return Err(StatusCode::ClientErrorAttributesOrValuesNotSupported);
        };
        for (name, supported) in [("media-source", "auto"), ("media-type", "stationery")] {
            if media
                .get(name)
                .is_some_and(|value| value != &IppValue::Keyword(supported.into()))
            {
                return Err(StatusCode::ClientErrorAttributesOrValuesNotSupported);
            }
        }
        if let Some(IppValue::Keyword(name)) = media.get("media-size-name") {
            options.media = name.clone();
        } else if let Some(IppValue::Collection(size)) = media.get("media-size") {
            let dimensions = (size.get("x-dimension"), size.get("y-dimension"));
            let matched = printer.capabilities.media_sizes.iter().find(|media| {
                dimensions
                    == (
                        Some(&IppValue::Integer(media.width)),
                        Some(&IppValue::Integer(media.height)),
                    )
            });
            options.media = matched
                .ok_or(StatusCode::ClientErrorAttributesOrValuesNotSupported)?
                .name
                .clone();
        } else {
            return Err(StatusCode::ClientErrorAttributesOrValuesNotSupported);
        }
    }
    if options.copies < 1
        || options.copies > printer.capabilities.max_copies
        || !["one-sided", "two-sided-long-edge", "two-sided-short-edge"]
            .contains(&options.sides.as_str())
        || (options.sides != "one-sided" && !printer.capabilities.duplex)
        || !["monochrome", "color", "auto"].contains(&options.color_mode.as_str())
        || (options.color_mode == "color" && !printer.capabilities.color)
        || !printer.capabilities.media.contains(&options.media)
    {
        return Err(StatusCode::ClientErrorAttributesOrValuesNotSupported);
    }
    let paper = printer
        .capabilities
        .media_sizes
        .iter()
        .find(|media| media.name == options.media)
        .ok_or(StatusCode::ClientErrorAttributesOrValuesNotSupported)?;
    options.paper_kind = paper.windows_kind;
    options.paper_width = (paper.width as f64 / 25.4).round() as i32;
    options.paper_height = (paper.height as f64 / 25.4).round() as i32;
    if let Some(compression) = string_attr(req, "compression") {
        if compression != "none" {
            return Err(StatusCode::ClientErrorCompressionNotSupported);
        }
    }
    Ok(options)
}

#[cfg(test)]
fn process(shared: &Shared, path: &str, host: &str, req: IppRequestResponse) -> IppRequestResponse {
    process_request(shared, path, host, req, false)
}

#[cfg(test)]
fn process_request(
    shared: &Shared,
    path: &str,
    host: &str,
    req: IppRequestResponse,
    secure: bool,
) -> IppRequestResponse {
    let port = host
        .rsplit_once(':')
        .and_then(|(_, number)| number.parse().ok())
        .unwrap_or(631);
    process_request_at_port(shared, path, host, req, secure, port)
}

#[cfg(test)]
fn process_request_at_port(
    shared: &Shared,
    path: &str,
    host: &str,
    req: IppRequestResponse,
    secure: bool,
    port: u16,
) -> IppRequestResponse {
    process_request_at_port_with_alias(shared, path, None, host, req, secure, port)
}

fn process_request_at_port_with_alias(
    shared: &Shared,
    path: &str,
    alias_path: Option<&str>,
    host: &str,
    req: IppRequestResponse,
    secure: bool,
    port: u16,
) -> IppRequestResponse {
    let id = req.header().request_id;
    let version = req.header().version;
    let error = |status| reply(version, id, status);
    if ![IppVersion::v1_1(), IppVersion::v2_0()].contains(&version) {
        return error(StatusCode::ServerErrorVersionNotSupported);
    }
    if string_attr(&req, "attributes-charset") != Some("utf-8") {
        return error(StatusCode::ClientErrorCharsetNotSupported);
    }
    let (mut printer, mut target_name) = {
        let mut state = shared.lock().unwrap_or_else(|e| e.into_inner());
        for job in state.jobs.values_mut().filter(|j| {
            j.state == 4
                && j.reason == "job-incoming"
                && j.created.elapsed() > Duration::from_secs(60)
        }) {
            job.state = 8;
            job.reason = "job-data-insufficient".into();
            job.message = "No document received within 60 seconds".into();
            job.ended = Some(Instant::now());
            trace!("[IPP] job={} timed out waiting for Send-Document", job.id);
        }
        let Some((p, _)) = state.printers.get(path) else {
            return error(StatusCode::ClientErrorNotFound);
        };
        let target = state
            .routes
            .get(path)
            .cloned()
            .unwrap_or_else(|| p.name.clone());
        if let Some(alias) = alias_path.filter(|alias| *alias != path) {
            let default_alias = alias == DEFAULT_PRINT_PATH && state.printers.len() == 1;
            let current_gateway_target = p.id == crate::models::printer::GATEWAY_PRINTER_ID
                && alias == legacy_target_resource_path(&target);
            if !default_alias && !current_gateway_target {
                return error(StatusCode::ClientErrorNotFound);
            }
        }
        (p.clone(), target)
    };
    let scheme = if secure { "ipps" } else { "ipp" };
    let uri = format!("{scheme}://{host}{path}");
    if let Some(target) = string_attr(&req, "printer-uri") {
        let hostname = shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .printers
            .get(path)
            .map(|(_, h)| h.clone())
            .unwrap_or_default();
        let valid = target.parse::<ipp::prelude::Uri>().ok().is_some_and(|uri| {
            uri.scheme_str() == Some(scheme)
                && (uri.path() == path || alias_path == Some(uri.path()))
                && uri.query().is_none()
                && uri
                    .authority()
                    .is_some_and(|a| valid_ipp_authority(a.as_str(), &hostname, port))
        });
        if !valid {
            trace!("[IPP] rejecting printer-uri={target:?}; expected={uri:?}");
            return error(StatusCode::ClientErrorNotFound);
        }
    }
    let op = req.header().operation_or_status;
    if op == 0x000b {
        #[cfg(debug_assertions)]
        trace!(
            "[IPP] requested-attributes={:?} document-format={:?}",
            attr(&req, "requested-attributes"),
            attr(&req, "document-format")
        );
        return printer_attributes(
            shared,
            &printer,
            &uri,
            version,
            id,
            attr(&req, "requested-attributes"),
        );
    }
    if op == 0x000a {
        let which = string_attr(&req, "which-jobs").unwrap_or("not-completed");
        if !["completed", "not-completed", "all"].contains(&which) {
            return error(StatusCode::ClientErrorAttributesOrValuesNotSupported);
        }
        let mine = attr(&req, "my-jobs") == Some(&IppValue::Boolean(true));
        let owner = string_attr(&req, "requesting-user-name").unwrap_or("anonymous");
        let limit = match attr(&req, "limit") {
            Some(IppValue::Integer(n)) if *n > 0 => *n as usize,
            None => usize::MAX,
            _ => return error(StatusCode::ClientErrorAttributesOrValuesNotSupported),
        };
        let state = shared.lock().unwrap_or_else(|e| e.into_inner());
        let mut response = error(StatusCode::SuccessfulOk);
        let mut jobs: Vec<_> = state
            .jobs
            .values()
            .filter(|j| {
                j.path == path
                    && (!mine || j.owner == owner)
                    && match which {
                        "completed" => j.state >= 7,
                        "all" => true,
                        _ => j.state < 7,
                    }
            })
            .collect();
        jobs.sort_by_key(|job| job.id);
        for job in jobs.into_iter().take(limit) {
            response
                .attributes_mut()
                .groups_mut()
                .push(job_group(job, &uri, state.started));
        }
        return response;
    }
    // PWG 5100.14 Table 4: Identify-Printer (0x003C). A Windows queue proxy has
    // no display or speaker to flash; accept and log instead of failing.
    if op == 0x003c {
        trace!("[IPP] request_id={id} Identify-Printer accepted (no physical indicator)");
        return error(StatusCode::SuccessfulOk);
    }
    // PWG 5100.14 Table 4: Cancel-My-Jobs (0x0039).
    if op == 0x0039 {
        let owner = string_attr(&req, "requesting-user-name")
            .unwrap_or("anonymous")
            .to_string();
        let mut state = shared.lock().unwrap_or_else(|e| e.into_inner());
        let ids: Vec<i32> = state
            .jobs
            .values()
            .filter(|j| j.path == path && j.owner == owner && j.state < 7)
            .map(|j| j.id)
            .collect();
        trace!("[IPP] request_id={id} Cancel-My-Jobs owner={owner:?} jobs={ids:?}");
        for job_id in ids {
            if let Some(current) = state.jobs.get_mut(&job_id) {
                if let Some(spool_id) = current.spool_id {
                    let _ = print_job::cancel_spool_job(&current.printer, spool_id);
                }
                current.canceled.store(true, Ordering::Release);
                current.state = 7;
                current.reason = "canceled-by-user".into();
                current.message.clear();
                current.ended = Some(Instant::now());
            }
        }
        return error(StatusCode::SuccessfulOk);
    }
    let supplied_job_id = match attr(&req, "job-id") {
        Some(IppValue::Integer(id)) => Some(*id),
        Some(_) => return error(StatusCode::ClientErrorBadRequest),
        None => None,
    };
    let hostname = shared
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .printers
        .get(path)
        .map(|(_, hostname)| hostname.clone())
        .unwrap_or_default();
    let supplied_job_uri_id = string_attr(&req, "job-uri")
        .and_then(|value| job_id_from_uri(value, path, alias_path, &hostname, port, scheme));
    if attr(&req, "job-uri").is_some() && supplied_job_uri_id.is_none() {
        return error(StatusCode::ClientErrorNotFound);
    }
    if supplied_job_id.is_some()
        && supplied_job_uri_id.is_some()
        && supplied_job_id != supplied_job_uri_id
    {
        return error(StatusCode::ClientErrorConflictingAttributes);
    }
    let target_id = supplied_job_id.or(supplied_job_uri_id);
    if [0x0008, 0x0009, 0x003b].contains(&op) {
        let (job, started) = {
            let state = shared.lock().unwrap_or_else(|e| e.into_inner());
            (
                state
                    .jobs
                    .get(&target_id.unwrap_or(0))
                    .filter(|j| j.path == path)
                    .cloned(),
                state.started,
            )
        };
        let Some(job) = job else {
            return error(StatusCode::ClientErrorNotFound);
        };
        if op == 0x0008 {
            let mut state = shared.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(current) = state.jobs.get_mut(&job.id) {
                if current.state >= 7 {
                    return error(StatusCode::ClientErrorNotPossible);
                }
                if let Some(spool_id) = current.spool_id {
                    if print_job::cancel_spool_job(&current.printer, spool_id).is_err() {
                        return error(StatusCode::ClientErrorNotPossible);
                    }
                }
                current.canceled.store(true, Ordering::Release);
                if current.state == 4 {
                    current.state = 7;
                    current.reason = "job-canceled-by-user".into();
                }
                current.ended = Some(Instant::now());
            }
            return error(StatusCode::SuccessfulOk);
        }
        // PWG 5100.14 Table 4: Close-Job (0x003B). Documents submitted via
        // Send-Document run immediately here; an empty job completes in place.
        if op == 0x003b {
            let mut state = shared.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(current) = state.jobs.get_mut(&job.id) {
                if current.state == 4 && current.reason == "job-incoming" {
                    current.state = 9;
                    current.reason = "job-completed-successfully".into();
                    current.message = "Closed without a document".into();
                    current.ended = Some(Instant::now());
                }
            }
        }
        let job = if op == 0x003b {
            shared
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .jobs
                .get(&job.id)
                .cloned()
                .unwrap_or(job)
        } else {
            job
        };
        let mut response = error(StatusCode::SuccessfulOk);
        response
            .attributes_mut()
            .groups_mut()
            .push(job_group(&job, &uri, started));
        return response;
    }
    if ![0x0002, 0x0004, 0x0005, 0x0006].contains(&op) {
        return error(StatusCode::ServerErrorOperationNotSupported);
    }
    let existing = if op == 0x0006 {
        let state = shared.lock().unwrap_or_else(|e| e.into_inner());
        match state
            .jobs
            .get(&target_id.unwrap_or(0))
            .filter(|j| j.path == path && j.state == 4 && j.reason == "job-incoming")
        {
            Some(job) => Some(job.clone()),
            None => return error(StatusCode::ClientErrorNotFound),
        }
    } else {
        None
    };
    if let Some(job) = &existing {
        printer = job.profile.clone();
        target_name = job.printer.clone();
    }
    if [0x0002, 0x0004, 0x0005, 0x0006].contains(&op)
        && matches!(
            printer.status,
            PrinterStatus::Offline | PrinterStatus::Error(_)
        )
    {
        return error(StatusCode::ServerErrorNotAcceptingJobs);
    }
    if op == 0x0006 && attr(&req, "last-document") != Some(&IppValue::Boolean(true)) {
        return error(StatusCode::ServerErrorMultipleDocumentJobsNotSupported);
    }
    #[cfg(debug_assertions)]
    for name in [
        "orientation-requested",
        "number-up",
        "print-quality",
        "page-ranges",
        "print-scaling",
        "copies",
        "sides",
        "media",
        "print-color-mode",
        "media-col",
        "compression",
        "document-format",
    ] {
        if let Some(value) = attr(&req, name) {
            trace!("[IPP] job option {name}={value:?}");
        }
    }
    let opts = match options(&req, &printer, existing.as_ref().map(|j| &j.options)) {
        Ok(o) => o,
        Err(e) => {
            trace!("[IPP] job options rejected: {e:?}");
            return error(e);
        }
    };
    let mut format = string_attr(&req, "document-format")
        .unwrap_or("application/pdf")
        .to_string();
    if format != "application/octet-stream" && !FORMATS.contains(&format.as_str()) {
        return error(StatusCode::ClientErrorDocumentFormatNotSupported);
    }
    if op == 0x0004 {
        return error(StatusCode::SuccessfulOk);
    }
    let owner = string_attr(&req, "requesting-user-name")
        .unwrap_or("anonymous")
        .to_string();
    let name = opts.job_name.clone();
    let mut document = Vec::new();
    if req.into_payload().read_to_end(&mut document).is_err() {
        return error(StatusCode::ClientErrorBadRequest);
    }
    if format == "application/octet-stream" && op != 0x0005 {
        match FORMATS
            .iter()
            .find(|format| print_job::validate_document(format, &document))
        {
            Some(detected) => format = (*detected).to_string(),
            None => return error(StatusCode::ClientErrorDocumentFormatNotSupported),
        }
    }
    if op != 0x0005 && !print_job::validate_document(&format, &document) {
        return error(StatusCode::ClientErrorDocumentFormatError);
    }
    if op == 0x0005 && !document.is_empty() {
        return error(StatusCode::ClientErrorBadRequest);
    }
    let job = {
        let mut state = shared.lock().unwrap_or_else(|e| e.into_inner());
        if !state.printers.contains_key(path) {
            return error(StatusCode::ClientErrorNotFound);
        }
        if state
            .jobs
            .values()
            .filter(|j| j.path == path && j.state < 7)
            .count()
            >= 32
            && existing.is_none()
        {
            return error(StatusCode::ServerErrorBusy);
        }
        if op != 0x0005
            && state
                .jobs
                .values()
                .filter(|j| j.path == path && matches!(j.state, 3 | 5 | 6))
                .count()
                >= 4
        {
            return error(StatusCode::ServerErrorBusy);
        }
        if state.jobs.len() >= 256 {
            if let Some(oldest) = state
                .jobs
                .values()
                .filter(|j| j.state >= 7)
                .map(|j| j.id)
                .min()
            {
                state.jobs.remove(&oldest);
            }
        }
        let mut job = if let Some(existing) = existing {
            if !state
                .jobs
                .get(&existing.id)
                .is_some_and(|j| j.state == 4 && j.reason == "job-incoming")
            {
                return error(StatusCode::ClientErrorNotPossible);
            }
            existing
        } else {
            let job_id = match NEXT_JOB_ID
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |id| id.checked_add(1))
            {
                Ok(id) => id,
                Err(_) => return error(StatusCode::ServerErrorBusy),
            };
            Job {
                id: job_id,
                path: path.into(),
                profile: printer.clone(),
                printer: target_name.clone(),
                options: opts.clone(),
                owner,
                name,
                state: 4,
                reason: "job-incoming".into(),
                message: String::new(),
                canceled: Arc::new(AtomicBool::new(false)),
                spool_id: None,
                created: Instant::now(),
                uuid: job_uuid(path, job_id),
                processed: None,
                ended: None,
            }
        };
        job.name = opts.job_name.clone();
        job.options = opts;
        if op != 0x0005 {
            job.state = 3;
            job.reason = "job-queued".into();
        }
        state.jobs.insert(job.id, job.clone());
        job
    };
    let started = shared.lock().unwrap_or_else(|e| e.into_inner()).started;
    let mut response = error(StatusCode::SuccessfulOk);
    response
        .attributes_mut()
        .groups_mut()
        .push(job_group(&job, &uri, started));
    if op != 0x0005 {
        run_job(shared.clone(), job, document, format);
    }
    response
}

fn job_group(job: &Job, uri: &str, started: Instant) -> IppAttributeGroup {
    let mut group = IppAttributeGroup::new(DelimiterTag::JobAttributes);
    let elapsed_secs =
        |at: Instant| at.duration_since(started).as_secs().min(i32::MAX as u64) as i32;
    // Instant carries no wall clock; reconstruct it from the current time.
    let wall_at = |at: Instant| {
        std::time::SystemTime::now()
            .checked_sub(at.elapsed())
            .unwrap_or(std::time::SystemTime::now())
    };
    // PWG 5100.14 Tables 10 (description) and 11 (status): full job attribute set.
    for (name, value) in [
        ("job-id", IppValue::Integer(job.id)),
        ("job-uri", IppValue::Uri(format!("{uri}/jobs/{}", job.id))),
        ("job-printer-uri", IppValue::Uri(uri.into())),
        ("job-state", IppValue::Enum(job.state)),
        ("job-state-reasons", IppValue::Keyword(job.reason.clone())),
        (
            "job-state-message",
            IppValue::TextWithoutLanguage(job.message.clone()),
        ),
        ("job-name", IppValue::NameWithoutLanguage(job.name.clone())),
        (
            "job-originating-user-name",
            IppValue::NameWithoutLanguage(job.owner.clone()),
        ),
        ("job-uuid", IppValue::Uri(format!("urn:uuid:{}", job.uuid))),
        (
            "job-printer-up-time",
            IppValue::Integer(elapsed_secs(Instant::now())),
        ),
        ("time-at-creation", IppValue::Integer(elapsed_secs(job.created))),
        (
            "time-at-processing",
            job.processed
                .map(|at| IppValue::Integer(elapsed_secs(at)))
                .unwrap_or(IppValue::NoValue),
        ),
        (
            "time-at-completed",
            job.ended
                .map(|at| IppValue::Integer(elapsed_secs(at)))
                .unwrap_or(IppValue::NoValue),
        ),
        ("date-time-at-creation", ipp_datetime(wall_at(job.created))),
        (
            "date-time-at-processing",
            job.processed
                .map(|at| ipp_datetime(wall_at(at)))
                .unwrap_or(IppValue::NoValue),
        ),
        (
            "date-time-at-completed",
            job.ended
                .map(|at| ipp_datetime(wall_at(at)))
                .unwrap_or(IppValue::NoValue),
        ),
        // Page counts are unknown until the Windows driver renders the job.
        ("job-impressions", IppValue::Integer(0)),
        ("job-impressions-completed", IppValue::Integer(0)),
    ] {
        group
            .attributes_mut()
            .insert(name.into(), IppAttribute::new(name, value));
    }
    group
}

fn run_job(shared: Shared, job: Job, document: Vec<u8>, format: String) {
    trace!(
        "[IPP] job={} path={:?} target_queue={:?} received document bytes={} format={format} options={:?}",
        job.id,
        job.path,
        job.printer,
        document.len(),
        job.options
    );
    let receive_directory = shared
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .receive_directory
        .clone();
    if let Some(directory) = receive_directory {
        let result = save_received_document(&directory, job.id, &format, &document);
        match result {
            Ok(path) => {
                let mut state = shared.lock().unwrap_or_else(|error| error.into_inner());
                if let Some(current) = state.jobs.get_mut(&job.id) {
                    if current.canceled.load(Ordering::Acquire) {
                        current.state = 7;
                        current.reason = "job-canceled-by-user".into();
                        current.message.clear();
                        current.ended = Some(Instant::now());
                    } else {
                        current.state = 4;
                        current.reason = "job-hold-until-specified".into();
                        current.message = format!(
                            "Received {} bytes; diagnostic only; no Windows print submitted. Saved: {}",
                            document.len(),
                            path.display()
                        );
                    }
                }
                trace!(
                    "[IPP] job={} receive-only saved bytes={} path={}",
                    job.id,
                    document.len(),
                    path.display()
                );
            }
            Err(error) => {
                trace!("[IPP] job={} receive-only save failed: {error}", job.id);
                set_job(&shared, job.id, 8, "document-unprintable-error", &error);
            }
        }
        return;
    }
    thread::spawn(move || {
        let backend = shared.lock().unwrap_or_else(|e| e.into_inner()).backend;
        set_job(&shared, job.id, 5, "job-transforming", "");
        match backend(
            &job.printer,
            &document,
            &format,
            &job.options,
            &job.canceled,
        ) {
            Err(error) => {
                trace!("[IPP] job={} Windows submission failed: {error}", job.id);
                if job.canceled.load(Ordering::Acquire) {
                    set_job(&shared, job.id, 7, "job-canceled-by-user", "");
                } else {
                    set_job(&shared, job.id, 8, "document-unprintable-error", &error);
                }
            }
            Ok(spool_id) => {
                trace!("[IPP] job={} Windows spool_id={spool_id}", job.id);
                if let Some(current) = shared
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .jobs
                    .get_mut(&job.id)
                {
                    current.spool_id = Some(spool_id);
                }
                loop {
                    if job.canceled.load(Ordering::Acquire) {
                        let _ = print_job::cancel_spool_job(&job.printer, spool_id);
                        set_job(&shared, job.id, 7, "job-canceled-by-user", "");
                        break;
                    }
                    #[cfg(test)]
                    if spool_id == 0 {
                        set_job(
                            &shared,
                            job.id,
                            9,
                            "job-completed-successfully",
                            "Simulated backend",
                        );
                        break;
                    }
                    match print_job::spool_state(&job.printer, spool_id) {
                        Ok(None) => {
                            set_job(&shared, job.id, 9, "job-completed-successfully", "Job left the Windows spool queue; physical output is not independently confirmed");
                            break;
                        }
                        Ok(Some(status)) if status & (0x80 | 0x1000) != 0 => {
                            set_job(
                                &shared,
                                job.id,
                                9,
                                "job-completed-successfully",
                                "Windows reports completion",
                            );
                            break;
                        }
                        Ok(Some(status)) if status & (0x4 | 0x100) != 0 => {
                            set_job(
                                &shared,
                                job.id,
                                7,
                                "job-canceled-at-device",
                                "Windows removed the job",
                            );
                            break;
                        }
                        Ok(Some(status)) if status & (0x1 | 0x2 | 0x20 | 0x40 | 0x200) != 0 => {
                            set_job(
                                &shared,
                                job.id,
                                6,
                                "printer-stopped",
                                "Windows printer requires attention",
                            )
                        }
                        Ok(Some(_)) => set_job(
                            &shared,
                            job.id,
                            5,
                            "job-queued-in-device",
                            "Submitted to the selected Windows queue",
                        ),
                        Err(error) => set_job(&shared, job.id, 6, "printer-stopped", &error),
                    }
                    thread::sleep(Duration::from_secs(2));
                }
            }
        }
    });
}

fn save_received_document(
    directory: &std::path::Path,
    job_id: i32,
    format: &str,
    document: &[u8],
) -> Result<PathBuf, String> {
    let extension = match format {
        "application/pdf" => ".pdf",
        "image/jpeg" => ".jpg",
        "image/urf" => ".urf",
        "image/pwg-raster" => ".pwg",
        _ => return Err(format!("Unsupported receive-only format: {format}")),
    };
    let prefix = format!("airprinter-job-{job_id}-");
    let mut temp = tempfile::Builder::new()
        .prefix(&prefix)
        .suffix(extension)
        .tempfile_in(directory)
        .map_err(|error| format!("Could not create receive file: {error}"))?;
    temp.write_all(document)
        .map_err(|error| format!("Could not write receive file: {error}"))?;
    temp.as_file()
        .sync_all()
        .map_err(|error| format!("Could not sync receive file: {error}"))?;
    temp.keep()
        .map(|(_, path)| path)
        .map_err(|error| format!("Could not keep receive file: {}", error.error))
}
fn set_job(shared: &Shared, id: i32, state: i32, reason: &str, message: &str) {
    if let Some(job) = shared
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .jobs
        .get_mut(&id)
    {
        if job.state != state || job.message != message {
            trace!("[IPP] job={id} state={state} reason={reason} message={message}");
        }
        if job.canceled.load(Ordering::Acquire) && state >= 7 {
            job.state = 7;
            job.reason = "job-canceled-by-user".into();
            job.message.clear();
        } else {
            job.state = state;
            job.reason = reason.into();
            job.message = message.chars().take(1024).collect();
        }
        let now = Instant::now();
        if matches!(job.state, 5 | 6) && job.processed.is_none() {
            job.processed = Some(now);
        }
        if job.state >= 7 && job.ended.is_none() {
            job.ended = Some(now);
        }
    }
}

fn printer_attributes(
    shared: &Shared,
    printer: &Printer,
    uri: &str,
    version: IppVersion,
    id: u32,
    requested: Option<&IppValue>,
) -> IppRequestResponse {
    let state = shared.lock().unwrap_or_else(|e| e.into_inner());
    let path = format!("/{}", printer.resource_path());
    let active = state
        .jobs
        .values()
        .filter(|j| j.path == path && j.state < 7)
        .count();
    let available = matches!(printer.status, PrinterStatus::Online | PrinterStatus::Busy);
    let caps = &printer.capabilities;
    let mut response = reply(version, id, StatusCode::SuccessfulOk);
    let mut put = |name: &str, value| {
        if wants_printer_attribute(requested, name) {
            add(&mut response, DelimiterTag::PrinterAttributes, name, value);
        }
    };
    put("printer-uri-supported", IppValue::Uri(uri.into()));
    let hostname = state
        .printers
        .values()
        .find(|(p, _)| p.id == printer.id)
        .map(|(_, h)| h.as_str())
        .unwrap_or("");
    // PWG 5100.14 section 8.4 / note 6: absolute HTTP(S) URIs derived from the
    // same authority the client just used, never link-local or invented names.
    let authority = uri
        .split("://")
        .nth(1)
        .and_then(|rest| rest.split('/').next())
        .unwrap_or("");
    let http_origin = format!(
        "{}://{}",
        if uri.starts_with("ipps:") { "https" } else { "http" },
        authority
    );
    let now_wall = std::time::SystemTime::now();
    let started_wall = now_wall
        .checked_sub(state.started.elapsed())
        .unwrap_or(now_wall);
    // PWG 5100.14 Table 5: identity attributes iOS requests in its discovery
    // query; "printer-dns-sd-name" ties the response to the mDNS instance.
    put(
        "printer-dns-sd-name",
        IppValue::NameWithoutLanguage(printer.dns_sd_name(hostname)),
    );
    put("printer-location", IppValue::TextWithoutLanguage(String::new()));
    put(
        "printer-organization",
        IppValue::TextWithoutLanguage(String::new()),
    );
    put(
        "printer-organizational-unit",
        IppValue::TextWithoutLanguage(String::new()),
    );
    // Geo-location is unknown; out-of-band "no-value" per PWG 5100.13.
    put("printer-geo-location", IppValue::NoValue);
    put(
        "printer-more-info",
        IppValue::Uri(format!("{http_origin}/")),
    );
    put(
        "printer-icons",
        IppValue::Array(vec![
            IppValue::Uri(format!("{http_origin}/icons/128.png")),
            IppValue::Uri(format!("{http_origin}/icons/512.png")),
        ]),
    );
    put(
        "printer-get-attributes-supported",
        keywords(&["document-format"]),
    );
    put("job-ids-supported", IppValue::Boolean(true));
    // No constraints/resolvers configured: empty 1setOf encodes as no-value.
    put("job-constraints-supported", IppValue::NoValue);
    put("job-resolvers-supported", IppValue::NoValue);
    put("preferred-attributes-supported", IppValue::Boolean(false));
    put(
        "multiple-operation-timeout-action",
        IppValue::Keyword("abort-job".into()),
    );
    put(
        "overrides-supported",
        keywords(&["document-numbers", "pages"]),
    );
    put("identify-actions-default", keywords(&["display"]));
    put("identify-actions-supported", keywords(&["display"]));
    // iOS requests this on every status poll; nothing is mandatory here.
    put("printer-mandatory-job-attributes", IppValue::NoValue);
    // PWG 5100.14 Table 7: status attributes.
    put("pages-per-minute", IppValue::Integer(1));
    put("pages-per-minute-color", IppValue::Integer(1));
    put("printer-config-change-time", IppValue::Integer(1));
    put(
        "printer-config-change-date-time",
        ipp_datetime(started_wall),
    );
    put("printer-state-change-time", IppValue::Integer(1));
    put("printer-state-change-date-time", ipp_datetime(started_wall));
    put("printer-current-time", ipp_datetime(now_wall));
    put(
        "printer-uuid",
        IppValue::Uri(format!("urn:uuid:{}", printer.uuid(hostname))),
    );
    put(
        "printer-name",
        IppValue::NameWithoutLanguage(printer.name.clone()),
    );
    put(
        "printer-info",
        IppValue::TextWithoutLanguage(printer.name.clone()),
    );
    put(
        "printer-state-message",
        IppValue::TextWithoutLanguage(
            if available {
                "Windows print queue available"
            } else {
                "Windows print queue unavailable"
            }
            .into(),
        ),
    );
    put("printer-kind", keywords(&["document"]));
    put(
        "printer-make-and-model",
        IppValue::TextWithoutLanguage(printer.name.clone()),
    );
    put(
        "printer-state",
        IppValue::Enum(if !available {
            5
        } else if active > 0 {
            4
        } else {
            3
        }),
    );
    put(
        "printer-state-reasons",
        keywords(if available { &["none"] } else { &["offline"] }),
    );
    put("printer-is-accepting-jobs", IppValue::Boolean(available));
    put(
        "printer-up-time",
        IppValue::Integer(state.started.elapsed().as_secs().min(i32::MAX as u64) as i32),
    );
    put("queued-job-count", IppValue::Integer(active as i32));
    put(
        "which-jobs-supported",
        keywords(&["completed", "not-completed", "all"]),
    );
    put("uri-authentication-supported", keywords(&["none"]));
    put(
        "uri-security-supported",
        keywords(&[if uri.starts_with("ipps:") {
            "tls"
        } else {
            "none"
        }]),
    );
    put("ipp-versions-supported", keywords(&["1.1", "2.0"]));
    // iOS/iPadOS 的系统打印对话框要求该标记才会列出打印机（见 AGENTS.md）。
    put("ipp-features-supported", keywords(&["ipp-everywhere"]));
    // PWG 5100.14 Table 4: includes Cancel-My-Jobs (0x0039), Close-Job
    // (0x003B) and Identify-Printer (0x003C).
    put(
        "operations-supported",
        IppValue::Array(
            [2, 4, 5, 6, 8, 9, 10, 11, 0x39, 0x3b, 0x3c]
                .map(IppValue::Enum)
                .to_vec(),
        ),
    );
    put(
        "document-format-supported",
        IppValue::Array(
            FORMATS
                .iter()
                .copied()
                .chain(std::iter::once("application/octet-stream"))
                .map(|s| IppValue::MimeMediaType(s.into()))
                .collect(),
        ),
    );
    put(
        "document-format-default",
        IppValue::MimeMediaType("application/pdf".into()),
    );
    put("document-password-supported", IppValue::Integer(0));
    // PWG 5100.13: declaring PDF/JPEG also requires their size/feature attributes.
    for name in [
        "pdf-k-octets-supported",
        "jpeg-k-octets-supported",
        "job-k-octets-supported",
    ] {
        put(
            name,
            IppValue::RangeOfInteger {
                min: 0,
                max: (MAX_UPLOAD / 1024) as i32,
            },
        );
    }
    put("jpeg-features-supported", keywords(&["none"]));
    // Conservative bounds fit within the Windows helper's 130-million-pixel limit.
    put(
        "jpeg-x-dimension-supported",
        IppValue::RangeOfInteger { min: 0, max: 10000 },
    );
    put(
        "jpeg-y-dimension-supported",
        IppValue::RangeOfInteger { min: 1, max: 10000 },
    );
    put(
        "pdf-versions-supported",
        keywords(&[
            "adobe-1.3",
            "adobe-1.4",
            "adobe-1.5",
            "adobe-1.6",
            "adobe-1.7",
        ]),
    );
    put("compression-supported", keywords(&["none"]));
    put("compression-default", IppValue::Keyword("none".into()));
    put("charset-configured", IppValue::Charset("utf-8".into()));
    put("charset-supported", IppValue::Charset("utf-8".into()));
    put(
        "natural-language-configured",
        IppValue::NaturalLanguage("en".into()),
    );
    put(
        "generated-natural-language-supported",
        IppValue::NaturalLanguage("en".into()),
    );
    put("copies-default", IppValue::Integer(1));
    put("orientation-requested-default", IppValue::Enum(3));
    put(
        "landscape-orientation-requested-preferred",
        IppValue::Enum(4),
    );
    put(
        "orientation-requested-supported",
        IppValue::Array([3, 4, 5, 6].map(IppValue::Enum).to_vec()),
    );
    put("number-up-default", IppValue::Integer(1));
    put("number-up-supported", IppValue::Integer(1));
    put("page-ranges-supported", IppValue::Boolean(true));
    put("print-quality-default", IppValue::Enum(4));
    put(
        "print-quality-supported",
        IppValue::Array([3, 4, 5].map(IppValue::Enum).to_vec()),
    );
    put("print-scaling-default", IppValue::Keyword("fit".into()));
    put(
        "print-scaling-supported",
        keywords(&["auto", "auto-fit", "fit", "fill", "none"]),
    );
    put(
        "copies-supported",
        IppValue::RangeOfInteger {
            min: 1,
            max: caps.max_copies,
        },
    );
    put("color-supported", IppValue::Boolean(caps.color));
    put("print-color-mode-default", IppValue::Keyword("auto".into()));
    put(
        "print-color-mode-supported",
        keywords(if caps.color {
            &["monochrome", "color", "auto"]
        } else {
            &["monochrome", "auto"]
        }),
    );
    put("sides-default", IppValue::Keyword("one-sided".into()));
    put(
        "sides-supported",
        keywords(if caps.duplex {
            &["one-sided", "two-sided-long-edge", "two-sided-short-edge"]
        } else {
            &["one-sided"]
        }),
    );
    put(
        "media-default",
        IppValue::Keyword(caps.default_media.clone()),
    );
    put("media-type-default", IppValue::Keyword("stationery".into()));
    put("media-type-supported", keywords(&["stationery"]));
    put("media-source-supported", keywords(&["auto"]));
    // These describe the bridge's automatic driver selection, not physical tray sensors.
    put(
        "printer-input-tray",
        IppValue::OctetString(
            "type=unknown;mediafeed=0;mediaxfeed=0;maxcapacity=-2;level=-2;status=0;name=auto;"
                .into(),
        ),
    );
    put("output-bin-default", IppValue::Keyword("auto".into()));
    put("output-bin-supported", keywords(&["auto"]));
    put("printer-output-tray", IppValue::OctetString(
        "type=unknown;maxcapacity=-2;remaining=-2;status=0;name=auto;stackingorder=unknown;pagedelivery=unknown;".into()));
    put("finishings-default", IppValue::Enum(3));
    put("finishings-supported", IppValue::Enum(3));
    put("media-ready", IppValue::Keyword(caps.default_media.clone()));
    put(
        "media-supported",
        IppValue::Array(
            caps.media
                .iter()
                .map(|s| IppValue::Keyword(s.clone()))
                .collect(),
        ),
    );
    // PWG 5100.14 Table 5: explicit size collections and printable margins.
    put(
        "media-size-supported",
        IppValue::Array(
            caps.media_sizes
                .iter()
                .map(|m| {
                    IppValue::Collection(
                        [
                            ("x-dimension".into(), IppValue::Integer(m.width)),
                            ("y-dimension".into(), IppValue::Integer(m.height)),
                        ]
                        .into_iter()
                        .collect(),
                    )
                })
                .collect(),
        ),
    );
    for name in [
        "media-bottom-margin-supported",
        "media-top-margin-supported",
        "media-left-margin-supported",
        "media-right-margin-supported",
    ] {
        // 6.35mm (1/4") in hundredths of a millimetre, matching media-col-*.
        put(name, IppValue::Integer(635));
    }
    let media_col = |name: &str| {
        let paper = caps.media_sizes.iter().find(|m| m.name == name);
        let (width, height) = paper.map(|m| (m.width, m.height)).unwrap_or((21000, 29700));
        IppValue::Collection(
            [
                (
                    "media-size".into(),
                    IppValue::Collection(
                        [
                            ("x-dimension".into(), IppValue::Integer(width)),
                            ("y-dimension".into(), IppValue::Integer(height)),
                        ]
                        .into_iter()
                        .collect(),
                    ),
                ),
                ("media-size-name".into(), IppValue::Keyword(name.into())),
                ("media-source".into(), IppValue::Keyword("auto".into())),
                ("media-type".into(), IppValue::Keyword("stationery".into())),
                ("media-top-margin".into(), IppValue::Integer(635)),
                ("media-bottom-margin".into(), IppValue::Integer(635)),
                ("media-left-margin".into(), IppValue::Integer(635)),
                ("media-right-margin".into(), IppValue::Integer(635)),
            ]
            .into_iter()
            .collect(),
        )
    };
    put("media-col-default", media_col(&caps.default_media));
    put("media-col-ready", media_col(&caps.default_media));
    put(
        "media-col-database",
        IppValue::Array(caps.media.iter().map(|m| media_col(m)).collect()),
    );
    put(
        "media-col-supported",
        keywords(&[
            "media-bottom-margin",
            "media-left-margin",
            "media-right-margin",
            "media-size",
            "media-size-name",
            "media-source",
            "media-top-margin",
            "media-type",
        ]),
    );
    // Mirrors the DNS-SD "URF" TXT key; CP1/DM1 appear only when the Windows
    // driver reports copy/duplex support.
    put("urf-supported", keywords(&caps.urf_features()));
    put(
        "pwg-raster-document-type-supported",
        keywords(&["sgray_8", "srgb_8"]),
    );
    put(
        "pwg-raster-document-sheet-back",
        IppValue::Keyword("normal".into()),
    );
    for name in [
        "printer-resolution-default",
        "printer-resolution-supported",
        "pwg-raster-document-resolution-supported",
    ] {
        put(
            name,
            IppValue::Resolution {
                cross_feed: 300,
                feed: 300,
                units: 3,
            },
        );
    }
    put(
        "pdl-override-supported",
        IppValue::Keyword("attempted".into()),
    );
    put("multiple-document-jobs-supported", IppValue::Boolean(false));
    put("multiple-operation-timeout", IppValue::Integer(60));
    put(
        "multiple-document-handling-default",
        IppValue::Keyword("separate-documents-collated-copies".into()),
    );
    put(
        "multiple-document-handling-supported",
        keywords(&["separate-documents-collated-copies"]),
    );
    put(
        "job-creation-attributes-supported",
        keywords(&[
            "copies",
            "sides",
            "media",
            "media-col",
            "print-color-mode",
            "orientation-requested",
            "page-ranges",
            "print-quality",
            "print-scaling",
            "job-name",
            "output-bin",
        ]),
    );
    response
}
#[cfg(test)]
mod tests {
    use super::*;
    use ipp::model::Operation;
    use std::io::Write;
    use std::net::TcpStream;
    use std::sync::mpsc;

    fn printer(name: &str) -> Printer {
        Printer {
            name: name.into(),
            id: name.into(),
            status: PrinterStatus::Online,
            capabilities: Default::default(),
        }
    }
    fn req(op: Operation, uri: &str) -> IppRequestResponse {
        // Preserve the wire URI: ipp 5.4's client constructor rewrites ipps to ipp.
        let mut request = IppRequestResponse::new(IppVersion::v2_0(), op, None);
        add(
            &mut request,
            DelimiterTag::OperationAttributes,
            "printer-uri",
            IppValue::Uri(uri.into()),
        );
        request
    }
    fn job_id(response: &IppRequestResponse) -> i32 {
        match attr(response, "job-id").unwrap() {
            IppValue::Integer(id) => *id,
            _ => panic!("Missing job ID"),
        }
    }
    fn unexpected_backend(
        _: &str,
        _: &[u8],
        _: &str,
        _: &PrintOptions,
        _: &AtomicBool,
    ) -> Result<i32, String> {
        panic!("receive-only mode must not call the Windows backend")
    }
    type RoutedSubmission = (String, Vec<u8>, String, PrintOptions);
    static ROUTED_SUBMISSIONS: std::sync::OnceLock<Mutex<Option<mpsc::Sender<RoutedSubmission>>>> =
        std::sync::OnceLock::new();
    static LEGACY_SUBMISSIONS: std::sync::OnceLock<Mutex<Option<mpsc::Sender<RoutedSubmission>>>> =
        std::sync::OnceLock::new();
    fn routed_backend(
        queue: &str,
        document: &[u8],
        format: &str,
        options: &PrintOptions,
        _: &AtomicBool,
    ) -> Result<i32, String> {
        ROUTED_SUBMISSIONS
            .get()
            .unwrap()
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .send((
                queue.to_string(),
                document.to_vec(),
                format.to_string(),
                options.clone(),
            ))
            .unwrap();
        Ok(0)
    }
    fn legacy_backend(
        queue: &str,
        document: &[u8],
        format: &str,
        options: &PrintOptions,
        _: &AtomicBool,
    ) -> Result<i32, String> {
        LEGACY_SUBMISSIONS
            .get()
            .unwrap()
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .send((
                queue.to_string(),
                document.to_vec(),
                format.to_string(),
                options.clone(),
            ))
            .unwrap();
        Ok(0)
    }
    fn http(address: &str, path: &str, body: &[u8], host: &str) -> (u16, Vec<u8>) {
        http_with_headers(address, path, body, host, &[])
    }

    fn http_with_headers(
        address: &str,
        path: &str,
        body: &[u8],
        host: &str,
        headers: &[(&str, &str)],
    ) -> (u16, Vec<u8>) {
        let mut socket = TcpStream::connect(address).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let extra_headers = headers
            .iter()
            .map(|(name, value)| format!("{name}: {value}\r\n"))
            .collect::<String>();
        write!(socket, "POST {path} HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/ipp\r\n{extra_headers}Content-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
        socket.write_all(body).unwrap();
        let mut response = Vec::new();
        socket.read_to_end(&mut response).unwrap();
        let boundary = response.windows(4).position(|s| s == b"\r\n\r\n").unwrap();
        let status = std::str::from_utf8(&response[..boundary])
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap()
            .parse()
            .unwrap();
        (status, response[boundary + 4..].to_vec())
    }

    fn http_get(address: &str, path: &str, host: &str) -> (u16, Vec<u8>) {
        let mut socket = TcpStream::connect(address).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        write!(
            socket,
            "GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        let mut response = Vec::new();
        socket.read_to_end(&mut response).unwrap();
        let boundary = response
            .windows(4)
            .position(|bytes| bytes == b"\r\n\r\n")
            .unwrap();
        let status = std::str::from_utf8(&response[..boundary])
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap()
            .parse()
            .unwrap();
        (status, response[boundary + 4..].to_vec())
    }

    #[test]
    fn http_routes_two_queues_and_releases_listener() {
        let mut server = IppServer::new("127.0.0.1", 0);
        let a = printer("办公室 A");
        let b = printer("USB B");
        server.add_printer(a.clone(), "pc.local.").unwrap();
        server.add_printer(b.clone(), "pc.local.").unwrap();
        server.start().unwrap();
        let address = server.address.clone();
        for printer in [&a, &b] {
            let path = format!("/{}", printer.resource_path());
            let uri = format!("ipp://{address}{path}");
            let request = req(Operation::GetPrinterAttributes, &uri);
            let (status, body) = http(&address, &path, &request.to_bytes(), &address);
            assert_eq!(status, 200);
            let response = IppParser::new(IppReader::new(Cursor::new(body)))
                .parse()
                .unwrap();
            assert_eq!(
                string_attr(&response, "printer-name"),
                Some(printer.name.as_str())
            );
            assert_eq!(
                string_attr(&response, "printer-uri-supported"),
                Some(uri.as_str())
            );
        }
        let path = format!("/{}", a.resource_path());
        let request = req(
            Operation::GetPrinterAttributes,
            &format!("ipp://{address}{path}"),
        );
        assert_eq!(
            http(&address, &path, &request.to_bytes(), "evil.example:631").0,
            400
        );
        server.remove_printer(&a).unwrap();
        assert_eq!(http(&address, &path, &request.to_bytes(), &address).0, 404);
        drop(server);
        // tiny_http owns a short-lived accept thread that releases its socket on drop.
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match std::net::TcpListener::bind(&address) {
                Ok(_) => break,
                Err(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
                Err(e) => panic!("Listener was not released: {e}"),
            }
        }
    }

    #[test]
    fn default_print_alias_requires_exactly_one_shared_printer() {
        let mut server = IppServer::new("127.0.0.1", 0);
        let first = printer("default alias A");
        let second = printer("default alias B");
        server.add_printer(first.clone(), "pc.local.").unwrap();
        server.start().unwrap();
        let address = server.address.clone();
        let canonical = format!("/{}", first.resource_path());
        let alias_uri = format!("ipp://{address}{DEFAULT_PRINT_PATH}");
        let request = req(Operation::GetPrinterAttributes, &alias_uri);

        let (status, body) = http(&address, DEFAULT_PRINT_PATH, &request.to_bytes(), &address);
        assert_eq!(status, 200);
        let response = IppParser::new(IppReader::new(Cursor::new(body)))
            .parse()
            .unwrap();
        assert_eq!(
            string_attr(&response, "printer-uri-supported"),
            Some(format!("ipp://{address}{canonical}").as_str())
        );

        server.add_printer(second.clone(), "pc.local.").unwrap();
        assert_eq!(
            http(&address, DEFAULT_PRINT_PATH, &request.to_bytes(), &address,).0,
            404
        );
        server.remove_printer(&first).unwrap();
        server.remove_printer(&second).unwrap();
        assert_eq!(
            http(&address, DEFAULT_PRINT_PATH, &request.to_bytes(), &address,).0,
            404
        );
    }

    #[test]
    fn secure_default_alias_keeps_canonical_job_binding_and_validates_uri_authority() {
        let server = IppServer::new("127.0.0.1", 0);
        let p = printer("secure default alias");
        server.add_printer(p.clone(), "pc.local.").unwrap();
        let path = format!("/{}", p.resource_path());
        let alias = DEFAULT_PRINT_PATH;
        let host = "pc.local:631";
        let alias_uri = format!("ipps://{host}{alias}");

        let response = process_request_at_port_with_alias(
            &server.shared,
            &path,
            Some(alias),
            host,
            req(Operation::GetPrinterAttributes, &alias_uri),
            true,
            631,
        );
        assert_eq!(response.header().operation_or_status, 0);
        assert_eq!(
            string_attr(&response, "printer-uri-supported"),
            Some(format!("ipps://{host}{path}").as_str())
        );

        for uri in [
            format!("ipp://{host}{alias}"),
            format!("ipps://pc.local:8631{alias}"),
        ] {
            let rejected = process_request_at_port_with_alias(
                &server.shared,
                &path,
                Some(alias),
                host,
                req(Operation::GetPrinterAttributes, &uri),
                true,
                631,
            );
            assert_eq!(
                rejected.header().operation_or_status,
                StatusCode::ClientErrorNotFound as u16
            );
        }

        let created = process_request_at_port_with_alias(
            &server.shared,
            &path,
            Some(alias),
            host,
            req(Operation::CreateJob, &alias_uri),
            true,
            631,
        );
        let id = job_id(&created);
        assert_eq!(
            string_attr(&created, "job-uri"),
            Some(format!("ipps://{host}{path}/jobs/{id}").as_str())
        );
        let mut lookup = req(Operation::GetJobAttributes, &alias_uri);
        add(
            &mut lookup,
            DelimiterTag::OperationAttributes,
            "job-uri",
            IppValue::Uri(format!("ipps://{host}{alias}/jobs/{id}")),
        );
        let found = process_request_at_port_with_alias(
            &server.shared,
            &path,
            Some(alias),
            host,
            lookup,
            true,
            631,
        );
        assert_eq!(found.header().operation_or_status, 0);
        assert_eq!(attr(&found, "job-id"), Some(&IppValue::Integer(id)));
    }

    #[test]
    fn gateway_keeps_public_uri_stable_and_pins_created_jobs_to_their_original_target() {
        let mut target_a = printer("Windows Queue A");
        target_a.capabilities.color = true;
        target_a.capabilities.duplex = true;
        target_a.capabilities.max_copies = 4;
        let mut target_b = printer("Windows Queue B");
        target_b.capabilities.max_copies = 1;
        let public_a = Printer::gateway_from(&target_a);
        let public_b = Printer::gateway_from(&target_b);
        let server = IppServer::new("127.0.0.1", 0);
        server
            .add_routed_printer(public_a.clone(), &target_a.name, "pc.local.")
            .unwrap();
        let (tx, rx) = mpsc::channel();
        *ROUTED_SUBMISSIONS
            .get_or_init(|| Mutex::new(None))
            .lock()
            .unwrap() = Some(tx);
        server.shared.lock().unwrap().backend = routed_backend;

        let path = format!("/{}", public_a.resource_path());
        assert_eq!(path, "/ipp/print");
        let host = "127.0.0.1:631";
        let uri = format!("ipp://{host}{path}");
        let before = process(
            &server.shared,
            &path,
            host,
            req(Operation::GetPrinterAttributes, &uri),
        );
        let before_uri = string_attr(&before, "printer-uri-supported")
            .unwrap()
            .to_string();
        let before_uuid = string_attr(&before, "printer-uuid").unwrap().to_string();
        assert_eq!(string_attr(&before, "printer-name"), Some("AirPrinter"));
        assert_eq!(
            attr(&before, "color-supported"),
            Some(&IppValue::Boolean(true))
        );

        let mut create = req(Operation::CreateJob, &uri);
        for (name, value) in [
            ("copies", IppValue::Integer(2)),
            ("sides", IppValue::Keyword("two-sided-long-edge".into())),
            ("print-color-mode", IppValue::Keyword("color".into())),
        ] {
            add(&mut create, DelimiterTag::JobAttributes, name, value);
        }
        let created = process(&server.shared, &path, host, create);
        assert_eq!(
            created.header().operation_or_status,
            StatusCode::SuccessfulOk as u16
        );
        let created_id = job_id(&created);

        server
            .update_routed_printer(public_b.clone(), &target_b.name)
            .unwrap();
        let after = process(
            &server.shared,
            &path,
            host,
            req(Operation::GetPrinterAttributes, &uri),
        );
        assert_eq!(
            string_attr(&after, "printer-uri-supported"),
            Some(before_uri.as_str())
        );
        assert_eq!(
            string_attr(&after, "printer-uuid"),
            Some(before_uuid.as_str())
        );
        assert_eq!(
            attr(&after, "color-supported"),
            Some(&IppValue::Boolean(false))
        );

        let document_a = b"%PDF-target-A";
        let mut send = req(Operation::SendDocument, &uri);
        add(
            &mut send,
            DelimiterTag::OperationAttributes,
            "job-id",
            IppValue::Integer(created_id),
        );
        add(
            &mut send,
            DelimiterTag::OperationAttributes,
            "last-document",
            IppValue::Boolean(true),
        );
        add(
            &mut send,
            DelimiterTag::OperationAttributes,
            "document-format",
            IppValue::MimeMediaType("application/pdf".into()),
        );
        *send.payload_mut() = ipp::payload::IppPayload::new(Cursor::new(document_a.to_vec()));
        assert_eq!(
            process(&server.shared, &path, host, send)
                .header()
                .operation_or_status,
            StatusCode::SuccessfulOk as u16
        );
        let (queue, document, format, options) = rx.recv_timeout(Duration::from_secs(3)).unwrap();
        assert_eq!(queue, target_a.name);
        assert_eq!(document, document_a);
        assert_eq!(format, "application/pdf");
        assert_eq!(options.copies, 2);
        assert_eq!(options.sides, "two-sided-long-edge");
        assert_eq!(options.color_mode, "color");

        let document_b = b"%PDF-target-B";
        let mut print = req(Operation::PrintJob, &uri);
        add(
            &mut print,
            DelimiterTag::OperationAttributes,
            "document-format",
            IppValue::MimeMediaType("application/pdf".into()),
        );
        *print.payload_mut() = ipp::payload::IppPayload::new(Cursor::new(document_b.to_vec()));
        assert_eq!(
            process(&server.shared, &path, host, print)
                .header()
                .operation_or_status,
            StatusCode::SuccessfulOk as u16
        );
        let (queue, document, format, options) = rx.recv_timeout(Duration::from_secs(3)).unwrap();
        assert_eq!(queue, target_b.name);
        assert_eq!(document, document_b);
        assert_eq!(format, "application/pdf");
        assert_eq!(options.copies, 1);
        assert_eq!(options.color_mode, "auto");
        assert!(server
            .shared
            .lock()
            .unwrap()
            .jobs
            .values()
            .all(|job| job.path == "/ipp/print"));
    }

    #[test]
    fn gateway_accepts_only_current_physical_legacy_path_over_http() {
        let target_a = printer("FF Q501h for DocuPrint P115 b");
        let target_b = printer("Windows Queue B");
        let public_a = Printer::gateway_from(&target_a);
        let public_b = Printer::gateway_from(&target_b);
        let old_path = format!("/{}", target_a.resource_path());
        let new_path = format!("/{}", target_b.resource_path());
        let canonical_path = format!("/{}", public_a.resource_path());
        assert_eq!(old_path, "/ipp/print/0446aacd44605763b86a4d6fa188cd7d");
        assert_eq!(canonical_path, "/ipp/print");

        let mut server = IppServer::new("127.0.0.1", 0);
        server
            .add_routed_printer(public_a.clone(), &target_a.name, "pc.local.")
            .unwrap();
        let (tx, rx) = mpsc::channel();
        *LEGACY_SUBMISSIONS
            .get_or_init(|| Mutex::new(None))
            .lock()
            .unwrap() = Some(tx);
        server.shared.lock().unwrap().backend = legacy_backend;
        server.start().unwrap();
        let address = server.address.clone();

        let legacy_uri = format!("ipp://{address}{old_path}");
        let get = req(Operation::GetPrinterAttributes, &legacy_uri);
        let (status, body) = http(&address, &old_path, &get.to_bytes(), &address);
        assert_eq!(status, 200);
        let response = IppParser::new(IppReader::new(Cursor::new(body)))
            .parse()
            .unwrap();
        assert_eq!(
            string_attr(&response, "printer-uri-supported"),
            Some(format!("ipp://{address}{canonical_path}").as_str())
        );

        let wrong_printer_uri = format!("ipp://{address}{new_path}");
        let wrong_path_request = req(Operation::GetPrinterAttributes, &wrong_printer_uri);
        assert_eq!(
            http(
                &address,
                &new_path,
                &wrong_path_request.to_bytes(),
                &address
            )
            .0,
            404
        );
        let unknown_path = "/ipp/print/ffffffffffffffffffffffffffffffff";
        assert_eq!(
            http(&address, unknown_path, &get.to_bytes(), &address).0,
            404
        );

        let document = b"%PDF-legacy-target-A";
        let mut print = req(Operation::PrintJob, &legacy_uri);
        add(
            &mut print,
            DelimiterTag::OperationAttributes,
            "document-format",
            IppValue::MimeMediaType("application/pdf".into()),
        );
        *print.payload_mut() = ipp::payload::IppPayload::new(Cursor::new(document.to_vec()));
        let mut body = print.to_bytes().to_vec();
        body.extend(document);
        let (status, response_body) = http(&address, &old_path, &body, &address);
        assert_eq!(status, 200);
        let response = IppParser::new(IppReader::new(Cursor::new(response_body)))
            .parse()
            .unwrap();
        let job_id = job_id(&response);
        assert_eq!(
            string_attr(&response, "job-uri"),
            Some(format!("ipp://{address}{canonical_path}/jobs/{job_id}").as_str())
        );
        let (queue, received, format, _) = rx.recv_timeout(Duration::from_secs(3)).unwrap();
        assert_eq!(queue, target_a.name);
        assert_eq!(received, document);
        assert_eq!(format, "application/pdf");

        let mut get_job = req(Operation::GetJobAttributes, &legacy_uri);
        add(
            &mut get_job,
            DelimiterTag::OperationAttributes,
            "job-id",
            IppValue::Integer(job_id),
        );
        let job_path = format!("{old_path}/jobs/{job_id}");
        let (status, body) = http(&address, &job_path, &get_job.to_bytes(), &address);
        assert_eq!(status, 200);
        let response = IppParser::new(IppReader::new(Cursor::new(body)))
            .parse()
            .unwrap();
        assert_eq!(
            string_attr(&response, "job-printer-uri"),
            Some(format!("ipp://{address}{canonical_path}").as_str())
        );
        let (resolved_path, was_legacy_alias) = {
            let state = server.shared.lock().unwrap();
            resolve_printer_path(&old_path, &state.printers, &state.routes).unwrap()
        };
        assert_eq!(resolved_path, canonical_path);
        assert!(was_legacy_alias);

        server
            .update_routed_printer(public_b.clone(), &target_b.name)
            .unwrap();
        let stale_alias = process_request_at_port_with_alias(
            &server.shared,
            &resolved_path,
            Some(&old_path),
            &address,
            req(Operation::GetPrinterAttributes, &legacy_uri),
            false,
            address.rsplit_once(':').unwrap().1.parse().unwrap(),
        );
        assert_eq!(
            stale_alias.header().operation_or_status,
            StatusCode::ClientErrorNotFound as u16
        );
        assert_eq!(http(&address, &old_path, &get.to_bytes(), &address).0, 404);
        let new_uri = format!("ipp://{address}{new_path}");
        let new_get = req(Operation::GetPrinterAttributes, &new_uri);
        let (status, body) = http(&address, &new_path, &new_get.to_bytes(), &address);
        assert_eq!(status, 200);
        let response = IppParser::new(IppReader::new(Cursor::new(body)))
            .parse()
            .unwrap();
        assert_eq!(
            string_attr(&response, "printer-uri-supported"),
            Some(format!("ipp://{address}{canonical_path}").as_str())
        );

        server.remove_printer(&public_b).unwrap();
        assert_eq!(
            http(&address, &new_path, &new_get.to_bytes(), &address).0,
            404
        );
    }

    #[test]
    fn receive_page_requires_receive_mode_and_exactly_one_shared_printer() {
        let directory = tempfile::tempdir().unwrap();
        let mut server = IppServer::new("127.0.0.1", 0);
        server
            .set_receive_directory(Some(directory.path().to_path_buf()))
            .unwrap();
        server.start().unwrap();
        let address = server.address.clone();
        assert_eq!(http_get(&address, "/receive", &address).0, 404);

        let first = printer("browser receiver A");
        let second = printer("browser receiver B");
        server.add_printer(first.clone(), "pc.local.").unwrap();
        let (status, body) = http_get(&address, "/receive", &address);
        assert_eq!(status, 200);
        let page = String::from_utf8(body).unwrap();
        assert!(page.contains("不会打印"));
        assert!(page.contains(&format!("data-printer-path=\"/{}\"", first.resource_path())));

        server.set_receive_directory(None).unwrap();
        assert_eq!(http_get(&address, "/receive", &address).0, 404);
        server
            .set_receive_directory(Some(directory.path().to_path_buf()))
            .unwrap();
        server.add_printer(second, "pc.local.").unwrap();
        assert_eq!(http_get(&address, "/receive", &address).0, 404);
    }

    #[test]
    fn receive_only_marker_rejects_stale_page_in_normal_mode_and_accepts_enabled_mode() {
        let document = b"%PDF-receive-marker";
        let mut normal = IppServer::new("127.0.0.1", 0);
        let normal_printer = printer("normal mode queue");
        normal
            .add_printer(normal_printer.clone(), "pc.local.")
            .unwrap();
        normal.shared.lock().unwrap().backend = unexpected_backend;
        normal.start().unwrap();
        let normal_address = normal.address.clone();
        let normal_path = format!("/{}", normal_printer.resource_path());
        let normal_uri = format!("ipp://{normal_address}{normal_path}");
        let mut normal_request = req(Operation::PrintJob, &normal_uri);
        add(
            &mut normal_request,
            DelimiterTag::OperationAttributes,
            "document-format",
            IppValue::MimeMediaType("application/pdf".into()),
        );
        *normal_request.payload_mut() =
            ipp::payload::IppPayload::new(Cursor::new(document.to_vec()));
        let mut normal_body = Vec::new();
        normal_request
            .into_read()
            .read_to_end(&mut normal_body)
            .unwrap();
        assert_eq!(
            http_with_headers(
                &normal_address,
                &normal_path,
                &normal_body,
                &normal_address,
                &[("X-AirPrinter-Receive-Only", "1")],
            )
            .0,
            409
        );
        assert!(normal.shared.lock().unwrap().jobs.is_empty());

        let directory = tempfile::tempdir().unwrap();
        let mut receiver = IppServer::new("127.0.0.1", 0);
        receiver
            .set_receive_directory(Some(directory.path().to_path_buf()))
            .unwrap();
        let receive_printer = printer("receive mode queue");
        receiver
            .add_printer(receive_printer.clone(), "pc.local.")
            .unwrap();
        receiver.shared.lock().unwrap().backend = unexpected_backend;
        receiver.start().unwrap();
        let address = receiver.address.clone();
        let path = format!("/{}", receive_printer.resource_path());
        let uri = format!("ipp://{address}{path}");
        let mut request = req(Operation::PrintJob, &uri);
        add(
            &mut request,
            DelimiterTag::OperationAttributes,
            "document-format",
            IppValue::MimeMediaType("application/pdf".into()),
        );
        *request.payload_mut() = ipp::payload::IppPayload::new(Cursor::new(document.to_vec()));
        let mut body = Vec::new();
        request.into_read().read_to_end(&mut body).unwrap();
        assert_eq!(
            http_with_headers(
                &address,
                &path,
                &body,
                &address,
                &[("X-AirPrinter-Receive-Only", "0")],
            )
            .0,
            409
        );
        assert!(receiver.shared.lock().unwrap().jobs.is_empty());

        let (status, response_body) = http_with_headers(
            &address,
            &path,
            &body,
            &address,
            &[("X-AirPrinter-Receive-Only", "1")],
        );
        assert_eq!(status, 200);
        let response = IppParser::new(IppReader::new(Cursor::new(response_body)))
            .parse()
            .unwrap();
        assert_eq!(
            response.header().operation_or_status,
            StatusCode::SuccessfulOk as u16
        );
        let id = job_id(&response);
        let state = receiver.shared.lock().unwrap();
        let job = state.jobs.get(&id).unwrap();
        assert_eq!(job.state, 4);
        assert_eq!(job.reason, "job-hold-until-specified");
        let files: Vec<_> = std::fs::read_dir(directory.path())
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(files.len(), 1);
        assert_eq!(std::fs::read(files[0].path()).unwrap(), document);
    }

    #[test]
    fn occupied_standard_port_uses_an_advertisable_alternate() {
        let mut first = IppServer::new("127.0.0.1", 631);
        first.start().unwrap();
        let mut second = IppServer::new("127.0.0.1", 631);
        second.start().unwrap();
        assert_ne!(first.port().unwrap(), second.port().unwrap());
        assert!((8631..=8699).contains(&second.port().unwrap()));
        std::net::TcpStream::connect(("127.0.0.1", second.port().unwrap())).unwrap();
    }

    #[test]
    fn create_send_validate_cancel_and_job_ids() {
        let server = IppServer::new("127.0.0.1", 0);
        let p = printer("test");
        server.add_printer(p.clone(), "pc.local.").unwrap();
        let path = format!("/{}", p.resource_path());
        let host = "127.0.0.1:631";
        let uri = format!("ipp://{host}{path}");
        let created = process(&server.shared, &path, host, req(Operation::CreateJob, &uri));
        let id = job_id(&created);
        assert_eq!(attr(&created, "job-state"), Some(&IppValue::Enum(4)));
        let again = process(&server.shared, &path, host, req(Operation::CreateJob, &uri));
        assert_ne!(job_id(&again), id);
        let mut cancel = req(Operation::CancelJob, &uri);
        add(
            &mut cancel,
            DelimiterTag::OperationAttributes,
            "job-id",
            IppValue::Integer(id),
        );
        assert_eq!(
            process(&server.shared, &path, host, cancel)
                .header()
                .operation_or_status,
            0
        );
        let mut get = req(Operation::GetJobAttributes, &uri);
        add(
            &mut get,
            DelimiterTag::OperationAttributes,
            "job-id",
            IppValue::Integer(id),
        );
        assert_eq!(
            attr(&process(&server.shared, &path, host, get), "job-state"),
            Some(&IppValue::Enum(7))
        );
        let mut validation = req(Operation::ValidateJob, &uri);
        add(
            &mut validation,
            DelimiterTag::JobAttributes,
            "copies",
            IppValue::Integer(999),
        );
        assert_eq!(
            process(&server.shared, &path, host, validation)
                .header()
                .operation_or_status,
            StatusCode::ClientErrorAttributesOrValuesNotSupported as u16
        );
        let mut send = req(Operation::SendDocument, &uri);
        add(
            &mut send,
            DelimiterTag::OperationAttributes,
            "job-id",
            IppValue::Integer(job_id(&again)),
        );
        add(
            &mut send,
            DelimiterTag::OperationAttributes,
            "last-document",
            IppValue::Boolean(false),
        );
        assert_eq!(
            process(&server.shared, &path, host, send)
                .header()
                .operation_or_status,
            StatusCode::ServerErrorMultipleDocumentJobsNotSupported as u16
        );
    }

    #[test]
    fn receive_only_create_send_saves_and_holds_without_backend() {
        let directory = tempfile::tempdir().unwrap();
        let mut server = IppServer::new("127.0.0.1", 0);
        server
            .set_receive_directory(Some(directory.path().to_path_buf()))
            .unwrap();
        let p = printer("receive-only queue");
        server.add_printer(p.clone(), "pc.local.").unwrap();
        server.shared.lock().unwrap().backend = unexpected_backend;
        let path = format!("/{}", p.resource_path());
        let host = "127.0.0.1:631";
        let uri = format!("ipp://{host}{path}");

        let created = process(&server.shared, &path, host, req(Operation::CreateJob, &uri));
        let id = job_id(&created);
        let document = b"%PDF-receive-only";
        let mut send = req(Operation::SendDocument, &uri);
        add(
            &mut send,
            DelimiterTag::OperationAttributes,
            "job-id",
            IppValue::Integer(id),
        );
        add(
            &mut send,
            DelimiterTag::OperationAttributes,
            "last-document",
            IppValue::Boolean(true),
        );
        add(
            &mut send,
            DelimiterTag::OperationAttributes,
            "document-format",
            IppValue::MimeMediaType("application/pdf".into()),
        );
        *send.payload_mut() = ipp::payload::IppPayload::new(Cursor::new(document.to_vec()));
        assert_eq!(
            process(&server.shared, &path, host, send)
                .header()
                .operation_or_status,
            StatusCode::SuccessfulOk as u16
        );

        let mut get = req(Operation::GetJobAttributes, &uri);
        add(
            &mut get,
            DelimiterTag::OperationAttributes,
            "job-id",
            IppValue::Integer(id),
        );
        let response = process(&server.shared, &path, host, get);
        assert_eq!(attr(&response, "job-state"), Some(&IppValue::Enum(4)));
        assert_eq!(
            string_attr(&response, "job-state-reasons"),
            Some("job-hold-until-specified")
        );
        let Some(IppValue::TextWithoutLanguage(message)) = attr(&response, "job-state-message")
        else {
            panic!("missing job-state-message")
        };
        assert!(message.contains(&format!("Received {} bytes", document.len())));
        assert!(message.contains("no Windows print submitted"));
        let files: Vec<_> = std::fs::read_dir(directory.path())
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(files.len(), 1);
        assert!(files[0]
            .file_name()
            .to_string_lossy()
            .contains(&format!("airprinter-job-{id}-")));
        assert!(files[0].file_name().to_string_lossy().ends_with(".pdf"));
        assert_eq!(std::fs::read(files[0].path()).unwrap(), document);

        server
            .shared
            .lock()
            .unwrap()
            .jobs
            .get_mut(&id)
            .unwrap()
            .created = Instant::now() - Duration::from_secs(120);
        let print_document = b"%PDF-print-job";
        let mut print = req(Operation::PrintJob, &uri);
        add(
            &mut print,
            DelimiterTag::OperationAttributes,
            "document-format",
            IppValue::MimeMediaType("application/pdf".into()),
        );
        *print.payload_mut() = ipp::payload::IppPayload::new(Cursor::new(print_document.to_vec()));
        let printed = process(&server.shared, &path, host, print);
        let print_id = job_id(&printed);
        let mut get = req(Operation::GetJobAttributes, &uri);
        add(
            &mut get,
            DelimiterTag::OperationAttributes,
            "job-id",
            IppValue::Integer(print_id),
        );
        let response = process(&server.shared, &path, host, get);
        assert_eq!(attr(&response, "job-state"), Some(&IppValue::Enum(4)));
        let mut get = req(Operation::GetJobAttributes, &uri);
        add(
            &mut get,
            DelimiterTag::OperationAttributes,
            "job-id",
            IppValue::Integer(id),
        );
        assert_eq!(
            attr(&process(&server.shared, &path, host, get), "job-state"),
            Some(&IppValue::Enum(4)),
            "received documents remain held past the incoming-document timeout"
        );
        let files: Vec<_> = std::fs::read_dir(directory.path())
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(files.len(), 2);
        let saved_print = files
            .iter()
            .find(|file| {
                file.file_name()
                    .to_string_lossy()
                    .contains(&format!("airprinter-job-{print_id}-"))
            })
            .unwrap();
        assert_eq!(std::fs::read(saved_print.path()).unwrap(), print_document);
    }

    #[test]
    fn bonjour_reply_uses_routed_ipv4_with_explicit_port() {
        let peer = "127.0.0.1:50123".parse().unwrap();
        let authority = response_authority(Some(peer), "pc.local", 631, false);
        assert_eq!(authority, "127.0.0.1:631");
        assert_eq!(
            response_authority(Some(peer), "PC.LOCAL.:8631", 8631, true),
            "PC.LOCAL.:8631"
        );
        assert_eq!(
            response_authority(None, "pc.local", 631, false),
            "pc.local:631"
        );
        let server = IppServer::new("127.0.0.1", 0);
        let p = printer("Bonjour queue");
        server.add_printer(p.clone(), "pc.local.").unwrap();
        let path = format!("/{}", p.resource_path());
        let incoming = format!("ipp://pc.local{path}");
        let mut query = req(Operation::GetPrinterAttributes, &incoming);
        add(
            &mut query,
            DelimiterTag::OperationAttributes,
            "requested-attributes",
            keywords(&["printer-state-reasons"]),
        );
        let response = process(&server.shared, &path, &authority, query);
        assert_eq!(response.header().operation_or_status, 0);
        assert_eq!(
            string_attr(&response, "printer-uri-supported"),
            Some(format!("ipp://127.0.0.1:631{path}").as_str())
        );
        assert_eq!(
            attr(&response, "uri-security-supported"),
            Some(&keywords(&["none"]))
        );
        assert!(attr(&response, "printer-name").is_none());
        let created = process(
            &server.shared,
            &path,
            &authority,
            req(Operation::CreateJob, &incoming),
        );
        assert!(string_attr(&created, "job-uri")
            .unwrap()
            .starts_with("ipp://127.0.0.1:631/"));
    }

    #[test]
    fn ipp_uris_may_omit_the_default_631_port() {
        assert!(valid_host("pc.local", "pc.local.", 631));
        assert!(valid_host("127.0.0.1", "pc.local.", 631));
        assert!(!valid_host("pc.local", "pc.local.", 8631));
        assert!(!valid_host("other.local", "pc.local.", 631));
        let server = IppServer::new("127.0.0.1", 0);
        let p = printer("default port client");
        server.add_printer(p.clone(), "pc.local.").unwrap();
        let path = format!("/{}", p.resource_path());
        let host = "127.0.0.1:631";
        let without_port = format!("ipp://127.0.0.1{path}");

        let attributes = process(
            &server.shared,
            &path,
            host,
            req(Operation::GetPrinterAttributes, &without_port),
        );
        assert_eq!(attributes.header().operation_or_status, 0);
        let bonjour_uri = format!("ipp://pc.local{path}");
        assert_eq!(
            process(
                &server.shared,
                &path,
                "pc.local",
                req(Operation::GetPrinterAttributes, &bonjour_uri)
            )
            .header()
            .operation_or_status,
            0
        );
        let mut create = req(Operation::CreateJob, &without_port);
        add(
            &mut create,
            DelimiterTag::JobAttributes,
            "print-quality",
            IppValue::Enum(5),
        );
        let created = process(&server.shared, &path, "127.0.0.1", create);
        assert_eq!(created.header().operation_or_status, 0);
        let id = job_id(&created);

        let mut lookup = req(Operation::GetJobAttributes, &without_port);
        add(
            &mut lookup,
            DelimiterTag::OperationAttributes,
            "job-uri",
            IppValue::Uri(format!("{without_port}/jobs/{id}")),
        );
        let result = process(&server.shared, &path, "127.0.0.1", lookup);
        assert_eq!(attr(&result, "job-state"), Some(&IppValue::Enum(4)));
        assert!(!valid_ipp_authority("127.0.0.1", "pc.local.", 8631));
    }

    #[test]
    fn windows_computer_local_name_is_allowed_with_port_and_scheme_checks() {
        assert!(is_local_hostname(
            "LicherPC.local.",
            "airprinter.local.",
            Some("LICHERPC")
        ));
        assert!(is_local_hostname(
            "LICHERPC.LOCAL",
            "airprinter.local",
            Some("LicherPC.local.")
        ));
        assert!(!is_local_hostname(
            "other.local",
            "airprinter.local",
            Some("LicherPC")
        ));
        assert!(valid_host("LicherPC.local:631", "airprinter.local.", 631));
        assert!(valid_ipp_authority(
            "LicherPC.local:631",
            "airprinter.local.",
            631
        ));
        assert!(!valid_host("other.local:631", "airprinter.local.", 631));
        assert!(!valid_host("LicherPC.local:8631", "airprinter.local.", 631));
        assert!(!valid_ipp_authority(
            "LicherPC.local:8631",
            "airprinter.local.",
            631
        ));
    }

    #[test]
    fn custom_driver_media_is_matched_by_dimensions() {
        let mut p = printer("label printer");
        p.capabilities.media = vec!["custom_win256_4x6in".into()];
        p.capabilities.default_media = p.capabilities.media[0].clone();
        p.capabilities.media_sizes = vec![crate::models::printer::MediaSize {
            name: p.capabilities.default_media.clone(),
            width: 10160,
            height: 15240,
            windows_kind: 256,
        }];
        let mut request = req(Operation::ValidateJob, "ipp://127.0.0.1:631/ipp/print/test");
        add(
            &mut request,
            DelimiterTag::JobAttributes,
            "media-col",
            IppValue::Collection(
                [(
                    "media-size".into(),
                    IppValue::Collection(
                        [
                            ("x-dimension".into(), IppValue::Integer(10160)),
                            ("y-dimension".into(), IppValue::Integer(15240)),
                        ]
                        .into_iter()
                        .collect(),
                    ),
                )]
                .into_iter()
                .collect(),
            ),
        );
        add(
            &mut request,
            DelimiterTag::JobAttributes,
            "print-quality",
            IppValue::Enum(5),
        );
        let selected = options(&request, &p, None).unwrap();
        assert_eq!(selected.quality, 5);
        assert_eq!(
            (
                selected.paper_kind,
                selected.paper_width,
                selected.paper_height
            ),
            (256, 400, 600)
        );
    }

    #[test]
    fn secure_requests_keep_ipps_uris_for_printer_and_jobs() {
        let server = IppServer::new("127.0.0.1", 0);
        let p = printer("secure queue");
        server.add_printer(p.clone(), "pc.local.").unwrap();
        let path = format!("/{}", p.resource_path());
        let host = "pc.local:8631";
        let uri = format!("ipps://{host}{path}");
        let response = process_request(
            &server.shared,
            &path,
            host,
            req(Operation::GetPrinterAttributes, &uri),
            true,
        );
        assert_eq!(response.header().operation_or_status, 0);
        assert_eq!(
            string_attr(&response, "printer-uri-supported"),
            Some(uri.as_str())
        );
        assert_eq!(
            attr(&response, "uri-security-supported"),
            Some(&keywords(&["tls"]))
        );
        let create = process_request(
            &server.shared,
            &path,
            host,
            req(Operation::CreateJob, &uri),
            true,
        );
        assert_eq!(create.header().operation_or_status, 0);
        let job_uri = string_attr(&create, "job-uri").unwrap();
        assert!(job_uri.starts_with("ipps://pc.local:8631/"));
        let mut lookup = req(Operation::GetJobAttributes, &uri);
        add(
            &mut lookup,
            DelimiterTag::OperationAttributes,
            "job-uri",
            IppValue::Uri(job_uri.into()),
        );
        assert_eq!(
            process_request(&server.shared, &path, host, lookup, true)
                .header()
                .operation_or_status,
            0
        );
        assert_ne!(
            process_request(
                &server.shared,
                &path,
                host,
                req(Operation::GetPrinterAttributes, &uri),
                false
            )
            .header()
            .operation_or_status,
            0
        );
    }

    #[test]
    fn created_job_preserves_print_intent_when_document_arrives() {
        static RECEIVED: std::sync::OnceLock<Mutex<Option<mpsc::Sender<PrintOptions>>>> =
            std::sync::OnceLock::new();
        fn backend(
            _: &str,
            _: &[u8],
            _: &str,
            opts: &PrintOptions,
            _: &AtomicBool,
        ) -> Result<i32, String> {
            RECEIVED
                .get()
                .unwrap()
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .send(opts.clone())
                .unwrap();
            Ok(0)
        }
        let (tx, rx) = mpsc::channel();
        *RECEIVED.get_or_init(|| Mutex::new(None)).lock().unwrap() = Some(tx);
        let server = IppServer::new("127.0.0.1", 0);
        let mut p = printer("selected USB queue");
        p.capabilities.max_copies = 5;
        p.capabilities.color = true;
        p.capabilities.duplex = true;
        server.add_printer(p.clone(), "pc.local.").unwrap();
        server.shared.lock().unwrap().backend = backend;
        let path = format!("/{}", p.resource_path());
        let host = "127.0.0.1:631";
        let uri = format!("ipp://{host}{path}");
        let mut create = req(Operation::CreateJob, &uri);
        for (name, value) in [
            (
                "job-name",
                IppValue::NameWithLanguage {
                    language: "zh".into(),
                    name: "合同".into(),
                },
            ),
            (
                "page-ranges",
                IppValue::Array(vec![
                    IppValue::RangeOfInteger { min: 2, max: 3 },
                    IppValue::RangeOfInteger { min: 5, max: 5 },
                ]),
            ),
            ("copies", IppValue::Integer(2)),
            ("print-quality", IppValue::Enum(5)),
            ("orientation-requested", IppValue::Enum(4)),
            ("sides", IppValue::Keyword("two-sided-long-edge".into())),
            ("print-color-mode", IppValue::Keyword("monochrome".into())),
            ("print-scaling", IppValue::Keyword("none".into())),
        ] {
            add(&mut create, DelimiterTag::JobAttributes, name, value);
        }
        let created = process(&server.shared, &path, host, create);
        assert_eq!(created.header().operation_or_status, 0);
        assert_eq!(string_attr(&created, "job-name"), Some("合同"));
        let mut send = req(Operation::SendDocument, &uri);
        add(
            &mut send,
            DelimiterTag::OperationAttributes,
            "job-id",
            IppValue::Integer(job_id(&created)),
        );
        add(
            &mut send,
            DelimiterTag::OperationAttributes,
            "last-document",
            IppValue::Boolean(true),
        );
        *send.payload_mut() =
            ipp::payload::IppPayload::new(Cursor::new(b"%PDF-simulated".to_vec()));
        assert_eq!(
            process(&server.shared, &path, host, send)
                .header()
                .operation_or_status,
            0
        );
        let received = rx.recv_timeout(Duration::from_secs(3)).unwrap();
        assert_eq!(received.page_ranges, vec![(2, 3), (5, 5)]);
        assert_eq!(received.job_name, "合同");
        assert_eq!(
            (received.copies, received.quality, received.orientation),
            (2, 5, 4)
        );
        assert_eq!(received.sides, "two-sided-long-edge");
        assert_eq!(received.color_mode, "monochrome");
        assert_eq!(received.scaling, "none");
        for ranges in [
            vec![IppValue::RangeOfInteger { min: 0, max: 2 }],
            vec![IppValue::RangeOfInteger { min: 3, max: 2 }],
            vec![
                IppValue::RangeOfInteger { min: 1, max: 3 },
                IppValue::RangeOfInteger { min: 3, max: 4 },
            ],
        ] {
            let mut validation = req(Operation::ValidateJob, &uri);
            add(
                &mut validation,
                DelimiterTag::JobAttributes,
                "page-ranges",
                IppValue::Array(ranges),
            );
            assert_eq!(
                process(&server.shared, &path, host, validation)
                    .header()
                    .operation_or_status,
                StatusCode::ClientErrorBadRequest as u16
            );
        }
    }

    #[test]
    fn apple_queries_get_format_limits_and_consistent_auto_trays() {
        let server = IppServer::new("127.0.0.1", 0);
        let p = printer("USB queue");
        server.add_printer(p.clone(), "pc.local.").unwrap();
        let path = format!("/{}", p.resource_path());
        let host = "127.0.0.1:631";
        let uri = format!("ipp://{host}{path}");
        let requested = [
            "printer-state-reasons",
            "media-source-supported",
            "printer-input-tray",
            "pdf-k-octets-supported",
            "jpeg-features-supported",
            "jpeg-k-octets-supported",
            "jpeg-x-dimension-supported",
            "jpeg-y-dimension-supported",
            "media-col-ready",
            "media-col-supported",
            "output-bin-default",
            "output-bin-supported",
            "printer-output-tray",
        ];
        let mut request = req(Operation::GetPrinterAttributes, &uri);
        add(
            &mut request,
            DelimiterTag::OperationAttributes,
            "requested-attributes",
            keywords(&requested),
        );
        let response = process(&server.shared, &path, host, request);
        // Check the serialized response, including nested collections and octet-string tags.
        let response = IppParser::new(IppReader::new(Cursor::new(response.to_bytes())))
            .parse()
            .unwrap();
        assert_eq!(response.header().operation_or_status, 0);
        for name in requested {
            assert!(attr(&response, name).is_some(), "Missing {name}");
        }
        assert_eq!(
            attr(&response, "pdf-k-octets-supported"),
            Some(&IppValue::RangeOfInteger { min: 0, max: 65536 })
        );
        assert_eq!(
            attr(&response, "jpeg-y-dimension-supported"),
            Some(&IppValue::RangeOfInteger { min: 1, max: 10000 })
        );
        let Some(IppValue::OctetString(tray)) = attr(&response, "printer-input-tray") else {
            panic!("Wrong tray value tag")
        };
        assert!(tray.contains("name=auto;"));
        let Some(IppValue::Collection(media)) = attr(&response, "media-col-ready") else {
            panic!("Missing ready media")
        };
        assert_eq!(
            media.get("media-source"),
            Some(&IppValue::Keyword("auto".into()))
        );
        assert_eq!(
            media.get("media-type"),
            Some(&IppValue::Keyword("stationery".into()))
        );
        let mut validate = req(Operation::ValidateJob, &uri);
        add(
            &mut validate,
            DelimiterTag::JobAttributes,
            "media-col",
            IppValue::Collection(media.clone()),
        );
        add(
            &mut validate,
            DelimiterTag::JobAttributes,
            "output-bin",
            IppValue::Keyword("auto".into()),
        );
        assert_eq!(
            process(&server.shared, &path, host, validate)
                .header()
                .operation_or_status,
            0
        );
    }

    #[test]
    fn printer_attributes_use_protocol_names_and_color_default() {
        let server = IppServer::new("127.0.0.1", 0);
        let mut p = printer("color queue");
        p.capabilities.color = true;
        server.add_printer(p.clone(), "pc.local.").unwrap();
        let path = format!("/{}", p.resource_path());
        let host = "127.0.0.1:631";
        let uri = format!("ipp://{host}{path}");
        let response = process(
            &server.shared,
            &path,
            host,
            req(Operation::GetPrinterAttributes, &uri),
        );
        assert_eq!(
            attr(&response, "multiple-operation-timeout"),
            Some(&IppValue::Integer(60))
        );
        assert_eq!(attr(&response, "multiple-operation-time-out"), None);
        assert_eq!(
            string_attr(&response, "print-color-mode-default"),
            Some("auto")
        );
        assert!(attr(&response, "which-jobs-supported").is_some());
        assert_eq!(
            attr(&response, "page-ranges-supported"),
            Some(&IppValue::Boolean(true))
        );
        assert_eq!(
            attr(&response, "printer-make-and-model"),
            Some(&IppValue::TextWithoutLanguage(p.name.clone()))
        );

        let mut request = req(Operation::GetPrinterAttributes, &uri);
        add(
            &mut request,
            DelimiterTag::OperationAttributes,
            "requested-attributes",
            keywords(&["printer-state-reasons", "media-source-supported"]),
        );
        let filtered = process(&server.shared, &path, host, request);
        assert!(attr(&filtered, "printer-state-reasons").is_some());
        assert_eq!(
            attr(&filtered, "media-source-supported"),
            Some(&keywords(&["auto"]))
        );
        assert!(attr(&filtered, "printer-name").is_none());
    }

    #[test]
    fn print_acknowledges_pending_and_reports_actual_backend_error() {
        static QUEUE: std::sync::OnceLock<Mutex<Option<mpsc::Sender<String>>>> =
            std::sync::OnceLock::new();
        fn failure(
            queue: &str,
            _: &[u8],
            _: &str,
            _: &PrintOptions,
            _: &AtomicBool,
        ) -> Result<i32, String> {
            QUEUE
                .get()
                .unwrap()
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .send(queue.into())
                .unwrap();
            Err("simulated driver failure".into())
        }
        let (tx, rx) = mpsc::channel();
        *QUEUE.get_or_init(|| Mutex::new(None)).lock().unwrap() = Some(tx);
        let server = IppServer::new("127.0.0.1", 0);
        let p = printer("physical-queue-B");
        server.add_printer(p.clone(), "pc.local.").unwrap();
        server.shared.lock().unwrap().backend = failure;
        let path = format!("/{}", p.resource_path());
        let host = "127.0.0.1:631";
        let uri = format!("ipp://{host}{path}");
        let mut request = req(Operation::PrintJob, &uri);
        *request.payload_mut() =
            ipp::payload::IppPayload::new(Cursor::new(b"%PDF-simulated".to_vec()));
        let response = process(&server.shared, &path, host, request);
        let id = job_id(&response);
        assert_eq!(attr(&response, "job-state"), Some(&IppValue::Enum(3)));
        assert_eq!(rx.recv_timeout(Duration::from_secs(3)).unwrap(), p.name);
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let job = server
                .shared
                .lock()
                .unwrap()
                .jobs
                .get(&id)
                .cloned()
                .unwrap();
            if job.state == 8 {
                assert!(job.message.contains("driver failure"));
                break;
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[cfg(target_os = "windows")]
    #[test]
    #[ignore = "Full local HTTP-to-Windows simulation; prints only to a PDF file"]
    fn http_to_windows_pdf_end_to_end() {
        let output_directory =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/airprint-simulation");
        std::fs::create_dir_all(&output_directory).unwrap();
        let output = output_directory.join(format!("http-urf-{}.pdf", std::process::id()));
        print_job::TEST_OUTPUT.set(output.clone()).unwrap();
        let mut server = IppServer::new("127.0.0.1", 0);
        let mut p = printer("Microsoft Print to PDF");
        p.capabilities.color = true;
        let public_printer = Printer::gateway_from(&p);
        server
            .add_routed_printer(public_printer.clone(), &p.name, "simulation.local.")
            .unwrap();
        server.shared.lock().unwrap().backend = print_job::submit_test_pdf;
        server.start().unwrap();
        let address = server.address.clone();
        let path = format!("/{}", public_printer.resource_path());
        let uri = format!("ipp://{address}{path}");
        let mut data = b"UNIRAST\0".to_vec();
        data.extend(1u32.to_be_bytes());
        let mut header = [0; 32];
        header[0] = 24;
        header[1] = 1;
        header[12..16].copy_from_slice(&100u32.to_be_bytes());
        header[16..20].copy_from_slice(&100u32.to_be_bytes());
        header[20..24].copy_from_slice(&300u32.to_be_bytes());
        data.extend(header);
        // Four quadrants: red, green, blue, white. Exercises both row and pixel RLE.
        data.extend([
            49, 49, 255, 0, 0, 49, 0, 255, 0, 49, 49, 0, 0, 255, 49, 255, 255, 255,
        ]);
        let mut request = req(Operation::PrintJob, &uri);
        add(
            &mut request,
            DelimiterTag::OperationAttributes,
            "document-format",
            IppValue::MimeMediaType("image/urf".into()),
        );
        add(
            &mut request,
            DelimiterTag::JobAttributes,
            "print-color-mode",
            IppValue::Keyword("color".into()),
        );
        let mut body = request.to_bytes().to_vec();
        body.extend(data);
        let (status, body) = http(&address, &path, &body, &address);
        assert_eq!(status, 200);
        let response = IppParser::new(IppReader::new(Cursor::new(body)))
            .parse()
            .unwrap();
        let id = job_id(&response);
        assert_eq!(attr(&response, "job-state"), Some(&IppValue::Enum(3)));
        let deadline = Instant::now() + Duration::from_secs(45);
        loop {
            let mut request = req(Operation::GetJobAttributes, &uri);
            add(
                &mut request,
                DelimiterTag::OperationAttributes,
                "job-id",
                IppValue::Integer(id),
            );
            let (_, body) = http(
                &address,
                &format!("{path}/jobs/{id}"),
                &request.to_bytes(),
                &address,
            );
            let response = IppParser::new(IppReader::new(Cursor::new(body)))
                .parse()
                .unwrap();
            if attr(&response, "job-state") == Some(&IppValue::Enum(9)) {
                break;
            }
            assert_ne!(
                attr(&response, "job-state"),
                Some(&IppValue::Enum(8)),
                "{:?}",
                attr(&response, "job-state-message")
            );
            assert!(
                Instant::now() < deadline,
                "Spool status did not complete: {:?}",
                attr(&response, "job-state-message")
            );
            thread::sleep(Duration::from_millis(250));
        }
        assert!(std::fs::read(&output).unwrap().starts_with(b"%PDF-"));
        println!("Simulation PDF: {}", output.display());
    }

    #[test]
    fn conformance_attribute_tables_and_operations() {
        let server = IppServer::new("127.0.0.1", 0);
        let p = printer("conformance queue");
        server.add_printer(p.clone(), "pc.local.").unwrap();
        let path = format!("/{}", p.resource_path());
        let host = "127.0.0.1:631";
        let uri = format!("ipp://{host}{path}");
        let response = process(
            &server.shared,
            &path,
            host,
            req(Operation::GetPrinterAttributes, &uri),
        );
        // Round-trip through the wire format: exercises NoValue/dateTime tags.
        let response = IppParser::new(IppReader::new(Cursor::new(response.to_bytes())))
            .parse()
            .unwrap();
        assert_eq!(response.header().operation_or_status, 0);
        for name in [
            // PWG 5100.14 Table 5 (description) REQUIRED entries
            "charset-configured",
            "charset-supported",
            "color-supported",
            "compression-supported",
            "copies-default",
            "copies-supported",
            "document-format-default",
            "document-format-supported",
            "document-password-supported",
            "generated-natural-language-supported",
            "identify-actions-default",
            "identify-actions-supported",
            "ipp-features-supported",
            "ipp-versions-supported",
            "job-creation-attributes-supported",
            "job-ids-supported",
            "media-bottom-margin-supported",
            "media-col-database",
            "media-col-default",
            "media-col-ready",
            "media-col-supported",
            "media-default",
            "media-left-margin-supported",
            "media-ready",
            "media-right-margin-supported",
            "media-size-supported",
            "media-source-supported",
            "media-supported",
            "media-top-margin-supported",
            "media-type-supported",
            "multiple-document-jobs-supported",
            "multiple-operation-timeout",
            "multiple-operation-timeout-action",
            "natural-language-configured",
            "operations-supported",
            "orientation-requested-default",
            "orientation-requested-supported",
            "output-bin-default",
            "output-bin-supported",
            "overrides-supported",
            "page-ranges-supported",
            "pdl-override-supported",
            "preferred-attributes-supported",
            "print-color-mode-default",
            "print-color-mode-supported",
            "print-quality-default",
            "print-quality-supported",
            "printer-geo-location",
            "printer-get-attributes-supported",
            "printer-icons",
            "printer-info",
            "printer-location",
            "printer-make-and-model",
            "printer-mandatory-job-attributes",
            "printer-name",
            "printer-organization",
            "printer-organizational-unit",
            "printer-resolution-default",
            "printer-resolution-supported",
            "pwg-raster-document-resolution-supported",
            "pwg-raster-document-sheet-back",
            "pwg-raster-document-type-supported",
            "sides-default",
            "sides-supported",
            "uri-authentication-supported",
            "uri-security-supported",
            "which-jobs-supported",
            // PWG 5100.14 Table 7 (status) REQUIRED entries
            "pages-per-minute",
            "pages-per-minute-color",
            "printer-config-change-date-time",
            "printer-config-change-time",
            "printer-is-accepting-jobs",
            "printer-more-info",
            "printer-state",
            "printer-state-change-date-time",
            "printer-state-change-time",
            "printer-state-message",
            "printer-state-reasons",
            "printer-up-time",
            "printer-uri-supported",
            "printer-uuid",
            "queued-job-count",
            // RECOMMENDED entries that iOS explicitly polls for
            "printer-dns-sd-name",
            "printer-current-time",
        ] {
            assert!(attr(&response, name).is_some(), "Missing {name}");
        }
        // PWG 5100.14 Table 4: every REQUIRED operation must be advertised.
        let Some(IppValue::Array(ops)) = attr(&response, "operations-supported") else {
            panic!("operations-supported missing");
        };
        for op in [2, 4, 5, 6, 8, 9, 10, 11, 0x39, 0x3b, 0x3c] {
            assert!(
                ops.contains(&IppValue::Enum(op)),
                "operation {op:#06x} not advertised"
            );
        }
        assert_eq!(
            attr(&response, "printer-dns-sd-name"),
            Some(&IppValue::NameWithoutLanguage(p.dns_sd_name("pc.local.")))
        );
        assert_eq!(
            attr(&response, "printer-mandatory-job-attributes"),
            Some(&IppValue::NoValue)
        );
        // Icons and the info page must be absolute HTTP URIs on this authority.
        let Some(IppValue::Array(icons)) = attr(&response, "printer-icons") else {
            panic!("printer-icons missing");
        };
        for icon in icons {
            let IppValue::Uri(icon) = icon else {
                panic!("printer-icons must be URIs");
            };
            assert!(icon.starts_with("http://127.0.0.1:631/icons/"), "{icon}");
        }
        assert_eq!(
            attr(&response, "printer-more-info"),
            Some(&IppValue::Uri("http://127.0.0.1:631/".into()))
        );
    }

    #[test]
    fn close_job_cancel_my_jobs_and_identify_printer() {
        let server = IppServer::new("127.0.0.1", 0);
        let p = printer("operations queue");
        server.add_printer(p.clone(), "pc.local.").unwrap();
        let path = format!("/{}", p.resource_path());
        let host = "127.0.0.1:631";
        let uri = format!("ipp://{host}{path}");
        // ipp 5.4's Operation enum lacks these codes; patch the header directly.
        let raw = |op: u16| {
            let mut request = req(Operation::GetPrinterAttributes, &uri);
            request.header_mut().operation_or_status = op;
            request
        };

        let response = process(&server.shared, &path, host, raw(0x003c));
        assert_eq!(response.header().operation_or_status, 0);

        let response = process(&server.shared, &path, host, req(Operation::CreateJob, &uri));
        assert_eq!(response.header().operation_or_status, 0);
        let id = job_id(&response);
        match attr(&response, "job-uuid") {
            Some(IppValue::Uri(uuid)) => assert!(uuid.starts_with("urn:uuid:")),
            other => panic!("missing job-uuid: {other:?}"),
        }
        for name in [
            "time-at-creation",
            "date-time-at-creation",
            "job-printer-up-time",
            "job-impressions",
            "job-impressions-completed",
        ] {
            assert!(attr(&response, name).is_some(), "Missing {name}");
        }
        assert_eq!(
            attr(&response, "time-at-completed"),
            Some(&IppValue::NoValue)
        );

        // Close-Job completes the empty job and reports its final state.
        let mut close = raw(0x003b);
        add(
            &mut close,
            DelimiterTag::OperationAttributes,
            "job-id",
            IppValue::Integer(id),
        );
        let response = process(&server.shared, &path, host, close);
        assert_eq!(response.header().operation_or_status, 0);
        assert_eq!(attr(&response, "job-state"), Some(&IppValue::Enum(9)));
        assert!(matches!(
            attr(&response, "time-at-completed"),
            Some(IppValue::Integer(_))
        ));

        // Cancel-My-Jobs cancels a job still waiting for its document.
        let response = process(&server.shared, &path, host, req(Operation::CreateJob, &uri));
        let id = job_id(&response);
        let response = process(&server.shared, &path, host, raw(0x0039));
        assert_eq!(response.header().operation_or_status, 0);
        let mut get = req(Operation::GetJobAttributes, &uri);
        add(
            &mut get,
            DelimiterTag::OperationAttributes,
            "job-id",
            IppValue::Integer(id),
        );
        let response = process(&server.shared, &path, host, get);
        assert_eq!(attr(&response, "job-state"), Some(&IppValue::Enum(7)));
        assert_eq!(
            attr(&response, "job-state-reasons"),
            Some(&IppValue::Keyword("canceled-by-user".into()))
        );
    }
}
