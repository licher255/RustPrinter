use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc, Mutex,
};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Clone, Copy)]
pub(super) struct Origin {
    pub peer: SocketAddr,
    pub public_port: u16,
}

pub(super) type OriginMap = Arc<Mutex<HashMap<SocketAddr, Origin>>>;

fn classify(first: u8) -> Option<bool> {
    if first == 0x16 {
        Some(true)
    } else if first.is_ascii_uppercase() {
        Some(false)
    } else {
        None
    }
}

pub(super) fn spawn_mux(
    listener: TcpListener,
    http_target: SocketAddr,
    tls_target: Arc<Mutex<Option<SocketAddr>>>,
    origins: OriginMap,
    running: Arc<AtomicBool>,
    public_port: u16,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let active = Arc::new(AtomicUsize::new(0));
        while running.load(Ordering::Acquire) {
            match listener.accept() {
                Ok((client, _)) => {
                    if active.fetch_add(1, Ordering::AcqRel) >= 16 {
                        active.fetch_sub(1, Ordering::AcqRel);
                        let _ = client.shutdown(Shutdown::Both);
                        continue;
                    }
                    let active = active.clone();
                    let tls_target = tls_target.clone();
                    let origins = origins.clone();
                    let running = running.clone();
                    thread::spawn(move || {
                        struct Guard(Arc<AtomicUsize>);
                        impl Drop for Guard {
                            fn drop(&mut self) {
                                self.0.fetch_sub(1, Ordering::AcqRel);
                            }
                        }
                        let _guard = Guard(active);
                        proxy(
                            client,
                            http_target,
                            tls_target,
                            origins,
                            running,
                            public_port,
                        );
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(50));
                }
                Err(error) => {
                    crate::services::diagnostics::record(&format!(
                        "[IPP] protocol mux accept failed: {error}"
                    ));
                    thread::sleep(Duration::from_millis(100));
                }
            }
        }
    })
}

fn proxy(
    client: TcpStream,
    http_target: SocketAddr,
    tls_target: Arc<Mutex<Option<SocketAddr>>>,
    origins: OriginMap,
    running: Arc<AtomicBool>,
    public_port: u16,
) {
    let Ok(peer) = client.peer_addr() else { return };
    if client.set_nonblocking(false).is_err() {
        return;
    }
    let _ = client.set_read_timeout(Some(Duration::from_secs(5)));
    let mut first = [0u8; 1];
    if client.peek(&mut first).ok() != Some(1) {
        return;
    }
    let target = match classify(first[0]) {
        Some(true) => *tls_target.lock().unwrap_or_else(|error| error.into_inner()),
        Some(false) => Some(http_target),
        None => None,
    };
    let protocol = if classify(first[0]) == Some(true) {
        "tls"
    } else {
        "http"
    };
    let Some(target) = target else {
        crate::services::diagnostics::record(&format!(
            "[IPP] protocol mux rejected first byte 0x{:02x} from {peer}",
            first[0]
        ));
        return;
    };
    let upstream = match TcpStream::connect_timeout(&target, Duration::from_secs(5)) {
        Ok(stream) => stream,
        Err(error) => {
            crate::services::diagnostics::record(&format!(
                "[IPP transport] peer={peer} public_port={public_port} protocol={protocol} upstream={target} connect_error={error}"
            ));
            return;
        }
    };
    let _ = upstream.set_nodelay(true);
    let _ = client.set_nodelay(true);
    let Ok(proxy_addr) = upstream.local_addr() else {
        return;
    };
    origins
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .insert(proxy_addr, Origin { peer, public_port });
    crate::services::diagnostics::record(&format!(
        "[IPP transport] peer={peer} public_port={public_port} protocol={protocol} upstream={target}"
    ));

    let Ok(client_read) = client.try_clone() else {
        origins
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&proxy_addr);
        return;
    };
    let Ok(upstream_read) = upstream.try_clone() else {
        origins
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&proxy_addr);
        return;
    };
    let Ok(client_shutdown) = client.try_clone() else {
        origins
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&proxy_addr);
        return;
    };
    let Ok(upstream_shutdown) = upstream.try_clone() else {
        origins
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&proxy_addr);
        return;
    };
    let activity = Arc::new(Mutex::new(Instant::now()));
    let (request_bytes, response_bytes) = thread::scope(|scope| {
        let response_activity = activity.clone();
        let response_running = running.clone();
        let response = scope.spawn(move || {
            let bytes = pump(
                upstream_read,
                client,
                response_activity,
                response_running,
                "backend-to-client",
            );
            let _ = client_shutdown.shutdown(Shutdown::Both);
            bytes
        });
        let request_bytes = pump(
            client_read,
            upstream,
            activity,
            running,
            "client-to-backend",
        );
        let _ = upstream_shutdown.shutdown(Shutdown::Write);
        (request_bytes, response.join().unwrap_or(0))
    });
    origins
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .remove(&proxy_addr);
    crate::services::diagnostics::record(&format!(
        "[IPP transport] peer={peer} public_port={public_port} protocol={protocol} upstream={target} request_bytes={request_bytes} response_bytes={response_bytes}"
    ));
}

