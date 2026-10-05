// SPDX-License-Identifier: Apache-2.0
//! `fetcher` — hostile-mirror-safe downloader for raw OSM `.osm.pbf` and DEM
//! `.tif` inputs (Phase 2; slice engine + pinning added in P-SOV.C2a).
//!
//! Invariants, all learned the hard way on this project:
//!
//! 1. **Magic-byte validation.** A payload is never trusted until its leading
//!    bytes are checked: the `OSMHeader` blob for PBFs, a TIFF byte-order
//!    marker for DEMs. An HTML page that arrives with `200 OK` (or a
//!    `Content-Type` that lies — openstreetmap.fr labels PBF as XML) is
//!    rejected loudly instead of corrupting the pipeline.
//!
//! 2. **Pinned identity.** Mirrors rotate: Geofabrik's `-latest` is a 302 to
//!    a dated file that changes every night; osm.fr regenerates the same URL
//!    in place. A `Range` resume across such a rotation splices two files
//!    into one that passes the magic-byte gate. So the entry URL is resolved
//!    ONCE (redirects disabled, every hop https), the final URL + strong
//!    ETag + total + published MD5 are pinned in a sidecar, and every resume
//!    goes to the pinned URL with `If-Range`. A 200 on a resume, a 404/416,
//!    or a mismatched `Content-Range` is a restart-clean event, capped.
//!
//! 3. **Budget-yield, kill-safe.** [`fetch_slice_blocking`] is the same
//!    contract as the compiler's `compile_chunk`: it runs for roughly the
//!    budget, leaves the data file fsync'd, and returns `Yielded`; the next
//!    call resumes from the file's length. Nothing lives in memory between
//!    slices, so process death between (or during) slices loses at most the
//!    un-fsync'd tail of one slice — and the MD5 catches anything worse.
//!
//! TLS is rustls-only (see Cargo.toml) so the crate cross-compiles to
//! aarch64 Android/iOS without an OpenSSL toolchain.

use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use md5::{Digest, Md5};
use reqwest::header::{CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, ETAG, LOCATION, RANGE};
use reqwest::{StatusCode, Url};
use tokio::io::AsyncWriteExt;

pub mod sidecar;
pub mod sources;
#[cfg(feature = "testing")]
pub mod testing;

use sidecar::{load_sidecar, save_sidecar, sidecar_path, Sidecar};
use sources::{Md5Spec, Source};

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Fetch failures. Plain enum (no `thiserror`) so the FFI layer can flatten
/// it cheaply, mirroring the `compiler` crate's error style.
#[derive(Debug)]
pub enum FetchError {
    /// Transport/HTTP error (DNS, TLS, connection, non-success status).
    Http(String),
    /// Filesystem error while reading/writing the partial file.
    Io(String),
    /// The payload's leading bytes failed magic-byte validation, or a
    /// published checksum file did not parse. This is the anti-HTML-redirect
    /// guard: an HTML page served with 200 lands here.
    InvalidPayload(String),
    /// The server ignored our Range request in a way we can't safely reconcile.
    RangeUnsupported(String),
    /// The durable fetch sidecar is unreadable, foreign, or of another
    /// version (refused loudly, never guessed at).
    State(String),
}

impl fmt::Display for FetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FetchError::Http(s) => write!(f, "http error: {s}"),
            FetchError::Io(s) => write!(f, "io error: {s}"),
            FetchError::InvalidPayload(s) => write!(f, "invalid payload: {s}"),
            FetchError::RangeUnsupported(s) => write!(f, "range unsupported: {s}"),
            FetchError::State(s) => write!(f, "state error: {s}"),
        }
    }
}

impl std::error::Error for FetchError {}

// ---------------------------------------------------------------------------
// Magic-byte validation
// ---------------------------------------------------------------------------

/// Expected payload kind, selected by the caller from the target filename.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayloadKind {
    /// OpenStreetMap Protocolbuffer Binary Format (`.osm.pbf`).
    OsmPbf,
    /// GeoTIFF Digital Elevation Model (`.tif`).
    Tiff,
}

/// Minimum bytes needed to make a validation decision for each kind. The
/// downloader ensures at least this many leading bytes exist before trusting.
pub const MIN_VALIDATION_BYTES: usize = 32;

impl PayloadKind {
    /// Validates the *leading* bytes of a payload. Reads only the head; never
    /// scans the whole file.
    pub fn validate(self, head: &[u8]) -> Result<(), FetchError> {
        match self {
            PayloadKind::OsmPbf => validate_osm_pbf(head),
            PayloadKind::Tiff => validate_tiff(head),
        }
    }
}

