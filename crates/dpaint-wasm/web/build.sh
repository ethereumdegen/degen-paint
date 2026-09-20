#!/usr/bin/env bash
# Assemble a servable directory for the browser build.
#
#   ./crates/dpaint-wasm/web/build.sh [outdir]      # default: target/wasm-studio
#
# Output layout:
#   index.html  boot.js            the boot shell (this directory)
#   pkg/                           wasm-bindgen glue + dpaint_wasm_bg.wasm
#   ui/                            verbatim copy of crates/dpaint-studio/ui/
#
# Requires `wasm-bindgen-cli` at exactly the `wasm-bindgen` version in Cargo.lock; a mismatch
# produces glue the module rejects at instantiation. `wasm-opt` is used when present.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
crate="$(dirname "$here")"
root="$(cd "$crate/../.." && pwd)"
out="${1:-$root/target/wasm-studio}"

# Run from the repo root so .cargo/config.toml (the getrandom backend rustflags) applies.
cd "$root"

want="$(awk '/^name = "wasm-bindgen"$/{getline; gsub(/version = "|"/, ""); print; exit}' Cargo.lock)"
if ! command -v wasm-bindgen >/dev/null 2>&1; then
  echo "error: wasm-bindgen not found. Install it with:" >&2
  echo "       cargo install wasm-bindgen-cli --version $want --locked" >&2
  exit 1
fi
have="$(wasm-bindgen --version | awk '{print $2}')"
if [ "$have" != "$want" ]; then
  echo "error: wasm-bindgen CLI is $have but Cargo.lock pins the crate at $want." >&2
  echo "       cargo install wasm-bindgen-cli --version $want --locked" >&2
  exit 1
fi

echo "==> cargo build -p dpaint-wasm --target wasm32-unknown-unknown --release"
cargo build -p dpaint-wasm --target wasm32-unknown-unknown --release

echo "==> wasm-bindgen --target web"
rm -rf "$out"
mkdir -p "$out/pkg" "$out/ui"
wasm-bindgen \
  --target web \
  --no-typescript \
  --out-dir "$out/pkg" \
  --out-name dpaint_wasm \
  "$root/target/wasm32-unknown-unknown/release/dpaint_wasm.wasm"

if command -v wasm-opt >/dev/null 2>&1; then
  echo "==> wasm-opt -Oz"
  wasm-opt -Oz --enable-bulk-memory "$out/pkg/dpaint_wasm_bg.wasm" -o "$out/pkg/dpaint_wasm_bg.wasm"
else
  echo "==> wasm-opt not found; shipping the unoptimized module (about 30% larger)"
fi

cp "$here/index.html" "$here/boot.js" "$out/"
# Verbatim: the browser build must run the same UI bytes the desktop app does.
cp "$root/crates/dpaint-studio/ui/index.html" \
   "$root/crates/dpaint-studio/ui/studio.css" \
   "$root/crates/dpaint-studio/ui/studio.js" \
   "$out/ui/"

echo
echo "bundle:"
for f in "$out/pkg/dpaint_wasm_bg.wasm" "$out/pkg/dpaint_wasm.js"; do
  printf '  %-28s %8s raw  %8s gzip\n' \
    "$(basename "$f")" \
    "$(wc -c <"$f" | tr -d ' ')" \
    "$(gzip -9 -c "$f" | wc -c | tr -d ' ')"
done
echo
echo "serve it:  python3 -m http.server -d $out 8787"