fn pump(
    mut reader: TcpStream,
    mut writer: TcpStream,
    activity: Arc<Mutex<Instant>>,
    running: Arc<AtomicBool>,
    direction: &str,
) -> u64 {
    let _ = reader.set_read_timeout(Some(Duration::from_secs(1)));
    let _ = writer.set_write_timeout(Some(Duration::from_secs(30)));
    let mut buffer = [0u8; 16 * 1024];
    let mut total = 0;
    while running.load(Ordering::Acquire) {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(size) => {
                *activity.lock().unwrap_or_else(|error| error.into_inner()) = Instant::now();
                if let Err(error) = writer.write_all(&buffer[..size]) {
                    crate::services::diagnostics::record(&format!(
                        "[IPP transport] {direction} write failed: {error}"
                    ));
                    break;
                }
                total += size as u64;
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                if activity
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .elapsed()
                    > Duration::from_secs(120)
                {
                    break;
                }
            }
            Err(error) => {
                crate::services::diagnostics::record(&format!(
                    "[IPP transport] {direction} read failed: {error}"
                ));
                break;
            }
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn roundtrip(
        client_address: SocketAddr,
        target: TcpListener,
        origins: OriginMap,
        bytes: &'static [u8],
        public_port: u16,
    ) {
        let expected_peer;
        let mut client = TcpStream::connect(client_address).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        expected_peer = client.local_addr().unwrap();
        client.write_all(bytes).unwrap();
        client.shutdown(Shutdown::Write).unwrap();

        let (mut backend, proxy_peer) = target.accept().unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        let origin = loop {
            let found = origins.lock().unwrap().get(&proxy_peer).copied();
            if let Some(origin) = found {
                break origin;
            }
            assert!(
                Instant::now() < deadline,
                "proxy origin mapping was not installed"
            );
            thread::sleep(Duration::from_millis(5));
        };
        assert_eq!(origin.peer, expected_peer);
        assert_eq!(origin.public_port, public_port);

        let mut received = Vec::new();
        backend.read_to_end(&mut received).unwrap();
        assert_eq!(received, bytes);
        backend.write_all(b"reply").unwrap();
        backend.shutdown(Shutdown::Write).unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).unwrap();
        assert_eq!(response, b"reply");
    }

    #[test]
    fn mux_preserves_plain_and_tls_bytes_and_maps_client_address() {
        let http_backend = TcpListener::bind("127.0.0.1:0").unwrap();
        let http_target = http_backend.local_addr().unwrap();
        let tls_backend = TcpListener::bind("127.0.0.1:0").unwrap();
        let tls_target_addr = tls_backend.local_addr().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mux_address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let origins = Arc::new(Mutex::new(HashMap::new()));
        let tls_target = Arc::new(Mutex::new(Some(tls_target_addr)));
        let running = Arc::new(AtomicBool::new(true));
        let mux = spawn_mux(
            listener,
            http_target,
            tls_target,
            origins.clone(),
            running.clone(),
            631,
        );
        roundtrip(
            mux_address,
            http_backend,
            origins.clone(),
            b"POST /ipp HTTP/1.1\r\n\r\n",
            631,
        );
        roundtrip(
            mux_address,
            tls_backend,
            origins.clone(),
            b"\x16\x03\x01\x00\x05hello",
            631,
        );
        let deadline = Instant::now() + Duration::from_secs(2);
        while !origins.lock().unwrap().is_empty() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        running.store(false, Ordering::Release);
        mux.join().unwrap();
        assert!(origins.lock().unwrap().is_empty());
    }
}
