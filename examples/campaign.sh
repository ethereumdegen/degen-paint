#!/usr/bin/env bash
# One source path -> an SVG logo, a glTF badge, and a raster poster.
#
# Every command here is verified: this script is the cross-mode pipeline from the README,
# and `crates/dpaint-cli/tests/pipeline.rs` asserts the same sequence in CI.
set -euo pipefail

DPAINT="${DPAINT:-dpaint}"
OUT="${OUT:-out}"
rm -rf campaign.dpaint "$OUT"

# ---------------------------------------------------------------- vector
$DPAINT new campaign --kind vector --size 512x512
$DPAINT op vector.object.add-path --d "M256 32 L480 448 L32 448 Z" --fill "#fb8500" --name mark
$DPAINT op vector.object.add-ellipse --cx 256 --cy 330 --rx 70 --name hole --fill "#000000"

# A real boolean subtract: the hole becomes part of the mark's geometry.
$DPAINT op vector.path.boolean --target "@mark, @hole" --op subtract

$DPAINT render "$OUT/logo.svg"
$DPAINT render "$OUT/logo.png" --scale 2

# ---------------------------------------------------------------- model
# The same path, extruded. No export/import dance between tools.
$DPAINT op doc.add --name badge --kind model
$DPAINT --doc badge op model.mesh.extrude --path "campaign:@mark" --depth 40 --bevel 4 --name badge-body
$DPAINT --doc badge op model.material.create --name gold --base-color "#d4af37" --metallic 1 --roughness 0.3
$DPAINT --doc badge op model.material.assign --target "@badge-body" --material "@gold"
$DPAINT --doc badge op model.validate

$DPAINT --doc badge render "$OUT/badge.glb"
$DPAINT --doc badge render "$OUT/badge.png" --width 512 --height 512 --background "#101418"
$DPAINT --doc badge op render.turntable --dir "$OUT/spin" --frames 8 --width 256 --height 256

# ---------------------------------------------------------------- raster
$DPAINT op doc.add --name poster --kind raster --width 800 --height 1000 --dpi 150
$DPAINT --doc poster op raster.layer.add --type fill --color "#14213d" --name bg
$DPAINT --doc poster op raster.layer.add --type linked --source campaign \
        --box "150,120,500,500" --fit contain --name badge-art
$DPAINT --doc poster op raster.layer.add --type text --text "URBAN EXPLORER" \
        --text.size 56 --text.align center --text.box "60,760,680,120" \
        --fill "#f4a261" --name title

$DPAINT --doc poster render "$OUT/poster.png"

# ---------------------------------------------------------------- feedback
# What an agent reads instead of looking at the image.
$DPAINT --doc poster inspect > "$OUT/digest.json"
$DPAINT --doc poster annotate "$OUT/annotated.png"
$DPAINT --doc poster lint || true          # exit 4 means "ran fine, found problems"

echo
echo "wrote:"
ls -1 "$OUT"
