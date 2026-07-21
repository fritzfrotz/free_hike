// SPDX-License-Identifier: Apache-2.0
//! Mapbox Vector Tile (MVT) encoding.
//!
//! Wire structs are hand-derived prost messages — the exact subset of
//! `vector_tile.proto` v2.1 this encoder emits, tagged to match the spec.
//! Same no-protoc/no-build.rs pattern as `pbf/src/proto.rs`: the format has
//! been frozen since 2014 and the mobile cross-compile path stays free of
//! host tooling.
//!
//! Geometry: Pass 3 stored full-precision Web Mercator segments clipped to
//! each tile's bounds + the 64/4096 buffer ([`pbf::tile::TILE_BUFFER_RATIO`]).
//! Here they are quantized onto the standard `4096` integer extent relative
//! to the tile's own bounds — buffered vertices legitimately land slightly
//! outside `0..=4096`, which MVT expects (that's what the buffer is for).
//!
//! **Layering (P5.C2):** features are grouped by their persisted layer
//! index into one named MVT layer each (`highway`/`waterway`/`natural`/
//! `landuse`, deterministic index order), and every feature carries a
//! single `class` attribute — its layer tag's value (`highway=path` →
//! layer `"highway"`, `class="path"`), encoded through the layer's
//! `keys`/`values` pools with per-layer value dedup.

use std::collections::{BTreeMap, HashMap};

use pbf::tile::TileFeature;
use prost::Message;

/// The standard MVT tile extent: coordinates are quantized to a
/// 4096×4096 integer grid per tile.
pub const MVT_EXTENT: u32 = 4096;

/// The attribute key every layer carries (per-feature `class` = the layer
/// tag's value). Client styles filter on `["get", "class"]`.
pub const CLASS_KEY: &str = "class";

/// Optional attribute on highway features: the trail difficulty grade
/// (P5.C3). Appended to a layer's key pool only when at least one of its
/// features carries a grade. Client styles color on
/// `["get", "sac_scale"]`.
pub const SAC_SCALE_KEY: &str = "sac_scale";

/// Optional attribute on ANY layer's features: the label text (P5.C4).
/// Appended to a layer's key pool only when at least one of its features
/// is named. Client styles label on `["get", "name"]`.
pub const NAME_KEY: &str = "name";

const GEOM_TYPE_POINT: i32 = 1;
const GEOM_TYPE_LINESTRING: i32 = 2;
const CMD_MOVE_TO: u32 = 1;
const CMD_LINE_TO: u32 = 2;

// ---------------------------------------------------------------------------
// Wire messages (vector_tile.proto subset, hand-tagged)
// ---------------------------------------------------------------------------

#[derive(Clone, PartialEq, Message)]
pub struct Tile {
    #[prost(message, repeated, tag = "3")]
    pub layers: Vec<Layer>,
}

#[derive(Clone, PartialEq, Message)]
pub struct Layer {
    /// Spec: required, MUST be 2 for v2.1 tiles.
    #[prost(uint32, tag = "15")]
    pub version: u32,
    #[prost(string, tag = "1")]
    pub name: String,
    #[prost(message, repeated, tag = "2")]
    pub features: Vec<Feature>,
    #[prost(string, repeated, tag = "3")]
    pub keys: Vec<String>,
    #[prost(message, repeated, tag = "4")]
    pub values: Vec<Value>,
    #[prost(uint32, tag = "5")]
    pub extent: u32,
}

#[derive(Clone, PartialEq, Message)]
pub struct Feature {
    #[prost(uint64, tag = "1")]
    pub id: u64,
    /// Packed key/value index pairs — empty in P5.C1 (no attributes yet).
    #[prost(uint32, repeated, tag = "2")]
    pub tags: Vec<u32>,
    /// `GeomType` enum on the wire; we emit LINESTRING (2) only.
    #[prost(int32, tag = "3")]
    pub geom_type: i32,
    /// Packed command/parameter integers (MoveTo/LineTo + zigzag deltas).
    #[prost(uint32, repeated, tag = "4")]
    pub geometry: Vec<u32>,
}

/// Attribute value variant — declared for wire completeness; unused until
/// tag persistence lands.
#[derive(Clone, PartialEq, Message)]
pub struct Value {
    #[prost(string, optional, tag = "1")]
    pub string_value: Option<String>,
}

