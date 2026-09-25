"""The Stream Deck images are rendered with the rest of the artwork:

    python installer/make_art.py
"""
import os
import runpy

runpy.run_path(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "installer", "make_art.py"), run_name="__main__")
