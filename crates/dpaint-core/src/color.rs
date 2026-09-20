//! Color. One type end to end, serialized as a hex string, computed in linear light.
//!
//! Compositing happens in linear f32 because blend math in gamma space is simply wrong:
//! a 50% blend of black and white in sRGB gives 0.5 (≈188 in 8-bit), not the 0.216 that
//! light actually produces.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Non-premultiplied sRGB color with alpha, stored 0..=1.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
    pub const TRANSPARENT: Color = Color { r: 0.0, g: 0.0, b: 0.0, a: 0.0 };
    pub const BLACK: Color = Color { r: 0.0, g: 0.0, b: 0.0, a: 1.0 };
    pub const WHITE: Color = Color { r: 1.0, g: 1.0, b: 1.0, a: 1.0 };

    pub fn rgba(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self { r, g, b, a }
    }

    pub fn rgb8(r: u8, g: u8, b: u8) -> Self {
        Self { r: r as f32 / 255.0, g: g as f32 / 255.0, b: b as f32 / 255.0, a: 1.0 }
    }

    /// Accepts `#rgb`, `#rgba`, `#rrggbb`, `#rrggbbaa`, with or without the `#`.
    pub fn parse(s: &str) -> Option<Self> {
        let h = s.trim().trim_start_matches('#');
        let n = |i: usize, w: usize| -> Option<f32> {
            let part = h.get(i..i + w)?;
            let v = u8::from_str_radix(part, 16).ok()?;
            Some(if w == 1 { (v * 17) as f32 / 255.0 } else { v as f32 / 255.0 })
        };
        match h.len() {
            3 => Some(Self { r: n(0, 1)?, g: n(1, 1)?, b: n(2, 1)?, a: 1.0 }),
            4 => Some(Self { r: n(0, 1)?, g: n(1, 1)?, b: n(2, 1)?, a: n(3, 1)? }),
            6 => Some(Self { r: n(0, 2)?, g: n(2, 2)?, b: n(4, 2)?, a: 1.0 }),
            8 => Some(Self { r: n(0, 2)?, g: n(2, 2)?, b: n(4, 2)?, a: n(6, 2)? }),
            _ => None,
        }
    }

    pub fn to_hex(self) -> String {
        let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        if self.a >= 1.0 {
            format!("#{:02x}{:02x}{:02x}", q(self.r), q(self.g), q(self.b))
        } else {
            format!("#{:02x}{:02x}{:02x}{:02x}", q(self.r), q(self.g), q(self.b), q(self.a))
        }
    }

    pub fn to_rgba8(self) -> [u8; 4] {
        let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        [q(self.r), q(self.g), q(self.b), q(self.a)]
    }

    /// sRGB -> linear light, per IEC 61966-2-1.
    pub fn to_linear(self) -> [f32; 4] {
        [srgb_to_linear(self.r), srgb_to_linear(self.g), srgb_to_linear(self.b), self.a]
    }

    pub fn from_linear(v: [f32; 4]) -> Self {
        Self {
            r: linear_to_srgb(v[0]),
            g: linear_to_srgb(v[1]),
            b: linear_to_srgb(v[2]),
            a: v[3],
        }
    }

    /// WCAG relative luminance.
    pub fn luminance(self) -> f32 {
        let l = self.to_linear();
        0.2126 * l[0] + 0.7152 * l[1] + 0.0722 * l[2]
    }

    /// WCAG 2.x contrast ratio, 1.0..=21.0. Lint uses this to judge legibility.
    pub fn contrast_ratio(self, other: Color) -> f32 {
        let (a, b) = (self.luminance(), other.luminance());
        let (hi, lo) = if a > b { (a, b) } else { (b, a) };
        (hi + 0.05) / (lo + 0.05)
    }

    /// Composite `self` over `bg` assuming both are opaque-background sRGB colors.
    pub fn over(self, bg: Color) -> Color {
        let (s, d) = (self.to_linear(), bg.to_linear());
        let a = s[3] + d[3] * (1.0 - s[3]);
        if a <= f32::EPSILON {
            return Color::TRANSPARENT;
        }
        let f = |i: usize| (s[i] * s[3] + d[i] * d[3] * (1.0 - s[3])) / a;
        Color::from_linear([f(0), f(1), f(2), a])
    }

    /// CIE Lab, D65.
    pub fn to_lab(self) -> [f32; 3] {
        let l = self.to_linear();
        let (x, y, z) = (
            0.4124 * l[0] + 0.3576 * l[1] + 0.1805 * l[2],
            0.2126 * l[0] + 0.7152 * l[1] + 0.0722 * l[2],
            0.0193 * l[0] + 0.1192 * l[1] + 0.9505 * l[2],
        );
        let f = |t: f32| {
            if t > 0.008856 {
                t.cbrt()
            } else {
                7.787 * t + 16.0 / 116.0
            }
        };
        let (fx, fy, fz) = (f(x / 0.95047), f(y), f(z / 1.08883));
        [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
    }

    /// CIEDE2000 difference. ~1.0 is the just-noticeable threshold; golden tests use it
    /// instead of byte equality so a one-bit rounding change is not a test failure.
    pub fn delta_e(self, other: Color) -> f32 {
        delta_e2000(self.to_lab(), other.to_lab())
    }
}

pub fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