/// A `.osm.pbf` begins with:
///   [0..4]  BE u32 BlobHeader length (small — tens of bytes)
///   [4]     0x0a   protobuf field 1 (`type`), wire type 2 (length-delimited)
///   [5]     string length
///   [6..]   the blob type string; the FIRST blob must be "OSMHeader"
///
/// An HTML redirect page starts with `<!DOCTYPE html>` / `<html`, whose first
/// four bytes decode to an absurd BlobHeader length, so it is rejected before
/// the string check even runs.
fn validate_osm_pbf(head: &[u8]) -> Result<(), FetchError> {
    if head.len() < 6 {
        return Err(FetchError::InvalidPayload(format!(
            "too short for a PBF header ({} bytes)",
            head.len()
        )));
    }
    let header_len = u32::from_be_bytes([head[0], head[1], head[2], head[3]]) as usize;
    // A real BlobHeader is tiny; anything large means this isn't a PBF (e.g.
    // "<!DO" → 0x3c214f44 ≈ 1.01 billion).
    if !(1..=64 * 1024).contains(&header_len) {
        return Err(FetchError::InvalidPayload(format!(
            "implausible BlobHeader length {header_len} (not a PBF — likely an HTML/error page)"
        )));
    }
    if head[4] != 0x0a {
        return Err(FetchError::InvalidPayload(
            "missing protobuf `type` field tag (0x0a) — not a PBF BlobHeader".to_string(),
        ));
    }
    let str_len = head[5] as usize;
    let start = 6;
    let end = start + str_len;
    if end > head.len() {
        return Err(FetchError::InvalidPayload(
            "declared blob-type string exceeds available header bytes".to_string(),
        ));
    }
    let blobtype = &head[start..end];
    if blobtype != b"OSMHeader" {
        let shown = String::from_utf8_lossy(blobtype);
        return Err(FetchError::InvalidPayload(format!(
            "first blob type is '{shown}', expected 'OSMHeader'"
        )));
    }
    Ok(())
}

/// A TIFF (and thus GeoTIFF) begins with a 4-byte marker: `II*\0`
/// (little-endian / Intel) or `MM\0*` (big-endian / Motorola).
fn validate_tiff(head: &[u8]) -> Result<(), FetchError> {
    if head.len() < 4 {
        return Err(FetchError::InvalidPayload(format!(
            "too short for a TIFF marker ({} bytes)",
            head.len()
        )));
    }
    match &head[0..4] {
        b"II\x2a\x00" | b"MM\x00\x2a" => Ok(()),
        other => Err(FetchError::InvalidPayload(format!(
            "bad TIFF byte-order marker {other:02x?} (expected II*\\0 or MM\\0*)"
        ))),
    }
}

