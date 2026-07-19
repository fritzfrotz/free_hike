// SPDX-License-Identifier: Apache-2.0
//! Lossless WebP encoding of Terrain-RGB buffers.
//!
//! Lossless is non-negotiable for Terrain-RGB: lossy quantisation would
//! corrupt the low-order elevation byte (each B step is 0.1m). The encoder is
//! the pure-Rust `image-webp` backend — nothing C-linked crosses into the
//! mobile builds.

use image::codecs::webp::WebPEncoder;
use image::{ExtendedColorType, ImageError};

/// Encodes a row-major RGB8 buffer (`width × height × 3` bytes) as lossless
/// WebP.
pub fn encode_rgb_lossless(rgb: &[u8], width: u32, height: u32) -> Result<Vec<u8>, ImageError> {
    let mut out = Vec::new();
    WebPEncoder::new_lossless(&mut out).encode(rgb, width, height, ExtendedColorType::Rgb8)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lossless_round_trip_preserves_every_byte() {
        // Gradient exercising all three channels.
        let (w, h) = (64u32, 64u32);
        let rgb: Vec<u8> = (0..w * h)
            .flat_map(|i| [(i % 256) as u8, (i / 256) as u8, (i % 251) as u8])
            .collect();

        let webp = encode_rgb_lossless(&rgb, w, h).unwrap();
        assert_eq!(&webp[0..4], b"RIFF");
        assert_eq!(&webp[8..12], b"WEBP");

        let decoded = image::load_from_memory(&webp).unwrap().into_rgb8();
        assert_eq!(decoded.dimensions(), (w, h));
        assert_eq!(decoded.as_raw(), &rgb);
    }
}
