// SPDX-License-Identifier: Apache-2.0
//! In-process HTTP/1.1 loopback server for the fetcher's behavioural tests
//! (feature `testing`; never enable in a production dependency).
//!
//! Deliberately tiny and dependency-free (`std::net`): one request per
//! connection (`Connection: close`), handler-driven responses, optional
//! body dribbling to make slice budgets observable, and a recorded request
//! log so tests can assert what the *server* saw (Range / If-Range) rather
//! than what the client believes it sent.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use md5::{Digest, Md5};

/// A parsed request; header names are lower-cased.
#[derive(Debug, Clone)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        let name = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(k, _)| *k == name)
            .map(|(_, v)| v.as_str())
    }
}

#[derive(Debug, Clone)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// `(chunk bytes, delay before each chunk)` — a slow mirror.
    pub dribble: Option<(usize, Duration)>,
}

impl Response {
    pub fn ok(body: Vec<u8>, content_type: &str) -> Self {
        Response {
            status: 200,
            headers: vec![("Content-Type".into(), content_type.into())],
            body,
            dribble: None,
        }
    }

    /// A Geofabrik-shaped 302: HTML body, `Location` to the dated file.
    pub fn redirect(location: &str) -> Self {
        Response {
            status: 302,
            headers: vec![
                ("Location".into(), location.into()),
                ("Content-Type".into(), "text/html; charset=iso-8859-1".into()),
            ],
            body: format!(
                "<!DOCTYPE HTML PUBLIC \"-//IETF//DTD HTML 2.0//EN\">\n<html><head>\n<title>302 Found</title>\n</head><body>\n<h1>Found</h1>\n<p>The document has moved <a href=\"{location}\">here</a>.</p>\n</body></html>\n"
            )
            .into_bytes(),
            dribble: None,
        }
    }

    pub fn not_found() -> Self {
        Response {
            status: 404,
            headers: vec![("Content-Type".into(), "text/html".into())],
            body: b"<html><body><h1>404 Not Found</h1></body></html>\n".to_vec(),
            dribble: None,
        }
    }
}

/// Apache-like `Range` / `If-Range` semantics for a static body.
pub fn range_response(body: &[u8], req: &Request, etag: &str) -> Response {
    let len = body.len() as u64;
    let common = |mut r: Response| {
        r.headers.push(("ETag".into(), etag.into()));
        r.headers.push(("Accept-Ranges".into(), "bytes".into()));
        r
    };
    let full = || common(Response::ok(body.to_vec(), "application/octet-stream"));

    let Some(range) = req.header("range") else {
        return full();
    };
    if let Some(if_range) = req.header("if-range") {
        if if_range != etag {
            return full();
        }
    }
    let Some(spec) = range.strip_prefix("bytes=") else {
        return full();
    };
    let (start, end) = match spec.split_once('-') {
        Some((s, e)) => (
            s.parse::<u64>().ok(),
            if e.is_empty() {
                None
            } else {
                e.parse::<u64>().ok()
            },
        ),
        None => (None, None),
    };
    let Some(start) = start else {
        return full();
    };
    if start >= len {
        return common(Response {
            status: 416,
            headers: vec![("Content-Range".into(), format!("bytes */{len}"))],
            body: Vec::new(),
            dribble: None,
        });
    }
    let end = end.map_or(len - 1, |e| e.min(len - 1));
    let slice = body[start as usize..=end as usize].to_vec();
    common(Response {
        status: 206,
        headers: vec![
            ("Content-Type".into(), "application/octet-stream".into()),
            ("Content-Range".into(), format!("bytes {start}-{end}/{len}")),
        ],
        body: slice,
        dribble: None,
    })
}

pub fn md5_hex(bytes: &[u8]) -> String {
    let digest = Md5::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Writes the sidecar a finished, verified fetch of `<dir>/<basename>` would
/// leave behind (P-SOV.C3a): downstream suites compile synthetic fixtures
/// through the D6 belt (`crate::verified_input`) without a mirror. `total`
/// is the file's current length (0 if it does not exist yet).
pub fn write_verified_sidecar(dir: &std::path::Path, source_id: &str, basename: &str) {
    let total = std::fs::metadata(dir.join(basename)).map_or(0, |m| m.len());
    let sc = crate::sidecar::Sidecar {
        source_id: source_id.to_string(),
        pinned_url: format!("https://mirror.invalid/{basename}"),
        etag: String::new(),
        total,
        expected_md5: String::new(),
        verified: true,
        restarts: 0,
    };
    crate::sidecar::save_sidecar(dir, &sc).expect("test sidecar write");
}

pub struct LoopbackServer {
    addr: SocketAddr,
    requests: Arc<Mutex<Vec<Request>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl LoopbackServer {
    pub fn start<F>(handler: F) -> Self
    where
        F: Fn(&Request) -> Response + Send + Sync + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        listener.set_nonblocking(true).expect("nonblocking");
        let addr = listener.local_addr().expect("addr");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let handler = Arc::new(handler);

        let t_requests = Arc::clone(&requests);
        let t_stop = Arc::clone(&stop);
        let thread = std::thread::spawn(move || {
            while !t_stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let _ = stream.set_nonblocking(false);
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                        if let Some(req) = read_request(&stream) {
                            let resp = handler(&req);
                            t_requests.lock().unwrap().push(req);
                            // Write errors (client dropped mid-dribble) are
                            // the expected way a budgeted slice ends.
                            let _ = write_response(stream, &resp);
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    Err(_) => break,
                }
            }
        });

        LoopbackServer {
            addr,
            requests,
            stop,
            thread: Some(thread),
        }
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://{}{}", self.addr, path)
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Every request served so far, in arrival order.
    pub fn requests(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for LoopbackServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn read_request(mut stream: &TcpStream) -> Option<Request> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
        if buf.len() > 64 * 1024 {
            return None;
        }
    }
    let text = String::from_utf8_lossy(&buf);
    let mut lines = text.split("\r\n");
    let request_line = lines.next()?;
    let mut parts = request_line.split(' ');
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();
    let mut headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
        }
    }
    Some(Request {
        method,
        path,
        headers,
    })
}

fn write_response(mut stream: TcpStream, resp: &Response) -> std::io::Result<()> {
    let reason = match resp.status {
        200 => "OK",
        206 => "Partial Content",
        302 => "Found",
        404 => "Not Found",
        416 => "Range Not Satisfiable",
        500 => "Internal Server Error",
        _ => "Status",
    };
    let mut head = format!("HTTP/1.1 {} {reason}\r\n", resp.status);
    for (k, v) in &resp.headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str(&format!("Content-Length: {}\r\n", resp.body.len()));
    head.push_str("Connection: close\r\n\r\n");
    stream.write_all(head.as_bytes())?;
    match resp.dribble {
        None => stream.write_all(&resp.body)?,
        Some((chunk, delay)) => {
            for piece in resp.body.chunks(chunk.max(1)) {
                std::thread::sleep(delay);
                stream.write_all(piece)?;
                stream.flush()?;
            }
        }
    }
    stream.flush()
}
