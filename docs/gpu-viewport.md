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
