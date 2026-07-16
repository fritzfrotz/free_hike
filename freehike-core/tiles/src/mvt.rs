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
//! **Known simplification (P5.C1, logged):** a single layer `"features"`
//! with the OSM way ID as feature id and no key/value attributes —
//! `TileFeatures` persists geometry only. Styled layers (highway/waterway/
//! natural classes) require Pass 2/3 to persist the matched tag; flagged as
//! follow-up work in the LOOPLOG.

use prost::Message;

/// The standard MVT tile extent: coordinates are quantized to a
/// 4096×4096 integer grid per tile.
pub const MVT_EXTENT: u32 = 4096;

/// Layer name for all P5.C1 output (see module docs on the single-layer
/// simplification). The client style must reference this source-layer.
pub const LAYER_NAME: &str = "features";

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

/// Encodes one tile's features into an (uncompressed) MVT payload.
///
/// `features` are `(way_id, disjoint segments)` in Web Mercator meters —
/// [`pbf::tile::TileFeature`], exactly as decoded from the `TileFeatures`
/// index. Returns `None` when nothing survives quantization (every segment
/// degenerate) — the caller writes no archive entry for such a tile.
pub fn encode_tile_mvt(
    zoom: u8,
    tile_x: u32,
    tile_y: u32,
    features: &[pbf::tile::TileFeature],
) -> Option<Vec<u8>> {
    let (min_x, _min_y, max_x, max_y) = geom::tile_bounds(zoom, tile_x, tile_y);
    let scale = f64::from(MVT_EXTENT) / (max_x - min_x);
    let origin = (min_x, max_y);

    let mut out_features = Vec::with_capacity(features.len());
    for (way_id, segments) in features {
        let mut geometry: Vec<u32> = Vec::new();
        // The cursor persists across ALL segments of one feature (spec:
        // parameter deltas are relative to the previous point of the same
        // feature, MoveTo included).
        let mut cursor = (0i64, 0i64);

        for seg in segments {
            // Quantize, dropping consecutive duplicates: sub-pixel wiggle
            // that collapses onto one grid point must not emit zero-length
            // LineTo steps (spec forbids them).
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

        if !geometry.is_empty() {
            out_features.push(Feature {
                id: *way_id,
                tags: Vec::new(),
                geom_type: GEOM_TYPE_LINESTRING,
                geometry,
            });
        }
    }

    if out_features.is_empty() {
        return None;
    }

    let tile = Tile {
        layers: vec![Layer {
            version: 2,
            name: LAYER_NAME.to_string(),
            features: out_features,
            keys: Vec::new(),
            values: Vec::new(),
            extent: MVT_EXTENT,
        }],
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
        let payload = encode_tile_mvt(Z, TX, TY, &[(42, vec![seg])]).unwrap();
        let tile = decode(&payload);

        assert_eq!(tile.layers.len(), 1);
        let layer = &tile.layers[0];
        assert_eq!(layer.version, 2);
        assert_eq!(layer.name, LAYER_NAME);
        assert_eq!(layer.extent, MVT_EXTENT);
        assert!(layer.keys.is_empty() && layer.values.is_empty());

        let f = &layer.features[0];
        assert_eq!(f.id, 42);
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
        let payload = encode_tile_mvt(Z, TX, TY, &[(7, vec![s1, s2])]).unwrap();
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
        let payload = encode_tile_mvt(Z, TX, TY, &[(1, vec![seg])]).unwrap();
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
        let payload =
            encode_tile_mvt(Z, TX, TY, &[(1, vec![degenerate]), (2, vec![real])]).unwrap();
        let layer = &decode(&payload).layers[0];
        assert_eq!(layer.features.len(), 1);
        assert_eq!(layer.features[0].id, 2);
    }

    #[test]
    fn all_degenerate_tile_encodes_none() {
        let p = merc_at(5.0, 5.0);
        assert_eq!(encode_tile_mvt(Z, TX, TY, &[(1, vec![vec![p, p]])]), None);
        assert_eq!(encode_tile_mvt(Z, TX, TY, &[]), None);
    }
}
