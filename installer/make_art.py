"""Renders every image of the project from vector sources (run: python installer/make_art.py).

Sources: installer/art/logo.svg (the logo) and installer/art/phosphor.json
(Phosphor Icons, MIT). Chrome draws one sheet with every image at its exact
pixel size (real font rendering, antialiasing) and Pillow cuts it into files:

  installer/art/app.ico, app.png          program and Setup icon
  installer/art/wizard-*.bmp, small-*.bmp Setup wizard images (every DPI scale)
  docs/img/logo.svg, docs/img/logo.png    README
  streamdeck/.../images/*.png             Stream Deck plugin keys and lists
"""
import json
import os
import shutil
import subprocess
import tempfile

from PIL import Image

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
ART = os.path.join(HERE, "art")
SD = os.path.join(ROOT, "streamdeck", "com.ragnarcb.dynamicdelay.sdPlugin", "images")
CHROME = r"C:\Program Files\Google\Chrome\Application\chrome.exe"

LOGO = open(os.path.join(ART, "logo.svg"), encoding="utf-8").read()
ICONS = json.load(open(os.path.join(ART, "phosphor.json"), encoding="utf-8"))
INK, PAPER, RED = "#17181c", "#f2f2f3", "#e5484d"
FONT = "'Segoe UI Variable Display', 'Segoe UI', sans-serif"


def logo(size):
    return LOGO.replace("<svg ", f'<svg width="{size}" height="{size}" ', 1)


def icon(name, size, color, weight="fill"):
    return f'<svg width="{size}" height="{size}" viewBox="0 0 256 256" fill="{color}">{ICONS[weight][name]}</svg>'


items = []  # (file, width, height, html)


def add(path, w, h, html, bg="transparent"):
    items.append((path, w, h, f'<div style="width:{w}px;height:{h}px;background:{bg};display:flex;align-items:center;justify-content:center;overflow:hidden;position:relative">{html}</div>'))


# ---- program icon, every Windows size drawn at its own size
for s in (16, 20, 24, 32, 40, 48, 64, 128, 256):
    add(f"ico-{s}", s, s, logo(s))
add(os.path.join(ART, "app.png"), 512, 512, logo(512))
add(os.path.join(ROOT, "docs", "img", "logo.png"), 256, 256, logo(256))

# ---- Setup wizard: flat dark side image and the tile on white for the inner pages
for scale in (100, 125, 150, 200, 250):
    k = scale / 100
    w, h = round(164 * k), round(314 * k)
    add(os.path.join(ART, f"wizard-{scale}.bmp"), w, h, f'''
      <div style="position:absolute;inset:0;background:#111216"></div>
      <div style="position:absolute;left:0;right:0;top:{round(h * 0.22)}px;display:flex;justify-content:center">{logo(round(w * 0.42))}</div>
      <div style="position:absolute;left:0;right:0;top:{round(h * 0.52)}px;text-align:center;font:600 {round(19 * k)}px/{round(24 * k)}px {FONT};color:{PAPER};letter-spacing:-.01em">Dynamic Delay</div>
      <div style="position:absolute;left:0;right:0;top:{round(h * 0.52 + 26 * k)}px;text-align:center;font:400 {round(11.5 * k)}px {FONT};color:#9a9ea9">for OBS Studio</div>
      <div style="position:absolute;left:{round(22 * k)}px;right:{round(22 * k)}px;bottom:{round(46 * k)}px;height:1px;background:#2a2c33"></div>
      <div style="position:absolute;left:0;right:0;bottom:{round(22 * k)}px;text-align:center;font:400 {round(10 * k)}px {FONT};color:#7d818c">ragnarcb · open source</div>''')
    s = round(55 * k)
    add(os.path.join(ART, f"small-{scale}.bmp"), s, s, logo(round(s * 0.86)), bg="#ffffff")

