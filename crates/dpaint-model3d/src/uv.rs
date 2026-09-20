//! UV projection, chart unwrapping and mikktspace tangents.

use crate::geom::{add, cross, dot, length, normalize, pos_key, scale, sub, MeshData, WELD_TOL};
use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, schemars::JsonSchema, Default)]
#[serde(rename_all = "lowercase")]
pub enum Axis {
    X,
    #[default]
    Y,
    Z,
}

impl Axis {
    fn index(self) -> usize {
        match self {
            Axis::X => 0,
            Axis::Y => 1,
            Axis::Z => 2,
        }
    }
}

/// Project every vertex along `axis` and normalize into `[0,1]` over the mesh bounds.
pub fn planar(mesh: &mut MeshData, axis: Axis) {
    let Some((lo, hi)) = mesh.bounds() else {
        return;
    };
    let a = axis.index();
    let (u_ax, v_ax) = ((a + 1) % 3, (a + 2) % 3);
    let (du, dv) = (
        (hi[u_ax] - lo[u_ax]).max(1e-9),
        (hi[v_ax] - lo[v_ax]).max(1e-9),
    );
    mesh.uvs = mesh
        .positions
        .iter()
        .map(|p| [(p[u_ax] - lo[u_ax]) / du, (p[v_ax] - lo[v_ax]) / dv])
        .collect();
}

/// Six-sided box projection: each triangle takes the axis its normal points along most,
/// so nothing stretches across a corner. Vertices are re-emitted per corner.
pub fn box_project(mesh: &mut MeshData) {
    let Some((lo, hi)) = mesh.bounds() else {
        return;
    };
    let ext = [
        (hi[0] - lo[0]).max(1e-9),
        (hi[1] - lo[1]).max(1e-9),
        (hi[2] - lo[2]).max(1e-9),
    ];
    let mut out = MeshData::default();
    let has_n = !mesh.normals.is_empty();
    for t in mesh.indices.chunks_exact(3) {
        let ps = [
            mesh.positions[t[0] as usize],
            mesh.positions[t[1] as usize],
            mesh.positions[t[2] as usize],
        ];
        let n = crate::extrude::triangle_normal(ps[0], ps[1], ps[2]);
        let axis = if n[0].abs() >= n[1].abs() && n[0].abs() >= n[2].abs() {
            0
        } else if n[1].abs() >= n[2].abs() {
            1
        } else {
            2
        };
        let (u_ax, v_ax) = ((axis + 1) % 3, (axis + 2) % 3);
        let flip = n[axis] < 0.0;
        let base = out.positions.len() as u32;
        for (k, p) in ps.iter().enumerate() {
            let mut u = (p[u_ax] - lo[u_ax]) / ext[u_ax];
            let v = (p[v_ax] - lo[v_ax]) / ext[v_ax];
            if flip {
                u = 1.0 - u;
            }
            out.positions.push(*p);
            if has_n {
                out.normals.push(mesh.normals[t[k] as usize]);
            }
            out.uvs.push([u, v]);
        }
        out.indices.extend_from_slice(&[base, base + 1, base + 2]);
    }
    *mesh = out;
}

