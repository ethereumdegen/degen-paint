// Forward shading for model previews.
//
// This is a deliberate port of `dpaint_render::preview3d`'s rig, not a nicer BRDF: a human
// dragging the viewport and an agent reading a turntable PNG must be looking at the same
// object. Every expression below has a line in `preview3d::render` it corresponds to.
//
// The target is a *linear-unorm* format and this shader writes sRGB-encoded values, because
// the CPU path encodes with `Color::from_linear` and then quantizes. Handing the encode to an
// `*Srgb` view instead would double-encode.

struct Camera {
    view_proj: mat4x4<f32>,
    // xyz: camera forward. Used to flip normals toward the viewer, exactly as the CPU
    // rasterizer does, which is what makes the renderer two-sided.
    forward: vec4<f32>,
    // xyz: normalized key direction, w: constant ambient.
    key: vec4<f32>,
    // xyz: normalized fill direction.
    fill: vec4<f32>,
    // xyz: normalize(key + -forward), the Blinn half vector. Constant per frame on the CPU
    // side too: `preview3d` builds it from the camera axis, not from the shaded point.
    half_dir: vec4<f32>,
};

struct MeshUniform {
    // Column-major world matrix, glTF `m[col][row]` — the same array the CPU path feeds to
    // `xform_point`, so no transpose happens anywhere.
    world: mat4x4<f32>,
    // Linear-light base color (`Color::to_linear`).
    base_color: vec4<f32>,
    // x: shininess, y: specular scale. Precomputed on the CPU from metallic/roughness so the
    // two renderers cannot disagree about the mapping.
    params: vec4<f32>,
};

@group(0) @binding(0) var<uniform> cam: Camera;
@group(1) @binding(0) var<uniform> mesh: MeshUniform;

// `preview3d::norm`: degenerate input becomes +Z rather than NaN.
fn norm3(v: vec3<f32>) -> vec3<f32> {
    let l = sqrt(dot(v, v));
    if l <= 1.1920929e-7 {
        return vec3<f32>(0.0, 0.0, 1.0);
    }
    return v / l;
}

fn linear_to_srgb1(c: f32) -> f32 {
    if c <= 0.0031308 {
        return c * 12.92;
    }
    return 1.055 * pow(c, 1.0 / 2.4) - 0.055;
}

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) normal: vec3<f32>,
};

@vertex
fn vs_main(@location(0) pos: vec3<f32>, @location(1) nrm: vec3<f32>) -> VsOut {
    let world_pos = mesh.world * vec4<f32>(pos, 1.0);
    // The CPU path rotates normals with the world matrix itself, not its inverse transpose,
    // and normalizes per vertex. Matched here, including the normalize placement.
    let basis = mat3x3<f32>(mesh.world[0].xyz, mesh.world[1].xyz, mesh.world[2].xyz);
    var out: VsOut;
    out.clip = cam.view_proj * world_pos;
    out.normal = norm3(basis * nrm);
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    var n = norm3(in.normal);
    if dot(n, cam.forward.xyz) > 0.0 {
        n = -n;
    }

    let diffuse = max(dot(n, cam.key.xyz), 0.0) + 0.35 * max(dot(n, cam.fill.xyz), 0.0);
    let spec = pow(max(dot(n, cam.half_dir.xyz), 0.0), mesh.params.x) * mesh.params.y;

    let lit = mesh.base_color.rgb * (cam.key.w + diffuse) + vec3<f32>(spec);
    let srgb = vec3<f32>(
        linear_to_srgb1(lit.r),
        linear_to_srgb1(lit.g),
        linear_to_srgb1(lit.b),
    );
    return vec4<f32>(clamp(srgb, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
