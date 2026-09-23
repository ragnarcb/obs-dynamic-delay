"""Generates the Stream Deck plugin images (needs Pillow and Windows fonts).

    python streamdeck/make_icons.py
"""
import os

from PIL import Image, ImageDraw, ImageFont

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, "com.ragnarcb.dynamicdelay.sdPlugin", "images")
FONT = r"C:\Windows\Fonts\segoeuib.ttf"
SYMBOL = r"C:\Windows\Fonts\seguisym.ttf"

BG = (39, 41, 48)
COLORS = {"live": (62, 207, 110), "delay": (240, 169, 59), "blue": (90, 167, 255), "red": (255, 93, 93), "grey": (154, 157, 171)}


def draw(size, color, label, glyph=None, filled=False, y=0.5):
    img = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    r = size // 6
    d.rounded_rectangle([0, 0, size - 1, size - 1], radius=r, fill=color if filled else BG)
    fg = (20, 20, 24) if filled else color
    if glyph:
        f = ImageFont.truetype(SYMBOL, int(size * 0.42))
        d.text((size / 2, size * 0.40), glyph, font=f, fill=fg, anchor="mm")
        f2 = ImageFont.truetype(FONT, int(size * 0.17))
        d.text((size / 2, size * 0.80), label, font=f2, fill=fg, anchor="mm")
    else:
        f = ImageFont.truetype(FONT, int(size * (0.30 if len(label) <= 4 else 0.22)))
        d.text((size / 2, size * y), label, font=f, fill=fg, anchor="mm")
    return img


def save(name, sizes, *args, **kw):
    for size, suffix in sizes:
        draw(size, *args, **kw).save(os.path.join(OUT, f"{name}{suffix}.png"))


os.makedirs(OUT, exist_ok=True)
KEY = [(72, ""), (144, "@2x")]
LIST = [(20, ""), (40, "@2x")]
save("plugin", [(256, ""), (512, "@2x")], COLORS["delay"], "DELAY", filled=True)
save("category", [(28, ""), (56, "@2x")], COLORS["delay"], "D", filled=True)
# the key title (current delay) is drawn by Stream Deck at the bottom
save("toggle_off", KEY, COLORS["live"], "LIVE", y=0.38)
save("toggle_on", KEY, COLORS["delay"], "DELAY", filled=True, y=0.38)
save("censor", KEY, COLORS["red"], "DELETE", glyph="✂")
save("replay", KEY, COLORS["blue"], "REPLAY", glyph="⟲")
save("clip", KEY, COLORS["blue"], "CLIP", glyph="●")
save("panic", KEY, COLORS["red"], "PANIC", glyph="⚠", filled=True)
for name, color, label, glyph in [("toggle_list", COLORS["delay"], "D", None), ("censor_list", COLORS["red"], "", "✂"),
                                  ("replay_list", COLORS["blue"], "", "⟲"), ("clip_list", COLORS["blue"], "", "●"),
                                  ("panic_list", COLORS["red"], "", "⚠")]:
    for size, suffix in LIST:
        img = Image.new("RGBA", (size, size), (0, 0, 0, 0))
        d = ImageDraw.Draw(img)
        f = ImageFont.truetype(SYMBOL if glyph else FONT, int(size * 0.8))
        d.text((size / 2, size / 2), glyph or label, font=f, fill=(255, 255, 255), anchor="mm")
        img.save(os.path.join(OUT, f"{name}{suffix}.png"))
print("icons written to", OUT)
