// SPDX-License-Identifier: Apache-2.0
//! `tiles` — MVT encoding and PMTiles v3 assembly (Phase 5, P5.C1).
//!
//! The Finalize stage of the compile pipeline: drains the `TileFeatures`
//! index Pass 3 built, packs each tile's clipped segments into a gzipped
//! Mapbox Vector Tile, and assembles a spec-compliant PMTiles v3 archive
//! with a Hilbert-clustered data section.
//!
//! Two-stage design (both stages honor the engine's budget-yield /
//! kill-resume contract; see [`finalize`] for the durability rules):
//!
//! 1. **Encode** ([`run_finalize_encode_slice`]) — per-tile MVT + gzip +
//!    dedup, appended to a temporary data file, with durable bookkeeping in
//!    two redb tables beside the pipeline's own index. Yieldable per tile.
//! 2. **Assemble** ([`assemble_archive`]) — one idempotent block that
//!    reorders payloads into ascending-tile-ID (Hilbert) order and writes
//!    header + root directory + metadata + data atomically (tmp + rename).
//!
//! MVT wire structs are hand-derived prost messages ([`mvt`]) — the same
//! no-protoc pattern as `pbf/src/proto.rs`. The PMTiles v3 header and
//! varint directory encoding live in [`pmtiles`]; tile-ID ↔ Hilbert math in
//! [`hilbert`].

pub mod finalize;
pub mod hilbert;
pub mod mvt;
pub mod pmtiles;

pub use finalize::{
    assemble_archive, run_finalize_encode_slice, tile_feature_row_count, ArchiveInfo,
    FinalizeError, FinalizeSlice,
};
pub use hilbert::{tile_id, tile_id_to_zxy};
pub use mvt::{encode_tile_mvt, MVT_EXTENT};
