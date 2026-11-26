#!/usr/bin/env python3
"""Coarse flash layout scan: per-block entropy / fill classification, merged into regions.
Usage: python3 -I flashmap.py IMAGE [blocksize]
"""
import math, sys
from collections import Counter

def ent(b):
    if not b: return 0.0
    c = Counter(b); n = len(b)
    return -sum(v/n*math.log2(v/n) for v in c.values())

def cls(b):
    if all(x == 0xff for x in b): return "FF"
    if all(x == 0x00 for x in b): return "00"
    e = ent(b)
    ascii_ = sum(32 <= x < 127 or x in (9,10,13) for x in b)/len(b)
    if ascii_ > 0.85: return "text"
    if e > 7.9: return "hi"     # compressed/encrypted
    if e > 6.0: return "mid"
    return "lo"

def main():
    path = sys.argv[1]; bs = int(sys.argv[2], 0) if len(sys.argv) > 2 else 0x1000
    data = open(path, "rb").read()
    regions = []
    for off in range(0, len(data), bs):
        b = data[off:off+bs]; c = cls(b)
        if regions and regions[-1][2] == c:
            regions[-1][1] = off + len(b)
        else:
            regions.append([off, off+len(b), c])
    for s, e, c in regions:
        print(f"{s:#010x}-{e:#010x} {e-s:#10x} {c}")

main()
