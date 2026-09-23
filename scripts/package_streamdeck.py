"""Packs the Stream Deck plugin folder into Dynamic-Delay-StreamDeck.streamDeckPlugin.

    python scripts/package_streamdeck.py [output path]
"""
import os
import sys
import zipfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PLUGIN = "com.ragnarcb.dynamicdelay.sdPlugin"
SRC = os.path.join(ROOT, "streamdeck", PLUGIN)
OUT = sys.argv[1] if len(sys.argv) > 1 else os.path.join(os.getcwd(), "Dynamic-Delay-StreamDeck.streamDeckPlugin")

with zipfile.ZipFile(OUT, "w", zipfile.ZIP_DEFLATED) as z:
    for folder, _, files in os.walk(SRC):
        for f in files:
            full = os.path.join(folder, f)
            # the archive holds the .sdPlugin folder itself at its root
            z.write(full, os.path.join(PLUGIN, os.path.relpath(full, SRC)).replace("\\", "/"))
print("written", OUT)
