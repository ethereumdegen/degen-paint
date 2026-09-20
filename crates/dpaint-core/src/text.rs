//! Text resources shared by every mode.
//!
//! `TextSpec` promises that the same string shapes identically in raster, in vector, and in
//! the 3D extruder. That promise only holds if all three fall back to the *same* face when a
//! requested family is missing, so the fallback lives here rather than in each engine.

/// Bytes of the deterministic fallback face. Embedded so a render never depends on what
/// fonts a machine happens to have installed.
pub const FALLBACK_FONT: &[u8] = include_bytes!("../assets/Roboto-Regular.ttf");

/// Family name the fallback reports as.
pub const FALLBACK_FAMILY: &str = "Roboto";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fallback_face_is_embedded_and_is_a_truetype_file() {
        assert!(FALLBACK_FONT.len() > 100_000, "font looks truncated: {} bytes", FALLBACK_FONT.len());
        // sfnt version 0x00010000 for TrueType outlines.
        assert_eq!(&FALLBACK_FONT[0..4], &[0x00, 0x01, 0x00, 0x00]);
    }
}
