"""Downloads the Phosphor icons (MIT, phosphoricons.com) used by the panel, the
phone deck and the Stream Deck plugin into installer/art/phosphor.json
(inner SVG markup, viewBox 0 0 256 256), so builds work offline.

    python installer/fetch_icons.py
"""
import json
import os
import re
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, "art", "phosphor.json")
# our name -> Phosphor name
ICONS = {
    "timer": "timer", "scissors": "scissors", "replay": "arrow-counter-clockwise", "clapper": "film-slate",
    "siren": "siren", "activity": "pulse", "tower": "broadcast", "layers": "stack", "chat": "chat-circle-text",
    "phone": "device-mobile", "grid": "squares-four", "settings": "gear-six", "sliders": "sliders-horizontal",
    "power": "power", "chevron": "caret-down", "up": "arrow-up", "down": "arrow-down", "x": "x", "plus": "plus",
    "copy": "copy", "check": "check", "folder": "folder-open", "external": "arrow-square-out", "alert": "warning",
    "info": "info", "download": "download-simple", "key": "key", "sparkles": "magic-wand", "plug": "plugs",
    "fastforward": "fast-forward", "zap": "lightning", "live": "cell-tower", "plusminus": "plus-minus",
    "scene": "image", "mic": "microphone", "micoff": "microphone-slash", "stream": "broadcast", "record": "record",
    "dot": "dot-outline",
}
WEIGHTS = ["bold", "fill"]
BASE = "https://cdn.jsdelivr.net/npm/@phosphor-icons/core@2/assets"


def get(name, weight):
    suffix = "" if weight == "regular" else "-" + weight
    svg = urllib.request.urlopen(f"{BASE}/{weight}/{name}{suffix}.svg", timeout=20).read().decode()
    inner = re.search(r"<svg[^>]*>(.*)</svg>", svg, re.S).group(1)
    inner = re.sub(r'<rect width="256" height="256" fill="none"\s*/>', "", inner)
    return re.sub(r"\s+", " ", inner).strip()


if __name__ == "__main__":
    data = {w: {ours: get(theirs, w) for ours, theirs in ICONS.items()} for w in WEIGHTS}
    json.dump(data, open(OUT, "w", encoding="utf-8"), indent=1)
    print("written", OUT, {w: len(v) for w, v in data.items()})
