#!/usr/bin/env bash
# Record a real agent session against the Ember showcase under Xvfb.
# Writes promo/out/take/{footage.mkv,ffmpeg.log,events.json}.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/.." && pwd)
OUT="$HERE/out/take"
mkdir -p "$OUT"
export DISPLAY=:99 WAYLAND_DISPLAY= XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/tmp/gpui-mcp-promo-xdg}"
mkdir -p "$XDG_RUNTIME_DIR" && chmod 700 "$XDG_RUNTIME_DIR"

cargo build --release --locked -p runtime-showcase -p gpui-mcp-server --manifest-path "$ROOT/Cargo.toml"

pgrep -x Xvfb >/dev/null || { setsid Xvfb :99 -screen 0 1920x1080x24 >"$OUT/xvfb.log" 2>&1 </dev/null & sleep 1.5; }
setsid "$ROOT/target/release/runtime-showcase" >"$OUT/app.log" 2>&1 </dev/null &
APP=$!
trap 'kill $APP 2>/dev/null || true' EXIT
sleep 4

W=$(xdotool search --name Ember | head -1)
X=$(xwininfo -id "$W" | awk '/Absolute upper-left X/{print $4}')
Y=$(xwininfo -id "$W" | awk '/Absolute upper-left Y/{print $4}')
ffmpeg -nostdin -y -f x11grab -draw_mouse 0 -framerate 30 -video_size 1200x760 -i ":99.0+$X,$Y" \
  -c:v libx264 -preset ultrafast -crf 10 -pix_fmt yuv444p "$OUT/footage.mkv" >"$OUT/ffmpeg.log" 2>&1 &
FF=$!
sleep 1.5
(cd "$HERE" && MCP_ERR="$OUT/server.log" python3 director.py "$ROOT/target/release/gpui-mcp" "$OUT/events.json")
sleep 0.5
kill -INT $FF; wait $FF || true
