//! `compiler` — orchestration core for the FreeHike on-device map compiler.
//!
//! `engine` holds the suspendable slice state machine (the Phase 7-shaped
//! contract: budget-bounded slices, durable checkpoints, resume by job
//! identity). The block work inside it is simulated until the real two-pass
//! PBF pipeline and terrain encoder land (see `research/On-Device Map
//! Compilation - Feasibility, Architecture, and Implementation Plan.md`,
//! Phases 3-7).

pub mod engine;

use std::fmt;

/// Geographic bounding box in WGS84 degrees, ordered `west,south,east,north`
/// (the same convention as our osmium extract commands and Geofabrik tooling).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BBox {
    pub west: f64,
    pub south: f64,
    pub east: f64,
    pub north: f64,
}

/// Why a bbox string was rejected. Kept as a plain enum (no `thiserror` dep)
/// so the FFI layer can flatten it into a message string cheaply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BBoxError {
    /// Not exactly four comma-separated fields.
    WrongArity(usize),
    /// A field failed to parse as a finite f64.
    NotANumber(String),
    /// Longitude outside [-180, 180] or latitude outside [-90, 90].
    OutOfRange(String),
    /// west >= east or south >= north.
    Inverted,
}

impl fmt::Display for BBoxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BBoxError::WrongArity(n) => {
                write!(
                    f,
                    "expected 4 comma-separated values (west,south,east,north), got {n}"
                )
            }
            BBoxError::NotANumber(s) => write!(f, "'{s}' is not a finite number"),
            BBoxError::OutOfRange(s) => write!(f, "coordinate out of range: {s}"),
            BBoxError::Inverted => write!(
                f,
                "bbox is inverted: requires west < east and south < north"
            ),
        }
    }
}

impl std::error::Error for BBoxError {}

impl BBox {
    /// Parses `"west,south,east,north"` in WGS84 degrees.
    pub fn parse(s: &str) -> Result<Self, BBoxError> {
        let fields: Vec<&str> = s.split(',').map(str::trim).collect();
        if fields.len() != 4 {
            return Err(BBoxError::WrongArity(fields.len()));
        }

        let mut vals = [0f64; 4];
        for (i, field) in fields.iter().enumerate() {
            let v: f64 = field
                .parse()
                .map_err(|_| BBoxError::NotANumber((*field).to_string()))?;
            if !v.is_finite() {
                return Err(BBoxError::NotANumber((*field).to_string()));
            }
            vals[i] = v;
        }

        let [west, south, east, north] = vals;
        for (lon, label) in [(west, "west"), (east, "east")] {
            if !(-180.0..=180.0).contains(&lon) {
                return Err(BBoxError::OutOfRange(format!("{label}={lon}")));
            }
        }
        for (lat, label) in [(south, "south"), (north, "north")] {
            if !(-90.0..=90.0).contains(&lat) {
                return Err(BBoxError::OutOfRange(format!("{label}={lat}")));
            }
        }
        if west >= east || south >= north {
            return Err(BBoxError::Inverted);
        }

        Ok(BBox {
            west,
            south,
            east,
            north,
        })
    }
}

/// Phase 0 stub for the compile entry point: acknowledges a validated bbox.
/// Replaced by the real chunked state machine in Phase 7; the JSON-ish shape
/// lets the WebView layer build against a stable envelope from day one.
pub fn compile_chunk_stub(bbox: &BBox) -> String {
    format!(
        concat!(
            r#"{{"status":"accepted","engine":"freehike-core {}","#,
            r#""bbox":[{},{},{},{}],"phase":"walking-skeleton"}}"#
        ),
        env!("CARGO_PKG_VERSION"),
        bbox.west,
        bbox.south,
        bbox.east,
        bbox.north,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Innsbruck sandbox bbox used across the whole project
    /// (scripts/compile_sandbox_data.sh INNSBRUCK_BBOX).
    const INNSBRUCK: &str = "11.15,47.05,11.65,47.45";

    #[test]
    fn parse_valid_bbox() {
        let b = BBox::parse(INNSBRUCK).expect("Innsbruck bbox must parse");
        assert_eq!(
            b,
            BBox {
                west: 11.15,
                south: 47.05,
                east: 11.65,
                north: 47.45
            }
        );
    }

    #[test]
    fn parse_tolerates_whitespace() {
        assert!(BBox::parse(" 11.15 , 47.05 , 11.65 , 47.45 ").is_ok());
    }

    #[test]
    fn rejects_wrong_arity() {
        assert_eq!(BBox::parse("1,2,3"), Err(BBoxError::WrongArity(3)));
        assert_eq!(BBox::parse("1,2,3,4,5"), Err(BBoxError::WrongArity(5)));
    }

    #[test]
    fn rejects_non_numeric() {
        assert!(matches!(
            BBox::parse("11.15,47.05,alps,47.45"),
            Err(BBoxError::NotANumber(_))
        ));
        assert!(matches!(
            BBox::parse("11.15,NaN,11.65,47.45"),
            Err(BBoxError::NotANumber(_))
        ));
    }

    #[test]
    fn rejects_out_of_range() {
        assert!(matches!(
            BBox::parse("181.0,47.05,11.65,47.45"),
            Err(BBoxError::OutOfRange(_))
        ));
        assert!(matches!(
            BBox::parse("11.15,-91.0,11.65,47.45"),
            Err(BBoxError::OutOfRange(_))
        ));
    }

    #[test]
    fn rejects_inverted_bbox() {
        assert_eq!(
            BBox::parse("11.65,47.05,11.15,47.45"),
            Err(BBoxError::Inverted)
        );
        assert_eq!(
            BBox::parse("11.15,47.45,11.65,47.05"),
            Err(BBoxError::Inverted)
        );
    }

    #[test]
    fn stub_reports_accepted() {
        let b = BBox::parse(INNSBRUCK).unwrap();
        let out = compile_chunk_stub(&b);
        assert!(out.contains(r#""status":"accepted""#));
        assert!(out.contains("11.15"));
        assert!(out.contains("47.45"));
    }
}
