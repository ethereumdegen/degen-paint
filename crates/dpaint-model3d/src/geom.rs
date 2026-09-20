//! Mesh data and the geometry operators every mesh op is built from.
//!
//! [`MeshData`] is the one interchange shape: parallel attribute arrays plus a triangle index
//! list. Everything else in this crate either produces one (primitives, extrusion, revolve,
//! loft, buffer decode) or consumes one (UV projection, tangents, validation, glTF export).

/// Triangle geometry with per-vertex attributes. Indices are triangle triples.
///
/// `normals` and `uvs` are either empty or exactly as long as `positions`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MeshData {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 2]>,
    pub indices: Vec<u32>,
}

#[inline]
pub fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

#[inline]
pub fn add(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

#[inline]
pub fn scale(a: [f32; 3], s: f32) -> [f32; 3] {
    [a[0] * s, a[1] * s, a[2] * s]
}

#[inline]
pub fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

#[inline]
pub fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

#[inline]
pub fn length(a: [f32; 3]) -> f32 {
    dot(a, a).sqrt()
}

/// Normalize, returning `+Y` for a zero-length vector so downstream math never sees NaN.
#[inline]
pub fn normalize(a: [f32; 3]) -> [f32; 3] {
    let l = length(a);
    if l <= f32::MIN_POSITIVE {
        [0.0, 1.0, 0.0]
    } else {
        [a[0] / l, a[1] / l, a[2] / l]
    }
}

/// Column-major 4x4 multiply: `m[col][row]`, matching the glTF matrix layout.
pub fn mat_mul(a: &[[f32; 4]; 4], b: &[[f32; 4]; 4]) -> [[f32; 4]; 4] {
    let mut out = [[0.0f32; 4]; 4];
    for (c, oc) in out.iter_mut().enumerate() {
        for (r, o) in oc.iter_mut().enumerate() {
            let mut s = 0.0;
            for k in 0..4 {
                s += a[k][r] * b[c][k];
            }
            *o = s;
        }
    }
    out
}

pub const IDENTITY4: [[f32; 4]; 4] = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

/// Column-major TRS composition, `T * R * S` as glTF specifies.
pub fn trs_matrix(t: [f32; 3], r: [f32; 4], s: [f32; 3]) -> [[f32; 4]; 4] {
    let [x, y, z, w] = r;
    let (x2, y2, z2) = (x + x, y + y, z + z);
    let (xx, xy, xz) = (x * x2, x * y2, x * z2);
    let (yy, yz, zz) = (y * y2, y * z2, z * z2);
    let (wx, wy, wz) = (w * x2, w * y2, w * z2);
    [
        [(1.0 - (yy + zz)) * s[0], (xy + wz) * s[0], (xz - wy) * s[0], 0.0],
        [(xy - wz) * s[1], (1.0 - (xx + zz)) * s[1], (yz + wx) * s[1], 0.0],
        [(xz + wy) * s[2], (yz - wx) * s[2], (1.0 - (xx + yy)) * s[2], 0.0],
        [t[0], t[1], t[2], 1.0],
    ]
}

/// Quaternion `(x, y, z, w)` from intrinsic XYZ Euler angles in degrees.
pub fn quat_from_euler_deg(e: [f32; 3]) -> [f32; 4] {
    let (hx, hy, hz) = (
        e[0].to_radians() * 0.5,
        e[1].to_radians() * 0.5,
        e[2].to_radians() * 0.5,
    );
    let (sx, cx) = (hx.sin(), hx.cos());
    let (sy, cy) = (hy.sin(), hy.cos());
    let (sz, cz) = (hz.sin(), hz.cos());
    normalize_quat([
        sx * cy * cz + cx * sy * sz,
        cx * sy * cz - sx * cy * sz,
        cx * cy * sz + sx * sy * cz,
        cx * cy * cz - sx * sy * sz,
    ])
}

pub fn normalize_quat(q: [f32; 4]) -> [f32; 4] {
    let l = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt();
    if l <= f32::MIN_POSITIVE {
        [0.0, 0.0, 0.0, 1.0]
    } else {
        [q[0] / l, q[1] / l, q[2] / l, q[3] / l]
    }
}

/// Rotation taking local `-Z` onto `dir`, with `up` resolving the roll — the glTF convention
/// for cameras and spot lights, and the one `model.node.look-at` uses for meshes too.
pub fn look_rotation(dir: [f32; 3], up: [f32; 3]) -> [f32; 4] {
    let f = normalize(dir);
    let mut u = normalize(up);
    if dot(f, u).abs() > 0.999 {
        u = if f[1].abs() > 0.9 { [0.0, 0.0, 1.0] } else { [0.0, 1.0, 0.0] };
    }
    let s = normalize(cross(f, u));
    let u = cross(s, f);
    // Column-major basis with -Z forward.
    let m = [[s[0], s[1], s[2]], [u[0], u[1], u[2]], [-f[0], -f[1], -f[2]]];
    let trace = m[0][0] + m[1][1] + m[2][2];
    let q = if trace > 0.0 {
        let r = (1.0 + trace).sqrt();
        let inv = 0.5 / r;
        [(m[1][2] - m[2][1]) * inv, (m[2][0] - m[0][2]) * inv, (m[0][1] - m[1][0]) * inv, 0.5 * r]
    } else if m[0][0] > m[1][1] && m[0][0] > m[2][2] {
        let r = (1.0 + m[0][0] - m[1][1] - m[2][2]).sqrt();
        let inv = 0.5 / r;
        [0.5 * r, (m[0][1] + m[1][0]) * inv, (m[2][0] + m[0][2]) * inv, (m[1][2] - m[2][1]) * inv]
    } else if m[1][1] > m[2][2] {
        let r = (1.0 - m[0][0] + m[1][1] - m[2][2]).sqrt();
        let inv = 0.5 / r;
        [(m[0][1] + m[1][0]) * inv, 0.5 * r, (m[1][2] + m[2][1]) * inv, (m[2][0] - m[0][2]) * inv]
    } else {
        let r = (1.0 - m[0][0] - m[1][1] + m[2][2]).sqrt();
        let inv = 0.5 / r;
        [(m[2][0] + m[0][2]) * inv, (m[1][2] + m[2][1]) * inv, 0.5 * r, (m[0][1] - m[1][0]) * inv]
    };
    normalize_quat(q)
}

/// Quantized position key, so coincident vertices produced by independent builders
/// (cap tessellation vs. side walls) hash to the same bucket.
#[inline]
pub fn pos_key(p: [f32; 3], tol: f32) -> (i64, i64, i64) {
    let q = |v: f32| (v as f64 / tol as f64).round() as i64;
    (q(p[0]), q(p[1]), q(p[2]))
}

pub const WELD_TOL: f32 = 1e-5;

impl MeshData {
    pub fn vertex_count(&self) -> usize {
        self.positions.len()
    }

    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }

    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }

    /// Axis-aligned bounds, or `None` for an empty mesh.
    pub fn bounds(&self) -> Option<([f32; 3], [f32; 3])> {
        let mut it = self.positions.iter();
        let first = *it.next()?;
        let (mut lo, mut hi) = (first, first);
        for p in it {
            for i in 0..3 {
                lo[i] = lo[i].min(p[i]);
                hi[i] = hi[i].max(p[i]);
            }
        }
        Some((lo, hi))
    }

    pub fn triangles(&self) -> impl Iterator<Item = [[f32; 3]; 3]> + '_ {
        self.indices.chunks_exact(3).map(move |t| {
            [
                self.positions[t[0] as usize],
                self.positions[t[1] as usize],
                self.positions[t[2] as usize],
            ]
        })
    }

    /// Total triangle area.
    pub fn surface_area(&self) -> f64 {
        self.triangles()
            .map(|[a, b, c]| 0.5 * length(cross(sub(b, a), sub(c, a))) as f64)
            .sum()
    }

    /// Enclosed volume by the divergence theorem. Meaningful only for a closed surface;
    /// the sign is positive when triangles wind counter-clockwise seen from outside.
    pub fn volume(&self) -> f64 {
        self.triangles()
            .map(|[a, b, c]| {
                let a = [a[0] as f64, a[1] as f64, a[2] as f64];
                let b = [b[0] as f64, b[1] as f64, b[2] as f64];
                let c = [c[0] as f64, c[1] as f64, c[2] as f64];
                (a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0])
                    + a[2] * (b[0] * c[1] - b[1] * c[0]))
                    / 6.0
            })
            .sum()
    }

    /// Append `other`, offsetting its indices. Missing attribute arrays are filled so the
    /// parallel-array invariant survives the merge.
    pub fn append(&mut self, other: &MeshData) {
        let base = self.positions.len() as u32;
        if !self.positions.is_empty() {
            if self.normals.is_empty() && !other.normals.is_empty() {
                self.normals = vec![[0.0, 1.0, 0.0]; self.positions.len()];
            }
            if self.uvs.is_empty() && !other.uvs.is_empty() {
                self.uvs = vec![[0.0, 0.0]; self.positions.len()];
            }
        }
        self.positions.extend_from_slice(&other.positions);
        if !self.normals.is_empty() || !other.normals.is_empty() {
            if other.normals.is_empty() {
                self.normals.extend(std::iter::repeat_n([0.0, 1.0, 0.0], other.positions.len()));
            } else {
                self.normals.extend_from_slice(&other.normals);
            }
        }
        if !self.uvs.is_empty() || !other.uvs.is_empty() {
            if other.uvs.is_empty() {
                self.uvs.extend(std::iter::repeat_n([0.0, 0.0], other.positions.len()));
            } else {
                self.uvs.extend_from_slice(&other.uvs);
            }
        }
        self.indices.extend(other.indices.iter().map(|i| i + base));
    }

    /// Bake a column-major transform into the geometry. Normals use the inverse transpose,
    /// and a mirroring transform flips the triangle winding so the surface stays outward.
    pub fn transform(&mut self, m: &[[f32; 4]; 4]) {
        for p in &mut self.positions {
            let v = *p;
            *p = [
                m[0][0] * v[0] + m[1][0] * v[1] + m[2][0] * v[2] + m[3][0],
                m[0][1] * v[0] + m[1][1] * v[1] + m[2][1] * v[2] + m[3][1],
                m[0][2] * v[0] + m[1][2] * v[1] + m[2][2] * v[2] + m[3][2],
            ];
        }
        let l = [
            [m[0][0], m[0][1], m[0][2]],
            [m[1][0], m[1][1], m[1][2]],
            [m[2][0], m[2][1], m[2][2]],
        ];
        let det = l[0][0] * (l[1][1] * l[2][2] - l[1][2] * l[2][1])
            - l[1][0] * (l[0][1] * l[2][2] - l[0][2] * l[2][1])
            + l[2][0] * (l[0][1] * l[1][2] - l[0][2] * l[1][1]);
        if det.abs() > f32::MIN_POSITIVE {
            let inv_t = [
                [
                    (l[1][1] * l[2][2] - l[1][2] * l[2][1]) / det,
                    (l[1][2] * l[2][0] - l[1][0] * l[2][2]) / det,
                    (l[1][0] * l[2][1] - l[1][1] * l[2][0]) / det,
                ],
                [
                    (l[0][2] * l[2][1] - l[0][1] * l[2][2]) / det,
                    (l[0][0] * l[2][2] - l[0][2] * l[2][0]) / det,
                    (l[0][1] * l[2][0] - l[0][0] * l[2][1]) / det,
                ],
                [
                    (l[0][1] * l[1][2] - l[0][2] * l[1][1]) / det,
                    (l[0][2] * l[1][0] - l[0][0] * l[1][2]) / det,
                    (l[0][0] * l[1][1] - l[0][1] * l[1][0]) / det,
                ],
            ];
            for n in &mut self.normals {
                let v = *n;
                *n = normalize([
                    inv_t[0][0] * v[0] + inv_t[1][0] * v[1] + inv_t[2][0] * v[2],
                    inv_t[0][1] * v[0] + inv_t[1][1] * v[1] + inv_t[2][1] * v[2],
                    inv_t[0][2] * v[0] + inv_t[1][2] * v[1] + inv_t[2][2] * v[2],
                ]);
            }
        }
        if det < 0.0 {
            for t in self.indices.chunks_exact_mut(3) {
                t.swap(1, 2);
            }
        }
    }

    /// Per-corner geometric normals averaged across incident faces whose normals lie within
    /// `angle_deg` of each other, so a cube stays faceted while a sphere stays smooth.
    /// Vertices are re-emitted and deduplicated by (position, normal, uv).
    pub fn recompute_normals(&mut self, angle_deg: f32) {
        let tri_count = self.triangle_count();
        if tri_count == 0 {
            self.normals = vec![[0.0, 1.0, 0.0]; self.positions.len()];
            return;
        }
        let cos_limit = angle_deg.clamp(0.0, 180.0).to_radians().cos();
        let face_normals: Vec<[f32; 3]> = self
            .triangles()
            .map(|[a, b, c]| {
                let n = cross(sub(b, a), sub(c, a));
                if length(n) <= f32::MIN_POSITIVE {
                    [0.0, 0.0, 0.0]
                } else {
                    normalize(n)
                }
            })
            .collect();
        // Weighted face normals per welded position.
        let mut by_pos: std::collections::HashMap<(i64, i64, i64), Vec<usize>> =
            std::collections::HashMap::new();
        for (f, tri) in self.indices.chunks_exact(3).enumerate() {
            for &vi in tri {
                by_pos
                    .entry(pos_key(self.positions[vi as usize], WELD_TOL))
                    .or_default()
                    .push(f);
            }
        }

        let has_uv = !self.uvs.is_empty();
        let mut out = MeshData {
            uvs: if has_uv { Vec::new() } else { Vec::new() },
            ..Default::default()
        };
        let mut dedup: std::collections::HashMap<((i64, i64, i64), (i64, i64, i64), (i64, i64)), u32> =
            std::collections::HashMap::new();
        let mut indices = Vec::with_capacity(self.indices.len());
        for (f, tri) in self.indices.chunks_exact(3).enumerate() {
            let fnorm = face_normals[f];
            for &vi in tri {
                let p = self.positions[vi as usize];
                let uv = if has_uv { self.uvs[vi as usize] } else { [0.0, 0.0] };
                let mut acc = [0.0f32; 3];
                if let Some(faces) = by_pos.get(&pos_key(p, WELD_TOL)) {
                    for &g in faces {
                        let gn = face_normals[g];
                        if dot(gn, fnorm) >= cos_limit {
                            acc = add(acc, gn);
                        }
                    }
                }
                let n = if length(acc) <= f32::MIN_POSITIVE { fnorm } else { normalize(acc) };
                let key = (
                    pos_key(p, WELD_TOL),
                    pos_key(n, 1e-3),
                    ((uv[0] as f64 / 1e-5).round() as i64, (uv[1] as f64 / 1e-5).round() as i64),
                );
                let idx = *dedup.entry(key).or_insert_with(|| {
                    out.positions.push(p);
                    out.normals.push(n);
                    if has_uv {
                        out.uvs.push(uv);
                    }
                    (out.positions.len() - 1) as u32
                });
                indices.push(idx);
            }
        }
        out.indices = indices;
        *self = out;
    }

    /// Merge vertices whose positions coincide within `tolerance`, dropping the triangles
    /// that collapse. Returns how many vertices disappeared.
    pub fn weld(&mut self, tolerance: f32) -> usize {
        let tol = tolerance.max(1e-7);
        let before = self.positions.len();
        let mut map: std::collections::HashMap<(i64, i64, i64), u32> =
            std::collections::HashMap::new();
        let mut remap = vec![0u32; self.positions.len()];
        let mut out = MeshData::default();
        let has_n = !self.normals.is_empty();
        let has_uv = !self.uvs.is_empty();
        let mut counts: Vec<f32> = Vec::new();
        for (i, p) in self.positions.iter().enumerate() {
            let key = pos_key(*p, tol);
            match map.get(&key) {
                Some(&j) => {
                    remap[i] = j;
                    let j = j as usize;
                    counts[j] += 1.0;
                    if has_n {
                        out.normals[j] = add(out.normals[j], self.normals[i]);
                    }
                    if has_uv {
                        out.uvs[j] = [out.uvs[j][0] + self.uvs[i][0], out.uvs[j][1] + self.uvs[i][1]];
                    }
                }
                None => {
                    let j = out.positions.len() as u32;
                    map.insert(key, j);
                    remap[i] = j;
                    out.positions.push(*p);
                    counts.push(1.0);
                    if has_n {
                        out.normals.push(self.normals[i]);
                    }
                    if has_uv {
                        out.uvs.push(self.uvs[i]);
                    }
                }
            }
        }
        for (j, c) in counts.iter().enumerate() {
            if has_n {
                out.normals[j] = normalize(out.normals[j]);
            }
            if has_uv {
                out.uvs[j] = [out.uvs[j][0] / c, out.uvs[j][1] / c];
            }
        }
        for t in self.indices.chunks_exact(3) {
            let (a, b, c) = (remap[t[0] as usize], remap[t[1] as usize], remap[t[2] as usize]);
            if a != b && b != c && a != c {
                out.indices.extend_from_slice(&[a, b, c]);
            }
        }
        *self = out;
        before - self.positions.len()
    }

    /// Drop triangles with (near) zero area. Returns how many went.
    pub fn drop_degenerate(&mut self, epsilon: f32) -> usize {
        let before = self.triangle_count();
        let mut keep = Vec::with_capacity(self.indices.len());
        for t in self.indices.chunks_exact(3) {
            let (a, b, c) = (
                self.positions[t[0] as usize],
                self.positions[t[1] as usize],
                self.positions[t[2] as usize],
            );
            let area2 = length(cross(sub(b, a), sub(c, a)));
            if t[0] != t[1] && t[1] != t[2] && t[0] != t[2] && area2 > epsilon {
                keep.extend_from_slice(t);
            }
        }
        self.indices = keep;
        before - self.triangle_count()
    }

    /// Vertex-clustering decimation to roughly `ratio` of the current triangle count.
    /// Positions collapse to their cluster centroid, so the silhouette and bounds survive
    /// while detail is dropped. Normals are recomputed afterwards.
    pub fn decimate(&mut self, ratio: f32, normal_angle: f32) {
        let ratio = ratio.clamp(0.001, 1.0);
        let target = ((self.triangle_count() as f32 * ratio).round() as usize).max(1);
        let Some((lo, hi)) = self.bounds() else { return };
        if self.triangle_count() <= target {
            return;
        }
        let ext = [
            (hi[0] - lo[0]).max(1e-6),
            (hi[1] - lo[1]).max(1e-6),
            (hi[2] - lo[2]).max(1e-6),
        ];
        let count_at = |n: u32| -> usize {
            let mut seen = std::collections::HashSet::new();
            let cell = |p: [f32; 3]| -> (u32, u32, u32) {
                let f = |v: f32, l: f32, e: f32| {
                    (((v - l) / e * n as f32).floor().max(0.0) as u32).min(n.saturating_sub(1))
                };
                (f(p[0], lo[0], ext[0]), f(p[1], lo[1], ext[1]), f(p[2], lo[2], ext[2]))
            };
            for t in self.indices.chunks_exact(3) {
                let (a, b, c) = (
                    cell(self.positions[t[0] as usize]),
                    cell(self.positions[t[1] as usize]),
                    cell(self.positions[t[2] as usize]),
                );
                if a != b && b != c && a != c {
                    let mut k = [a, b, c];
                    k.sort_unstable();
                    seen.insert(k);
                }
            }
            seen.len()
        };
        let mut grid = 1u32;
        for n in 1..=192u32 {
            grid = n;
            if count_at(n) >= target {
                break;
            }
        }
        let n = grid;
        let cell = |p: [f32; 3]| -> (u32, u32, u32) {
            let f = |v: f32, l: f32, e: f32| {
                (((v - l) / e * n as f32).floor().max(0.0) as u32).min(n.saturating_sub(1))
            };
            (f(p[0], lo[0], ext[0]), f(p[1], lo[1], ext[1]), f(p[2], lo[2], ext[2]))
        };
        let mut map: std::collections::HashMap<(u32, u32, u32), u32> =
            std::collections::HashMap::new();
        let mut sums: Vec<([f64; 3], [f64; 2], f64)> = Vec::new();
        let mut remap = vec![0u32; self.positions.len()];
        let has_uv = !self.uvs.is_empty();
        for (i, p) in self.positions.iter().enumerate() {
            let k = cell(*p);
            let idx = *map.entry(k).or_insert_with(|| {
                sums.push(([0.0; 3], [0.0; 2], 0.0));
                (sums.len() - 1) as u32
            });
            remap[i] = idx;
            let s = &mut sums[idx as usize];
            s.0[0] += p[0] as f64;
            s.0[1] += p[1] as f64;
            s.0[2] += p[2] as f64;
            if has_uv {
                s.1[0] += self.uvs[i][0] as f64;
                s.1[1] += self.uvs[i][1] as f64;
            }
            s.2 += 1.0;
        }
        let mut out = MeshData::default();
        for (p, uv, c) in &sums {
            out.positions.push([
                (p[0] / c) as f32,
                (p[1] / c) as f32,
                (p[2] / c) as f32,
            ]);
            if has_uv {
                out.uvs.push([(uv[0] / c) as f32, (uv[1] / c) as f32]);
            }
        }
        let mut seen = std::collections::HashSet::new();
        for t in self.indices.chunks_exact(3) {
            let (a, b, c) = (remap[t[0] as usize], remap[t[1] as usize], remap[t[2] as usize]);
            if a == b || b == c || a == c {
                continue;
            }
            let mut k = [a, b, c];
            k.sort_unstable();
            if seen.insert(k) {
                out.indices.extend_from_slice(&[a, b, c]);
            }
        }
        *self = out;
        self.recompute_normals(normal_angle);
    }
}
