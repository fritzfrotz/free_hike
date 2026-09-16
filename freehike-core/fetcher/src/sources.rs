// SPDX-License-Identifier: Apache-2.0
//! The source table (P-SOV.C2a, D7): every raw-extract origin the app can
//! fetch, named by a stable id. The WebView never names a host — it passes
//! an id across the bridge and this table (Rust, native binary) owns the
//! URL. That keeps ARCHITECTURE.md P10a (`no https?:// under src/`) intact
//! and closes the door on a compromised WebView steering the fetcher at an
//! arbitrary origin.
//!
//! Every entry is https. Plain http is accepted by the engine only for the
//! loopback test server (see [`crate::url_allowed`]).

use crate::PayloadKind;

/// Where the published MD5 for a source lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Md5Spec {
    /// `<pinned URL>.md5`, whose filename field must equal the pinned
    /// basename. Geofabrik publishes one beside every dated file.
    Derived,
    /// A fixed URL whose filename field must equal `filename` — for mirrors
    /// whose `-latest` symlink and `.md5` carry different names (osm.fr:
    /// `lisbon-latest.osm.pbf` vs `lisbon.osm.pbf.md5`).
    Explicit { url: String, filename: String },
}

/// One fetchable origin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    /// Stable id the foreign layer uses (`[a-z0-9-]`).
    pub id: String,
    /// Human-readable label for the UI.
    pub label: String,
    /// Entry URL. May redirect (Geofabrik `-latest` → dated file); the engine
    /// resolves it once and pins the final URL.
    pub url: String,
    pub kind: PayloadKind,
    pub md5: Md5Spec,
}

/// (id, label, url, kind, explicit md5 (url, filename)).
type Row = (
    &'static str,
    &'static str,
    &'static str,
    PayloadKind,
    Option<(&'static str, &'static str)>,
);

const TABLE: &[Row] = &[
    (
        "geofabrik-portugal",
        "Portugal (Geofabrik daily extract, ~403 MiB)",
        "https://download.geofabrik.de/europe/portugal-latest.osm.pbf",
        PayloadKind::OsmPbf,
        None,
    ),
    (
        "osmfr-lisbon",
        "Lisbon district (openstreetmap.fr, ~53 MiB)",
        "https://download.openstreetmap.fr/extracts/europe/portugal/lisbon-latest.osm.pbf",
        PayloadKind::OsmPbf,
        Some((
            "https://download.openstreetmap.fr/extracts/europe/portugal/lisbon.osm.pbf.md5",
            "lisbon.osm.pbf",
        )),
    ),
];

fn row_to_source(row: &Row) -> Source {
    let (id, label, url, kind, md5) = row;
    Source {
        id: (*id).to_string(),
        label: (*label).to_string(),
        url: (*url).to_string(),
        kind: *kind,
        md5: match md5 {
            None => Md5Spec::Derived,
            Some((u, f)) => Md5Spec::Explicit {
                url: (*u).to_string(),
                filename: (*f).to_string(),
            },
        },
    }
}

/// The full table, in declaration order.
pub fn sources() -> Vec<Source> {
    TABLE.iter().map(row_to_source).collect()
}

/// Looks a source up by id.
pub fn find_source(id: &str) -> Option<Source> {
    TABLE.iter().find(|r| r.0 == id).map(row_to_source)
}
