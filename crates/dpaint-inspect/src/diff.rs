//! Perceptual diff. Answers two questions an agent cannot answer by looking:
//! "did my edit change only what I intended?" and "has output drifted since the golden?"

use dpaint_core::{Color, Error, Result};
use image::RgbaImage;

#[derive(Debug, Clone, serde::Serialize)]
pub struct Diff {
    pub size: [u32; 2],
    /// Structural similarity, 1.0 = identical.
    pub ssim: f64,
    pub mean_delta_e: f64,
    pub max_delta_e: f64,
    /// Fraction of pixels differing beyond `just_noticeable`.
    pub changed_fraction: f64,
    /// Bounding box of the changed region, `[x, y, w, h]`, absent when nothing changed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changed_bbox: Option<[u32; 4]>,
    pub identical: bool,
}

/// ΔE2000 above which a human would notice.
pub const JUST_NOTICEABLE: f64 = 1.0;

pub fn compare(a: &RgbaImage, b: &RgbaImage) -> Result<Diff> {
    if a.dimensions() != b.dimensions() {
        return Err(Error::Invalid(format!(
            "cannot diff {}x{} against {}x{}",
            a.width(),
            a.height(),
            b.width(),
            b.height()
        )));
    }
    let (w, h) = a.dimensions();
    let n = (w * h) as f64;

    let mut sum_de = 0.0;
    let mut max_de: f64 = 0.0;
    let mut changed = 0u64;
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0u32, 0u32);

    for y in 0..h {
        for x in 0..w {
            let pa = a.get_pixel(x, y).0;
            let pb = b.get_pixel(x, y).0;
            let ca = Color::rgba(
                pa[0] as f32 / 255.0,
                pa[1] as f32 / 255.0,
                pa[2] as f32 / 255.0,
                pa[3] as f32 / 255.0,
            );
            let cb = Color::rgba(
                pb[0] as f32 / 255.0,
                pb[1] as f32 / 255.0,
                pb[2] as f32 / 255.0,
                pb[3] as f32 / 255.0,
            );
            // Alpha difference is a real visual difference; fold it in against the
            // color distance rather than ignoring it.
            let de = ca.delta_e(cb) as f64 + (ca.a - cb.a).abs() as f64 * 100.0;
            sum_de += de;
            max_de = max_de.max(de);
            if de > JUST_NOTICEABLE {
                changed += 1;
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
            }
        }
    }

    Ok(Diff {
        size: [w, h],
        ssim: ssim(a, b),
        mean_delta_e: sum_de / n,
        max_delta_e: max_de,
        changed_fraction: changed as f64 / n,
        changed_bbox: (changed > 0).then(|| [x0, y0, x1 - x0 + 1, y1 - y0 + 1]),
        identical: changed == 0,
    })
}

/// Mean SSIM over 8×8 windows on luma. Local windows are what make SSIM catch a small
/// displaced element that a global metric would average away.
pub fn ssim(a: &RgbaImage, b: &RgbaImage) -> f64 {
    let (w, h) = a.dimensions();
    let luma = |img: &RgbaImage, x: u32, y: u32| -> f64 {
        let p = img.get_pixel(x, y).0;
        let alpha = p[3] as f64 / 255.0;
        // Composite on white so transparent regions compare as the surface they render on.
        let f = |c: u8| (c as f64 * alpha + 255.0 * (1.0 - alpha)) / 255.0;
        0.2126 * f(p[0]) + 0.7152 * f(p[1]) + 0.0722 * f(p[2])
    };

    const C1: f64 = 0.01 * 0.01;
    const C2: f64 = 0.03 * 0.03;
    const WIN: u32 = 8;

    let mut total = 0.0;
    let mut windows = 0.0;
    let mut y = 0;
    while y < h {
        let mut x = 0;
        while x < w {
            let (xe, ye) = ((x + WIN).min(w), (y + WIN).min(h));
            let count = ((xe - x) * (ye - y)) as f64;
            let (mut ma, mut mb) = (0.0, 0.0);
            for yy in y..ye {
                for xx in x..xe {
                    ma += luma(a, xx, yy);
                    mb += luma(b, xx, yy);
                }
            }
            ma /= count;
            mb /= count;
            let (mut va, mut vb, mut cov) = (0.0, 0.0, 0.0);
            for yy in y..ye {
                for xx in x..xe {
                    let da = luma(a, xx, yy) - ma;
                    let db = luma(b, xx, yy) - mb;
                    va += da * da;
                    vb += db * db;
                    cov += da * db;
                }
            }
            va /= count;
            vb /= count;
            cov /= count;
            total += ((2.0 * ma * mb + C1) * (2.0 * cov + C2))
                / ((ma * ma + mb * mb + C1) * (va + vb + C2));
            windows += 1.0;
            x += WIN;
        }
        y += WIN;
    }
    if windows == 0.0 {
        1.0
    } else {
        total / windows
    }
}

