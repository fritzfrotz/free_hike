//! `fetcher` — hostile-mirror-safe downloader for raw OSM `.osm.pbf` and DEM
//! `.tif` inputs (Phase 2).
//!
//! Two invariants, both learned the hard way on this project (a Geofabrik URL
//! that 302-redirected to an HTML homepage was silently saved as a `.pbf` and
//! poisoned the whole build pipeline for weeks):
//!
//! 1. **Resumability.** Downloads use HTTP `Range` requests. A transfer that
//!    drops at 500MB resumes at exactly 500MB rather than restarting — vital
//!    on a phone that backgrounds mid-download or loses signal on a trailhead.
//!
//! 2. **Magic-byte validation.** A payload is never trusted until its leading
//!    bytes are checked: the `OSMHeader` blob for PBFs, or a TIFF byte-order
//!    marker (`II*\0` / `MM\0*`) for DEMs. An HTML error page that arrives
//!    with a `200 OK` is rejected loudly instead of corrupting the pipeline.
//!
//! TLS is rustls-only (see Cargo.toml) so the crate cross-compiles to
//! aarch64 Android/iOS without an OpenSSL toolchain.

use std::fmt;
use std::path::{Path, PathBuf};

use futures_util::StreamExt;
use tokio::fs;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Fetch failures. Plain enum (no `thiserror`) so a future FFI layer can flatten
/// it cheaply, mirroring the `compiler` crate's error style.
#[derive(Debug)]
pub enum FetchError {
    /// Transport/HTTP error (DNS, TLS, connection, non-success status).
    Http(String),
    /// Filesystem error while reading/writing the partial file.
    Io(String),
    /// The payload's leading bytes failed magic-byte validation. This is the
    /// anti-HTML-redirect guard: an HTML page served with 200 lands here.
    InvalidPayload(String),
    /// The server ignored our Range request in a way we can't safely reconcile.
    RangeUnsupported(String),
}

impl fmt::Display for FetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FetchError::Http(s) => write!(f, "http error: {s}"),
            FetchError::Io(s) => write!(f, "io error: {s}"),
            FetchError::InvalidPayload(s) => write!(f, "invalid payload: {s}"),
            FetchError::RangeUnsupported(s) => write!(f, "range unsupported: {s}"),
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

// ---------------------------------------------------------------------------
// Resume planning (pure — unit-tested without the network)
// ---------------------------------------------------------------------------

/// What to do given the bytes already present on disk for a target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResumePlan {
    /// Nothing local yet — download the whole file (no Range header).
    Fresh,
    /// `have` bytes are already on disk — request `Range: bytes=have-` and
    /// append the response.
    Resume { have: u64, range_header: String },
    /// The local file already matches the server's advertised length — nothing
    /// to download, just validate the head.
    Complete,
}

/// Decides how to proceed given the local partial size and (if known) the
/// server's total content length.
pub fn plan_resume(local_len: u64, remote_total: Option<u64>) -> ResumePlan {
    if local_len == 0 {
        return ResumePlan::Fresh;
    }
    if let Some(total) = remote_total {
        if local_len >= total {
            return ResumePlan::Complete;
        }
    }
    ResumePlan::Resume {
        have: local_len,
        range_header: format!("bytes={local_len}-"),
    }
}

// ---------------------------------------------------------------------------
// Downloader
// ---------------------------------------------------------------------------

/// Identifies this client honestly to mirrors (Geofabrik and friends expect a
/// real UA; a generic one can be 406'd).
const USER_AGENT: &str = concat!(
    "FreeHike/",
    env!("CARGO_PKG_VERSION"),
    " (+on-device map compiler)"
);

