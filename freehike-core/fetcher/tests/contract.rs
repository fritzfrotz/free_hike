// SPDX-License-Identifier: Apache-2.0
//! P-SOV.C2a contract proofs for the fetcher's pure pieces — no network, no
//! sockets; the only I/O is a scratch directory for the sidecar round-trip.

use std::path::{Path, PathBuf};

use fetcher::sidecar::{load_sidecar, save_sidecar, sidecar_path, Sidecar, SIDECAR_VERSION};
use fetcher::sources::{find_source, sources, Md5Spec};
use fetcher::{
    parse_md5_line, pinned_basename, resume_headers, url_allowed, FetchError, PayloadKind,
};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "freehike-fetcher-contract-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

// ---------------------------------------------------------------------------
// Source table (D7)
// ---------------------------------------------------------------------------

#[test]
fn sources_are_https_only() {
    let table = sources();
    assert!(table.len() >= 2, "table must carry both demo sources");
    let mut ids: Vec<&str> = table.iter().map(|s| s.id.as_str()).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), table.len(), "source ids must be unique");
    for s in &table {
        assert!(s.url.starts_with("https://"), "{}: url must be https", s.id);
        if let Md5Spec::Explicit { url, filename } = &s.md5 {
            assert!(
                url.starts_with("https://"),
                "{}: md5 url must be https",
                s.id
            );
            assert!(!filename.is_empty());
        }
        assert!(!s.label.is_empty());
    }
}

#[test]
fn find_source_by_id() {
    let pt = find_source("geofabrik-portugal").expect("portugal source");
    assert_eq!(pt.kind, PayloadKind::OsmPbf);
    assert!(pt.url.ends_with("/europe/portugal-latest.osm.pbf"));
    assert!(matches!(pt.md5, Md5Spec::Derived));

    let lx = find_source("osmfr-lisbon").expect("lisbon source");
    assert_eq!(lx.kind, PayloadKind::OsmPbf);
    assert!(matches!(&lx.md5, Md5Spec::Explicit { filename, .. } if filename == "lisbon.osm.pbf"));

    assert!(find_source("nope").is_none());
    assert!(find_source("").is_none());
}

#[test]
fn url_policy_https_or_loopback() {
    assert!(url_allowed(
        "https://download.geofabrik.de/europe/x.osm.pbf"
    ));
    assert!(!url_allowed(
        "http://download.geofabrik.de/europe/x.osm.pbf"
    ));
    assert!(!url_allowed("ftp://download.geofabrik.de/x"));
    assert!(!url_allowed("file:///etc/passwd"));
    assert!(!url_allowed("not a url"));
    // The in-process test server is the one plain-http exception.
    assert!(url_allowed("http://127.0.0.1:8080/x.osm.pbf"));
    assert!(url_allowed("http://localhost:1/x.osm.pbf"));
    assert!(url_allowed("http://[::1]:9/x.osm.pbf"));
    assert!(!url_allowed("http://127.0.0.1.evil.example/x.osm.pbf"));
}

// ---------------------------------------------------------------------------
// Sidecar (D3 on-disk format)
// ---------------------------------------------------------------------------

fn sample() -> Sidecar {
    Sidecar {
        source_id: "geofabrik-portugal".into(),
        pinned_url: "https://download.geofabrik.de/europe/portugal-260910.osm.pbf".into(),
        etag: "\"192f3d49-65b29852ca094\"".into(),
        total: 422_526_281,
        expected_md5: "8b00397cb329e78755c05b62aa84ac76".into(),
        verified: false,
        restarts: 2,
    }
}

