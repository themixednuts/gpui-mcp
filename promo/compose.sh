#!/usr/bin/env bash
# Composite the recorded take into promo/out/gpui-mcp-promo{,-music}.mp4.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
TAKE="$HERE/out/take"
WORK="$HERE/out/compose"
BPM="${BPM:-122}"
mkdir -p "$WORK/frames"
cp "$HERE"/compose/{index.html,render.mjs} "$WORK/"
ffmpeg -loglevel error -y -i "$TAKE/footage.mkv" -q:v 2 "$WORK/frames/%05d.jpg"
cd "$WORK"
python3 "$HERE/compose/prep.py" "$TAKE" "$(ls frames | wc -l)" "$BPM"
[ -d node_modules/playwright ] || npm install --no-save --silent playwright@1.61.1
node render.mjs "$HERE/out/gpui-mcp-promo.mp4"

# Soundtrack: synthesized to the same timeline, normalized for the web.
python3 "$HERE/compose/music.py" timeline.js music.wav
ffmpeg -loglevel error -y -i "$HERE/out/gpui-mcp-promo.mp4" -i music.wav \
  -map 0:v -map 1:a -c:v copy -af "loudnorm=I=-16:TP=-1.5:LRA=11" -c:a aac -b:a 192k -ar 48000 \
  -shortest -movflags +faststart "$HERE/out/gpui-mcp-promo-music.mp4"
