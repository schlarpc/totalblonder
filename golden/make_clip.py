#!/usr/bin/env python3
"""Deterministic raw 4:2:0 test clip for the golden vectors (no ffmpeg, no random module).

Usage: make_clip.py WIDTH HEIGHT FRAMES SEED [interlaced|noisy|white|patch] > clip.yuv

Content: a textured background that pans, a block that moves the other way, per-pixel noise
from a fixed generator, and a hard cut to another pattern at the middle frame. With
`interlaced` the two fields of a frame are 1/60 s apart (the bottom field is the next phase of
the motion), which makes the encoder's field tools worth choosing. With `noisy` the noise is
strong (+-64 instead of +-4): a picture that cannot be coded well at any rate.
"""
import sys


def main():
    w, h, n, seed = (int(a) for a in sys.argv[1:5])
    interlaced = len(sys.argv) > 5 and sys.argv[5] == "interlaced"
    div = 1 if len(sys.argv) > 5 and sys.argv[5] == "noisy" else 16
    kind = sys.argv[5] if len(sys.argv) > 5 else ""

    def white(y):
        # "white": uniform noise everywhere; "patch": in the band of lines 48 to 79
        return kind == "white" or (kind == "patch" and 48 <= y < 80)

    state = (seed * 0x9E3779B97F4A7C15 + 1) & (2**64 - 1)
    out = sys.stdout.buffer

    def luma(x, y, t, scene):
        if scene == 0:
            v = 96 + ((x + 3 * t) * 5 + y * 3) % 97 + (((x + 3 * t) // 16 + y // 16) % 2) * 40
            bx, by = 20 + (2 * n - 2 * t) % (w - 48), 24 + t % 40
            if bx <= x < bx + 32 and by <= y < by + 32:
                v = 40 + ((x - bx) * 7 + (y - by) * 11) % 150
        else:
            v = 60 + ((x - 2 * t) * 2 + (y + t) * 4) % 140
            if ((x - 2 * t) // 24 + (y + t) // 24) % 3 == 0:
                v = 220 - v // 2
        return v

    for f in range(n):
        scene = 0 if f < n // 2 else 1
        y_plane = bytearray(w * h)
        for y in range(h):
            t = 2 * f + (y & 1) if interlaced else 2 * f
            row = y * w
            for x in range(w):
                state = (state * 6364136223846793005 + 1442695040888963407) & (2**64 - 1)
                v = luma(x, y, t, scene) + ((state >> 57) - 64) // div
                y_plane[row + x] = state >> 56 if white(y) else 16 if v < 16 else 235 if v > 235 else v
        out.write(y_plane)
        for c in range(2):
            plane = bytearray((w // 2) * (h // 2))
            for y in range(h // 2):
                for x in range(w // 2):
                    v = 128 + (((x + f) // 8 + (y // 8) + c + scene) % 5 - 2) * 12
                    if white(2 * y):
                        state = (state * 6364136223846793005 + 1442695040888963407) & (2**64 - 1)
                        v = state >> 56
                    plane[y * (w // 2) + x] = v
            out.write(plane)


if __name__ == "__main__":
    main()