#[test]
fn sidecar_roundtrip() {
    let s = sample();
    let text = s.encode();
    assert!(text.starts_with(&format!("version={SIDECAR_VERSION}\n")));
    assert_eq!(Sidecar::decode(&text).unwrap(), s);

    let dir = scratch("roundtrip");
    save_sidecar(&dir, &s).unwrap();
    assert!(sidecar_path(&dir, &s.source_id).is_file());
    assert_eq!(load_sidecar(&dir, &s.source_id).unwrap(), Some(s.clone()));
    assert_eq!(load_sidecar(&dir, "other").unwrap(), None);

    // Unresolved shape (after a restart): empty pin fields are legal.
    let unresolved = Sidecar {
        pinned_url: String::new(),
        etag: String::new(),
        total: 0,
        expected_md5: String::new(),
        ..s
    };
    assert_eq!(Sidecar::decode(&unresolved.encode()).unwrap(), unresolved);
}

#[test]
fn sidecar_version_mismatch_refused() {
    let text = sample()
        .encode()
        .replacen(&format!("version={SIDECAR_VERSION}"), "version=999", 1);
    match Sidecar::decode(&text) {
        Err(FetchError::State(msg)) => assert!(msg.contains("version"), "got: {msg}"),
        other => panic!("expected State error, got {other:?}"),
    }
}

#[test]
fn sidecar_missing_field_refused() {
    let text: String = sample()
        .encode()
        .lines()
        .filter(|l| !l.starts_with("total="))
        .map(|l| format!("{l}\n"))
        .collect();
    assert!(matches!(Sidecar::decode(&text), Err(FetchError::State(_))));
    assert!(matches!(Sidecar::decode(""), Err(FetchError::State(_))));
    assert!(matches!(
        Sidecar::decode("version=1\ntotal=notanumber\n"),
        Err(FetchError::State(_))
    ));
}

// ---------------------------------------------------------------------------
// MD5 sidecar line (D4)
// ---------------------------------------------------------------------------

#[test]
fn md5_line_parses_strictly() {
    let h = "8b00397cb329e78755c05b62aa84ac76";
    assert_eq!(
        parse_md5_line(
            &format!("{h}  portugal-260910.osm.pbf\n"),
            "portugal-260910.osm.pbf"
        )
        .unwrap(),
        h
    );
    // Binary-mode marker and upper-case hex are accepted; hex is normalized.
    assert_eq!(
        parse_md5_line(
            &format!("{}  *portugal-260910.osm.pbf", h.to_uppercase()),
            "portugal-260910.osm.pbf"
        )
        .unwrap(),
        h
    );
    assert_eq!(
        parse_md5_line(&format!("{h} lisbon.osm.pbf"), "lisbon.osm.pbf").unwrap(),
        h
    );
}

