# The GPU viewport

## Why a second renderer

degen-paint already renders everything on the CPU, and that stays. The CPU path is what
golden tests, perceptual diffs and `dpaint render` depend on: byte-identical output on a
laptop, in CI, and in a container with no display. A GPU cannot promise that — driver and
vendor differences move the last bits — so making it authoritative would quietly destroy the
determinism the agent surface is built on.

What the CPU path cannot do is drive an interactive viewport. Re-rasterizing a 3D scene per
mouse-drag frame on the CPU is fine for an 8-frame turntable and useless at 60 fps.

So the split is explicit, and it is a **policy**, not an accident:

| Path | Renderer | Rule |
|---|---|---|
| `dpaint render`, `render.turntable`, goldens, digests, diffs | CPU (`dpaint-render::preview3d`) | always, never GPU |
| Interactive viewport — native window, Tauri, browser canvas | GPU (`dpaint-gpu`) | falls back to CPU when no adapter exists |

A GPU frame and a CPU frame of the same scene will not be bit-identical, so parity is measured
rather than assumed — and the threshold that matters is the tight one.

| Configuration | Bound | Why |
|---|---|---|
| multisampled, any driver | SSIM ≥ 0.93, mean ΔE2000 ≤ 6 | survives MSAA and filtering differences across vendors |
| 1×, matched rasterizer | SSIM ≥ 0.99, mean ΔE2000 ≤ 0.5 | with MSAA off both renderers cover the same pixel centres, so there is no excuse for disagreement |

The loose pair alone is not a sufficient detector, which was established by breaking things on
purpose and measuring rather than by reasoning about it:

| Deliberate breakage | SSIM | mean ΔE | caught by 0.93 / 6? |
|---|---|---|---|
| transposed world matrix | 0.668 | 44.5 | yes, overwhelmingly |
| flipped normal | 0.892 | 4.77 | by SSIM only — the ΔE bound misses it |
| unlit material | 0.970 | 1.94 | **no** |

Mean ΔE over a whole frame averages a defect away, and under this lighting rig
`base * (ambient + diffuse) + spec` lands near `base` for a mid-bright colour, so an unlit
shader produces almost the right picture. The tight 1× bound catches all three (measured
agreement in the good case: SSIM 0.9997, mean ΔE 0.016), and every metric is also computed
over the subject's bounding box so empty background cannot dilute it.

**What the scene parity test does not cover:** `preview3d` does no backface culling — it draws
every triangle and flips the interpolated normal toward the camera — so a one-sided surface
seen from behind is still lit. Closed solids always occlude their own back faces, so removing
that flip from the GPU shader changes the parity scene by nothing at all (SSIM 0.9995). It is
covered by a separate test that views a plane from below, where dropping the flip moves SSIM
from 1.000 to 0.786. The parity test covers the lighting rig; it does not cover two-sidedness.

## Shape

```
  dpaint-gpu
    Gpu            instance / adapter / device / queue, created once and shared
    SceneRenderer  forward PBR for model documents: MeshData -> buffers, WGSL
                   metallic-roughness, depth buffer, 4x MSAA, camera + light uniforms
    CanvasRenderer 2D: an engine-rendered pixmap uploaded once, then pan and zoom are
                   pure GPU state — the texture is not re-uploaded while you drag
    Target         Offscreen (render to texture, read back a Pixmap) or
                   Surface (a window or an HTML canvas)
```

Native it runs on Metal, Vulkan or DX12; in the browser on WebGPU. Same crate, same WGSL.

## The native viewport

`dpaint-view --project <dir>.dpaint [--doc <id|name>]` opens a window on a project. Model
documents orbit: drag to turn, shift-drag or right-drag to pan, wheel to zoom, `f` frames the
subject, `1` resets to the view `dpaint render` starts from. Raster and vector documents are
rasterized once by `dpaint_render::render_document` and then panned and zoomed as pure GPU
state — the texture is not re-uploaded while you drag. `c` toggles the checkerboard, `p`
toggles nearest-neighbour magnification, `s` hides the status readout, `r` forces a reload,
`q` or Escape quits.

The window polls the project's journal twice a second, so an op applied by an agent, the CLI
or the Studio appears here within half a second — the same shared-journal property the rest of
the tool has, made visible.

`--frames N --out <dir>` renders those same frames offscreen through the same code path the
window uses. That is how the viewport is inspected on a machine with no display, and it
reports per-frame draw cost.

Measured on an Apple A18 Pro, release build, a 2560×1600 physical window (1280×800 logical at
2× scale) with 4× MSAA: **60.0 fps, vsync-locked**, with a 4.0 ms median draw while orbiting —
roughly 250 fps of headroom. The canvas path costs 1.52 ms at the same size. Across a 60-frame
orbit the geometry upload count stays at 1, and across a 60-frame pan/zoom the pixmap upload
count stays at 1; both are asserted, because that is the actual reason a drag is cheap.

With no adapter, `dpaint-view` prints the `dpaint render` command that does the same job on the
CPU and exits 2. It never opens a black window.

## The browser viewport

The Studio UI probes for `navigator.gpu`. When WebGPU is available, a `<canvas>` replaces the
`<img>` and pan, zoom and orbit become GPU state; when it is not, the existing image path runs
unchanged and the status bar says `CPU · no WebGPU in this browser`. The same three UI files
serve all three shells, so `dpaint serve` and the Tauri app are unaffected either way.

Verified in a real browser: a 40-move orbit drag on a model document issued **zero** engine
calls — the render-call counter stayed at 0 for the whole session, and the invoke counter
tracked the one-per-second state poll exactly. Adding wgpu cost +2.4% gzipped wasm
(2.85 → 2.92 MB) and +11 KB gzipped of JS glue.

## Lighting parity

`SceneRenderer` reproduces `preview3d`'s rig rather than inventing a prettier one, because a
human dragging the viewport and an agent reading a turntable PNG must be looking at the same
object: a key light, a fill at 35%, a constant ambient term, and a roughness-driven specular
lobe. Materials come from the document's PBR fields. When the two renderers disagree the CPU
is right by definition, and the parity test is what notices.

## Capability, not assumption

There is no GPU requirement anywhere in degen-paint. `Gpu::new()` returns `Option`: no adapter
means the viewport uses the CPU path and says so in `dpaint doctor` and in the Studio's status
bar. In the browser, `navigator.gpu` absent means the same thing. A missing GPU costs you
frames per second, never a feature.