pub fn linear_to_srgb(c: f32) -> f32 {
    if c <= 0.0031308 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

pub fn delta_e2000(lab1: [f32; 3], lab2: [f32; 3]) -> f32 {
    let (l1, a1, b1) = (lab1[0] as f64, lab1[1] as f64, lab1[2] as f64);
    let (l2, a2, b2) = (lab2[0] as f64, lab2[1] as f64, lab2[2] as f64);
    let c1 = (a1 * a1 + b1 * b1).sqrt();
    let c2 = (a2 * a2 + b2 * b2).sqrt();
    let cbar = (c1 + c2) / 2.0;
    let g = 0.5 * (1.0 - (cbar.powi(7) / (cbar.powi(7) + 25f64.powi(7))).sqrt());
    let (a1p, a2p) = (a1 * (1.0 + g), a2 * (1.0 + g));
    let c1p = (a1p * a1p + b1 * b1).sqrt();
    let c2p = (a2p * a2p + b2 * b2).sqrt();
    let h = |ap: f64, b: f64| {
        if ap == 0.0 && b == 0.0 {
            0.0
        } else {
            let d = b.atan2(ap).to_degrees();
            if d < 0.0 {
                d + 360.0
            } else {
                d
            }
        }
    };
    let (h1p, h2p) = (h(a1p, b1), h(a2p, b2));
    let dlp = l2 - l1;
    let dcp = c2p - c1p;
    let dhp = if c1p * c2p == 0.0 {
        0.0
    } else if (h2p - h1p).abs() <= 180.0 {
        h2p - h1p
    } else if h2p - h1p > 180.0 {
        h2p - h1p - 360.0
    } else {
        h2p - h1p + 360.0
    };
    let dhp = 2.0 * (c1p * c2p).sqrt() * (dhp.to_radians() / 2.0).sin();
    let lbar = (l1 + l2) / 2.0;
    let cbarp = (c1p + c2p) / 2.0;
    let hbarp = if c1p * c2p == 0.0 {
        h1p + h2p
    } else if (h1p - h2p).abs() <= 180.0 {
        (h1p + h2p) / 2.0
    } else if h1p + h2p < 360.0 {
        (h1p + h2p + 360.0) / 2.0
    } else {
        (h1p + h2p - 360.0) / 2.0
    };
    let t = 1.0 - 0.17 * (hbarp - 30.0).to_radians().cos()
        + 0.24 * (2.0 * hbarp).to_radians().cos()
        + 0.32 * (3.0 * hbarp + 6.0).to_radians().cos()
        - 0.20 * (4.0 * hbarp - 63.0).to_radians().cos();
    let sl = 1.0 + (0.015 * (lbar - 50.0).powi(2)) / (20.0 + (lbar - 50.0).powi(2)).sqrt();
    let sc = 1.0 + 0.045 * cbarp;
    let sh = 1.0 + 0.015 * cbarp * t;
    let rt = -2.0
        * (cbarp.powi(7) / (cbarp.powi(7) + 25f64.powi(7))).sqrt()
        * (60.0 * (-((hbarp - 275.0) / 25.0).powi(2)).exp()).to_radians().sin();
    (((dlp / sl).powi(2) + (dcp / sc).powi(2) + (dhp / sh).powi(2))
        + rt * (dcp / sc) * (dhp / sh))
        .max(0.0)
        .sqrt() as f32
}

impl Serialize for Color {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for Color {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Color::parse(&s).ok_or_else(|| serde::de::Error::custom(format!("bad color '{s}'")))
    }
}

impl schemars::JsonSchema for Color {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Color".into()
    }
    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "description": "hex color: #rgb, #rgba, #rrggbb or #rrggbbaa",
            "pattern": r"^#?([0-9a-fA-F]{3,4}|[0-9a-fA-F]{6}|[0-9a-fA-F]{8})$"
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ColorSpace {
    #[default]
    Srgb,
    DisplayP3,
    LinearSrgb,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trips_in_every_accepted_form() {
        assert_eq!(Color::parse("#f80").unwrap().to_hex(), "#ff8800");
        assert_eq!(Color::parse("#fb8500").unwrap().to_hex(), "#fb8500");
        assert_eq!(Color::parse("fb850080").unwrap().to_hex(), "#fb850080");
        assert!(Color::parse("#xyz").is_none());
    }

    #[test]
    fn contrast_ratio_matches_wcag_anchors() {
        let r = Color::WHITE.contrast_ratio(Color::BLACK);
        assert!((r - 21.0).abs() < 0.01, "white on black must be 21:1, got {r}");
        assert!((Color::WHITE.contrast_ratio(Color::WHITE) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn blending_happens_in_linear_light_not_gamma_space() {
        let half_white = Color::rgba(1.0, 1.0, 1.0, 0.5);
        let mixed = half_white.over(Color::BLACK);
        // Gamma-space averaging would give 0.5 (#808080). Linear light gives ~0.735.
        assert!(mixed.r > 0.70 && mixed.r < 0.76, "got {}", mixed.r);
    }

    #[test]
    fn delta_e_is_zero_for_identical_and_large_for_opposite() {
        assert!(Color::WHITE.delta_e(Color::WHITE) < 1e-4);
        assert!(Color::WHITE.delta_e(Color::BLACK) > 90.0);
    }
}
