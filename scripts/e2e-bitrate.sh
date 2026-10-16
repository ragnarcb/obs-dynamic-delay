#!/usr/bin/env bash
# End-to-end test of a destination with a bitrate of its own: ffmpeg plays OBS at
# 4000 kbps, the main destination gets it as it is, a second one, started while
# live, gets the video re-encoded at 1000 kbps. The delay is turned on and off mid-stream. Checks that
# both decode cleanly, the bitrates and that audio and video stay together.
# Needs an ffmpeg with an H.264 encoder (DD_FFMPEG can point at one).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$ROOT/target/release/obs-dynamic-delay"
[ -x "$BIN" ] || [ -x "$BIN.exe" ] || cargo build --release --manifest-path "$ROOT/Cargo.toml"
WORK="$(mktemp -d)"
cd "$WORK"

cat > test.toml <<'EOF'
listen = "127.0.0.1:1936"
upstream_url = "rtmp://127.0.0.1:1940/app"
delay_seconds = 6
http_listen = "127.0.0.1:8797"
udp_listen = "127.0.0.1:8798"

[features]
multistream = true

[[destinations]]
name = "Low"
url = "rtmp://127.0.0.1:1941/app"
key = "lowkey"
video_kbps = 1000
auto_start = false
EOF

ffmpeg -hide_banner -loglevel error -y -timeout 120 -listen 1 -i rtmp://127.0.0.1:1940/app/testkey -c copy main.flv &
RECV1=$!
ffmpeg -hide_banner -loglevel error -y -timeout 120 -listen 1 -i rtmp://127.0.0.1:1941/app/lowkey -c copy low.flv &
RECV2=$!
sleep 1
"$BIN" test.toml > relay.log 2>&1 &
RELAY=$!
trap 'kill $RELAY $RECV1 $RECV2 2>/dev/null || true' EXIT
sleep 1

ffmpeg -hide_banner -loglevel error -re \
  -f lavfi -i "testsrc2=size=1280x720:rate=30,noise=alls=10:allf=t" -f lavfi -i sine=frequency=440:sample_rate=48000 \
  -c:v libx264 -preset veryfast -g 60 -b:v 4000k -maxrate 4000k -bufsize 8000k -c:a aac -b:a 128k -t 30 \
  -f flv rtmp://127.0.0.1:1936/live/testkey &
SRC=$!

TOKEN="$(sed -n 's/^api_token = "\(.*\)"/\1/p' test.toml)"
api() { curl -fsS -H "x-dd-token: $TOKEN" -X POST "http://127.0.0.1:8797/api/cmd/$1" > /dev/null; }
status() { curl -fsS -H "x-dd-token: $TOKEN" http://127.0.0.1:8797/api/status; }

sleep 3
# the second destination is started from the panel, with the stream already running
curl -fsS -H "x-dd-token: $TOKEN" -X POST http://127.0.0.1:8797/api/output/1/start > /dev/null
sleep 3; api on
sleep 10
ENC="$(status | sed -n 's/.*"encoder":"\([^"]\+\)".*/\1/p')"
[ -n "$ENC" ] || { echo "FAIL: not re-encoding"; status; exit 1; }
api off
wait $SRC
sleep 5
kill $RELAY; wait $RECV1 $RECV2 || true

for f in main.flv low.flv; do
  ERRORS="$(ffmpeg -v error -i $f -fps_mode passthrough -enc_time_base 1:1000 -f null - 2>&1 || true)"
  [ -z "$ERRORS" ] || { echo "FAIL: decode errors in $f:"; echo "$ERRORS"; exit 1; }
done
# video kbps: video packet bytes over the video duration
kbps() {
  ffprobe -v error -select_streams v -show_entries packet=pts_time,size -of csv=p=0 "$1" |
    awk -F, 'NR==1{a=$1} {b=$1; s+=$2} END{printf "%d", s*8/(b-a)/1000}'
}
MAIN="$(kbps main.flv)"; LOW="$(kbps low.flv)"
echo "main ${MAIN} kbps, re-encoded ${LOW} kbps with $ENC"
[ "$MAIN" -gt 3000 ] || { echo "FAIL: main should be ~4000 kbps"; exit 1; }
[ "$LOW" -gt 800 ] && [ "$LOW" -lt 1150 ] || { echo "FAIL: re-encoded should be ~1000 kbps"; exit 1; }
# audio and video of the re-encoded copy cover the same time span
span() { ffprobe -v error -select_streams "$2" -show_entries packet=pts_time -of csv=p=0 "$1" | awk 'NR==1{a=$1} {b=$1} END{printf "%.2f %.2f", a, b}'; }
read -r VA VB <<< "$(span low.flv v)"; read -r AA AB <<< "$(span low.flv a)"
awk -v va="$VA" -v aa="$AA" -v vb="$VB" -v ab="$AB" 'BEGIN{d1=va-aa; d2=vb-ab; if (d1<0) d1=-d1; if (d2<0) d2=-d2; exit !(d1 < 1.0 && d2 < 1.0)}' ||
  { echo "FAIL: audio $AA-$AB s and video $VA-$VB s apart"; exit 1; }
echo "OK: both decode cleanly, audio and video together (work dir: $WORK)"
