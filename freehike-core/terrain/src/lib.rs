//! `terrain` — DEM GeoTIFF processing (Phase 6).
//!
//! Will own: async windowed GeoTIFF reads, Terrain-RGB (Mapbox encoding,
//! base −10000 / interval 0.1) WebP tile generation, and Marching Squares
//! contour extraction. Placeholder in Phase 0.

/// Crate identity used by walking-skeleton diagnostics.
pub const CRATE: &str = "terrain";