/// Convenience: pick the payload kind from a filename's extension.
pub fn kind_for_path(path: &Path) -> Option<PayloadKind> {
    let name = path.file_name()?.to_str()?.to_ascii_lowercase();
    if name.ends_with(".osm.pbf") || name.ends_with(".pbf") {
        Some(PayloadKind::OsmPbf)
    } else if name.ends_with(".tif") || name.ends_with(".tiff") {
        Some(PayloadKind::Tiff)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Pure helpers (contract-tested without the network)
// ---------------------------------------------------------------------------

/// URL policy: https anywhere; plain http only to the loopback host (the
/// in-process test server). Anything else — other schemes, http to a real
/// host, unparsable — is refused before a socket is opened.
pub fn url_allowed(url: &str) -> bool {
    let Ok(u) = Url::parse(url) else {
        return false;
    };
    match u.scheme() {
        "https" => true,
        "http" => {
            // `[::1]` arrives bracketed; a dotted-quad-looking domain
            // ("127.0.0.1.evil.example") does not parse as an IP.
            let host = u.host_str().unwrap_or("");
            let bare = host.trim_start_matches('[').trim_end_matches(']');
            bare.parse::<std::net::IpAddr>()
                .map(|ip| ip.is_loopback())
                .unwrap_or_else(|_| bare.eq_ignore_ascii_case("localhost"))
        }
        _ => false,
    }
}

/// The on-disk name of a pinned URL: its last path segment, restricted to
/// `[A-Za-z0-9._-]`, not dot-led, and carrying `kind`'s extension. The
/// pinned URL comes from the mirror's `Location` header, i.e. from the
/// network — this is the only thing standing between it and a path join.
pub fn pinned_basename(url: &str, kind: PayloadKind) -> Result<String, FetchError> {
    let u = Url::parse(url).map_err(|e| FetchError::Http(format!("bad url {url:?}: {e}")))?;
    let seg = u
        .path_segments()
        .and_then(|mut s| s.next_back())
        .unwrap_or("")
        .to_string();
    let charset_ok = !seg.is_empty()
        && !seg.starts_with('.')
        && seg
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-');
    if !charset_ok {
        return Err(FetchError::InvalidPayload(format!(
            "refusing pinned basename {seg:?} (only [A-Za-z0-9._-], not dot-led)"
        )));
    }
    if kind_for_path(Path::new(&seg)) != Some(kind) {
        return Err(FetchError::InvalidPayload(format!(
            "pinned basename {seg:?} does not carry a {kind:?} extension"
        )));
    }
    Ok(seg)
}

/// Parses a `md5sum`-style line (`<32 hex>  [*]<filename>`), returning the
/// lower-case hex. Exactly one non-blank line, exactly two tokens, and the
/// filename must match — an HTML page, a truncated hash or a checksum for a
/// different file are all rejected.
pub fn parse_md5_line(text: &str, expected_filename: &str) -> Result<String, FetchError> {
    let bad = |what: &str| FetchError::InvalidPayload(format!("md5 file: {what}"));
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let line = lines.next().ok_or_else(|| bad("empty"))?;
    if lines.next().is_some() {
        return Err(bad("more than one line"));
    }
    let tokens: Vec<&str> = line.split_whitespace().collect();
    if tokens.len() != 2 {
        return Err(bad(&format!("expected `<hash>  <filename>`, got {line:?}")));
    }
    let hash = tokens[0];
    if hash.len() != 32 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(bad(&format!("bad hash {hash:?}")));
    }
    let name = tokens[1].strip_prefix('*').unwrap_or(tokens[1]);
    if name != expected_filename {
        return Err(bad(&format!(
            "checksum names {name:?}, expected {expected_filename:?}"
        )));
    }
    Ok(hash.to_ascii_lowercase())
}

/// Headers for a data request: always `Range: bytes=<have>-`; plus
/// `If-Range: <etag>` when resuming a non-empty file against a strong ETag,
/// so a changed entity yields a 200 (restart) instead of a spliced 206.
pub fn resume_headers(have: u64, etag: &str) -> Vec<(String, String)> {
    let mut h = vec![("Range".to_string(), format!("bytes={have}-"))];
    if have > 0 && !etag.is_empty() {
        h.push(("If-Range".to_string(), etag.to_string()));
    }
    h
}

/// `bytes A-B/N` → `(A, B, N)`.
fn parse_content_range(v: &str) -> Option<(u64, u64, u64)> {
    let spec = v.trim().strip_prefix("bytes ")?;
    let (range, total) = spec.split_once('/')?;
    let (a, b) = range.split_once('-')?;
    Some((a.parse().ok()?, b.parse().ok()?, total.parse().ok()?))
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ---------------------------------------------------------------------------
// Public state / outcome types
// ---------------------------------------------------------------------------

/// Restart-clean events tolerated per source before giving up: a mirror that
/// ignores `Range`, or an entity that changes on every attempt, would
/// otherwise loop forever at zero net progress.
pub const MAX_RESTARTS: u32 = 3;

/// Snapshot of a source's durable fetch state (sidecar + data file length).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchState {
    pub source_id: String,
    /// Empty until the entry URL has been resolved.
    pub pinned_url: String,
    /// `<dest_dir>/<pinned basename>`; empty until resolved.
    pub path: PathBuf,
    pub have: u64,
    pub total: u64,
    pub verified: bool,
    pub restarts: u32,
    pub expected_md5: String,
}

/// Result of one fetch slice (the Surface v1 shape).
#[derive(Debug)]
pub enum FetchOutcome {
    /// The file is complete, magic-checked and MD5-verified at `path`.
    Finished { path: PathBuf, bytes: u64 },
    /// Budget expired; `have` bytes are fsync'd on disk. Call again.
    Yielded {
        path: PathBuf,
        have: u64,
        total: u64,
    },
    /// Non-retryable: bad payload, checksum mismatch, refused URL, restart
    /// cap, unreadable sidecar. `purge_fetch` clears the state.
    FailedFatal(String),
    /// The network or the disk refused this slice (DNS, TLS, connection,
    /// 5xx, ENOSPC/EIO). Durable state untouched — retry later.
    FailedTransient(String),
}

// ---------------------------------------------------------------------------
// Sidecar-backed queries
// ---------------------------------------------------------------------------

fn data_path(dest_dir: &Path, sc: &Sidecar, kind: PayloadKind) -> Result<PathBuf, FetchError> {
    Ok(dest_dir.join(pinned_basename(&sc.pinned_url, kind)?))
}

fn file_len(path: &Path) -> Result<u64, FetchError> {
    match std::fs::metadata(path) {
        Ok(m) => Ok(m.len()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(e) => Err(FetchError::Io(format!("stat {}: {e}", path.display()))),
    }
}

/// Durable state for `source_id`, or `None` if nothing was ever resolved.
pub fn query_fetch(source_id: &str, dest_dir: &Path) -> Result<Option<FetchState>, FetchError> {
    let Some(sc) = load_sidecar(dest_dir, source_id)? else {
        return Ok(None);
    };
    let (path, have) = if sc.is_resolved() {
        // The pinned URL carries the kind via its extension.
        let kind = kind_for_path(Path::new(&sc.pinned_url)).unwrap_or(PayloadKind::OsmPbf);
        let p = data_path(dest_dir, &sc, kind)?;
        let n = file_len(&p)?;
        (p, n)
    } else {
        (PathBuf::new(), 0)
    };
    Ok(Some(FetchState {
        source_id: sc.source_id,
        pinned_url: sc.pinned_url,
        path,
        have,
        total: sc.total,
        verified: sc.verified,
        restarts: sc.restarts,
        expected_md5: sc.expected_md5,
    }))
}

/// Removes the sidecar and the data file (partial or complete). Returns
/// true if anything existed. Lenient about the sidecar's contents so a
/// version-refused sidecar can still be cleared.
pub fn purge_fetch(source_id: &str, dest_dir: &Path) -> bool {
    let sc_path = sidecar_path(dest_dir, source_id);
    let mut removed = false;
    if let Ok(text) = std::fs::read_to_string(&sc_path) {
        if let Some(url) = text.lines().find_map(|l| l.strip_prefix("pinned_url=")) {
            if let Some(kind) = kind_for_path(Path::new(url)) {
                if let Ok(name) = pinned_basename(url, kind) {
                    removed |= std::fs::remove_file(dest_dir.join(name)).is_ok();
                }
            }
        }
    }
    removed |= std::fs::remove_file(&sc_path).is_ok();
    let _ = std::fs::remove_file(sc_path.with_extension("fetch.tmp"));
    removed
}

/// The D6 Rust belt (P-SOV.C3a): `Ok(())` only if a sidecar beside `path`
/// pins exactly this basename, is marked verified, and the file still has
/// the verified length. Shells gate on `query_fetch` before enqueueing; this
/// is the core's own check, so no caller can hand the compiler an input the
/// fetcher never vouched for. Sidecars are keyed by source id, not by file,
/// hence the directory scan; one that fails to decode belongs to some other
/// source and is skipped rather than blocking every input beside it.
pub fn verified_input(path: &Path) -> Result<(), FetchError> {
    let refuse =
        |why: String| FetchError::State(format!("unverified input {}: {why}", path.display()));
    let (Some(dir), Some(name)) = (path.parent(), path.file_name().and_then(|n| n.to_str())) else {
        return Err(refuse("no parent directory or file name".into()));
    };
    let Some(kind) = kind_for_path(path) else {
        return Err(refuse("not a .pbf/.tif input".into()));
    };
    let entries = std::fs::read_dir(dir)
        .map_err(|e| refuse(format!("cannot list {} for a sidecar: {e}", dir.display())))?;

    let mut unverified = None;
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let Some(id) = file_name.to_str().and_then(|f| f.strip_suffix(".fetch")) else {
            continue;
        };
        let Ok(Some(sc)) = load_sidecar(dir, id) else {
            continue;
        };
        if !sc.is_resolved() || pinned_basename(&sc.pinned_url, kind).ok().as_deref() != Some(name)
        {
            continue;
        }
        if !sc.verified {
            unverified = Some(id.to_string());
            continue;
        }
        let have = file_len(path)?;
        if have != sc.total {
            return Err(refuse(format!(
                "length {have} != verified total {} in sidecar {id}.fetch",
                sc.total
            )));
        }
        return Ok(());
    }
    Err(refuse(match unverified {
        Some(id) => format!("sidecar {id}.fetch is not verified"),
        None => "no fetch sidecar pins this file".into(),
    }))
}

// ---------------------------------------------------------------------------
// The slice engine
// ---------------------------------------------------------------------------

/// Identifies this client honestly to mirrors (Geofabrik and friends expect a
/// real UA; a generic one can be 406'd).
const USER_AGENT: &str = concat!(
    "FreeHike/",
    env!("CARGO_PKG_VERSION"),
    " (+on-device map compiler)"
);

const MAX_REDIRECT_HOPS: usize = 5;
const MAX_MD5_BYTES: usize = 4096;
const PROGRESS_STEP: u64 = 1 << 20;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const READ_TIMEOUT: Duration = Duration::from_secs(30);

/// Why the current pinned entity must be abandoned.
struct Restart(String);

enum SliceEnd {
    Done(FetchOutcome),
    /// Data complete on disk; the blocking wrapper runs magic + MD5.
    Verify {
        sc: Sidecar,
        path: PathBuf,
    },
}

struct Ctx<'a> {
    client: reqwest::Client,
    source: &'a Source,
    dest_dir: &'a Path,
    budget: Duration,
    started: Instant,
    on_progress: &'a mut dyn FnMut(f32, String),
}

