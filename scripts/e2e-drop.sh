#!/usr/bin/env bash
# End-to-end test: the platform drops and comes back. The relay resends what it kept,
# so that platform runs behind; turning the delay off must bring it back to live.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$ROOT/target/release/obs-dynamic-delay"
[ -x "$BIN" ] || [ -x "$BIN.exe" ] || cargo build --release --manifest-path "$ROOT/Cargo.toml"
WORK="$(mktemp -d)"
cd "$WORK"

cat > test.toml <<'T'
listen = "127.0.0.1:1946"
upstream_url = "rtmp://127.0.0.1:1947/app"
delay_seconds = 3
http_listen = "127.0.0.1:8807"
udp_listen = "127.0.0.1:8808"
T

"$BIN" test.toml > relay.log 2>&1 &
RELAY=$!
recv() { ffmpeg -hide_banner -loglevel error -y -timeout 30 -listen 1 -i rtmp://127.0.0.1:1947/app/k -c copy "out$1.flv" > "recv$1.log" 2>&1 & echo $!; }
R=$(recv 1)
trap 'kill $RELAY $R 2>/dev/null || true' EXIT
sleep 1
TOKEN="$(sed -n 's/^api_token = "\(.*\)"/\1/p' test.toml)"
behind() { curl -fsS -H "x-dd-token: $TOKEN" http://127.0.0.1:8807/api/status | python -c "import json,sys; print(json.load(sys.stdin)['outputs'][0]['behind_ms'])"; }
udp() { python -c "import socket,sys; socket.socket(socket.AF_INET,socket.SOCK_DGRAM).sendto(sys.argv[1].encode(),('127.0.0.1',8808))" "$1"; }

ffmpeg -hide_banner -loglevel error -re -f lavfi -i testsrc2=size=320x240:rate=30 -f lavfi -i sine \
  -c:v libx264 -g 60 -c:a aac -t 40 -f flv rtmp://127.0.0.1:1946/live/k &
PUB=$!
sleep 8
kill $R   # the platform drops
sleep 8
R=$(recv 2)
sleep 8
b1=$(behind)
udp on; sleep 1; udp off; sleep 2
b2=$(behind)
wait $PUB
echo "behind after the drop: ${b1} ms; after turning the delay off: ${b2} ms"
[ "$b1" -ge 5000 ] || { echo "FAIL: the drop left no backlog"; cat relay.log; exit 1; }
[ "$b2" -le 2000 ] || { echo "FAIL: turning the delay off left the platform behind"; cat relay.log; exit 1; }
echo PASS