// ---------------------------------------------------------------------------
// Geometry command encoding
// ---------------------------------------------------------------------------

fn command(id: u32, count: u32) -> u32 {
    (id & 0x7) | (count << 3)
}

/// MVT parameter integers are zigzag-encoded deltas.
fn zigzag(v: i64) -> u32 {
    ((v << 1) ^ (v >> 63)) as u32
}

/// Quantizes one Web Mercator vertex to tile-local integer space.
/// `origin` is the tile's (min_x, max_y) corner — MVT y grows *downward*,
/// Web Mercator y grows upward, so y is measured from the tile's top edge.
fn quantize(merc: (f64, f64), origin: (f64, f64), scale: f64) -> (i64, i64) {
    let px = ((merc.0 - origin.0) * scale).round() as i64;
    let py = ((origin.1 - merc.1) * scale).round() as i64;
    (px, py)
}

/// Quantizes one feature's disjoint segments into an MVT geometry command
/// stream. The cursor persists across ALL segments of one feature (spec:
/// parameter deltas are relative to the previous point of the same
/// feature, MoveTo included). Empty result = every segment degenerate.
/// MULTIPOINT encoding: one MoveTo command whose count covers every
/// distinct quantized point, parameters as cursor-relative zigzag deltas
/// (spec §4.3.4.2). Duplicate grid points are dropped — the spec forbids
/// coincident points within a MULTIPOINT.
fn encode_point_geometry(segments: &[Vec<(f64, f64)>], origin: (f64, f64), scale: f64) -> Vec<u32> {
    let mut pts: Vec<(i64, i64)> = Vec::with_capacity(segments.len());
    for seg in segments {
        for &v in seg {
            let p = quantize(v, origin, scale);
            if !pts.contains(&p) {
                pts.push(p);
            }
        }
    }
    if pts.is_empty() {
        return Vec::new();
    }
    let mut geometry = Vec::with_capacity(1 + pts.len() * 2);
    geometry.push(command(CMD_MOVE_TO, pts.len() as u32));
    let mut cursor = (0i64, 0i64);
    for p in pts {
        geometry.push(zigzag(p.0 - cursor.0));
        geometry.push(zigzag(p.1 - cursor.1));
        cursor = p;
    }
    geometry
}

fn encode_geometry(segments: &[Vec<(f64, f64)>], origin: (f64, f64), scale: f64) -> Vec<u32> {
    let mut geometry: Vec<u32> = Vec::new();
    let mut cursor = (0i64, 0i64);

    for seg in segments {
        // Quantize, dropping consecutive duplicates: sub-pixel wiggle that
        // collapses onto one grid point must not emit zero-length LineTo
        // steps (spec forbids them).
        let mut pts: Vec<(i64, i64)> = Vec::with_capacity(seg.len());
        for &v in seg {
            let p = quantize(v, origin, scale);
            if pts.last() != Some(&p) {
                pts.push(p);
            }
        }
        if pts.len() < 2 {
            continue; // degenerate after quantization
        }

        geometry.push(command(CMD_MOVE_TO, 1));
        geometry.push(zigzag(pts[0].0 - cursor.0));
        geometry.push(zigzag(pts[0].1 - cursor.1));
        geometry.push(command(CMD_LINE_TO, (pts.len() - 1) as u32));
        for pair in pts.windows(2) {
            geometry.push(zigzag(pair[1].0 - pair[0].0));
            geometry.push(zigzag(pair[1].1 - pair[0].1));
        }
        cursor = pts[pts.len() - 1];
    }
    geometry
}