impl Ctx<'_> {
    fn over_budget(&self) -> bool {
        self.started.elapsed() >= self.budget
    }

    fn progress(&mut self, have: u64, total: u64, name: &str) {
        let pct = if total == 0 {
            0.0
        } else {
            (have as f64 / total as f64 * 100.0) as f32
        };
        let mib = |b: u64| b as f64 / (1024.0 * 1024.0);
        (self.on_progress)(
            pct,
            format!(
                "fetch: {:.1} MiB / {:.1} MiB ({name})",
                mib(have),
                mib(total)
            ),
        );
    }

    fn note(&mut self, msg: String) {
        (self.on_progress)(0.0, msg);
    }
}

fn io_outcome(what: &str, e: &std::io::Error) -> FetchOutcome {
    // ENOSPC (28) and EIO (5) can clear; everything else (EACCES, ENOTDIR,
    // EROFS…) fails the same way next time.
    match e.raw_os_error() {
        Some(28) | Some(5) => FetchOutcome::FailedTransient(format!("{what}: {e}")),
        _ => FetchOutcome::FailedFatal(format!("{what}: {e}")),
    }
}

fn status_outcome(what: &str, status: StatusCode, url: &str) -> FetchOutcome {
    let code = status.as_u16();
    if status.is_server_error() || code == 408 || code == 429 {
        FetchOutcome::FailedTransient(format!("{what}: {status} for {url}"))
    } else {
        FetchOutcome::FailedFatal(format!("{what}: {status} for {url}"))
    }
}