# ---- Stream Deck: plugin + category icons, action list icons (white, per Elgato), keys
add(os.path.join(SD, "plugin.png"), 256, 256, logo(256))
add(os.path.join(SD, "plugin@2x.png"), 512, 512, logo(512))
MARK = '<svg width="{s}" height="{s}" viewBox="40 40 176 176"><path d="M62 84 Q62 70 74 77 L140 116 Q151 128 140 140 L74 179 Q62 186 62 172 Z" fill="#fff" fill-opacity=".45"/><path d="M102 84 Q102 70 114 77 L180 116 Q191 128 180 140 L114 179 Q102 186 102 172 Z" fill="#fff"/><circle cx="196" cy="60" r="15" fill="#fff"/></svg>'
for s, suf in ((28, ""), (56, "@2x")):
    add(os.path.join(SD, f"category{suf}.png"), s, s, MARK.format(s=s))
LIST = {"toggle_list": "timer", "censor_list": "scissors", "replay_list": "replay", "clip_list": "clapper", "panic_list": "siren"}
for name, ic in LIST.items():
    for s, suf in ((20, ""), (40, "@2x")):
        add(os.path.join(SD, f"{name}{suf}.png"), s, s, icon(ic, s, "#ffffff"))
# keys: Stream Deck draws the title (for example the current delay) over the bottom
KEYS = {
    "toggle_off": ("live", "#34d399", "LIVE", False),
    "toggle_on": ("timer", "#f5a524", "DELAY", True),
    "censor": ("scissors", RED, "DELETE", False),
    "replay": ("replay", "#60a5fa", "REPLAY", False),
    "clip": ("clapper", PAPER, "CLIP", False),
    "panic": ("siren", "#ffffff", "PANIC", True),
}
for name, (ic, color, label, strong) in KEYS.items():
    for s, suf in ((72, ""), (144, "@2x")):
        k = s / 72
        bg = RED if name == "panic" else ("#3a2a0c" if strong else INK)
        top = "38%" if name.startswith("toggle") else "40%"
        add(os.path.join(SD, f"{name}{suf}.png"), s, s, f'''
          <div style="position:absolute;inset:0;background:{bg}"></div>
          <div style="position:absolute;left:0;right:0;top:{top};transform:translateY(-50%);display:flex;justify-content:center">{icon(ic, round(30 * k), color)}</div>
          <div style="position:absolute;left:0;right:0;top:{"62%" if name.startswith("toggle") else "70%"};text-align:center;font:700 {round(10.5 * k)}px {FONT};letter-spacing:.06em;color:{"#fff" if name == "panic" else color}">{label}</div>''')

# ---- one sheet, one screenshot
pad, x, y, rowh, width = 8, 8, 8, 0, 1400
placed = []
for path, w, h, html in items:
    if x + w > width:
        x, y, rowh = 8, y + rowh + pad, 0
    placed.append((path, x, y, w, h, html))
    x += w + pad
    rowh = max(rowh, h)
height = y + rowh + pad
tmp = tempfile.mkdtemp()
page = os.path.join(tmp, "sheet.html")
open(page, "w", encoding="utf-8").write(
    '<!doctype html><meta charset="utf-8"><style>html,body{margin:0;background:transparent}</style>'
    + "".join(f'<div style="position:absolute;left:{px}px;top:{py}px">{html}</div>' for _, px, py, _, _, html in placed))
shot = os.path.join(tmp, "sheet.png")
subprocess.run([CHROME, "--headless=new", "--disable-gpu", "--hide-scrollbars", "--force-device-scale-factor=1",
                "--default-background-color=00000000", f"--user-data-dir={os.path.join(tmp, 'prof')}",
                f"--window-size={width},{height}", f"--screenshot={shot}", "file:///" + page.replace("\\", "/")],
               check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
sheet = Image.open(shot).convert("RGBA")
ico = {}
for path, px, py, w, h, _ in placed:
    part = sheet.crop((px, py, px + w, py + h))
    if path.startswith("ico-"):
        ico[w] = part
        continue
    os.makedirs(os.path.dirname(path), exist_ok=True)
    if path.endswith(".bmp"):
        part.convert("RGB").save(path)
    else:
        part.save(path)
# .ico with each size drawn natively (not scaled from the 256 one)
big = ico[256]
big.save(os.path.join(ART, "app.ico"), sizes=[(s, s) for s in sorted(ico)], append_images=[ico[s] for s in sorted(ico) if s != 256])
shutil.copy(os.path.join(ART, "logo.svg"), os.path.join(ROOT, "docs", "img", "logo.svg"))
shutil.rmtree(tmp, ignore_errors=True)
print(f"art ok: {len(placed)} images")
