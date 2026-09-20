//! Procedural primitives.
//!
//! Every primitive is centred on the origin and inscribed in `size` (its full extents),
//! carries outward unit normals and UVs inside `[0,1]`.

use crate::geom::{cross, normalize, sub, MeshData};
use dpaint_core::doc::model::Primitive;
use dpaint_core::{Error, Result};

/// Build a primitive.
///
/// `size` is the full extent on each axis. `segments` is the angular/grid resolution and is
/// ignored by `Box`, whose six flat faces gain nothing from subdivision.
pub fn primitive(shape: Primitive, size: [f32; 3], segments: u32) -> Result<MeshData> {
    if size.iter().any(|v| !v.is_finite() || *v <= 0.0) {
        return Err(Error::DegenerateGeometry(format!(
            "primitive size must be positive and finite, got {size:?}"
        )));
    }
    let seg = segments.clamp(3, 512);
    Ok(match shape {
        Primitive::Box => box_mesh(size),
        Primitive::Plane => plane(size, segments.clamp(1, 512)),
        Primitive::Sphere => sphere(size, seg),
        Primitive::Cylinder => cylinder(size, seg),
        Primitive::Cone => cone(size, seg),
        Primitive::Torus => torus(size, seg),
        Primitive::Capsule => capsule(size, seg),
    })
}