fn header_str(resp: &reqwest::Response, name: reqwest::header::HeaderName) -> Option<&str> {
    resp.headers().get(name).and_then(|v| v.to_str().ok())
}

/// Strong ETags only: a weak validator (`W/"…"`) is not valid for
/// `If-Range`, and sending it would make every resume a restart.
fn strong_etag(resp: &reqwest::Response) -> String {
    match header_str(resp, ETAG) {
        Some(e) if e.starts_with('"') => e.to_string(),
        _ => String::new(),
    }
}

/// Resolves the entry URL (redirects disabled, ≤ MAX_REDIRECT_HOPS, every
/// hop policy-checked), fetches the published MD5, pins everything in the
/// sidecar and clears any stale data file at the pinned name.
async fn resolve(ctx: &mut Ctx<'_>, sc: &mut Sidecar) -> Result<(), FetchOutcome> {
    let mut url = ctx.source.url.clone();
    let (etag, total, content_type) = 'resolved: {
        for _hop in 0..=MAX_REDIRECT_HOPS {
            if !url_allowed(&url) {
                return Err(FetchOutcome::FailedFatal(format!(
                    "refusing to fetch {url:?}: only https (or loopback for tests) is allowed"
                )));
            }
            let resp = ctx
                .client
                .get(&url)
                .header(RANGE, "bytes=0-0")
                .send()
                .await
                .map_err(|e| FetchOutcome::FailedTransient(format!("resolve {url}: {e}")))?;
            let status = resp.status();
            if status.is_redirection() {
                let Some(loc) = header_str(&resp, LOCATION) else {
                    return Err(FetchOutcome::FailedFatal(format!(
                        "resolve: {status} without Location for {url}"
                    )));
                };
                let base = Url::parse(&url).map_err(|e| {
                    FetchOutcome::FailedFatal(format!("resolve: bad url {url}: {e}"))
                })?;
                let next = base.join(loc).map_err(|e| {
                    FetchOutcome::FailedFatal(format!("resolve: bad Location {loc:?}: {e}"))
                })?;
                url = next.to_string();
                continue;
            }
            let content_type = header_str(&resp, CONTENT_TYPE).unwrap_or("-").to_string();
            let total = match status.as_u16() {
                206 => header_str(&resp, CONTENT_RANGE)
                    .and_then(parse_content_range)
                    .map(|(_, _, n)| n),
                200 => header_str(&resp, CONTENT_LENGTH).and_then(|v| v.parse().ok()),
                _ => return Err(status_outcome("resolve", status, &url)),
            };
            let Some(total) = total else {
                return Err(FetchOutcome::FailedFatal(format!(
                    "resolve: {status} for {url} without a usable length"
                )));
            };
            if total == 0 {
                return Err(FetchOutcome::FailedFatal(format!(
                    "resolve: {url} is empty"
                )));
            }
            break 'resolved (strong_etag(&resp), total, content_type);
        }
        return Err(FetchOutcome::FailedFatal(format!(
            "resolve: more than {MAX_REDIRECT_HOPS} redirects from {}",
            ctx.source.url
        )));
    };

    let name = pinned_basename(&url, ctx.source.kind)
        .map_err(|e| FetchOutcome::FailedFatal(e.to_string()))?;
    ctx.note(format!(
        "fetch: resolved {name} ({total} bytes, content-type {content_type:?}, etag {})",
        if etag.is_empty() {
            "none"
        } else {
            etag.as_str()
        }
    ));

    // Published checksum, pinned alongside so a rotation between now and
    // verification cannot swap the reference.
    let (md5_url, md5_name) = match &ctx.source.md5 {
        Md5Spec::Derived => (format!("{url}.md5"), name.clone()),
        Md5Spec::Explicit { url, filename } => (url.clone(), filename.clone()),
    };
    if !url_allowed(&md5_url) {
        return Err(FetchOutcome::FailedFatal(format!(
            "refusing md5 url {md5_url:?}"
        )));
    }
    let resp = ctx
        .client
        .get(&md5_url)
        .send()
        .await
        .map_err(|e| FetchOutcome::FailedTransient(format!("md5 {md5_url}: {e}")))?;
    if resp.status() != StatusCode::OK {
        return Err(status_outcome("md5", resp.status(), &md5_url));
    }
    if let Some(n) = header_str(&resp, CONTENT_LENGTH).and_then(|v| v.parse::<usize>().ok()) {
        if n > MAX_MD5_BYTES {
            return Err(FetchOutcome::FailedFatal(format!(
                "md5 {md5_url}: implausible size {n} bytes"
            )));
        }
    }
    let body = resp
        .bytes()
        .await
        .map_err(|e| FetchOutcome::FailedTransient(format!("md5 {md5_url}: {e}")))?;
    if body.len() > MAX_MD5_BYTES {
        return Err(FetchOutcome::FailedFatal(format!(
            "md5 {md5_url}: implausible size {} bytes",
            body.len()
        )));
    }
    let expected_md5 = parse_md5_line(&String::from_utf8_lossy(&body), &md5_name)
        .map_err(|e| FetchOutcome::FailedFatal(format!("{md5_url}: {e}")))?;

    sc.pinned_url = url;
    sc.etag = etag;
    sc.total = total;
    sc.expected_md5 = expected_md5;
    sc.verified = false;
    // A leftover file under the new name (from an older, purged-by-hand
    // attempt) must not be mistaken for a resumable partial of this entity.
    let path = ctx.dest_dir.join(&name);
    if let Err(e) = std::fs::remove_file(&path) {
        if e.kind() != std::io::ErrorKind::NotFound {
            return Err(io_outcome("clear stale partial", &e));
        }
    }
    save_sidecar(ctx.dest_dir, sc).map_err(|e| FetchOutcome::FailedFatal(e.to_string()))?;
    Ok(())
}

