//! `pbf` — OSM Protocolbuffer Binary Format ingestion (Phase 3).
//!
//! Will own: mmap'd read-only PBF access, PrimitiveBlock stream decoding,
//! StringTable tag pre-filtering, and the Pass-1 node → redb coordinate index.
//! Placeholder in Phase 0 so the workspace and dependency graph are stable.

/// Crate identity used by walking-skeleton diagnostics.
pub const CRATE: &str = "pbf";
