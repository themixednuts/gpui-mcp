#!/usr/bin/env bash
# Composite the recorded take into promo/out/gpui-mcp-promo.mp4.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
TAKE="$HERE/out/take"
WORK="$HERE/out/compose"
mkdir -p "$WORK/frames"
cp "$HERE"/compose/{index.html,render.mjs} "$WORK/"
ffmpeg -loglevel error -y -i "$TAKE/footage.mkv" -q:v 2 "$WORK/frames/%05d.jpg"
cd "$WORK"
python3 "$HERE/compose/prep.py" "$TAKE" "$(ls frames | wc -l)"
[ -d node_modules/playwright ] || npm install --no-save --silent playwright@1.61.1
node render.mjs "$HERE/out/gpui-mcp-promo.mp4"
