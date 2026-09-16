// SPDX-License-Identifier: Apache-2.0
//! P-SOV.C2a behavioural proofs against an in-process HTTP/1.1 server on
//! 127.0.0.1 (`fetcher::testing`). No external network, no new dependency.
//! "Kill" here is slice-level: state lives only on disk between
//! `fetch_slice_blocking` calls, which is the same model process death
//! exercises; a real SIGKILL cycle is the C2b device smoke.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use fetcher::sources::{Md5Spec, Source};
use fetcher::testing::{md5_hex, range_response, LoopbackServer, Request, Response};
use fetcher::{fetch_slice_blocking, purge_fetch, query_fetch, FetchOutcome, PayloadKind};

/// Byte-for-byte prefix of a real OSM PBF (BlobHeader length 13, then the
/// "OSMHeader" blob type) — enough for the magic-byte gate.
const PBF_HEAD: &[u8] = &[
    0x00, 0x00, 0x00, 0x0d, 0x0a, 0x09, b'O', b'S', b'M', b'H', b'e', b'a', b'd', b'e', b'r', 0x18,
    0x4b, 0x10, 0x3f, 0x1a,
];

fn pbf_body(len: usize, seed: u64) -> Vec<u8> {
    let mut out = PBF_HEAD.to_vec();
    let mut x = seed | 1;
    while out.len() < len {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        out.push((x & 0xff) as u8);
    }
    out.truncate(len);
    out
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "freehike-fetcher-loopback-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// A Geofabrik-shaped mirror: `-latest` 302s to a dated file, the dated file
/// honours Range/If-Range, and `<dated>.md5` is published beside it.
#[derive(Clone)]
struct Mirror {
    body: Vec<u8>,
    etag: String,
    dated: String,
    /// When set, the served md5 line carries this hash instead of the real one.
    md5_override: Option<String>,
    /// Dribble the data body: (chunk bytes, delay per chunk).
    dribble: Option<(usize, Duration)>,
    /// Broken mirror: ignore Range entirely and always answer 200 full.
    ignore_range: bool,
    /// Stale proxy: a resume (`Range: bytes=N-`, N > 0) is answered with
    /// a 206 whose Content-Range starts at N/2 — bytes consistent with the
    /// claim, so only the offset check can see it.
    wrong_offset_206: bool,
}

impl Mirror {
    fn new(body: Vec<u8>, etag: &str, dated: &str) -> Self {
        Mirror {
            body,
            etag: etag.to_string(),
            dated: dated.to_string(),
            md5_override: None,
            dribble: None,
            ignore_range: false,
            wrong_offset_206: false,
        }
    }
}

fn start(mirror: Mirror) -> (LoopbackServer, Arc<Mutex<Mirror>>) {
    let state = Arc::new(Mutex::new(mirror));
    let handler_state = Arc::clone(&state);
    let server = LoopbackServer::start(move |req: &Request| {
        let m = handler_state.lock().unwrap().clone();
        let dated_path = format!("/europe/{}", m.dated);
        let md5_path = format!("{dated_path}.md5");
        if req.path == "/europe/portugal-latest.osm.pbf" {
            Response::redirect(&dated_path)
        } else if req.path == dated_path {
            let mut resp = if m.ignore_range {
                Response::ok(m.body.clone(), "application/octet-stream")
            } else if m.wrong_offset_206 && resume_offset(req).is_some_and(|n| n > 0) {
                // Serve exactly the bytes the client still needs, but from
                // the wrong place: the spliced file ends at the right length,
                // so only the Content-Range check (or the MD5) can see it.
                let n = resume_offset(req).unwrap_or(0);
                let total = m.body.len() as u64;
                let start = n / 2;
                let end = start + (total - n) - 1;
                let mut stale = req.clone();
                for (k, v) in stale.headers.iter_mut() {
                    if k == "range" {
                        *v = format!("bytes={start}-{end}");
                    }
                }
                range_response(&m.body, &stale, &m.etag)
            } else {
                range_response(&m.body, req, &m.etag)
            };
            resp.dribble = m.dribble;
            resp
        } else if req.path == md5_path {
            let hash = m.md5_override.clone().unwrap_or_else(|| md5_hex(&m.body));
            Response::ok(format!("{hash}  {}\n", m.dated).into_bytes(), "text/plain")
        } else {
            Response::not_found()
        }
    });
    (server, state)
}

/// `Range: bytes=N-` → N.
fn resume_offset(req: &Request) -> Option<u64> {
    req.header("range")?
        .strip_prefix("bytes=")?
        .strip_suffix('-')?
        .parse()
        .ok()
}

fn source(server: &LoopbackServer) -> Source {
    Source {
        id: "test-geofabrik".into(),
        label: "Loopback mirror".into(),
        url: server.url("/europe/portugal-latest.osm.pbf"),
        kind: PayloadKind::OsmPbf,
        md5: Md5Spec::Derived,
    }
}

fn run(src: &Source, dir: &Path, budget_ms: u64, log: &mut Vec<(f32, String)>) -> FetchOutcome {
    fetch_slice_blocking(src, dir, Duration::from_millis(budget_ms), &mut |p, s| {
        log.push((p, s))
    })
}

fn run_to_end(
    src: &Source,
    dir: &Path,
    budget_ms: u64,
    max_slices: usize,
    log: &mut Vec<(f32, String)>,
) -> (FetchOutcome, usize) {
    for n in 1..=max_slices {
        match run(src, dir, budget_ms, log) {
            FetchOutcome::Yielded { .. } => continue,
            terminal => return (terminal, n),
        }
    }
    panic!(
        "no terminal outcome after {max_slices} slices; log tail: {:?}",
        log.last()
    );
}

/// Requests for the dated data file that carried a Range header, in order.
fn data_ranges(server: &LoopbackServer, dated: &str) -> Vec<(String, Option<String>)> {
    server
        .requests()
        .iter()
        .filter(|r| r.path == format!("/europe/{dated}"))
        .filter_map(|r| {
            r.header("range")
                .map(|rg| (rg.to_string(), r.header("if-range").map(str::to_string)))
        })
        .collect()
}

#[test]
fn latest_redirect_is_pinned_to_dated_url() {
    let body = pbf_body(200_000, 1);
    let (server, _m) = start(Mirror::new(
        body.clone(),
        "\"e-260910\"",
        "portugal-260910.osm.pbf",
    ));
    let src = source(&server);
    let dir = scratch("pin");

    let (outcome, _) = run_to_end(&src, &dir, 10_000, 5, &mut Vec::new());
    let FetchOutcome::Finished { path, bytes } = outcome else {
        panic!("expected Finished, got {outcome:?}")
    };
    assert_eq!(path, dir.join("portugal-260910.osm.pbf"));
    assert_eq!(bytes, body.len() as u64);
    assert_eq!(std::fs::read(&path).unwrap(), body);

    let reqs = server.requests();
    let latest_hits: Vec<_> = reqs
        .iter()
        .filter(|r| r.path == "/europe/portugal-latest.osm.pbf")
        .collect();
    assert_eq!(
        latest_hits.len(),
        1,
        "-latest must be resolved exactly once"
    );
    assert_eq!(latest_hits[0].header("range"), Some("bytes=0-0"));
    // Every data request went to the pinned dated URL.
    assert!(reqs
        .iter()
        .filter(|r| r.path != "/europe/portugal-latest.osm.pbf")
        .all(|r| r.path.starts_with("/europe/portugal-260910.osm.pbf")));

    let state = query_fetch(&src.id, &dir)
        .unwrap()
        .expect("state after finish");
    assert!(state.verified);
    assert!(state
        .pinned_url
        .ends_with("/europe/portugal-260910.osm.pbf"));
    assert_eq!(state.have, body.len() as u64);
    assert_eq!(state.total, body.len() as u64);
}

#[test]
fn resume_appends_on_206() {
    let body = pbf_body(900_000, 2);
    let mut mirror = Mirror::new(body.clone(), "\"e-1\"", "portugal-260910.osm.pbf");
    mirror.dribble = Some((8_192, Duration::from_millis(5)));
    let (server, state) = start(mirror);
    let src = source(&server);
    let dir = scratch("resume");
    let mut log = Vec::new();

    let first = run(&src, &dir, 60, &mut log);
    let FetchOutcome::Yielded { have, total, .. } = first else {
        panic!("expected Yielded, got {first:?}")
    };
    assert!(have > 0 && have < total, "have={have} total={total}");
    assert_eq!(total, body.len() as u64);

    state.lock().unwrap().dribble = None;
    let second = run(&src, &dir, 10_000, &mut log);
    assert!(
        matches!(second, FetchOutcome::Finished { .. }),
        "got {second:?}"
    );
    assert_eq!(
        std::fs::read(dir.join("portugal-260910.osm.pbf")).unwrap(),
        body
    );

    let ranges = data_ranges(&server, "portugal-260910.osm.pbf");
    // [resolve probe 0-0], [bytes=0-], [bytes=<have>- with If-Range]
    assert_eq!(ranges[0].0, "bytes=0-0");
    assert_eq!(ranges[1].0, "bytes=0-");
    assert_eq!(
        ranges[2],
        (format!("bytes={have}-"), Some("\"e-1\"".to_string()))
    );
    assert_eq!(ranges.len(), 3);
}

#[test]
fn if_range_mismatch_restarts_clean() {
    let old = pbf_body(900_000, 3);
    let mut mirror = Mirror::new(old.clone(), "\"e-old\"", "lisbon-latest.osm.pbf");
    mirror.dribble = Some((8_192, Duration::from_millis(5)));
    let (server, state) = start(mirror);
    let src = source(&server);
    let dir = scratch("ifrange");
    let mut log = Vec::new();

    let first = run(&src, &dir, 60, &mut log);
    assert!(
        matches!(first, FetchOutcome::Yielded { .. }),
        "got {first:?}"
    );

    // osm.fr-style in-place regeneration: same URL, new bytes, new ETag.
    let new = pbf_body(250_000, 4);
    {
        let mut m = state.lock().unwrap();
        m.body = new.clone();
        m.etag = "\"e-new\"".into();
        m.dribble = None;
    }

    let (outcome, _) = run_to_end(&src, &dir, 10_000, 5, &mut log);
    assert!(
        matches!(outcome, FetchOutcome::Finished { .. }),
        "got {outcome:?}"
    );
    assert_eq!(
        std::fs::read(dir.join("lisbon-latest.osm.pbf")).unwrap(),
        new
    );

    let st = query_fetch(&src.id, &dir).unwrap().unwrap();
    assert!(st.verified);
    assert_eq!(st.restarts, 1, "exactly one restart-clean event");
    assert!(
        log.iter().any(|(_, s)| s.contains("restart")),
        "progress must announce the restart; log: {log:?}"
    );
}

#[test]
fn dated_404_re_resolves() {
    let old = pbf_body(900_000, 5);
    let mut mirror = Mirror::new(old, "\"e-0910\"", "portugal-260910.osm.pbf");
    mirror.dribble = Some((8_192, Duration::from_millis(5)));
    let (server, state) = start(mirror);
    let src = source(&server);
    let dir = scratch("rotate");
    let mut log = Vec::new();

    let first = run(&src, &dir, 60, &mut log);
    assert!(
        matches!(first, FetchOutcome::Yielded { .. }),
        "got {first:?}"
    );
    assert!(dir.join("portugal-260910.osm.pbf").is_file());

    // Daily rotation: the pinned dated file disappears; -latest points at a new one.
    let new = pbf_body(220_000, 6);
    {
        let mut m = state.lock().unwrap();
        m.body = new.clone();
        m.etag = "\"e-0911\"".into();
        m.dated = "portugal-260911.osm.pbf".into();
        m.dribble = None;
    }

    let (outcome, _) = run_to_end(&src, &dir, 10_000, 5, &mut log);
    let FetchOutcome::Finished { path, .. } = outcome else {
        panic!("expected Finished, got {outcome:?}")
    };
    assert_eq!(path, dir.join("portugal-260911.osm.pbf"));
    assert_eq!(std::fs::read(&path).unwrap(), new);
    assert!(
        !dir.join("portugal-260910.osm.pbf").exists(),
        "the stale partial must be removed on restart"
    );
    let st = query_fetch(&src.id, &dir).unwrap().unwrap();
    assert_eq!(st.restarts, 1);
    assert!(st.pinned_url.ends_with("portugal-260911.osm.pbf"));
}

#[test]
fn html_200_rejected_by_magic() {
    let html =
        b"<!DOCTYPE HTML PUBLIC \"-//IETF//DTD HTML 2.0//EN\">\n<html><body>Found</body></html>\n"
            .to_vec();
    let mut mirror = Mirror::new(html.clone(), "\"e-html\"", "portugal-260910.osm.pbf");
    // The md5 matches the HTML, so only the magic-byte gate can catch this.
    mirror.md5_override = Some(md5_hex(&html));
    let (server, _s) = start(mirror);
    let src = source(&server);
    let dir = scratch("html");

    let (outcome, _) = run_to_end(&src, &dir, 10_000, 3, &mut Vec::new());
    let FetchOutcome::FailedFatal(reason) = outcome else {
        panic!("expected FailedFatal, got {outcome:?}")
    };
    assert!(
        reason.contains("BlobHeader") || reason.contains("HTML"),
        "reason must name the magic-byte failure: {reason}"
    );
    let st = query_fetch(&src.id, &dir).unwrap().unwrap();
    assert!(!st.verified);
}

#[test]
fn md5_mismatch_fails_fatal_and_keeps_partial() {
    let body = pbf_body(150_000, 7);
    let mut mirror = Mirror::new(body.clone(), "\"e-md5\"", "portugal-260910.osm.pbf");
    mirror.md5_override = Some("00000000000000000000000000000000".into());
    let (server, _s) = start(mirror);
    let src = source(&server);
    let dir = scratch("md5");

    let (outcome, _) = run_to_end(&src, &dir, 10_000, 3, &mut Vec::new());
    let FetchOutcome::FailedFatal(reason) = outcome else {
        panic!("expected FailedFatal, got {outcome:?}")
    };
    assert!(reason.to_lowercase().contains("md5"), "reason: {reason}");

    let path = dir.join("portugal-260910.osm.pbf");
    assert_eq!(
        std::fs::metadata(&path).unwrap().len(),
        body.len() as u64,
        "the full-length file is kept for inspection"
    );
    let st = query_fetch(&src.id, &dir).unwrap().unwrap();
    assert!(!st.verified);
    assert_eq!(st.have, st.total);

    // A second call must not silently succeed.
    let mut log = Vec::new();
    assert!(matches!(
        run(&src, &dir, 10_000, &mut log),
        FetchOutcome::FailedFatal(_)
    ));

    assert!(purge_fetch(&src.id, &dir));
    assert!(!path.exists());
    assert_eq!(query_fetch(&src.id, &dir).unwrap(), None);
    assert!(!purge_fetch(&src.id, &dir), "second purge finds nothing");
}

#[test]
fn slice_yields_within_budget() {
    let body = pbf_body(512_000, 8);
    let mut mirror = Mirror::new(body, "\"e-b\"", "portugal-260910.osm.pbf");
    mirror.dribble = Some((4_096, Duration::from_millis(5)));
    let (server, _s) = start(mirror);
    let src = source(&server);
    let dir = scratch("budget");
    let mut log = Vec::new();

    let t0 = Instant::now();
    let outcome = run(&src, &dir, 80, &mut log);
    let elapsed = t0.elapsed();
    let FetchOutcome::Yielded { have, total, path } = outcome else {
        panic!("expected Yielded, got {outcome:?}")
    };
    assert!(have > 0 && have < total, "have={have} total={total}");
    assert_eq!(
        std::fs::metadata(&path).unwrap().len(),
        have,
        "file length is the resume truth"
    );
    assert!(
        elapsed < Duration::from_millis(600),
        "slice overran its 80 ms budget by too much: {elapsed:?}"
    );
    let last = log.last().expect("progress emitted");
    assert!(last.0 > 0.0 && last.0 < 100.0, "pct={}", last.0);
}

#[test]
fn kill_resume_byte_identical() {
    let body = pbf_body(1_200_000, 9);
    let mut mirror = Mirror::new(body.clone(), "\"e-k\"", "portugal-260910.osm.pbf");
    mirror.dribble = Some((8_192, Duration::from_millis(3)));
    let (server, _s) = start(mirror);
    let src = source(&server);
    let dir = scratch("kill");

    let (outcome, slices) = run_to_end(&src, &dir, 40, 200, &mut Vec::new());
    assert!(
        matches!(outcome, FetchOutcome::Finished { .. }),
        "got {outcome:?}"
    );
    assert!(
        slices >= 3,
        "the test must actually resume; slices={slices}"
    );

    let on_disk = std::fs::read(dir.join("portugal-260910.osm.pbf")).unwrap();
    assert_eq!(md5_hex(&on_disk), md5_hex(&body));
    assert_eq!(on_disk, body);

    // Every resume asked for exactly the bytes after what was on disk:
    // offsets strictly increase and each starts where the previous slice
    // left off (the server's own view — no splice, no re-download).
    let ranges = data_ranges(&server, "portugal-260910.osm.pbf");
    let offsets: Vec<u64> = ranges
        .iter()
        .skip(1) // the resolve probe
        .map(|(r, _)| {
            r.trim_start_matches("bytes=")
                .trim_end_matches('-')
                .parse()
                .unwrap()
        })
        .collect();
    assert_eq!(offsets[0], 0);
    assert!(
        offsets.windows(2).all(|w| w[1] > w[0]),
        "offsets: {offsets:?}"
    );
    assert!(ranges
        .iter()
        .skip(2)
        .all(|(_, ifr)| ifr.as_deref() == Some("\"e-k\"")));
}

/// The squid scenario from the C2 memo: a resume answered with a 206 whose
/// Content-Range does not start where the local file ends. Appending it
/// would splice; the engine must restart clean and rebuild from zero.
#[test]
fn wrong_offset_206_restarts_clean() {
    let body = pbf_body(900_000, 12);
    let mut mirror = Mirror::new(body.clone(), "\"e-squid\"", "portugal-260910.osm.pbf");
    mirror.dribble = Some((8_192, Duration::from_millis(5)));
    let (server, state) = start(mirror);
    let src = source(&server);
    let dir = scratch("squid");
    let mut log = Vec::new();

    let first = run(&src, &dir, 60, &mut log);
    let FetchOutcome::Yielded { have, .. } = first else {
        panic!("expected Yielded, got {first:?}")
    };
    assert!(have > 0);

    {
        let mut m = state.lock().unwrap();
        m.wrong_offset_206 = true;
        m.dribble = None;
    }
    // Wrong-offset mode bites resumes only, so after the restart the fresh
    // `bytes=0-` download completes inside this slice.
    let second = run(&src, &dir, 10_000, &mut log);
    assert!(
        matches!(second, FetchOutcome::Finished { .. }),
        "got {second:?}"
    );
    assert_eq!(
        std::fs::read(dir.join("portugal-260910.osm.pbf")).unwrap(),
        body,
        "no spliced bytes may survive"
    );

    let st = query_fetch(&src.id, &dir).unwrap().unwrap();
    assert!(st.verified);
    assert_eq!(st.restarts, 1, "exactly one restart-clean event");
    assert!(
        log.iter()
            .any(|(_, s)| s.contains("restart #1") && s.contains("Content-Range starts at")),
        "the restart must name the offset mismatch; log: {log:?}"
    );

    // Server's view: the poisoned resume was asked for, and a fresh
    // from-zero request followed it.
    let ranges = data_ranges(&server, "portugal-260910.osm.pbf");
    let poisoned = ranges
        .iter()
        .position(|(r, _)| *r == format!("bytes={have}-"))
        .expect("the resume request must have reached the server");
    assert!(
        ranges[poisoned + 1..].iter().any(|(r, _)| r == "bytes=0-"),
        "a from-zero request must follow the restart; ranges: {ranges:?}"
    );
}

#[test]
fn restart_cap_fails_fatal() {
    let body = pbf_body(400_000, 10);
    let mut mirror = Mirror::new(body, "\"e-broken\"", "portugal-260910.osm.pbf");
    mirror.ignore_range = true;
    mirror.dribble = Some((4_096, Duration::from_millis(5)));
    let (server, _s) = start(mirror);
    let src = source(&server);
    let dir = scratch("cap");

    let (outcome, _) = run_to_end(&src, &dir, 40, 100, &mut Vec::new());
    let FetchOutcome::FailedFatal(reason) = outcome else {
        panic!("expected FailedFatal, got {outcome:?}")
    };
    assert!(reason.contains("restart"), "reason: {reason}");
    let st = query_fetch(&src.id, &dir).unwrap().unwrap();
    assert!(st.restarts > 3);
}

#[test]
fn finished_is_idempotent() {
    let body = pbf_body(100_000, 11);
    let (server, _s) = start(Mirror::new(body, "\"e-i\"", "portugal-260910.osm.pbf"));
    let src = source(&server);
    let dir = scratch("idem");

    let (first, _) = run_to_end(&src, &dir, 10_000, 3, &mut Vec::new());
    assert!(matches!(first, FetchOutcome::Finished { .. }));
    let n = server.requests().len();

    let mut log = Vec::new();
    let again = run(&src, &dir, 10_000, &mut log);
    assert!(
        matches!(again, FetchOutcome::Finished { .. }),
        "got {again:?}"
    );
    assert_eq!(
        server.requests().len(),
        n,
        "a verified file must cost zero requests"
    );
}

#[test]
fn transport_failure_is_transient() {
    // A listener that is closed before the fetch: connection refused.
    let dead = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap()
    };
    let src = Source {
        id: "dead".into(),
        label: "dead".into(),
        url: format!("http://{dead}/europe/portugal-latest.osm.pbf"),
        kind: PayloadKind::OsmPbf,
        md5: Md5Spec::Derived,
    };
    let dir = scratch("dead");
    let mut log = Vec::new();
    let outcome = run(&src, &dir, 1_000, &mut log);
    assert!(
        matches!(outcome, FetchOutcome::FailedTransient(_)),
        "got {outcome:?}"
    );
    assert_eq!(
        query_fetch(&src.id, &dir).unwrap(),
        None,
        "nothing durable before resolve"
    );
}

/// Live probe against the real mirror — opt in only:
///   cargo test -p fetcher --test loopback -- --ignored live_geofabrik_portugal_probe
/// Resolves the redirect, fetches the md5 and the first chunk, then purges.
#[test]
#[ignore]
fn live_geofabrik_portugal_probe() {
    let src = fetcher::sources::find_source("geofabrik-portugal").unwrap();
    let dir = scratch("live");
    let mut log = Vec::new();
    let outcome = run(&src, &dir, 1, &mut log);
    let FetchOutcome::Yielded { have, total, path } = outcome else {
        panic!("expected Yielded after one chunk, got {outcome:?}")
    };
    assert!(have > 0);
    assert!(total > 300_000_000, "Portugal is ~403 MiB; got {total}");
    assert!(path
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("portugal-2"));
    let st = query_fetch(&src.id, &dir).unwrap().unwrap();
    assert_eq!(st.expected_md5.len(), 32);
    assert!(purge_fetch(&src.id, &dir));
}