#[test]
fn md5_line_rejects_html_and_wrong_name() {
    let h = "8b00397cb329e78755c05b62aa84ac76";
    for bad in [
        "<!DOCTYPE HTML PUBLIC \"-//IETF//DTD HTML 2.0//EN\">".to_string(),
        format!("{h}  portugal-260911.osm.pbf"),
        format!("{}  portugal-260910.osm.pbf", &h[..31]),
        h.to_string(),
        format!("{h}  portugal-260910.osm.pbf\nextra line"),
        "zz00397cb329e78755c05b62aa84ac76  portugal-260910.osm.pbf".to_string(),
    ] {
        assert!(
            matches!(
                parse_md5_line(&bad, "portugal-260910.osm.pbf"),
                Err(FetchError::InvalidPayload(_))
            ),
            "must reject: {bad:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Pinned basename (the raw file's on-disk name)
// ---------------------------------------------------------------------------

#[test]
fn pinned_basename_rejects_traversal() {
    let ok = |u: &str| pinned_basename(u, PayloadKind::OsmPbf).unwrap();
    assert_eq!(
        ok("https://h/europe/portugal-260910.osm.pbf"),
        "portugal-260910.osm.pbf"
    );
    assert_eq!(
        ok("https://h/europe/portugal-260910.osm.pbf?x=1#frag"),
        "portugal-260910.osm.pbf"
    );
    assert_eq!(
        ok("http://127.0.0.1:1/lisbon-latest.osm.pbf"),
        "lisbon-latest.osm.pbf"
    );
    assert_eq!(
        pinned_basename("https://h/dem/tile.tif", PayloadKind::Tiff).unwrap(),
        "tile.tif"
    );

    for bad in [
        "https://h/europe/",
        "https://h",
        "https://h/x/..%2Fetc.osm.pbf",
        "https://h/x/.hidden.osm.pbf",
        "https://h/x/has space.osm.pbf",
        "https://h/x/notes.txt",
        "https://h/x/tile.tif",
    ] {
        assert!(
            pinned_basename(bad, PayloadKind::OsmPbf).is_err(),
            "must reject: {bad}"
        );
    }
}

// ---------------------------------------------------------------------------
// Resume request headers (D3)
// ---------------------------------------------------------------------------

#[test]
fn resume_headers_carry_range_and_if_range() {
    assert_eq!(
        resume_headers(0, ""),
        vec![("Range".to_string(), "bytes=0-".to_string())]
    );
    assert_eq!(
        resume_headers(0, "\"e1\""),
        vec![("Range".to_string(), "bytes=0-".to_string())]
    );
    assert_eq!(
        resume_headers(500, "\"e1\""),
        vec![
            ("Range".to_string(), "bytes=500-".to_string()),
            ("If-Range".to_string(), "\"e1\"".to_string()),
        ]
    );
    assert_eq!(
        resume_headers(500, ""),
        vec![("Range".to_string(), "bytes=500-".to_string())]
    );
}

#[test]
fn sidecar_path_is_keyed_by_source_id() {
    let p = sidecar_path(Path::new("/tmp/raw"), "geofabrik-portugal");
    assert_eq!(p, Path::new("/tmp/raw/geofabrik-portugal.fetch"));
}

// ---------------------------------------------------------------------------
// D6 Rust belt (P-SOV.C3a): `verified_input`
// ---------------------------------------------------------------------------

fn input_file(dir: &Path, name: &str) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, b"payload bytes").unwrap();
    p
}

#[test]
fn verified_input_detected_by_sidecar() {
    let dir = scratch("belt-ok");
    let pbf = input_file(&dir, "portugal-260101.osm.pbf");
    fetcher::testing::write_verified_sidecar(&dir, "geofabrik-portugal", "portugal-260101.osm.pbf");
    fetcher::verified_input(&pbf).expect("a verified sidecar pinning this file passes the belt");
}

#[test]
fn unverified_sidecar_rejected() {
    let dir = scratch("belt-unverified");
    let pbf = input_file(&dir, "x.osm.pbf");
    fetcher::testing::write_verified_sidecar(&dir, "src", "x.osm.pbf");
    let mut sc = load_sidecar(&dir, "src").unwrap().unwrap();
    sc.verified = false;
    save_sidecar(&dir, &sc).unwrap();
    let err = fetcher::verified_input(&pbf).unwrap_err().to_string();
    assert!(err.contains("not verified"), "got: {err}");
}

#[test]
fn missing_sidecar_rejected() {
    let dir = scratch("belt-missing");
    let pbf = input_file(&dir, "x.osm.pbf");
    let err = fetcher::verified_input(&pbf).unwrap_err().to_string();
    assert!(err.contains("sidecar"), "got: {err}");
}

#[test]
fn sidecar_for_other_basename_ignored() {
    let dir = scratch("belt-other");
    let pbf = input_file(&dir, "x.osm.pbf");
    input_file(&dir, "y.osm.pbf");
    fetcher::testing::write_verified_sidecar(&dir, "src-y", "y.osm.pbf");
    let err = fetcher::verified_input(&pbf).unwrap_err().to_string();
    assert!(err.contains("no fetch sidecar"), "got: {err}");
}

#[test]
fn verified_length_mismatch_rejected() {
    // The file was swapped or truncated after verification: the sidecar's
    // verdict no longer describes the bytes on disk.
    let dir = scratch("belt-len");
    let pbf = input_file(&dir, "x.osm.pbf");
    fetcher::testing::write_verified_sidecar(&dir, "src", "x.osm.pbf");
    std::fs::write(&pbf, b"swapped after verification").unwrap();
    let err = fetcher::verified_input(&pbf).unwrap_err().to_string();
    assert!(err.contains("length"), "got: {err}");
}
