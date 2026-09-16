// SPDX-License-Identifier: Apache-2.0
//! The fetch sidecar (P-SOV.C2a, D3): `<dest_dir>/<source_id>.fetch`, the
//! durable identity of an in-progress or finished download.
//!
//! Same discipline as the engine's checkpoint: `key=value` text, a leading
//! version that is refused loudly on mismatch (never guessed at), and an
//! atomic tmp → fsync → rename → parent-dir fsync write. The sidecar never
//! runs ahead of the data: it records *which entity* is being downloaded
//! (pinned URL, ETag, total, expected MD5) and whether it has been verified;
//! the data file's own length is the resume cursor.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::FetchError;

/// Bump on any field/semantics change; old sidecars are refused, not read.
pub const SIDECAR_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sidecar {
    pub source_id: String,
    /// Final URL after redirect resolution. Empty = unresolved (fresh, or
    /// after a restart); every other pin field is then meaningless.
    pub pinned_url: String,
    /// Strong ETag exactly as served (quotes included), sent back verbatim
    /// as `If-Range`. Empty when the mirror served none or a weak one.
    pub etag: String,
    pub total: u64,
    /// Lower-case hex from the mirror's `.md5`; empty if none was published.
    pub expected_md5: String,
    pub verified: bool,
    /// Restart-clean events for this source (entity changed, Range ignored,
    /// pinned file gone). Survives restarts by design — it is the loop
    /// breaker.
    pub restarts: u32,
}

impl Sidecar {
    /// A fresh, unresolved sidecar.
    pub fn new(source_id: &str) -> Self {
        Sidecar {
            source_id: source_id.to_string(),
            pinned_url: String::new(),
            etag: String::new(),
            total: 0,
            expected_md5: String::new(),
            verified: false,
            restarts: 0,
        }
    }

    pub fn is_resolved(&self) -> bool {
        !self.pinned_url.is_empty()
    }

    /// Forgets the pinned entity (keeps `source_id` and `restarts`).
    pub fn unresolve(&mut self) {
        self.pinned_url.clear();
        self.etag.clear();
        self.total = 0;
        self.expected_md5.clear();
        self.verified = false;
    }

    pub fn encode(&self) -> String {
        // Header values cannot legally contain line breaks; strip defensively
        // so a hostile ETag can never smuggle a second key.
        let clean = |s: &str| s.replace(['\r', '\n'], "");
        format!(
            "version={SIDECAR_VERSION}\nsource_id={}\npinned_url={}\netag={}\ntotal={}\nexpected_md5={}\nverified={}\nrestarts={}\n",
            clean(&self.source_id),
            clean(&self.pinned_url),
            clean(&self.etag),
            self.total,
            clean(&self.expected_md5),
            u8::from(self.verified),
            self.restarts,
        )
    }

    pub fn decode(text: &str) -> Result<Sidecar, FetchError> {
        let mut version = None;
        let mut source_id = None;
        let mut pinned_url = None;
        let mut etag = None;
        let mut total = None;
        let mut expected_md5 = None;
        let mut verified = None;
        let mut restarts = None;

        let bad = |what: &str| FetchError::State(format!("corrupted fetch sidecar: {what}"));

        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else {
                return Err(bad(&format!("malformed line {line:?}")));
            };
            match k {
                "version" => version = Some(v.parse::<u32>().map_err(|_| bad("bad version"))?),
                "source_id" => source_id = Some(v.to_string()),
                "pinned_url" => pinned_url = Some(v.to_string()),
                "etag" => etag = Some(v.to_string()),
                "total" => total = Some(v.parse::<u64>().map_err(|_| bad("bad total"))?),
                "expected_md5" => expected_md5 = Some(v.to_string()),
                "verified" => {
                    verified = Some(match v {
                        "0" => false,
                        "1" => true,
                        _ => return Err(bad("bad verified flag")),
                    })
                }
                "restarts" => restarts = Some(v.parse::<u32>().map_err(|_| bad("bad restarts"))?),
                // Unknown keys within the same version are tolerated.
                _ => {}
            }
        }

        match version {
            Some(SIDECAR_VERSION) => {}
            Some(v) => {
                return Err(bad(&format!(
                "unsupported version {v} (this build reads {SIDECAR_VERSION}); purge and refetch"
            )))
            }
            None => return Err(bad("missing version")),
        }

        match (
            source_id,
            pinned_url,
            etag,
            total,
            expected_md5,
            verified,
            restarts,
        ) {
            (Some(s), Some(p), Some(e), Some(t), Some(m), Some(v), Some(r)) => Ok(Sidecar {
                source_id: s,
                pinned_url: p,
                etag: e,
                total: t,
                expected_md5: m,
                verified: v,
                restarts: r,
            }),
            _ => Err(bad("missing required fields")),
        }
    }
}

pub fn sidecar_path(dest_dir: &Path, source_id: &str) -> PathBuf {
    dest_dir.join(format!("{source_id}.fetch"))
}

/// `Ok(None)` when no sidecar exists; `Err(State)` when one exists but is
/// unreadable, foreign (other source id) or of another version.
pub fn load_sidecar(dest_dir: &Path, source_id: &str) -> Result<Option<Sidecar>, FetchError> {
    let path = sidecar_path(dest_dir, source_id);
    let text = match fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(FetchError::Io(format!("read {}: {e}", path.display()))),
    };
    let sc = Sidecar::decode(&text)?;
    if sc.source_id != source_id {
        return Err(FetchError::State(format!(
            "corrupted fetch sidecar: source_id mismatch (file='{}', requested='{source_id}')",
            sc.source_id
        )));
    }
    Ok(Some(sc))
}

/// Atomic, durable write: tmp → fsync → rename → parent-dir fsync.
pub fn save_sidecar(dest_dir: &Path, sc: &Sidecar) -> Result<(), FetchError> {
    let final_path = sidecar_path(dest_dir, &sc.source_id);
    let tmp_path = final_path.with_extension("fetch.tmp");
    let io = |what: &str, e: std::io::Error| FetchError::Io(format!("sidecar {what}: {e}"));

    let mut f = fs::File::create(&tmp_path).map_err(|e| io("create", e))?;
    f.write_all(sc.encode().as_bytes())
        .map_err(|e| io("write", e))?;
    f.sync_all().map_err(|e| io("fsync", e))?;
    drop(f);
    fs::rename(&tmp_path, &final_path).map_err(|e| io("rename", e))?;
    fsync_dir(dest_dir)
}

/// fsync the directory so the rename itself survives power loss (see the
/// engine's `fsync_dir` for the rationale). No-op on non-Unix dev hosts.
pub(crate) fn fsync_dir(dir: &Path) -> Result<(), FetchError> {
    #[cfg(unix)]
    {
        let d = fs::File::open(dir).map_err(|e| {
            FetchError::Io(format!("directory open for fsync ({}): {e}", dir.display()))
        })?;
        d.sync_all()
            .map_err(|e| FetchError::Io(format!("directory fsync ({}): {e}", dir.display())))
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        Ok(())
    }
}
