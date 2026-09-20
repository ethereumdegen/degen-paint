//! Camera fitting, ported line-for-line from `dpaint_render::preview3d::render`.
//!
//! The GPU viewport and the CPU turntable must frame the subject identically, so the fit is
//! not re-derived here — it is the same arithmetic in the same order: bbox centre, radius as
//! the largest half-extent, distance `radius / tan(yfov/2) * 1.6 * zoom`, and the same
//! right/up basis. The only addition is `Scene::pan`, which offsets the orbit *target*; with
//! `pan == [0, 0]` the matrices below reproduce `preview3d`'s screen mapping exactly.

use crate::Scene;

pub(crate) fn norm(v: [f32; 3]) -> [f32; 3] {
    let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if l <= f32::EPSILON {
        [0.0, 0.0, 1.0]
    } else {
        [v[0] / l, v[1] / l, v[2] / l]
    }
}

pub(crate) fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

pub(crate) fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

/// Everything the shader needs about the viewpoint for one frame.
pub(crate) struct Frame {
    /// Column-major `proj * view`, ready for `mat4x4<f32>`.
    pub view_proj: [[f32; 4]; 4],
    pub forward: [f32; 3],
    pub key: [f32; 3],
    pub fill: [f32; 3],
    /// `normalize(key + -forward)`, the Blinn half vector `preview3d` builds per mesh.
    pub half_dir: [f32; 3],
    pub ambient: f32,
}

impl Frame {
    pub(crate) fn build(scene: &Scene, size: [u32; 2]) -> Frame {
        let (lo, hi) = scene.bounds();
        let center = [
            (lo[0] + hi[0]) * 0.5,
            (lo[1] + hi[1]) * 0.5,
            (lo[2] + hi[2]) * 0.5,
        ];
        let radius = scene.fit_radius();
        let cam = scene.camera;

        let dist = (radius / (cam.yfov * 0.5).tan()) * 1.6 * cam.zoom.max(0.05);
        let (sy, cy) = cam.yaw.to_radians().sin_cos();
        let (sp, cp) = cam.pitch.to_radians().sin_cos();
        // The orbit offset from target to eye. Panning moves target and eye by the same
        // vector, so this — and therefore the basis below — is unaffected.
        let offset = [dist * cp * sy, dist * sp, dist * cp * cy];

        let forward = norm(sub([0.0; 3], offset));
        let right = norm(cross(forward, [0.0, 1.0, 0.0]));
        let up = cross(right, forward);

        let pan = scene.pan;
        let target = [
            center[0] + (right[0] * pan[0] + up[0] * pan[1]) * radius,
            center[1] + (right[1] * pan[0] + up[1] * pan[1]) * radius,
            center[2] + (right[2] * pan[0] + up[2] * pan[1]) * radius,
        ];
        let eye = [
            target[0] + offset[0],
            target[1] + offset[1],
            target[2] + offset[2],
        ];

        let aspect = size[0].max(1) as f32 / size[1].max(1) as f32;
        let f = 1.0 / (cam.yfov * 0.5).tan();
        let (znear, zfar) = (dist - radius * 4.0, dist + radius * 8.0);
        let znear = znear.max(radius * 0.01);

        // View: rows are the camera basis, +Z is forward, which is what `preview3d`'s
        // `view = (dot(rel, right), dot(rel, up), dot(rel, forward))` computes.
        let view = [
            [right[0], up[0], forward[0], 0.0],
            [right[1], up[1], forward[1], 0.0],
            [right[2], up[2], forward[2], 0.0],
            [-dot(eye, right), -dot(eye, up), -dot(eye, forward), 1.0],
        ];

        // Projection: `w = view.z`, so `ndc.x = view.x * f/aspect / view.z` and
        // `ndc.y = view.y * f / view.z` — identical to the CPU's screen mapping, including
        // the Y flip, since wgpu's NDC and `preview3d`'s `1.0 - (ndc_y * 0.5 + 0.5)` agree.
        // Depth maps [znear, zfar] to wgpu's [0, 1].
        let (a, b) = if (zfar - znear).abs() < f32::EPSILON {
            (1.0, 0.0)
        } else {
            (zfar / (zfar - znear), -znear * zfar / (zfar - znear))
        };
        let proj = [
            [f / aspect, 0.0, 0.0, 0.0],
            [0.0, f, 0.0, 0.0],
            [0.0, 0.0, a, 1.0],
            [0.0, 0.0, b, 0.0],
        ];

        let key = norm(scene.lighting.key);
        let fill = norm(scene.lighting.fill);
        let view_dir = [-forward[0], -forward[1], -forward[2]];
        let half_dir = norm([
            key[0] + view_dir[0],
            key[1] + view_dir[1],
            key[2] + view_dir[2],
        ]);

        Frame {
            view_proj: mul(&proj, &view),
            forward,
            key,
            fill,
            half_dir,
            ambient: scene.lighting.ambient,
        }
    }
}

/// Column-major 4x4 product: `out[col][row] = sum_k a[k][row] * b[col][k]`.
fn mul(a: &[[f32; 4]; 4], b: &[[f32; 4]; 4]) -> [[f32; 4]; 4] {
    let mut out = [[0.0f32; 4]; 4];
    for col in 0..4 {
        for row in 0..4 {
            let mut s = 0.0;
            for k in 0..4 {
                s += a[k][row] * b[col][k];
            }
            out[col][row] = s;
        }
    }
    out
}

/// Column-major matrix times point, matching `preview3d::xform_point`.
pub(crate) fn xform_point(m: &[[f32; 4]; 4], p: [f32; 3]) -> [f32; 3] {
    [
        m[0][0] * p[0] + m[1][0] * p[1] + m[2][0] * p[2] + m[3][0],
        m[0][1] * p[0] + m[1][1] * p[1] + m[2][1] * p[2] + m[3][1],
        m[0][2] * p[0] + m[1][2] * p[1] + m[2][2] * p[2] + m[3][2],
    ]
}