/// 24 vertices (four per face, so each face keeps its own normal and UV square) and
/// 12 triangles.
fn box_mesh(size: [f32; 3]) -> MeshData {
    let (hx, hy, hz) = (size[0] * 0.5, size[1] * 0.5, size[2] * 0.5);
    // (normal, u axis, v axis) per face; corners are origin + u*±1 + v*±1 scaled by halves.
    let faces: [([f32; 3], [f32; 3], [f32; 3]); 6] = [
        ([0.0, 0.0, 1.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
        ([0.0, 0.0, -1.0], [-1.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
        ([1.0, 0.0, 0.0], [0.0, 0.0, -1.0], [0.0, 1.0, 0.0]),
        ([-1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 1.0, 0.0]),
        ([0.0, 1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, -1.0]),
        ([0.0, -1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]),
    ];
    let h = [hx, hy, hz];
    let mut m = MeshData::default();
    for (n, u, v) in faces {
        let base = m.positions.len() as u32;
        for (su, sv, uv) in [
            (-1.0f32, -1.0f32, [0.0f32, 1.0f32]),
            (1.0, -1.0, [1.0, 1.0]),
            (1.0, 1.0, [1.0, 0.0]),
            (-1.0, 1.0, [0.0, 0.0]),
        ] {
            let p = [
                (n[0] + su * u[0] + sv * v[0]) * h[0],
                (n[1] + su * u[1] + sv * v[1]) * h[1],
                (n[2] + su * u[2] + sv * v[2]) * h[2],
            ];
            m.positions.push(p);
            m.normals.push(n);
            m.uvs.push(uv);
        }
        m.indices
            .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    m
}

/// A grid in the XZ plane facing `+Y`.
fn plane(size: [f32; 3], seg: u32) -> MeshData {
    let (hx, hz) = (size[0] * 0.5, size[2] * 0.5);
    let mut m = MeshData::default();
    for i in 0..=seg {
        for j in 0..=seg {
            let (u, v) = (j as f32 / seg as f32, i as f32 / seg as f32);
            m.positions.push([-hx + u * size[0], 0.0, -hz + v * size[2]]);
            m.normals.push([0.0, 1.0, 0.0]);
            m.uvs.push([u, v]);
        }
    }
    let stride = seg + 1;
    for i in 0..seg {
        for j in 0..seg {
            let a = i * stride + j;
            let (b, c, d) = (a + 1, a + stride + 1, a + stride);
            // Counter-clockwise seen from +Y.
            m.indices.extend_from_slice(&[a, c, b, a, d, c]);
        }
    }
    m
}

/// UV sphere; `size` gives the ellipsoid diameters.
fn sphere(size: [f32; 3], seg: u32) -> MeshData {
    let (rx, ry, rz) = (size[0] * 0.5, size[1] * 0.5, size[2] * 0.5);
    let sectors = seg;
    let stacks = (seg / 2).max(2);
    let mut m = MeshData::default();
    for i in 0..=stacks {
        let v = i as f32 / stacks as f32;
        let phi = std::f32::consts::PI * (0.5 - v); // +pi/2 at the north pole
        let (sp, cp) = (phi.sin(), phi.cos());
        for j in 0..=sectors {
            let u = j as f32 / sectors as f32;
            let theta = std::f32::consts::TAU * u;
            let (st, ct) = (theta.sin(), theta.cos());
            let p = [rx * cp * ct, ry * sp, rz * cp * st];
            m.positions.push(p);
            m.normals
                .push(normalize([p[0] / (rx * rx), p[1] / (ry * ry), p[2] / (rz * rz)]));
            m.uvs.push([u, v]);
        }
    }
    let stride = sectors + 1;
    for i in 0..stacks {
        for j in 0..sectors {
            let a = i * stride + j;
            let (b, c, d) = (a + 1, a + stride + 1, a + stride);
            if i != 0 {
                m.indices.extend_from_slice(&[a, b, d]);
            }
            if i != stacks - 1 {
                m.indices.extend_from_slice(&[b, c, d]);
            }
        }
    }
    m
}

/// Ring of side vertices plus two capped discs.
fn cylinder(size: [f32; 3], seg: u32) -> MeshData {
    let (rx, rz, hy) = (size[0] * 0.5, size[2] * 0.5, size[1] * 0.5);
    let mut m = MeshData::default();
    // Side.
    for i in 0..=seg {
        let u = i as f32 / seg as f32;
        let theta = std::f32::consts::TAU * u;
        let (st, ct) = (theta.sin(), theta.cos());
        let n = normalize([ct / rx, 0.0, st / rz]);
        for (k, y) in [(0u32, -hy), (1, hy)] {
            m.positions.push([rx * ct, y, rz * st]);
            m.normals.push(n);
            m.uvs.push([u, 1.0 - k as f32]);
        }
    }
    for i in 0..seg {
        let a = i * 2;
        m.indices
            .extend_from_slice(&[a, a + 1, a + 2, a + 1, a + 3, a + 2]);
    }
    disc(&mut m, rx, rz, hy, seg, true);
    disc(&mut m, rx, rz, -hy, seg, false);
    m
}

fn disc(m: &mut MeshData, rx: f32, rz: f32, y: f32, seg: u32, up: bool) {
    let base = m.positions.len() as u32;
    let n = if up { [0.0, 1.0, 0.0] } else { [0.0, -1.0, 0.0] };
    m.positions.push([0.0, y, 0.0]);
    m.normals.push(n);
    m.uvs.push([0.5, 0.5]);
    for i in 0..=seg {
        let theta = std::f32::consts::TAU * (i as f32 / seg as f32);
        let (st, ct) = (theta.sin(), theta.cos());
        m.positions.push([rx * ct, y, rz * st]);
        m.normals.push(n);
        m.uvs.push([0.5 + 0.5 * ct, 0.5 + 0.5 * st]);
    }
    for i in 0..seg {
        let (a, b) = (base + 1 + i, base + 2 + i);
        if up {
            m.indices.extend_from_slice(&[base, b, a]);
        } else {
            m.indices.extend_from_slice(&[base, a, b]);
        }
    }
}

/// Apex at `+size.y/2`, base disc at `-size.y/2`.
fn cone(size: [f32; 3], seg: u32) -> MeshData {
    let (rx, rz, h) = (size[0] * 0.5, size[2] * 0.5, size[1]);
    let hy = h * 0.5;
    let mut m = MeshData::default();
    for i in 0..=seg {
        let u = i as f32 / seg as f32;
        let theta = std::f32::consts::TAU * u;
        let (st, ct) = (theta.sin(), theta.cos());
        let n = normalize([h * rz * ct, rx * rz, h * rx * st]);
        m.positions.push([rx * ct, -hy, rz * st]);
        m.normals.push(n);
        m.uvs.push([u, 1.0]);
        m.positions.push([0.0, hy, 0.0]);
        m.normals.push(n);
        m.uvs.push([u, 0.0]);
    }
    for i in 0..seg {
        let a = i * 2;
        m.indices.extend_from_slice(&[a, a + 1, a + 2]);
    }
    disc(&mut m, rx, rz, -hy, seg, false);
    m
}

/// `size.x` is the centreline diameter; the tube diameter is `size.y / 2`.
fn torus(size: [f32; 3], seg: u32) -> MeshData {
    let major = size[0] * 0.5;
    let minor = size[1] * 0.25;
    let rings = seg;
    let sides = (seg / 2).max(3);
    let mut m = MeshData::default();
    for i in 0..=rings {
        let u = i as f32 / rings as f32;
        let theta = std::f32::consts::TAU * u;
        let (st, ct) = (theta.sin(), theta.cos());
        for j in 0..=sides {
            let v = j as f32 / sides as f32;
            let phi = std::f32::consts::TAU * v;
            let (sp, cp) = (phi.sin(), phi.cos());
            m.positions.push([
                (major + minor * cp) * ct,
                minor * sp,
                (major + minor * cp) * st,
            ]);
            m.normals.push([cp * ct, sp, cp * st]);
            m.uvs.push([u, v]);
        }
    }
    let stride = sides + 1;
    for i in 0..rings {
        for j in 0..sides {
            let a = i * stride + j;
            let (b, c, d) = (a + 1, a + stride + 1, a + stride);
            m.indices.extend_from_slice(&[a, b, d, b, c, d]);
        }
    }
    m
}

/// Cylinder of radius `size.x/2` with hemispherical caps; `size.y` is the total height.
fn capsule(size: [f32; 3], seg: u32) -> MeshData {
    let r = size[0] * 0.5;
    let total = size[1].max(size[0]);
    let cyl = (total - 2.0 * r).max(0.0);
    let sectors = seg;
    let half = (seg / 4).max(2);
    let mut m = MeshData::default();
    let ring = |phi: f32, y_off: f32, v: f32, m: &mut MeshData| {
        let (sp, cp) = (phi.sin(), phi.cos());
        for j in 0..=sectors {
            let u = j as f32 / sectors as f32;
            let theta = std::f32::consts::TAU * u;
            let (st, ct) = (theta.sin(), theta.cos());
            m.positions.push([r * cp * ct, r * sp + y_off, r * cp * st]);
            m.normals.push([cp * ct, sp, cp * st]);
            m.uvs.push([u, v]);
        }
    };
    let rows = 2 * half + 2;
    let mut row = 0.0f32;
    // North cap, from the pole down to the equator.
    for i in 0..=half {
        let phi = std::f32::consts::FRAC_PI_2 * (1.0 - i as f32 / half as f32);
        ring(phi, cyl * 0.5, row / (rows - 1) as f32, &mut m);
        row += 1.0;
    }
    // South cap.
    for i in 0..=half {
        let phi = -std::f32::consts::FRAC_PI_2 * (i as f32 / half as f32);
        ring(phi, -cyl * 0.5, row / (rows - 1) as f32, &mut m);
        row += 1.0;
    }
    let stride = sectors + 1;
    let rings_total = 2 * half + 2;
    for i in 0..rings_total - 1 {
        for j in 0..sectors {
            let a = i * stride + j;
            let (b, c, d) = (a + 1, a + stride + 1, a + stride);
            m.indices.extend_from_slice(&[a, b, d, b, c, d]);
        }
    }
    m.drop_degenerate(1e-12);
    m
}

/// Outward orientation check: does every triangle normal agree with its own winding?
pub fn winding_matches_normals(m: &MeshData) -> bool {
    m.indices.chunks_exact(3).all(|t| {
        let (a, b, c) = (
            m.positions[t[0] as usize],
            m.positions[t[1] as usize],
            m.positions[t[2] as usize],
        );
        let fnorm = cross(sub(b, a), sub(c, a));
        let vn = m.normals[t[0] as usize];
        crate::geom::dot(fnorm, vn) >= -1e-4
    })
}