/// Heatmap of where two images differ: dark where equal, hot where they diverge.
pub fn heatmap(a: &RgbaImage, b: &RgbaImage) -> Result<RgbaImage> {
    if a.dimensions() != b.dimensions() {
        return Err(Error::Invalid("heatmap needs equal dimensions".into()));
    }
    let (w, h) = a.dimensions();
    let mut out = RgbaImage::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let pa = a.get_pixel(x, y).0;
            let pb = b.get_pixel(x, y).0;
            let ca = Color::rgba(
                pa[0] as f32 / 255.0,
                pa[1] as f32 / 255.0,
                pa[2] as f32 / 255.0,
                pa[3] as f32 / 255.0,
            );
            let cb = Color::rgba(
                pb[0] as f32 / 255.0,
                pb[1] as f32 / 255.0,
                pb[2] as f32 / 255.0,
                pb[3] as f32 / 255.0,
            );
            let de = (ca.delta_e(cb) + (ca.a - cb.a).abs() * 100.0).min(50.0) / 50.0;
            // Black -> red -> yellow, so magnitude is readable at a glance.
            let r = (de * 2.0).min(1.0);
            let g = ((de - 0.5) * 2.0).max(0.0);
            out.put_pixel(
                x,
                y,
                image::Rgba([(r * 255.0) as u8, (g * 255.0) as u8, 0, 255]),
            );
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32, c: [u8; 4]) -> RgbaImage {
        RgbaImage::from_pixel(w, h, image::Rgba(c))
    }

    #[test]
    fn identical_images_score_perfectly() {
        let a = solid(32, 32, [120, 60, 30, 255]);
        let d = compare(&a, &a.clone()).unwrap();
        assert!(d.identical);
        assert_eq!(d.changed_fraction, 0.0);
        assert!((d.ssim - 1.0).abs() < 1e-9, "ssim was {}", d.ssim);
        assert!(d.max_delta_e < 1e-6);
        assert!(d.changed_bbox.is_none());
    }

    #[test]
    fn a_localized_change_is_localized_in_the_report() {
        let a = solid(32, 32, [255, 255, 255, 255]);
        let mut b = a.clone();
        for y in 4..8 {
            for x in 10..14 {
                b.put_pixel(x, y, image::Rgba([0, 0, 0, 255]));
            }
        }
        let d = compare(&a, &b).unwrap();
        assert!(!d.identical);
        assert_eq!(
            d.changed_bbox,
            Some([10, 4, 4, 4]),
            "the bbox must bound exactly the edit"
        );
        assert!((d.changed_fraction - 16.0 / 1024.0).abs() < 1e-9);
        assert!(
            d.ssim < 1.0 && d.ssim > 0.8,
            "a small edit should dent ssim slightly: {}",
            d.ssim
        );
    }

    #[test]
    fn wholesale_inversion_tanks_the_score() {
        let a = solid(32, 32, [0, 0, 0, 255]);
        let b = solid(32, 32, [255, 255, 255, 255]);
        let d = compare(&a, &b).unwrap();
        assert_eq!(d.changed_fraction, 1.0);
        assert!(d.max_delta_e > 90.0);
        assert!(d.ssim < 0.2, "ssim was {}", d.ssim);
    }

    #[test]
    fn alpha_differences_are_not_invisible_to_the_diff() {
        let a = solid(16, 16, [255, 0, 0, 255]);
        let b = solid(16, 16, [255, 0, 0, 0]);
        let d = compare(&a, &b).unwrap();
        assert!(
            !d.identical,
            "a fully transparent copy is not the same image"
        );
    }

    #[test]
    fn mismatched_sizes_are_an_error_rather_than_a_silent_crop() {
        let a = solid(16, 16, [0; 4]);
        let b = solid(16, 8, [0; 4]);
        assert_eq!(compare(&a, &b).unwrap_err().code(), "invalid");
    }

    #[test]
    fn the_heatmap_marks_changed_pixels_and_leaves_the_rest_black() {
        let a = solid(8, 8, [255, 255, 255, 255]);
        let mut b = a.clone();
        b.put_pixel(3, 3, image::Rgba([0, 0, 0, 255]));
        let hm = heatmap(&a, &b).unwrap();
        assert_eq!(hm.get_pixel(0, 0).0[0], 0);
        assert!(hm.get_pixel(3, 3).0[0] > 200);
    }
}
