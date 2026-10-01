#!/usr/bin/env bash
# Two audio tracks through real RTMP, including a live-only tone absent from the VOD.
# Needs FFmpeg 8+ for Enhanced FLV. FFMPEG_FIXTURE may be an older full FFmpeg build.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="${DD_TEST_BIN:-$ROOT/target/release/obs-dynamic-delay}"
if [ ! -x "$BIN" ] && [ -x "$BIN.exe" ]; then BIN="$BIN.exe"; fi
if [ ! -x "$BIN" ]; then cargo build --release --locked --manifest-path "$ROOT/Cargo.toml"; fi
FFMPEG="${FFMPEG_BIN:-ffmpeg}"
FIXTURE="${FFMPEG_FIXTURE:-ffmpeg}"
WORK="${DD_E2E_WORKDIR:-$(mktemp -d)}"
mkdir -p "$WORK"
cd "$WORK"
pids=()
cleanup() {
  for pid in "${pids[@]}"; do kill "$pid" 2>/dev/null || true; done
  for pid in "${pids[@]}"; do wait "$pid" 2>/dev/null || true; done
  echo "VOD test logs: $WORK"
}
trap cleanup EXIT

# Voice (880 Hz) is in both tracks; music (440 Hz) is ONLY in the live track.
"$FIXTURE" -hide_banner -loglevel error -y \
  -f lavfi -i testsrc=size=640x360:rate=30 \
  -f lavfi -i 'aevalsrc=0.06*sin(2*PI*440*t)+0.06*sin(2*PI*880*t):s=48000' \
  -f lavfi -i sine=frequency=880:sample_rate=48000 \
  -map 0:v -map 1:a -map 2:a -c:v libx264 -preset ultrafast -pix_fmt yuv420p -g 60 -bf 0 \
  -c:a aac -ac 2 -b:a 128k -t 25 fixture.mkv

cat > test.toml <<'EOF'
listen = "127.0.0.1:1937"
upstream_url = "rtmp://127.0.0.1:1941/app"
delay_seconds = 5
http_listen = "127.0.0.1:8799"
udp_listen = "127.0.0.1:8800"
EOF

"$FFMPEG" -hide_banner -loglevel error -y -timeout 30 -listen 1 \
  -i rtmp://127.0.0.1:1941/app/testkey -map 0:v -map 0:a -c copy out.flv > receiver.log 2>&1 &
RECV=$!; pids+=("$RECV")
sleep 1
"$BIN" test.toml > relay.log 2>&1 &
RELAY=$!; pids+=("$RELAY")
for _ in {1..100}; do
  if [ -f test.toml ]; then TOKEN="$(sed -n 's/^api_token = "\(.*\)"/\1/p' test.toml)"; fi
  if [ -n "${TOKEN:-}" ] && curl -fsS --max-time 1 -H "x-dd-token: $TOKEN" http://127.0.0.1:8799/api/status >/dev/null 2>&1; then break; fi
  kill -0 "$RELAY" || { cat relay.log; exit 1; }
  sleep 0.1
done
api() { curl -fsS --max-time 3 -H "x-dd-token: $TOKEN" -X POST "http://127.0.0.1:8799/api/cmd/$1" >/dev/null; }
phase() { curl -fsS --max-time 3 -H "x-dd-token: $TOKEN" http://127.0.0.1:8799/api/status | python3 -c 'import json,sys; print((json.load(sys.stdin).get("engine") or {}).get("phase", "offline"))'; }

"$FFMPEG" -hide_banner -loglevel error -re -i fixture.mkv -map 0 -c copy \
  -f flv rtmp://127.0.0.1:1937/live/testkey > source.log 2>&1 &
SRC=$!; pids+=("$SRC")
sleep 5
api on
sleep 9
[ "$(phase)" = delayed ] || { echo 'FAIL: did not enter delayed mode'; exit 1; }
api off
sleep 3
[ "$(phase)" = live ] || { echo 'FAIL: did not return to live mode'; exit 1; }
wait "$SRC"
sleep 4
kill "$RELAY" 2>/dev/null || true
for _ in {1..100}; do
  kill -0 "$RECV" 2>/dev/null || break
  sleep 0.1
done
if kill -0 "$RECV" 2>/dev/null; then echo 'FAIL: receiver did not finish'; exit 1; fi
wait "$RECV" || true # EOF when the relay closes is expected; verify the actual media below.
python3 "$ROOT/scripts/verify_vod.py" out.flv
