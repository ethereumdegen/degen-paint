//! CPU rasterizer for model previews.
//!
//! Why not a GPU: an agent's turntable has to render identically on a laptop, in CI, and in a
//! container with no display. A small deterministic z-buffer rasterizer gives byte-identical
//! output everywhere, which is what golden tests and perceptual diffs require. The interactive
//! Tauri viewport is where `wgpu` belongs.

use dpaint_core::{Color, Result};
use tiny_skia::Pixmap;

pub struct Mesh {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub indices: Vec<u32>,
    /// Column-major world matrix, glTF convention.
    pub world: [[f32; 4]; 4],
    pub base_color: Color,
    pub metallic: f32,
    pub roughness: f32,
}

#[derive(Debug, Clone, Copy)]
pub struct Camera {
    /// Azimuth in degrees around the up axis.
    pub yaw: f32,
    /// Elevation in degrees.
    pub pitch: f32,
    /// Distance multiplier applied to the fitted radius.
    pub zoom: f32,
    pub yfov: f32,
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            yaw: 35.0,
            pitch: 20.0,
            zoom: 1.0,
            yfov: 0.6,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Lighting {
    pub key: [f32; 3],
    pub fill: [f32; 3],
    pub ambient: f32,
}

impl Default for Lighting {
    fn default() -> Self {
        Self {
            key: [0.5, 0.8, 0.6],
            fill: [-0.6, 0.3, -0.4],
            ambient: 0.22,
        }
    }
}

fn norm(v: [f32; 3]) -> [f32; 3] {
    let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if l <= f32::EPSILON {
        [0.0, 0.0, 1.0]
    } else {
        [v[0] / l, v[1] / l, v[2] / l]
    }
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

/// Column-major matrix times point.
fn xform_point(m: &[[f32; 4]; 4], p: [f32; 3]) -> [f32; 3] {
    [
        m[0][0] * p[0] + m[1][0] * p[1] + m[2][0] * p[2] + m[3][0],
        m[0][1] * p[0] + m[1][1] * p[1] + m[2][1] * p[2] + m[3][1],
        m[0][2] * p[0] + m[1][2] * p[1] + m[2][2] * p[2] + m[3][2],
    ]
}

fn xform_dir(m: &[[f32; 4]; 4], p: [f32; 3]) -> [f32; 3] {
    [
        m[0][0] * p[0] + m[1][0] * p[1] + m[2][0] * p[2],
        m[0][1] * p[0] + m[1][1] * p[1] + m[2][1] * p[2],
        m[0][2] * p[0] + m[1][2] * p[1] + m[2][2] * p[2],
    ]
}

/// Axis-aligned bounds of everything drawable, in world space.
pub fn bounds(meshes: &[Mesh]) -> ([f32; 3], [f32; 3]) {
    let mut lo = [f32::INFINITY; 3];
    let mut hi = [f32::NEG_INFINITY; 3];
    for m in meshes {
        for p in &m.positions {
            let w = xform_point(&m.world, *p);
            for i in 0..3 {
                lo[i] = lo[i].min(w[i]);
                hi[i] = hi[i].max(w[i]);
            }
        }
    }
    if lo[0] > hi[0] {
        (([0.0; 3]), ([0.0; 3]))
    } else {
        (lo, hi)
    }
}

/// Render meshes to a pixmap. Deterministic: fixed traversal order, no threading, no clock.
pub fn render(
    meshes: &[Mesh],
    width: u32,
    height: u32,
    cam: Camera,
    light: Lighting,
    background: Option<Color>,
) -> Result<Pixmap> {
    let mut pm = Pixmap::new(width.max(1), height.max(1))
        .ok_or_else(|| dpaint_core::Error::Invalid("preview size must be positive".into()))?;
    if let Some(bg) = background {
        let c = bg.to_rgba8();
        pm.fill(tiny_skia::Color::from_rgba8(c[0], c[1], c[2], c[3]));
    }
    if meshes.is_empty() {
        return Ok(pm);
    }

    let (lo, hi) = bounds(meshes);
    let center = [
        (lo[0] + hi[0]) * 0.5,
        (lo[1] + hi[1]) * 0.5,
        (lo[2] + hi[2]) * 0.5,
    ];
    let radius = (0..3)
        .map(|i| (hi[i] - lo[i]) * 0.5)
        .fold(0.0f32, f32::max)
        .max(1e-4);

    // Frame the subject: distance from the fitted sphere and the vertical field of view.
    let dist = (radius / (cam.yfov * 0.5).tan()) * 1.6 * cam.zoom.max(0.05);
    let (sy, cy) = cam.yaw.to_radians().sin_cos();
    let (sp, cp) = cam.pitch.to_radians().sin_cos();
    let eye = [
        center[0] + dist * cp * sy,
        center[1] + dist * sp,
        center[2] + dist * cp * cy,
    ];

    let forward = norm(sub(center, eye));
    let right = norm(cross(forward, [0.0, 1.0, 0.0]));
    let up = cross(right, forward);

    let aspect = width as f32 / height as f32;
    let f = 1.0 / (cam.yfov * 0.5).tan();
    let (znear, zfar) = (dist - radius * 4.0, dist + radius * 8.0);
    let znear = znear.max(radius * 0.01);

    let key = norm(light.key);
    let fill = norm(light.fill);

    let mut depth = vec![f32::INFINITY; (width * height) as usize];
    let mut color: Vec<[f32; 3]> = vec![[0.0; 3]; (width * height) as usize];
    let mut covered = vec![false; (width * height) as usize];

    for mesh in meshes {
        let base = mesh.base_color.to_linear();
        for tri in mesh.indices.chunks_exact(3) {
            let vs: Vec<[f32; 3]> = tri
                .iter()
                .map(|&i| xform_point(&mesh.world, mesh.positions[i as usize]))
                .collect();
            let ns: Vec<[f32; 3]> = tri
                .iter()
                .map(|&i| {
                    mesh.normals
                        .get(i as usize)
                        .map(|n| norm(xform_dir(&mesh.world, *n)))
                        .unwrap_or([0.0, 1.0, 0.0])
                })
                .collect();

            // World -> view -> clip -> screen.
            let mut screen = [[0.0f32; 3]; 3];
            let mut behind = false;
            for (k, v) in vs.iter().enumerate() {
                let rel = sub(*v, eye);
                let view = [dot(rel, right), dot(rel, up), dot(rel, forward)];
                if view[2] <= znear {
                    behind = true;
                    break;
                }
                let ndc_x = (view[0] * f / aspect) / view[2];
                let ndc_y = (view[1] * f) / view[2];
                screen[k] = [
                    (ndc_x * 0.5 + 0.5) * width as f32,
                    (1.0 - (ndc_y * 0.5 + 0.5)) * height as f32,
                    view[2],
                ];
            }
            if behind {
                continue;
            }

            let area = (screen[1][0] - screen[0][0]) * (screen[2][1] - screen[0][1])
                - (screen[2][0] - screen[0][0]) * (screen[1][1] - screen[0][1]);
            if area.abs() < 1e-7 {
                continue;
            }

            let min_x = screen
                .iter()
                .map(|p| p[0])
                .fold(f32::INFINITY, f32::min)
                .floor()
                .max(0.0) as u32;
            let max_x = (screen
                .iter()
                .map(|p| p[0])
                .fold(f32::NEG_INFINITY, f32::max)
                .ceil())
            .min(width as f32 - 1.0);
            let min_y = screen
                .iter()
                .map(|p| p[1])
                .fold(f32::INFINITY, f32::min)
                .floor()
                .max(0.0) as u32;
            let max_y = (screen
                .iter()
                .map(|p| p[1])
                .fold(f32::NEG_INFINITY, f32::max)
                .ceil())
            .min(height as f32 - 1.0);
            if max_x < 0.0 || max_y < 0.0 {
                continue;
            }

            for y in min_y..=(max_y as u32) {
                for x in min_x..=(max_x as u32) {
                    let px = x as f32 + 0.5;
                    let py = y as f32 + 0.5;
                    let w0 = ((screen[1][0] - screen[0][0]) * (py - screen[0][1])
                        - (px - screen[0][0]) * (screen[1][1] - screen[0][1]))
                        / area;
                    let w1 = ((px - screen[0][0]) * (screen[2][1] - screen[0][1])
                        - (screen[2][0] - screen[0][0]) * (py - screen[0][1]))
                        / area;
                    let w2 = 1.0 - w0 - w1;
                    if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                        continue;
                    }
                    // Perspective-correct depth.
                    let inv_z = w2 / screen[0][2] + w1 / screen[1][2] + w0 / screen[2][2];
                    if inv_z <= 0.0 {
                        continue;
                    }
                    let z = 1.0 / inv_z;
                    if z > zfar {
                        continue;
                    }
                    let idx = (y * width + x) as usize;
                    if z >= depth[idx] {
                        continue;
                    }

                    let n = norm([
                        ns[0][0] * w2 + ns[1][0] * w1 + ns[2][0] * w0,
                        ns[0][1] * w2 + ns[1][1] * w1 + ns[2][1] * w0,
                        ns[0][2] * w2 + ns[1][2] * w1 + ns[2][2] * w0,
                    ]);
                    let n = if dot(n, forward) > 0.0 {
                        [-n[0], -n[1], -n[2]]
                    } else {
                        n
                    };

                    let diffuse = dot(n, key).max(0.0) + 0.35 * dot(n, fill).max(0.0);
                    let view_dir = [-forward[0], -forward[1], -forward[2]];
                    let half = norm([
                        key[0] + view_dir[0],
                        key[1] + view_dir[1],
                        key[2] + view_dir[2],
                    ]);
                    let shininess =
                        (2.0 / (mesh.roughness.clamp(0.03, 1.0).powi(4)) - 2.0).clamp(1.0, 4096.0);
                    let spec = dot(n, half).max(0.0).powf(shininess)
                        * (0.04 + 0.96 * mesh.metallic)
                        * (1.0 - mesh.roughness * 0.7);

                    let lit = |c: f32| c * (light.ambient + diffuse) + spec;
                    depth[idx] = z;
                    covered[idx] = true;
                    color[idx] = [lit(base[0]), lit(base[1]), lit(base[2])];
                }
            }
        }
    }

    // The background was already filled above, as sRGB bytes straight into the pixmap.
    // Do not "improve" this into a linear-light fill: the GPU viewport matches this
    // behavior exactly, and dpaint-gpu's empty-scene parity test fails if it changes.
    for (i, px) in pm.pixels_mut().iter_mut().enumerate() {
        if !covered[i] {
            continue;
        }
        let c = Color::from_linear([color[i][0], color[i][1], color[i][2], 1.0]);
        let rgba = c.to_rgba8();
        *px = tiny_skia::PremultipliedColorU8::from_rgba(rgba[0], rgba[1], rgba[2], 255)
            .unwrap_or_else(|| tiny_skia::PremultipliedColorU8::from_rgba(0, 0, 0, 255).unwrap());
    }
    Ok(pm)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit_cube() -> Mesh {
        // 8 corners, 12 triangles, outward normals approximated per corner.
        let p = vec![
            [-1.0, -1.0, -1.0],
            [1.0, -1.0, -1.0],
            [1.0, 1.0, -1.0],
            [-1.0, 1.0, -1.0],
            [-1.0, -1.0, 1.0],
            [1.0, -1.0, 1.0],
            [1.0, 1.0, 1.0],
            [-1.0, 1.0, 1.0],
        ];
        let n: Vec<[f32; 3]> = p.iter().map(|v| norm(*v)).collect();
        let i = vec![
            0, 2, 1, 0, 3, 2, // back
            4, 5, 6, 4, 6, 7, // front
            0, 1, 5, 0, 5, 4, // bottom
            3, 7, 6, 3, 6, 2, // top
            0, 4, 7, 0, 7, 3, // left
            1, 2, 6, 1, 6, 5, // right
        ];
        Mesh {
            positions: p,
            normals: n,
            indices: i,
            world: [
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ],
            base_color: Color::parse("#d4af37").unwrap(),
            metallic: 1.0,
            roughness: 0.3,
        }
    }

    fn coverage(pm: &Pixmap) -> f32 {
        let n = pm.pixels().iter().filter(|p| p.alpha() > 0).count();
        n as f32 / (pm.width() * pm.height()) as f32
    }

    #[test]
    fn a_cube_renders_shaded_pixels_in_the_middle_of_the_frame() {
        let pm = render(
            &[unit_cube()],
            64,
            64,
            Camera::default(),
            Lighting::default(),
            None,
        )
        .unwrap();
        let cov = coverage(&pm);
        assert!(
            cov > 0.15 && cov < 0.8,
            "cube should fill part of the frame, covered {cov}"
        );
        let center = pm.pixel(32, 32).unwrap();
        assert!(center.alpha() > 0, "the subject must be centered in frame");
    }

    #[test]
    fn rotating_the_camera_changes_the_image_but_not_the_silhouette_size() {
        let a = render(
            &[unit_cube()],
            64,
            64,
            Camera {
                yaw: 0.0,
                ..Camera::default()
            },
            Lighting::default(),
            None,
        )
        .unwrap();
        let b = render(
            &[unit_cube()],
            64,
            64,
            Camera {
                yaw: 90.0,
                ..Camera::default()
            },
            Lighting::default(),
            None,
        )
        .unwrap();
        assert_ne!(
            a.data(),
            b.data(),
            "a 90 degree turn must change the render"
        );
        assert!(
            (coverage(&a) - coverage(&b)).abs() < 0.08,
            "a cube is symmetric under a quarter turn"
        );
    }

    #[test]
    fn rendering_is_deterministic_across_runs() {
        let a = render(
            &[unit_cube()],
            48,
            48,
            Camera::default(),
            Lighting::default(),
            None,
        )
        .unwrap();
        let b = render(
            &[unit_cube()],
            48,
            48,
            Camera::default(),
            Lighting::default(),
            None,
        )
        .unwrap();
        assert_eq!(
            a.data(),
            b.data(),
            "golden tests depend on byte-identical repeats"
        );
    }

    #[test]
    fn nearer_geometry_occludes_farther_geometry() {
        let mut near = unit_cube();
        near.base_color = Color::parse("#ff0000").unwrap();
        near.world[3][2] = 3.0; // shifted toward the camera along +Z
        let mut far = unit_cube();
        far.base_color = Color::parse("#0000ff").unwrap();
        far.world[3][2] = -3.0;

        let pm = render(
            &[far, near],
            64,
            64,
            Camera {
                yaw: 0.0,
                pitch: 0.0,
                ..Camera::default()
            },
            Lighting {
                ambient: 1.0,
                key: [0.0, 0.0, 1.0],
                fill: [0.0, 0.0, 1.0],
            },
            None,
        )
        .unwrap();
        let c = pm.pixel(32, 32).unwrap();
        assert!(
            c.red() > c.blue(),
            "the near red cube must win the depth test, got {c:?}"
        );
    }

    #[test]
    fn an_empty_scene_produces_the_background_and_no_geometry() {
        let pm = render(
            &[],
            16,
            16,
            Camera::default(),
            Lighting::default(),
            Some(Color::WHITE),
        )
        .unwrap();
        assert_eq!(coverage(&pm), 1.0);
        assert_eq!(pm.pixel(8, 8).unwrap().red(), 255);
    }
}
