// 2D viewport: one engine-rendered pixmap as a texture, then pan and zoom are uniform state.
// Nothing here touches the texture, which is the whole point — dragging must not re-upload.
//
// The pixmap is premultiplied sRGB (tiny-skia's layout) and is passed through unchanged, so
// linear filtering interpolates premultiplied values, which is the correct thing to filter.

struct Canvas {
    // Image rect in device pixels: centre and half extent. centre = viewport/2 + pan,
    // half_extent = texture_size * zoom / 2.
    centre: vec2<f32>,
    half_extent: vec2<f32>,
    viewport: vec2<f32>,
    // x: 1.0 draws the transparency checker behind the image.
    flags: vec2<f32>,
};

@group(0) @binding(0) var<uniform> cv: Canvas;
@group(0) @binding(1) var tex: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;

const CHECK_PX: f32 = 16.0;
const CHECK_LIGHT: vec3<f32> = vec3<f32>(207.0 / 255.0, 212.0 / 255.0, 218.0 / 255.0);
const CHECK_DARK: vec3<f32> = vec3<f32>(154.0 / 255.0, 162.0 / 255.0, 172.0 / 255.0);

// Full-viewport triangle pair, so the checker covers everywhere the image does not.
@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(1.0, -1.0),
        vec2<f32>(-1.0, 1.0),
        vec2<f32>(-1.0, 1.0),
        vec2<f32>(1.0, -1.0),
        vec2<f32>(1.0, 1.0),
    );
    return vec4<f32>(corners[vi], 0.0, 1.0);
}

@fragment
fn fs_main(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let px = frag.xy;

    var under = vec4<f32>(0.0, 0.0, 0.0, 0.0);
    if cv.flags.x > 0.5 {
        let cell = floor(px / CHECK_PX);
        let odd = (cell.x + cell.y) - 2.0 * floor((cell.x + cell.y) * 0.5);
        let rgb = select(CHECK_LIGHT, CHECK_DARK, odd > 0.5);
        under = vec4<f32>(rgb, 1.0);
    }

    var img = vec4<f32>(0.0, 0.0, 0.0, 0.0);
    let extent = max(cv.half_extent * 2.0, vec2<f32>(1e-6));
    let uv = (px - (cv.centre - cv.half_extent)) / extent;
    if uv.x >= 0.0 && uv.x <= 1.0 && uv.y >= 0.0 && uv.y <= 1.0 {
        img = textureSampleLevel(tex, samp, uv, 0.0);
    }

    // Premultiplied source-over.
    return vec4<f32>(
        img.rgb + under.rgb * (1.0 - img.a),
        img.a + under.a * (1.0 - img.a),
    );
}
