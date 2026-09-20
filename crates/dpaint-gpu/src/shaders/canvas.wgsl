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
    // x: 1.0 draws the transparency checker behind the document, inside its rect only.
    flags: vec2<f32>,
};

@group(0) @binding(0) var<uniform> cv: Canvas;
@group(0) @binding(1) var tex: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;

const CHECK_PX: f32 = 16.0;
const CHECK_LIGHT: vec3<f32> = vec3<f32>(207.0 / 255.0, 212.0 / 255.0, 218.0 / 255.0);
const CHECK_DARK: vec3<f32> = vec3<f32>(154.0 / 255.0, 162.0 / 255.0, 172.0 / 255.0);

// Full-viewport triangle pair: the document rect can sit anywhere in it, and the fragment
// stage decides what each pixel belongs to.
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
    let lo = cv.centre - cv.half_extent;
    let hi = cv.centre + cv.half_extent;
    // Signed distance outside the document rect, in pixels: <= 0 inside.
    let outside = max(max(lo.x - px.x, px.x - hi.x), max(lo.y - px.y, px.y - hi.y));

    if outside > 0.0 {
        // A 1px dark edge, so the document's boundary stays readable against a dark
        // viewport. Replaces the `box-shadow: 0 0 0 1px #000` the CSS viewport drew.
        if outside <= 1.0 {
            return vec4<f32>(0.0, 0.0, 0.0, 1.0);
        }
        return vec4<f32>(0.0, 0.0, 0.0, 0.0);
    }

    // The checker marks where the *document* is transparent, so it is clamped to the
    // document rect rather than flooding the viewport. Outside the rect there is no
    // document to be transparent, and a checker there would make an empty document
    // indistinguishable from empty space.
    var under = vec4<f32>(0.0, 0.0, 0.0, 0.0);
    if cv.flags.x > 0.5 {
        // Phase is anchored to the viewport, not the document, so the squares do not crawl
        // while you drag.
        let cell = floor(px / CHECK_PX);
        let odd = (cell.x + cell.y) - 2.0 * floor((cell.x + cell.y) * 0.5);
        let rgb = select(CHECK_LIGHT, CHECK_DARK, odd > 0.5);
        under = vec4<f32>(rgb, 1.0);
    }

    let extent = max(cv.half_extent * 2.0, vec2<f32>(1e-6));
    let uv = (px - lo) / extent;
    let img = textureSampleLevel(tex, samp, clamp(uv, vec2<f32>(0.0), vec2<f32>(1.0)), 0.0);

    // Premultiplied source-over.
    return vec4<f32>(
        img.rgb + under.rgb * (1.0 - img.a),
        img.a + under.a * (1.0 - img.a),
    );
}