/// Abandons the pinned entity: removes its data file, bumps `restarts`,
/// unresolves the sidecar. Over the cap → fatal (state kept for diagnosis).
fn restart(ctx: &mut Ctx<'_>, sc: &mut Sidecar, why: Restart) -> Result<(), FetchOutcome> {
    if sc.is_resolved() {
        if let Ok(path) = data_path(ctx.dest_dir, sc, ctx.source.kind) {
            let _ = std::fs::remove_file(path);
        }
    }
    sc.restarts += 1;
    sc.unresolve();
    save_sidecar(ctx.dest_dir, sc).map_err(|e| FetchOutcome::FailedFatal(e.to_string()))?;
    ctx.note(format!("fetch: restart #{} ({})", sc.restarts, why.0));
    if sc.restarts > MAX_RESTARTS {
        return Err(FetchOutcome::FailedFatal(format!(
            "gave up after {} restarts (cap {MAX_RESTARTS}): {}",
            sc.restarts, why.0
        )));
    }
    Ok(())
}

enum DownloadEnd {
    Complete,
    Budget,
}

/// One budgeted data request against the pinned URL, appending to `path`.
async fn download(
    ctx: &mut Ctx<'_>,
    sc: &Sidecar,
    path: &Path,
    name: &str,
    mut have: u64,
) -> Result<Result<DownloadEnd, Restart>, FetchOutcome> {
    let mut req = ctx.client.get(&sc.pinned_url);
    for (k, v) in resume_headers(have, &sc.etag) {
        req = req.header(k, v);
    }
    let resp = req
        .send()
        .await
        .map_err(|e| FetchOutcome::FailedTransient(format!("download {}: {e}", sc.pinned_url)))?;
    let status = resp.status();
    match status.as_u16() {
        206 => {
            let Some((start, _, n)) =
                header_str(&resp, CONTENT_RANGE).and_then(parse_content_range)
            else {
                return Ok(Err(Restart("206 without a parsable Content-Range".into())));
            };
            if start != have || n != sc.total {
                return Ok(Err(Restart(format!(
                    "Content-Range starts at {start}/{n}, expected {have}/{}",
                    sc.total
                ))));
            }
        }
        200 => {
            if have != 0 {
                return Ok(Err(Restart(
                    "200 on a resume request: entity changed under us or Range ignored".into(),
                )));
            }
            match header_str(&resp, CONTENT_LENGTH).and_then(|v| v.parse::<u64>().ok()) {
                Some(n) if n == sc.total => {}
                Some(n) => {
                    return Ok(Err(Restart(format!(
                        "Content-Length {n} differs from the pinned total {}",
                        sc.total
                    ))))
                }
                None => {}
            }
        }
        404 | 416 => return Ok(Err(Restart(format!("{status} for the pinned url")))),
        _ => return Err(status_outcome("download", status, &sc.pinned_url)),
    }

    let mut file = tokio::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)
        .await
        .map_err(|e| io_outcome("open data file", &e))?;

    let mut stream = resp.bytes_stream();
    let mut last_reported = have;
    let mut end = DownloadEnd::Budget;
    let mut overrun: Option<Restart> = None;
    while let Some(chunk) = stream.next().await {
        let bytes = chunk.map_err(|e| {
            FetchOutcome::FailedTransient(format!("download {}: {e}", sc.pinned_url))
        })?;
        file.write_all(&bytes)
            .await
            .map_err(|e| io_outcome("write data file", &e))?;
        have += bytes.len() as u64;
        if have > sc.total {
            overrun = Some(Restart(format!(
                "server sent more than the pinned total ({have} > {})",
                sc.total
            )));
            break;
        }
        if have - last_reported >= PROGRESS_STEP {
            last_reported = have;
            ctx.progress(have, sc.total, name);
        }
        if have == sc.total {
            end = DownloadEnd::Complete;
            break;
        }
        if ctx.over_budget() {
            break;
        }
    }
    // Durable before we report anything: the file length is the resume truth.
    file.flush()
        .await
        .map_err(|e| io_outcome("flush data file", &e))?;
    file.sync_all()
        .await
        .map_err(|e| io_outcome("fsync data file", &e))?;
    drop(file);
    ctx.progress(have.min(sc.total), sc.total, name);
    if let Some(r) = overrun {
        return Ok(Err(r));
    }
    Ok(Ok(end))
}