/// Angle-based chart unwrap: split the surface where it folds by more than `angle_deg`,
/// project each chart onto its average plane, then shelf-pack the charts into `[0,1]²`
/// with no overlap.
pub fn unwrap(mesh: &mut MeshData, angle_deg: f32) {
    let faces = mesh.triangle_count();
    if faces == 0 {
        return;
    }
    let cos_limit = angle_deg.clamp(1.0, 179.0).to_radians().cos();

    // Welded vertex ids give us real face adjacency even across duplicated corners.
    let mut wmap: std::collections::HashMap<(i64, i64, i64), u32> =
        std::collections::HashMap::new();
    let welded: Vec<u32> = mesh
        .positions
        .iter()
        .map(|p| {
            let k = pos_key(*p, WELD_TOL);
            let next = wmap.len() as u32;
            *wmap.entry(k).or_insert(next)
        })
        .collect();

    let normals: Vec<[f32; 3]> = mesh
        .triangles()
        .map(|[a, b, c]| crate::extrude::triangle_normal(a, b, c))
        .collect();

    let mut edge_faces: std::collections::HashMap<(u32, u32), Vec<usize>> =
        std::collections::HashMap::new();
    for (f, t) in mesh.indices.chunks_exact(3).enumerate() {
        for k in 0..3 {
            let (a, b) = (welded[t[k] as usize], welded[t[(k + 1) % 3] as usize]);
            edge_faces.entry((a.min(b), a.max(b))).or_default().push(f);
        }
    }

    let mut chart_of = vec![usize::MAX; faces];
    let mut charts: Vec<Vec<usize>> = Vec::new();
    for seed in 0..faces {
        if chart_of[seed] != usize::MAX {
            continue;
        }
        let id = charts.len();
        let mut members = Vec::new();
        let mut stack = vec![seed];
        chart_of[seed] = id;
        while let Some(f) = stack.pop() {
            members.push(f);
            let t = &mesh.indices[f * 3..f * 3 + 3];
            for k in 0..3 {
                let (a, b) = (welded[t[k] as usize], welded[t[(k + 1) % 3] as usize]);
                for &g in edge_faces.get(&(a.min(b), a.max(b))).into_iter().flatten() {
                    if chart_of[g] == usize::MAX && dot(normals[f], normals[g]) >= cos_limit {
                        chart_of[g] = id;
                        stack.push(g);
                    }
                }
            }
        }
        charts.push(members);
    }

    // Project each chart onto its own average plane.
    struct Chart {
        faces: Vec<usize>,
        uv: Vec<[f32; 2]>, // three per face, in face order
        lo: [f32; 2],
        size: [f32; 2],
    }
    let mut built: Vec<Chart> = Vec::with_capacity(charts.len());
    for members in charts {
        let mut avg = [0.0f32; 3];
        for &f in &members {
            avg = add(avg, normals[f]);
        }
        let n = normalize(avg);
        let helper = if n[1].abs() > 0.9 {
            [1.0, 0.0, 0.0]
        } else {
            [0.0, 1.0, 0.0]
        };
        let tan = normalize(cross(helper, n));
        let bit = cross(n, tan);
        let mut uv = Vec::with_capacity(members.len() * 3);
        let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
        for &f in &members {
            for k in 0..3 {
                let p = mesh.positions[mesh.indices[f * 3 + k] as usize];
                let c = [dot(p, tan), dot(p, bit)];
                lo[0] = lo[0].min(c[0]);
                lo[1] = lo[1].min(c[1]);
                hi[0] = hi[0].max(c[0]);
                hi[1] = hi[1].max(c[1]);
                uv.push(c);
            }
        }
        built.push(Chart {
            faces: members,
            uv,
            lo,
            size: [(hi[0] - lo[0]).max(1e-6), (hi[1] - lo[1]).max(1e-6)],
        });
    }

    // Shelf-pack at a uniform scale, binary searching the largest scale that fits.
    let pad = 0.004f32;
    let order: Vec<usize> = {
        let mut o: Vec<usize> = (0..built.len()).collect();
        o.sort_by(|&a, &b| {
            built[b].size[1]
                .partial_cmp(&built[a].size[1])
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.cmp(&b))
        });
        o
    };
    let try_pack = |s: f32| -> Option<Vec<[f32; 2]>> {
        let mut origins = vec![[0.0f32; 2]; built.len()];
        let (mut x, mut y, mut shelf) = (pad, pad, 0.0f32);
        for &i in &order {
            let (w, h) = (built[i].size[0] * s, built[i].size[1] * s);
            if w + 2.0 * pad > 1.0 || h + 2.0 * pad > 1.0 {
                return None;
            }
            if x + w + pad > 1.0 {
                x = pad;
                y += shelf + pad;
                shelf = 0.0;
            }
            if y + h + pad > 1.0 {
                return None;
            }
            origins[i] = [x, y];
            x += w + pad;
            shelf = shelf.max(h);
        }
        Some(origins)
    };
    let (mut lo_s, mut hi_s) = (
        1e-6f32,
        1.0f32
            / built
                .iter()
                .fold(1e-6f32, |a, c| a.max(c.size[0].max(c.size[1]))),
    );
    hi_s = hi_s.max(lo_s * 2.0);
    let mut best = try_pack(lo_s).unwrap_or_default();
    for _ in 0..24 {
        let mid = 0.5 * (lo_s + hi_s);
        match try_pack(mid) {
            Some(o) => {
                best = o;
                lo_s = mid;
            }
            None => hi_s = mid,
        }
    }
    let s = lo_s;

    let has_n = !mesh.normals.is_empty();
    let mut out = MeshData::default();
    for (ci, chart) in built.iter().enumerate() {
        let origin = best.get(ci).copied().unwrap_or([0.0, 0.0]);
        for (fi, &f) in chart.faces.iter().enumerate() {
            let base = out.positions.len() as u32;
            for k in 0..3 {
                let vi = mesh.indices[f * 3 + k] as usize;
                let c = chart.uv[fi * 3 + k];
                out.positions.push(mesh.positions[vi]);
                if has_n {
                    out.normals.push(mesh.normals[vi]);
                }
                out.uvs.push([
                    (origin[0] + (c[0] - chart.lo[0]) * s).clamp(0.0, 1.0),
                    (origin[1] + (c[1] - chart.lo[1]) * s).clamp(0.0, 1.0),
                ]);
            }
            out.indices.extend_from_slice(&[base, base + 1, base + 2]);
        }
    }
    *mesh = out;
}