/// Encodes one tile's features into an (uncompressed) MVT payload.
///
/// `features` come straight from the `TileFeatures` index decode. They are
/// grouped by layer index into one named MVT layer each (ascending index —
/// deterministic output, which payload dedup and the engine's determinism
/// proof rely on). Attributes per feature (P5.C3/P5.C4):
/// - `class` (always) — pooled first for every feature, so it always
///   lands at key index 0.
/// - `sac_scale` (optional, highway) and `name` (optional, any layer) —
///   each key is appended to its layer's KEY pool lazily, on the first
///   feature that carries it; a layer's key order is therefore first-seen
///   order (deterministic given the deterministic feature order), and
///   attribute-free layers stay byte-stable at exactly `["class"]`.
///
/// Per-layer VALUE pools are shared across keys (valid MVT — coinciding
/// strings share one entry), deduped in first-seen order. `tags` is the
/// concatenation of the feature's `[key_idx, value_idx]` pairs. Returns
/// `None` when nothing survives quantization — the caller writes no
/// archive entry for such a tile.
pub fn encode_tile_mvt(
    zoom: u8,
    tile_x: u32,
    tile_y: u32,
    features: &[TileFeature],
) -> Option<Vec<u8>> {
    let (min_x, _min_y, max_x, max_y) = geom::tile_bounds(zoom, tile_x, tile_y);
    let scale = f64::from(MVT_EXTENT) / (max_x - min_x);
    let origin = (min_x, max_y);

    // Grouped by layer index; BTreeMap gives ascending-index layer order.
    let mut layers: BTreeMap<u8, Layer> = BTreeMap::new();
    // Per-layer pools, both first-seen-ordered: key string → key index and
    // value string → value index (values shared across keys in a layer).
    let mut key_pool: HashMap<(u8, &'static str), u32> = HashMap::new();
    let mut value_pool: HashMap<(u8, Vec<u8>), u32> = HashMap::new();

    for feature in features {
        // Point features (P-CORE.C8: node POIs — every segment is a single
        // vertex) take the POINT geometry path; anything with a real
        // polyline stays on the LINESTRING path.
        let is_point_feature =
            !feature.segments.is_empty() && feature.segments.iter().all(|s| s.len() == 1);
        let (geometry, geom_type) = if is_point_feature {
            (
                encode_point_geometry(&feature.segments, origin, scale),
                GEOM_TYPE_POINT,
            )
        } else {
            (
                encode_geometry(&feature.segments, origin, scale),
                GEOM_TYPE_LINESTRING,
            )
        };
        if geometry.is_empty() {
            continue;
        }

        // Layer indices come from decode_tile_segments, which rejects
        // out-of-range values — reaching this expect means the index and
        // the taxonomy went out of sync, which is a bug, not bad data.
        let name =
            pbf::layer_name(feature.layer).expect("layer index validated at TileFeatures decode");
        let layer = layers.entry(feature.layer).or_insert_with(|| Layer {
            version: 2,
            name: name.to_string(),
            features: Vec::new(),
            keys: Vec::new(), // grown lazily through key_pool below
            values: Vec::new(),
            extent: MVT_EXTENT,
        });

        // Appends one `[key_idx, value_idx]` pair to `tags`, growing the
        // layer's key/value pools on first sight of either string.
        let layer_idx = feature.layer;
        let mut push_attr =
            |layer: &mut Layer, tags: &mut Vec<u32>, key: &'static str, value: &[u8]| {
                let k = *key_pool.entry((layer_idx, key)).or_insert_with(|| {
                    layer.keys.push(key.to_string());
                    (layer.keys.len() - 1) as u32
                });
                let v = match value_pool.get(&(layer_idx, value.to_vec())) {
                    Some(&idx) => idx,
                    None => {
                        let idx = layer.values.len() as u32;
                        layer.values.push(Value {
                            string_value: Some(String::from_utf8_lossy(value).into_owned()),
                        });
                        value_pool.insert((layer_idx, value.to_vec()), idx);
                        idx
                    }
                };
                tags.extend_from_slice(&[k, v]);
            };

        let mut tags = Vec::with_capacity(6);
        push_attr(layer, &mut tags, CLASS_KEY, &feature.class);
        if !feature.sac_scale.is_empty() {
            push_attr(layer, &mut tags, SAC_SCALE_KEY, &feature.sac_scale);
        }
        if !feature.name.is_empty() {
            push_attr(layer, &mut tags, NAME_KEY, &feature.name);
        }

        layer.features.push(Feature {
            id: feature.way_id,
            tags,
            geom_type,
            geometry,
        });
    }

    if layers.is_empty() {
        return None;
    }
    let tile = Tile {
        layers: layers.into_values().collect(),
    };
    Some(tile.encode_to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// z14 tile somewhere over Innsbruck; every test builds coordinates
    /// from its real bounds so the quantization math is exercised end to
    /// end, not stubbed.
    const Z: u8 = 14;
    const TX: u32 = 8703;
    const TY: u32 = 5747;

    /// TileFeature shorthand for encoder tests (no grade, no name).
    fn feat(way_id: u64, layer: u8, class: &[u8], segments: Vec<Vec<(f64, f64)>>) -> TileFeature {
        TileFeature {
            way_id,
            layer,
            class: class.to_vec(),
            sac_scale: Vec::new(),
            name: Vec::new(),
            segments,
        }
    }

    /// Grade-bearing variant of [`feat`].
    fn graded(
        way_id: u64,
        layer: u8,
        class: &[u8],
        sac: &[u8],
        segments: Vec<Vec<(f64, f64)>>,
    ) -> TileFeature {
        TileFeature {
            sac_scale: sac.to_vec(),
            ..feat(way_id, layer, class, segments)
        }
    }

    /// Name-bearing variant of [`feat`].
    fn named(
        way_id: u64,
        layer: u8,
        class: &[u8],
        name: &str,
        segments: Vec<Vec<(f64, f64)>>,
    ) -> TileFeature {
        TileFeature {
            name: name.as_bytes().to_vec(),
            ..feat(way_id, layer, class, segments)
        }
    }

    /// Web Mercator point that quantizes to exactly `(px, py)` tile-local.
    fn merc_at(px: f64, py: f64) -> (f64, f64) {
        let (min_x, _, max_x, max_y) = geom::tile_bounds(Z, TX, TY);
        let unit = (max_x - min_x) / f64::from(MVT_EXTENT);
        (min_x + px * unit, max_y - py * unit)
    }

    fn decode(payload: &[u8]) -> Tile {
        Tile::decode(payload).expect("payload must prost-decode")
    }

    #[test]
    fn commands_encode_moveto_lineto_zigzag() {
        let seg = vec![merc_at(10.0, 20.0), merc_at(15.0, 20.0), merc_at(15.0, 8.0)];
        let payload = encode_tile_mvt(Z, TX, TY, &[feat(42, 0, b"path", vec![seg])]).unwrap();
        let tile = decode(&payload);

        assert_eq!(tile.layers.len(), 1);
        let layer = &tile.layers[0];
        assert_eq!(layer.version, 2);
        assert_eq!(layer.name, "highway");
        assert_eq!(layer.extent, MVT_EXTENT);
        assert_eq!(layer.keys, vec![CLASS_KEY.to_string()]);
        assert_eq!(
            layer.values,
            vec![Value {
                string_value: Some("path".into())
            }]
        );

        let f = &layer.features[0];
        assert_eq!(f.id, 42);
        assert_eq!(f.tags, vec![0, 0], "keys[0]=class, values[0]=path");
        assert_eq!(f.geom_type, GEOM_TYPE_LINESTRING);
        // MoveTo(1) (10,20); LineTo(2) (+5,0), (0,-12).
        assert_eq!(
            f.geometry,
            vec![
                command(CMD_MOVE_TO, 1),
                zigzag(10),
                zigzag(20),
                command(CMD_LINE_TO, 2),
                zigzag(5),
                zigzag(0),
                zigzag(0),
                zigzag(-12),
            ]
        );
    }

    #[test]
    fn disjoint_segments_share_one_feature_cursor() {
        let s1 = vec![merc_at(0.0, 0.0), merc_at(4.0, 0.0)];
        let s2 = vec![merc_at(10.0, 10.0), merc_at(10.0, 14.0)];
        let payload = encode_tile_mvt(Z, TX, TY, &[feat(7, 0, b"path", vec![s1, s2])]).unwrap();
        let f = &decode(&payload).layers[0].features[0];
        // Second MoveTo is relative to the END of segment 1 at (4,0).
        assert_eq!(
            f.geometry,
            vec![
                command(CMD_MOVE_TO, 1),
                zigzag(0),
                zigzag(0),
                command(CMD_LINE_TO, 1),
                zigzag(4),
                zigzag(0),
                command(CMD_MOVE_TO, 1),
                zigzag(6),
                zigzag(10),
                command(CMD_LINE_TO, 1),
                zigzag(0),
                zigzag(4),
            ]
        );
    }

    /// Vertices in the clip buffer land outside 0..=4096 — negative and
    /// >extent values must survive (that's the seam-join geometry).
    #[test]
    fn buffered_vertices_exceed_extent() {
        let seg = vec![merc_at(-30.0, 2000.0), merc_at(4120.0, 2000.0)];
        let payload = encode_tile_mvt(Z, TX, TY, &[feat(1, 0, b"path", vec![seg])]).unwrap();
        let f = &decode(&payload).layers[0].features[0];
        assert_eq!(f.geometry[1], zigzag(-30));
        assert_eq!(f.geometry[4], zigzag(4150)); // delta from -30 to 4120
    }

    #[test]
    fn degenerate_segments_are_dropped() {
        // Sub-quantum wiggle: both vertices collapse onto one grid point.
        let (min_x, _, max_x, max_y) = geom::tile_bounds(Z, TX, TY);
        let unit = (max_x - min_x) / f64::from(MVT_EXTENT);
        let p = (min_x + 100.0 * unit, max_y - 100.0 * unit);
        let degenerate = vec![p, (p.0 + unit * 0.05, p.1)];
        let real = vec![merc_at(0.0, 0.0), merc_at(9.0, 0.0)];

        // Degenerate-only feature vanishes; the tile keeps the real one.
        let payload = encode_tile_mvt(
            Z,
            TX,
            TY,
            &[
                feat(1, 0, b"path", vec![degenerate]),
                feat(2, 0, b"path", vec![real]),
            ],
        )
        .unwrap();
        let layer = &decode(&payload).layers[0];
        assert_eq!(layer.features.len(), 1);
        assert_eq!(layer.features[0].id, 2);
        assert_eq!(
            layer.values.len(),
            1,
            "the dropped feature must not leak a value-pool entry"
        );
    }

    #[test]
    fn all_degenerate_tile_encodes_none() {
        let p = merc_at(5.0, 5.0);
        assert_eq!(
            encode_tile_mvt(Z, TX, TY, &[feat(1, 0, b"path", vec![vec![p, p]])]),
            None
        );
        assert_eq!(encode_tile_mvt(Z, TX, TY, &[]), None);
    }

    /// The P5.C2 core proof: features of different layers land in separate
    /// named MVT layers, ordered by ascending layer index regardless of
    /// input order.
    #[test]
    fn features_group_into_named_layers_in_index_order() {
        let seg = |y: f64| vec![merc_at(0.0, y), merc_at(50.0, y)];
        // Input order: natural, highway, waterway — output must be
        // highway(0), waterway(1), natural(2).
        let payload = encode_tile_mvt(
            Z,
            TX,
            TY,
            &[
                feat(30, 2, b"wood", vec![seg(30.0)]),
                feat(10, 0, b"path", vec![seg(10.0)]),
                feat(20, 1, b"stream", vec![seg(20.0)]),
            ],
        )
        .unwrap();
        let tile = decode(&payload);

        let names: Vec<&str> = tile.layers.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, vec!["highway", "waterway", "natural"]);
        for layer in &tile.layers {
            assert_eq!(layer.version, 2);
            assert_eq!(layer.keys, vec![CLASS_KEY.to_string()]);
            assert_eq!(layer.features.len(), 1);
            assert_eq!(layer.features[0].tags, vec![0, 0]);
        }
        assert_eq!(
            tile.layers[2].values,
            vec![Value {
                string_value: Some("wood".into())
            }]
        );
    }

    /// Class strings dedup within a layer: two path features share one
    /// value-pool entry; a third class appends a second entry.
    #[test]
    fn class_values_dedup_within_layer() {
        let seg = |y: f64| vec![merc_at(0.0, y), merc_at(50.0, y)];
        let payload = encode_tile_mvt(
            Z,
            TX,
            TY,
            &[
                feat(1, 0, b"path", vec![seg(10.0)]),
                feat(2, 0, b"track", vec![seg(20.0)]),
                feat(3, 0, b"path", vec![seg(30.0)]),
            ],
        )
        .unwrap();
        let tile = decode(&payload);
        assert_eq!(tile.layers.len(), 1);
        let layer = &tile.layers[0];

        assert_eq!(
            layer.values,
            vec![
                Value {
                    string_value: Some("path".into())
                },
                Value {
                    string_value: Some("track".into())
                },
            ],
            "first-seen order, no duplicates"
        );
        let tags: Vec<&Vec<u32>> = layer.features.iter().map(|f| &f.tags).collect();
        assert_eq!(tags, vec![&vec![0, 0], &vec![0, 1], &vec![0, 0]]);
    }

    /// The P5.C3 core proof: a grade-bearing highway feature carries BOTH
    /// attributes — keys grow to ["class","sac_scale"] and its tags array
    /// points at both, while a grade-less sibling in the same layer keeps a
    /// 2-element tags array.
    #[test]
    fn sac_scale_encodes_as_second_attribute() {
        let seg = |y: f64| vec![merc_at(0.0, y), merc_at(50.0, y)];
        let payload = encode_tile_mvt(
            Z,
            TX,
            TY,
            &[
                graded(1, 0, b"path", b"alpine_hiking", vec![seg(10.0)]),
                feat(2, 0, b"track", vec![seg(20.0)]),
                graded(3, 0, b"path", b"alpine_hiking", vec![seg(30.0)]),
            ],
        )
        .unwrap();
        let tile = decode(&payload);
        assert_eq!(tile.layers.len(), 1);
        let layer = &tile.layers[0];

        assert_eq!(
            layer.keys,
            vec![CLASS_KEY.to_string(), SAC_SCALE_KEY.to_string()]
        );
        // Value pool, first-seen: path, alpine_hiking, track — shared
        // across both keys, deduped across features 1 and 3.
        let values: Vec<Option<&str>> = layer
            .values
            .iter()
            .map(|v| v.string_value.as_deref())
            .collect();
        assert_eq!(
            values,
            vec![Some("path"), Some("alpine_hiking"), Some("track")]
        );
        let tags: Vec<&Vec<u32>> = layer.features.iter().map(|f| &f.tags).collect();
        assert_eq!(
            tags,
            vec![&vec![0, 0, 1, 1], &vec![0, 2], &vec![0, 0, 1, 1]],
            "graded features carry [class, sac_scale]; plain ones just [class]"
        );
    }

    /// A layer with no graded features must stay byte-identical to its
    /// P5.C2 shape: keys exactly ["class"], 2-element tags.
    #[test]
    fn grade_free_layers_stay_class_only() {
        let seg = vec![merc_at(0.0, 5.0), merc_at(40.0, 5.0)];
        let payload = encode_tile_mvt(Z, TX, TY, &[feat(9, 1, b"stream", vec![seg])]).unwrap();
        let layer = &decode(&payload).layers[0];
        assert_eq!(layer.keys, vec![CLASS_KEY.to_string()]);
        assert_eq!(layer.features[0].tags, vec![0, 0]);
    }

    /// A class string and a grade string that coincide share one entry in
    /// the layer's value pool (pools are per-layer, not per-key).
    #[test]
    fn class_and_sac_values_share_the_pool() {
        let seg = vec![merc_at(0.0, 5.0), merc_at(40.0, 5.0)];
        // Contrived but legal: class "hiking" + sac_scale "hiking".
        let payload =
            encode_tile_mvt(Z, TX, TY, &[graded(1, 0, b"hiking", b"hiking", vec![seg])]).unwrap();
        let layer = &decode(&payload).layers[0];
        assert_eq!(layer.values.len(), 1, "one pooled value serves both keys");
        assert_eq!(layer.features[0].tags, vec![0, 0, 1, 0]);
    }

    /// P5.C4: a named feature on a NON-highway layer carries the name
    /// attribute — keys grow to ["class","name"] with tags [0,c,1,n], and
    /// UTF-8 label text survives to the wire.
    #[test]
    fn name_encodes_on_any_layer() {
        let seg = vec![merc_at(0.0, 5.0), merc_at(40.0, 5.0)];
        let payload =
            encode_tile_mvt(Z, TX, TY, &[named(1, 1, b"river", "Inn", vec![seg])]).unwrap();
        let layer = &decode(&payload).layers[0];
        assert_eq!(layer.name, "waterway");
        assert_eq!(
            layer.keys,
            vec![CLASS_KEY.to_string(), NAME_KEY.to_string()]
        );
        assert_eq!(layer.values[1].string_value.as_deref(), Some("Inn"));
        assert_eq!(layer.features[0].tags, vec![0, 0, 1, 1]);
    }

    /// THE key-pool regression test for the P5.C4 refactor: with two lazy
    /// keys, a layer that sees a named-only feature FIRST and a graded-only
    /// feature SECOND must assign key indices in first-seen order — and
    /// every feature's tags must point at the right key.
    #[test]
    fn lazy_key_indices_follow_first_seen_order() {
        let seg = |y: f64| vec![merc_at(0.0, y), merc_at(50.0, y)];
        let payload = encode_tile_mvt(
            Z,
            TX,
            TY,
            &[
                named(1, 0, b"path", "Höhenweg", vec![seg(10.0)]),
                graded(2, 0, b"path", b"alpine_hiking", vec![seg(20.0)]),
                // Fully-attributed: tags are emitted class, sac, name — but
                // the key INDICES come from the layer's first-seen pool.
                TileFeature {
                    sac_scale: b"alpine_hiking".to_vec(),
                    ..named(3, 0, b"path", "Höhenweg", vec![seg(30.0)])
                },
            ],
        )
        .unwrap();
        let layer = &decode(&payload).layers[0];

        // name was seen before sac_scale → keys [class, name, sac_scale].
        assert_eq!(
            layer.keys,
            vec![
                CLASS_KEY.to_string(),
                NAME_KEY.to_string(),
                SAC_SCALE_KEY.to_string()
            ]
        );
        // Values first-seen: path, Höhenweg, alpine_hiking.
        let values: Vec<Option<&str>> = layer
            .values
            .iter()
            .map(|v| v.string_value.as_deref())
            .collect();
        assert_eq!(
            values,
            vec![Some("path"), Some("Höhenweg"), Some("alpine_hiking")]
        );
        let tags: Vec<&Vec<u32>> = layer.features.iter().map(|f| &f.tags).collect();
        assert_eq!(
            tags,
            vec![
                &vec![0, 0, 1, 1],       // class=path, name=Höhenweg
                &vec![0, 0, 2, 2],       // class=path, sac_scale=alpine_hiking
                &vec![0, 0, 2, 2, 1, 1], // all three, indices per pool order
            ],
        );
    }

    #[test]
    fn single_vertex_features_encode_as_points() {
        let (min_x, _min_y, max_x, max_y) = geom::tile_bounds(Z, TX, TY);
        let unit = (max_x - min_x) / f64::from(MVT_EXTENT);
        let at = |px: f64, py: f64| (min_x + px * unit, max_y - py * unit);

        let peak = TileFeature {
            way_id: 7,
            layer: 2,
            class: b"peak".to_vec(),
            sac_scale: Vec::new(),
            name: "Hafelekarspitze".as_bytes().to_vec(),
            segments: vec![vec![at(100.0, 200.0)]],
        };
        let trail = TileFeature {
            way_id: 8,
            layer: 0,
            class: b"path".to_vec(),
            sac_scale: Vec::new(),
            name: Vec::new(),
            segments: vec![vec![at(0.0, 0.0), at(50.0, 0.0)]],
        };

        let payload = encode_tile_mvt(Z, TX, TY, &[peak, trail]).unwrap();
        let tile = Tile::decode(payload.as_slice()).unwrap();

        let natural = tile.layers.iter().find(|l| l.name == "natural").unwrap();
        assert_eq!(natural.features.len(), 1);
        let p = &natural.features[0];
        assert_eq!(p.geom_type, GEOM_TYPE_POINT);
        // One MoveTo command with count 1: (1 & 0x7) | (1 << 3) = 9, then
        // two zigzag params.
        assert_eq!(p.geometry.len(), 3);
        assert_eq!(p.geometry[0], 9);
        assert_eq!(p.geometry[1], 200); // zigzag(+100)
        assert_eq!(p.geometry[2], 400); // zigzag(+200)
        assert_eq!(
            natural.values[1].string_value.as_deref(),
            Some("Hafelekarspitze"),
            "peak keeps its name attribute"
        );

        // The line feature is untouched by the point path (regression).
        let highway = tile.layers.iter().find(|l| l.name == "highway").unwrap();
        assert_eq!(highway.features[0].geom_type, GEOM_TYPE_LINESTRING);
    }
}