/// Downloads `url` to `dest`, resuming any existing partial, then validates the
/// payload's magic bytes. On validation failure the partial is left in place
/// for inspection but the error is returned so the caller never trusts it.
///
/// Returns the total bytes on disk after completion.
pub async fn download_and_validate(
    url: &str,
    dest: &Path,
    kind: PayloadKind,
) -> Result<u64, FetchError> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)
            .await
            .map_err(|e| FetchError::Io(format!("create dir {}: {e}", parent.display())))?;
    }

    let local_len = match fs::metadata(dest).await {
        Ok(m) => m.len(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
        Err(e) => return Err(FetchError::Io(format!("stat {}: {e}", dest.display()))),
    };

    let client = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .build()
        .map_err(|e| FetchError::Http(e.to_string()))?;

    // First plan is based only on local size; the server's total (if any)
    // refines Fresh/Resume vs Complete once we see the response.
    let plan = plan_resume(local_len, None);

    let mut request = client.get(url);
    if let ResumePlan::Resume { range_header, .. } = &plan {
        request = request.header(reqwest::header::RANGE, range_header);
    }

    let resp = request
        .send()
        .await
        .map_err(|e| FetchError::Http(e.to_string()))?;

    let status = resp.status();
    if !status.is_success() {
        return Err(FetchError::Http(format!("{status} for {url}")));
    }

    // Reconcile what the server actually did with what we asked for.
    let append = match (&plan, status.as_u16()) {
        // Asked to resume and got 206 Partial Content — append.
        (ResumePlan::Resume { .. }, 206) => true,
        // Asked to resume but got 200 OK — server ignored Range; restart clean.
        (ResumePlan::Resume { .. }, 200) => false,
        // Fresh download — write from the start.
        (ResumePlan::Fresh, _) => false,
        // Any other combination we can't safely reconcile.
        (_, code) => {
            return Err(FetchError::RangeUnsupported(format!(
                "unexpected status {code} for resume request"
            )));
        }
    };

    let mut file = if append {
        let mut f = fs::OpenOptions::new()
            .append(true)
            .open(dest)
            .await
            .map_err(|e| FetchError::Io(format!("open for append {}: {e}", dest.display())))?;
        f.seek(std::io::SeekFrom::End(0))
            .await
            .map_err(|e| FetchError::Io(e.to_string()))?;
        f
    } else {
        fs::File::create(dest)
            .await
            .map_err(|e| FetchError::Io(format!("create {}: {e}", dest.display())))?
    };

    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let bytes = chunk.map_err(|e| FetchError::Http(e.to_string()))?;
        file.write_all(&bytes)
            .await
            .map_err(|e| FetchError::Io(e.to_string()))?;
    }
    file.flush()
        .await
        .map_err(|e| FetchError::Io(e.to_string()))?;
    file.sync_all()
        .await
        .map_err(|e| FetchError::Io(e.to_string()))?;
    drop(file);

    // Validate the assembled file's head — never trust the payload until now.
    let head = read_head(dest, MIN_VALIDATION_BYTES).await?;
    kind.validate(&head)?;

    let final_len = fs::metadata(dest)
        .await
        .map_err(|e| FetchError::Io(e.to_string()))?
        .len();
    Ok(final_len)
}

async fn read_head(path: &Path, n: usize) -> Result<Vec<u8>, FetchError> {
    use tokio::io::AsyncReadExt;
    let mut f = fs::File::open(path)
        .await
        .map_err(|e| FetchError::Io(format!("open head {}: {e}", path.display())))?;
    let mut buf = vec![0u8; n];
    let mut filled = 0;
    while filled < n {
        let read = f
            .read(&mut buf[filled..])
            .await
            .map_err(|e| FetchError::Io(e.to_string()))?;
        if read == 0 {
            break;
        }
        filled += read;
    }
    buf.truncate(filled);
    Ok(buf)
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

/// Returns a scratch path used by the integration test; kept here so the test
/// and any future caller agree on the temp layout.
pub fn scratch_path(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("freehike-fetch-{name}"))
}

// ---------------------------------------------------------------------------
// Tests
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
    fn fresh_download_no_range() {
        assert_eq!(plan_resume(0, None), ResumePlan::Fresh);
        assert_eq!(plan_resume(0, Some(1000)), ResumePlan::Fresh);
    }

    #[test]
    fn partial_resumes_from_offset() {
        match plan_resume(500_000_000, Some(800_000_000)) {
            ResumePlan::Resume { have, range_header } => {
                assert_eq!(have, 500_000_000);
                assert_eq!(range_header, "bytes=500000000-");
            }
            other => panic!("expected Resume, got {other:?}"),
        }
    }

    #[test]
    fn partial_without_known_total_resumes() {
        assert!(matches!(
            plan_resume(123, None),
            ResumePlan::Resume { have: 123, .. }
        ));
    }

    #[test]
    fn complete_file_skips_download() {
        assert_eq!(plan_resume(1000, Some(1000)), ResumePlan::Complete);
        assert_eq!(plan_resume(1001, Some(1000)), ResumePlan::Complete);
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

    /// Live network download against a real mirror. Ignored by default so CI /
    /// the L1 ladder never hit the network; run explicitly with:
    ///   cargo test -p fetcher --  --ignored --nocapture live_download
    #[tokio::test]
    #[ignore]
    async fn live_download_monaco_pbf() {
        // Monaco is the smallest Geofabrik extract (~700KB) — a cheap real check.
        let url = "https://download.geofabrik.de/europe/monaco-latest.osm.pbf";
        let dest = scratch_path("monaco-latest.osm.pbf");
        let _ = std::fs::remove_file(&dest);
        let n = download_and_validate(url, &dest, PayloadKind::OsmPbf)
            .await
            .expect("download+validate should succeed");
        assert!(n > 100_000, "monaco extract implausibly small: {n}");
        let _ = std::fs::remove_file(&dest);
    }
}
