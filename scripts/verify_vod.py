"""Check received VOD media, including audio isolation (no third-party packages)."""
from array import array
import json
import math
import os
from pathlib import Path
import subprocess
import sys


def tone_power(samples, frequency):
    coefficient = 2 * math.cos(2 * math.pi * frequency / 48000)
    a = b = 0.0
    for sample in samples:
        a, b = sample + coefficient * a - b, a
    return (a * a + b * b - coefficient * a * b) / len(samples) ** 2


def verify(path):
    ffmpeg = os.environ.get("FFMPEG_BIN", "ffmpeg")
    ffprobe = os.environ.get("FFPROBE_BIN", "ffprobe")
    data = Path(path).read_bytes()
    assert data[:3] == b"FLV", "No FLV output"
    i, counts = int.from_bytes(data[5:9], "big") + 4, {}
    while i + 11 <= len(data):
        size = int.from_bytes(data[i + 1:i + 4], "big")
        assert i + 11 + size + 4 <= len(data), "Truncated FLV tag"
        if data[i] == 8 and size >= 2:
            key = tuple(data[i + 11:i + 13])
            counts[key] = counts.get(key, 0) + 1
        i += 11 + size + 4
    main, vod = counts.get((0xAF, 1), 0), counts.get((0x95, 1), 0)
    assert counts.get((0x95, 0), 0) >= 1, "Missing VOD codec header"
    assert vod > 500 and abs(main - vod) < main * 0.1, ("Unbalanced/missing audio", counts)

    probe = subprocess.run([ffprobe, "-v", "error", "-show_streams", "-show_packets", "-of", "json", path],
                           capture_output=True, text=True, check=True)
    media = json.loads(probe.stdout)
    assert len([s for s in media["streams"] if s["codec_type"] == "audio"]) == 2, "Expected two audio tracks"
    packets = [p for p in media["packets"] if p["codec_type"] == "video" and "pts" in p and "dts" in p]
    pts, dts = [p["pts"] for p in packets], [p["dts"] for p in packets]
    assert packets and len(pts) == len(set(pts)), "Missing video or repeated PTS"
    assert all(a <= b for a, b in zip(dts, dts[1:])), "Backwards video DTS"

    for track in range(2):
        decoded = subprocess.run([ffmpeg, "-v", "error", "-xerror", "-i", path, "-map", f"0:a:{track}",
                                  "-ac", "1", "-ar", "48000", "-c:a", "pcm_s16le", "-f", "s16le", "-"],
                                 capture_output=True)
        assert decoded.returncode == 0, decoded.stderr.decode(errors="replace")
        assert not decoded.stderr, decoded.stderr.decode(errors="replace")
        samples = array("h", decoded.stdout)
        if sys.byteorder != "little":
            samples.byteswap()
        powers = []
        for offset in range(4800, len(samples) - 4096, 12000):
            window = samples[offset:offset + 4096]
            if sum(s * s for s in window) / len(window) > 10000:
                powers.append((tone_power(window, 440), tone_power(window, 880)))
        assert len(powers) >= 20, f"Track {track} is silent/too short"
        for music, voice in powers:
            assert voice > 1000, f"Voice missing from track {track}"
            if track == 0:
                assert music > voice * 0.1, "Live-only audio missing from the live track"
            else:
                assert music < voice * 0.03, "Live-only audio leaked into the VOD"
    print(f"OK: {main} live / {vod} VOD frames; valid timestamps; live-only audio absent from VOD")


if __name__ == "__main__":
    verify(sys.argv[1])