async fn run_slice(ctx: &mut Ctx<'_>) -> SliceEnd {
    if let Err(e) = std::fs::create_dir_all(ctx.dest_dir) {
        return SliceEnd::Done(io_outcome("create dest dir", &e));
    }
    let mut sc = match load_sidecar(ctx.dest_dir, &ctx.source.id) {
        Ok(Some(sc)) => sc,
        Ok(None) => Sidecar::new(&ctx.source.id),
        Err(e) => return SliceEnd::Done(FetchOutcome::FailedFatal(e.to_string())),
    };

    loop {
        if !sc.is_resolved() {
            if let Err(o) = resolve(ctx, &mut sc).await {
                return SliceEnd::Done(o);
            }
        }
        let path = match data_path(ctx.dest_dir, &sc, ctx.source.kind) {
            Ok(p) => p,
            Err(e) => return SliceEnd::Done(FetchOutcome::FailedFatal(e.to_string())),
        };
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("?")
            .to_string();
        let have = match file_len(&path) {
            Ok(n) => n,
            Err(e) => return SliceEnd::Done(FetchOutcome::FailedFatal(e.to_string())),
        };

        if have > sc.total {
            match restart(
                ctx,
                &mut sc,
                Restart("local file longer than the pinned entity".into()),
            ) {
                Ok(()) => continue,
                Err(o) => return SliceEnd::Done(o),
            }
        }
        if have == sc.total {
            if sc.verified {
                return SliceEnd::Done(FetchOutcome::Finished {
                    path,
                    bytes: sc.total,
                });
            }
            return SliceEnd::Verify { sc, path };
        }
        // No budget check here on purpose: minimum forward progress is one
        // data chunk per slice (the engine's one-block rule), so a slice
        // that spent its budget resolving still lands bytes; `download`
        // returns after the first chunk once the budget is gone. Restart
        // iterations are bounded by MAX_RESTARTS, not by the clock.
        match download(ctx, &sc, &path, &name, have).await {
            Ok(Ok(DownloadEnd::Complete)) => return SliceEnd::Verify { sc, path },
            Ok(Ok(DownloadEnd::Budget)) => {
                let have = match file_len(&path) {
                    Ok(n) => n,
                    Err(e) => return SliceEnd::Done(FetchOutcome::FailedFatal(e.to_string())),
                };
                return SliceEnd::Done(FetchOutcome::Yielded {
                    path,
                    have,
                    total: sc.total,
                });
            }
            Ok(Err(why)) => match restart(ctx, &mut sc, why) {
                Ok(()) => continue,
                Err(o) => return SliceEnd::Done(o),
            },
            Err(o) => return SliceEnd::Done(o),
        }
    }
}

/// Magic bytes, then a streaming MD5 against the pinned checksum. May
/// overrun the slice budget by the hash time (~1–2 s per 400 MB): the
/// verification is one indivisible unit of work.
fn verify(ctx: &mut Ctx<'_>, mut sc: Sidecar, path: PathBuf) -> FetchOutcome {
    let mut f = match std::fs::File::open(&path) {
        Ok(f) => f,
        Err(e) => return io_outcome("open for verify", &e),
    };
    let mut head = vec![0u8; MIN_VALIDATION_BYTES];
    let mut filled = 0;
    while filled < head.len() {
        match f.read(&mut head[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) => return io_outcome("read head", &e),
        }
    }
    head.truncate(filled);
    if let Err(e) = ctx.source.kind.validate(&head) {
        return FetchOutcome::FailedFatal(format!(
            "{}: {e} (kept for inspection; purge_fetch to retry)",
            path.display()
        ));
    }

    if !sc.expected_md5.is_empty() {
        let mut f = match std::fs::File::open(&path) {
            Ok(f) => f,
            Err(e) => return io_outcome("open for md5", &e),
        };
        let mut hasher = Md5::new();
        let mut buf = vec![0u8; 1 << 20];
        loop {
            match f.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => hasher.update(&buf[..n]),
                Err(e) => return io_outcome("read for md5", &e),
            }
        }
        let got = to_hex(&hasher.finalize());
        if got != sc.expected_md5 {
            return FetchOutcome::FailedFatal(format!(
                "md5 mismatch for {}: expected {}, got {got} (kept for inspection; purge_fetch to retry)",
                path.display(),
                sc.expected_md5
            ));
        }
    }

    sc.verified = true;
    if let Err(e) = save_sidecar(ctx.dest_dir, &sc) {
        return FetchOutcome::FailedFatal(e.to_string());
    }
    ctx.note(format!(
        "fetch: verified {} ({} bytes)",
        path.display(),
        sc.total
    ));
    FetchOutcome::Finished {
        path,
        bytes: sc.total,
    }
}

