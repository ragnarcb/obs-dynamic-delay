#!/usr/bin/env bash
# End-to-end test: ffmpeg plays OBS, a second ffmpeg plays the platform.
# Toggles the delay mid-stream and checks the received stream decodes cleanly.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$ROOT/target/release/obs-dynamic-delay"
[ -x "$BIN" ] || [ -x "$BIN.exe" ] || cargo build --release --manifest-path "$ROOT/Cargo.toml"
WORK="$(mktemp -d)"
cd "$WORK"

cat > test.toml <<'EOF'
listen = "127.0.0.1:1936"
upstream_url = "rtmp://127.0.0.1:1940/app"
delay_seconds = 8
http_listen = "127.0.0.1:8797"
udp_listen = "127.0.0.1:8798"
EOF

ffmpeg -hide_banner -loglevel error -y -timeout 120 -listen 1 \
  -i rtmp://127.0.0.1:1940/app/testkey -c copy out.flv &
RECV=$!
sleep 1
"$BIN" test.toml > relay.log 2>&1 &
RELAY=$!
trap 'kill $RELAY $RECV 2>/dev/null || true' EXIT
sleep 1

ffmpeg -hide_banner -loglevel error -re \
  -f lavfi -i testsrc=size=640x360:rate=30 -f lavfi -i sine=frequency=440:sample_rate=48000 \
  -c:v libx264 -preset veryfast -bf 3 -g 60 -b:v 800k -c:a aac -b:a 128k -t 30 \
  -f flv rtmp://127.0.0.1:1936/live/testkey &
SRC=$!

api() { curl -fsS "http://127.0.0.1:8797/api/$1" > /dev/null; }
phase() { curl -fsS http://127.0.0.1:8797/api/status | sed -n 's/.*"phase":"\([a-z_]*\)".*/\1/p'; }

sleep 5;  api on
sleep 12; [ "$(phase)" = delayed ] || { echo "FAIL: expected delayed, got $(phase)"; exit 1; }
api off
sleep 3;  [ "$(phase)" = live ] || { echo "FAIL: expected live, got $(phase)"; exit 1; }
wait $SRC
sleep 4
kill $RELAY; wait $RECV || true

ERRORS="$(ffmpeg -v error -i out.flv -f null - 2>&1 || true)"
if [ -n "$ERRORS" ]; then
  echo "FAIL: decode errors:"; echo "$ERRORS"; exit 1
fi
DUR="$(ffprobe -v error -show_entries format=duration -of csv=p=0 out.flv)"
echo "OK: received ${DUR}s, no decode errors (work dir: $WORK)"
