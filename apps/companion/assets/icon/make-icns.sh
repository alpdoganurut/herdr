#!/usr/bin/env bash
# Builds eye-plus.icns (the packaged app icon) from the PNG masters in this directory.
# Small sizes use the simplified eye-plus-small mark; 128 px and up use the full eye-plus.
set -euo pipefail
D="$(cd "$(dirname "$0")" && pwd)"
S="$(mktemp -d)/eye-plus.iconset"
mkdir -p "$S"
cp "$D/eye-plus-small-16.png" "$S/icon_16x16.png"
cp "$D/eye-plus-small-32.png" "$S/icon_16x16@2x.png"
cp "$D/eye-plus-small-32.png" "$S/icon_32x32.png"
cp "$D/eye-plus-small-64.png" "$S/icon_32x32@2x.png"
cp "$D/eye-plus-128.png" "$S/icon_128x128.png"
cp "$D/eye-plus-256.png" "$S/icon_128x128@2x.png"
cp "$D/eye-plus-256.png" "$S/icon_256x256.png"
cp "$D/eye-plus-512.png" "$S/icon_256x256@2x.png"
cp "$D/eye-plus-512.png" "$S/icon_512x512.png"
cp "$D/eye-plus-1024.png" "$S/icon_512x512@2x.png"
iconutil -c icns -o "$D/eye-plus.icns" "$S"
rm -r "$(dirname "$S")"
echo "$D/eye-plus.icns"