struct TangentGeometry<'a> {
    mesh: &'a MeshData,
    tangents: Vec<[f32; 4]>,
}

impl mikktspace::Geometry for TangentGeometry<'_> {
    fn num_faces(&self) -> usize {
        self.mesh.triangle_count()
    }

    fn num_vertices_of_face(&self, _face: usize) -> usize {
        3
    }

    fn position(&self, face: usize, vert: usize) -> [f32; 3] {
        self.mesh.positions[self.mesh.indices[face * 3 + vert] as usize]
    }

    fn normal(&self, face: usize, vert: usize) -> [f32; 3] {
        self.mesh.normals[self.mesh.indices[face * 3 + vert] as usize]
    }

    fn tex_coord(&self, face: usize, vert: usize) -> [f32; 2] {
        self.mesh.uvs[self.mesh.indices[face * 3 + vert] as usize]
    }

    fn set_tangent_encoded(&mut self, tangent: [f32; 4], face: usize, vert: usize) {
        let vi = self.mesh.indices[face * 3 + vert] as usize;
        self.tangents[vi] = tangent;
    }
}

/// Per-vertex `(x, y, z, w)` tangents via mikktspace, the reference implementation glTF
/// normal maps are authored against. Falls back to a UV-derived tangent basis when the
/// mesh has no usable normals or UVs.
pub fn tangents(mesh: &MeshData) -> Vec<[f32; 4]> {
    if mesh.positions.is_empty() || mesh.normals.len() != mesh.positions.len() {
        return Vec::new();
    }
    if mesh.uvs.len() != mesh.positions.len() {
        return fallback_tangents(mesh);
    }
    let mut geo = TangentGeometry {
        mesh,
        tangents: vec![[1.0, 0.0, 0.0, 1.0]; mesh.positions.len()],
    };
    if !mikktspace::generate_tangents(&mut geo) {
        return fallback_tangents(mesh);
    }
    let mut out = geo.tangents;
    for (t, n) in out.iter_mut().zip(mesh.normals.iter()) {
        // Re-orthogonalize so the exported tangent is exactly perpendicular to the normal.
        let tv = [t[0], t[1], t[2]];
        let proj = sub(tv, scale(*n, dot(*n, tv)));
        let tv = if length(proj) <= 1e-6 {
            tv
        } else {
            normalize(proj)
        };
        *t = [tv[0], tv[1], tv[2], if t[3] < 0.0 { -1.0 } else { 1.0 }];
    }
    out
}

fn fallback_tangents(mesh: &MeshData) -> Vec<[f32; 4]> {
    mesh.normals
        .iter()
        .map(|n| {
            let helper = if n[1].abs() > 0.9 {
                [1.0, 0.0, 0.0]
            } else {
                [0.0, 1.0, 0.0]
            };
            let t = normalize(cross(helper, *n));
            [t[0], t[1], t[2], 1.0]
        })
        .collect()
}