/// Runs one budget-bounded fetch slice for `source` into `dest_dir`. See the
/// module docs for the Finished / Yielded / FailedFatal / FailedTransient
/// contract. Builds a private current-thread runtime for the duration of
/// the call — the FFI boundary stays synchronous.
pub fn fetch_slice_blocking(
    source: &Source,
    dest_dir: &Path,
    budget: Duration,
    on_progress: &mut dyn FnMut(f32, String),
) -> FetchOutcome {
    let client = match reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(READ_TIMEOUT)
        .build()
    {
        Ok(c) => c,
        Err(e) => return FetchOutcome::FailedFatal(format!("http client: {e}")),
    };
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => return FetchOutcome::FailedFatal(format!("runtime: {e}")),
    };
    let mut ctx = Ctx {
        client,
        source,
        dest_dir,
        budget,
        started: Instant::now(),
        on_progress,
    };
    let end = rt.block_on(run_slice(&mut ctx));
    // Tear the runtime (and any half-read connection) down before the
    // synchronous verification, so no socket lingers across the hash.
    drop(rt);
    match end {
        SliceEnd::Done(o) => o,
        SliceEnd::Verify { sc, path } => verify(&mut ctx, sc, path),
    }
}

// ---------------------------------------------------------------------------
// Tests (pure; behavioural proofs live in tests/contract.rs + tests/loopback.rs)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // Real OSM PBF header prefix, taken byte-for-byte from
    // offline_sandbox/raw_data/innsbruck.osm.pbf:
    //   00 00 00 0d  0a 09  "OSMHeader" ...
    const PBF_HEAD: &[u8] = &[
        0x00, 0x00, 0x00, 0x0d, 0x0a, 0x09, b'O', b'S', b'M', b'H', b'e', b'a', b'd', b'e', b'r',
        0x18, 0x4b, 0x10, 0x3f, 0x1a,
    ];

    #[test]
    fn pbf_osmheader_accepted() {
        assert!(PayloadKind::OsmPbf.validate(PBF_HEAD).is_ok());
    }

    #[test]
    fn pbf_html_redirect_rejected() {
        // The exact poisoning we hit in practice: an HTML page saved as .pbf.
        let html = b"<!DOCTYPE html>\n<html><head><title>Geofabrik</title>";
        match PayloadKind::OsmPbf.validate(html) {
            Err(FetchError::InvalidPayload(msg)) => {
                assert!(
                    msg.contains("HTML") || msg.contains("BlobHeader"),
                    "got: {msg}"
                );
            }
            other => panic!("expected InvalidPayload, got {other:?}"),
        }
    }

    #[test]
    fn pbf_wrong_blobtype_rejected() {
        // Valid framing but the first blob is OSMData, not OSMHeader.
        let mut buf = vec![0x00, 0x00, 0x00, 0x0d, 0x0a, 0x07];
        buf.extend_from_slice(b"OSMData");
        assert!(matches!(
            PayloadKind::OsmPbf.validate(&buf),
            Err(FetchError::InvalidPayload(_))
        ));
    }

    #[test]
    fn tiff_little_endian_accepted() {
        // From offline_sandbox/raw_data/innsbruck_dem.tif: 49 49 2a 00 ...
        let head = [0x49, 0x49, 0x2a, 0x00, 0xc0, 0x00, 0x00, 0x00];
        assert!(PayloadKind::Tiff.validate(&head).is_ok());
    }

    #[test]
    fn tiff_big_endian_accepted() {
        let head = [0x4d, 0x4d, 0x00, 0x2a, 0x00, 0x00, 0x00, 0x08];
        assert!(PayloadKind::Tiff.validate(&head).is_ok());
    }

    #[test]
    fn tiff_garbage_rejected() {
        assert!(matches!(
            PayloadKind::Tiff.validate(b"<!DOCTYPE html>"),
            Err(FetchError::InvalidPayload(_))
        ));
    }

    #[test]
    fn empty_payload_rejected() {
        assert!(PayloadKind::OsmPbf.validate(&[]).is_err());
        assert!(PayloadKind::Tiff.validate(&[]).is_err());
    }

    #[test]
    fn truncated_header_rejected() {
        assert!(PayloadKind::OsmPbf.validate(&[0x00, 0x00, 0x00]).is_err());
        assert!(PayloadKind::Tiff.validate(&[0x49, 0x49]).is_err());
    }

    #[test]
    fn kind_inference_from_filename() {
        assert_eq!(
            kind_for_path(Path::new("a/innsbruck.osm.pbf")),
            Some(PayloadKind::OsmPbf)
        );
        assert_eq!(
            kind_for_path(Path::new("b/dem.tif")),
            Some(PayloadKind::Tiff)
        );
        assert_eq!(kind_for_path(Path::new("c/notes.txt")), None);
    }

    #[test]
    fn content_range_parses() {
        assert_eq!(
            parse_content_range("bytes 0-31/422526281"),
            Some((0, 31, 422_526_281))
        );
        assert_eq!(
            parse_content_range("bytes 100000000-100000015/422526281"),
            Some((100_000_000, 100_000_015, 422_526_281))
        );
        assert_eq!(parse_content_range("bytes */422526281"), None);
        assert_eq!(parse_content_range("items 0-1/2"), None);
        assert_eq!(parse_content_range(""), None);
    }

    #[test]
    fn hex_encoding_is_lowercase_two_digit() {
        assert_eq!(to_hex(&[0x00, 0x8b, 0xff]), "008bff");
    }
}
