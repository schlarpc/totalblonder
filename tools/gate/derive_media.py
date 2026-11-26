#!/usr/bin/env python3
"""Derived test clips made by copying whole frames of the prepared raw clips (no decoding):

  cuts_<fmt>   a scene cut every 0.37-0.8 s, not aligned to the GOP, cycling through
               crowd_run, fourpeople, park_joy, pedestrian_area
  noise_<fmt>  2 s of uniform random bytes in all three planes (nothing is harder to code),
               then 1 s of crowd_run so that the encoder has to recover
  still_<fmt>  1 s of crowd_run, then 3 s of one repeated frame, then 1 s of black
               (pictures that cost almost nothing: decoder buffer overflow side)
Run in the media directory ($M2V_MEDIA; tools/gate/prep_media.sh does); existing files are kept.
"""
import os
import random

FMT = {"1080i": (1920, 1080, 30), "1080p": (1920, 1080, 30), "720p": (1280, 720, 60), "480i": (704, 480, 30)}
SRC = ["crowd_run", "fourpeople", "park_joy", "pedestrian_area"]
SEG = [0.37, 0.8, 0.43, 0.6, 0.5, 0.7, 0.4, 0.75, 0.45]   # seconds; total 5.0


def frames(path, fb):
    return os.path.getsize(path) // fb


for fmt, (w, h, r) in FMT.items():
    fb = w * h * 3 // 2
    out = f"cuts_{fmt}.yuv"
    if not os.path.exists(out) and all(os.path.exists(f"{c}_{fmt}.yuv") for c in SRC):
        with open(out + ".part", "wb") as o:
            pos = 0
            for i, sec in enumerate(SEG):
                n = round(sec * r)
                p = f"{SRC[i % 4]}_{fmt}.yuv"
                start = (pos if i >= 4 else 0) % max(1, frames(p, fb) - n)
                with open(p, "rb") as f:
                    f.seek(start * fb)
                    o.write(f.read(n * fb))
                pos += n
        os.rename(out + ".part", out)
    out = f"noise_{fmt}.yuv"
    if not os.path.exists(out):
        rnd = random.Random(1234)
        with open(out + ".part", "wb") as o:
            for _ in range(2 * r):
                o.write(rnd.randbytes(fb))
            with open(f"crowd_run_{fmt}.yuv", "rb") as f:
                o.write(f.read(r * fb))
        os.rename(out + ".part", out)
    out = f"still_{fmt}.yuv"
    if not os.path.exists(out):
        with open(f"crowd_run_{fmt}.yuv", "rb") as f, open(out + ".part", "wb") as o:
            o.write(f.read(r * fb))
            f.seek((r - 1) * fb)
            last = f.read(fb)
            for _ in range(3 * r):
                o.write(last)
            black = bytes([16]) * (w * h) + bytes([128]) * (w * h // 2)
            for _ in range(r):
                o.write(black)
        os.rename(out + ".part", out)
