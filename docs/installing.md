# Installing

Everything here is one Rust workspace. There is no npm step, no bundler and no Chromium: the
Studio frontend is three plain files (`crates/dpaint-studio/ui/`) served as-is by both shells.

## Requirements

| | |
|---|---|
| Rust | 1.80 or newer (`rust-version` in the workspace); built and tested on 1.96 |
| Platforms | macOS (aarch64, x86_64) and Linux x86_64 are what CI covers |
| Disk | ~2 GB for a full `target/` directory |

Install Rust with [rustup](https://rustup.rs) if you do not have it:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

**Linux only.** The CLI links the optional AI provider layer, which uses the system TLS stack
and the secret-service keyring backend:

```bash
sudo apt-get install -y libdbus-1-dev libssl-dev pkg-config        # Debian / Ubuntu
sudo pacman -S --needed dbus openssl pkgconf                        # Arch
```

The desktop app additionally needs the Tauri v2 webview stack:

```bash
# Debian / Ubuntu
sudo apt-get install -y \
  libwebkit2gtk-4.1-dev libjavascriptcoregtk-4.1-dev libgtk-3-dev \
  libayatana-appindicator3-dev librsvg2-dev libsoup-3.0-dev
# Arch
sudo pacman -S --needed webkit2gtk-4.1 gtk3 libayatana-appindicator librsvg libsoup3
```

The desktop app sets `WEBKIT_DISABLE_DMABUF_RENDERER=1` for itself on Linux: WebKitGTK's
DMA-BUF renderer crashes the window on Wayland with the NVIDIA proprietary driver. Export the
variable yourself (to `0`) to override that choice. The native `dpaint-view` viewport does not
go through WebKit and needs no such setting; it picks Vulkan on Linux.

macOS needs the Xcode command line tools (`xcode-select --install`) and nothing else.

## The `dpaint` CLI

### From a checkout

```bash
git clone https://github.com/ethereumdegen/degen-paint
cd degen-paint
cargo install --path crates/dpaint-cli
```

That puts `dpaint` in `~/.cargo/bin`. Add it to your `PATH` if cargo tells you to, or install
somewhere else with `--root`:

```bash
cargo install --path crates/dpaint-cli --root /usr/local
```

### From crates.io

*Not yet published.* Once the `dpaint-*` crates are on crates.io this is the whole install:

```bash
cargo install dpaint-cli
```

Until then use the `--path` form above. The release workflow runs `cargo publish --dry-run`
on every tag so the packages stay publishable, but it deliberately does not publish.

### Build without installing

```bash
cargo build --release        # binary at target/release/dpaint
./target/release/dpaint --help
```

### Check the install

```bash
$ dpaint --version
dpaint 0.1.0

$ dpaint doctor
dpaint 0.1.0
  ops registered: 212
  providers: {"fal":{"provider":"fal","configured":false,…},"quiver":{…,"configured":false,…}}
  project: none found from this directory
```

`doctor` is the capability probe: op count, output formats, which AI providers resolved a key
and from where, and the project it found from the current directory. `configured: false` for
both providers is the normal, fully functional state — see
[AI provider keys](#ai-provider-keys-optional).

A 30-second end-to-end check:

```bash
dpaint new poster --kind raster --size 1200x800 --dpi 144
dpaint op raster.layer.add --type fill --color "#123456" --name backdrop
dpaint render out/poster.png
dpaint lint --json          # exit 4 if the document has problems, 0 if clean
dpaint inspect --json       # the render digest
```

`dpaint op --list` prints the whole catalog (212 ops); `dpaint op <id> --help` prints one op's
flags, generated from its JSON Schema, and `dpaint schema <id>` prints that schema.

## The Studio

### Same UI in a browser

```bash
cd poster.dpaint/..            # anywhere the project is discoverable
dpaint serve                   # http://127.0.0.1:4317
dpaint serve --addr 127.0.0.1:9000
dpaint serve --ui-dir crates/dpaint-studio/ui    # serve the UI from a working copy
```

`serve` needs a project, exactly like every other command: it discovers one from the current
directory, or takes `--project <dir>`. There is no build step — the server hosts the same UI
files the desktop app bundles.

### Desktop app

Run it from the checkout:

```bash
cargo run -p dpaint-studio-app -- --project poster.dpaint
```

Build distributable bundles (`.app` and `.dmg` on macOS, `.deb`/`.AppImage` on Linux):

```bash
cargo install tauri-cli --version "^2" --locked
cd apps/studio
cargo tauri build
```

The bundles land in `target/<triple>/release/bundle/`. They are **unsigned** — no Developer ID
is configured in this repository — so macOS will quarantine a downloaded build until you clear
it (`xattr -dr com.apple.quarantine /Applications/degen-paint.app`). Builds you make yourself
and run locally are unaffected.

## MCP server

`dpaint mcp` speaks MCP over stdio: one tool per op, plus `dpaint_overview`, `dpaint_render`,
`dpaint_lint`, `dpaint_apply` and `dpaint_history` — 217 tools in total, all generated from the
op registry. Like every other command it needs a project: it discovers one from the process's
working directory, or you point it at one with `--project`.

### Claude Code

```bash
claude mcp add degen-paint -- dpaint --project /abs/path/to/poster.dpaint mcp
claude mcp list        # degen-paint: … - ✔ Connected
```

Add `--scope project` to write a checked-in `.mcp.json` next to the project instead of
configuring it for yourself only. That file is the same thing by hand:

```json
{
  "mcpServers": {
    "degen-paint": {
      "type": "stdio",
      "command": "dpaint",
      "args": ["--project", "/abs/path/to/poster.dpaint", "mcp"],
      "env": {}
    }
  }
}
```

### Any other MCP client

The generic stdio form, which most clients accept verbatim:

```json
{
  "mcpServers": {
    "degen-paint": {
      "command": "dpaint",
      "args": ["--project", "/abs/path/to/poster.dpaint", "mcp"]
    }
  }
}
```

If your client lets you set a working directory, you can drop the `--project` pair and let
`dpaint` discover the project from that directory instead.

### Check it by hand

Two lines of JSON-RPC on stdin is a complete smoke test:

```bash
printf '%s\n%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}' \
  | dpaint --project /abs/path/to/poster.dpaint mcp
```

The first response reports `"serverInfo":{"name":"degen-paint","version":"0.1.0"}`, the second
lists the tools.

## AI provider keys (optional)

degen-paint is a complete editor with no AI configured; only ops whose id starts with `ai.`
ever touch a network. To enable them, supply a key for the provider you want:

| Provider | Environment variable |
|---|---|
| [fal.ai](https://fal.ai) | `FAL_KEY` |
| [QuiverAI](https://quiver.ai) | `QUIVERAI_API_KEY` |

Keys resolve in this order, first hit wins: an explicit key passed to the op, the environment
variable, the OS keychain (service `degen-paint`, account `fal` or `quiver`), then
`~/.config/degen-paint/config.toml` — which is ignored unless it is readable by you alone
(`chmod 600`).

```toml
# ~/.config/degen-paint/config.toml   (chmod 600)
[ai.fal]
key = "…"
generate = "fal-ai/flux/dev"     # model ids are configuration, not constants

[ai.quiver]
key = "…"
model = "arrow-2"
```

Confirm what resolved, and from where, without printing the key:

```bash
$ dpaint doctor --json | jq -c '.providers[] | {provider, configured, source}'
{"provider":"fal","configured":true,"source":"config-file"}
{"provider":"quiver","configured":true,"source":"config-file"}
```

`source` is one of `explicit`, `env`, `keychain` or `config-file`.

Cost control, provenance, caching and the per-project budget are described in
[`ai-providers.md`](./ai-providers.md).

## Uninstall

```bash
cargo uninstall dpaint-cli     # removes the `dpaint` binary
```

Projects are ordinary directories; deleting the `.dpaint` directory removes everything the tool
wrote, including the asset store and the journal.
