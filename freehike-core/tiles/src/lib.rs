//! `tiles` — vector tile encoding and PMTiles v3 assembly (Phases 4-5).
//!
//! Will own: Web Mercator projection, Ramer-Douglas-Peucker simplification,
//! Sutherland-Hodgman clipping, MVT encoding (MLT behind a feature flag per
//! operating-manual risk R4), and the Hilbert-sorted sequential PMTiles
//! writer. Placeholder in Phase 0.

/// Crate identity used by walking-skeleton diagnostics.
pub const CRATE: &str = "tiles";
