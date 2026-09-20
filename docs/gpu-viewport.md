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

A GPU frame and a CPU frame of the same scene will not be bit-identical. They must be
*recognizably the same image*, and that is tested: the parity test renders both and requires
SSIM ≥ 0.93 with mean ΔE2000 ≤ 6 — tight enough to catch a flipped normal, a wrong matrix
convention or an unlit material, loose enough to survive rasterization and filtering
differences.

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
