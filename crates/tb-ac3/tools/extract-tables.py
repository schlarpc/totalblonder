#!/usr/bin/env python3
"""Extract the AC-3 tables that tb-ac3 hard-codes from the text of ATSC A/52:2012.

Usage:
    pdftotext -layout docs/external/standards/2026-11-24-atsc-itu-etsi/A52-201212-17.pdf - \
        | python3 -I crates/tb-ac3/tools/extract-tables.py > crates/tb-ac3/tests/data/a52_tables.txt

The output is one line per table, `name: v0 v1 ...`, in the order of the table's index. The
test `tests/tables_pdf.rs` compares every constant in `src/tables.rs` with this file, so the
constants are pinned to what the PDF says, not to what anyone remembered.
"""

import re
import sys

text = sys.stdin.read()
lines = text.split("\n")


def find(title):
    # The last occurrence: the first ones are in the list of tables at the front.
    hits = [i for i, line in enumerate(lines) if title in line]
    if not hits:
        raise SystemExit(f"table not found: {title}")
    return hits[-1]


def num(tok):
    tok = tok.replace(",", "")
    if tok.lower().startswith("0x"):
        return int(tok, 16)
    if "." in tok:
        return float(tok)
    return int(tok)


def address_table(title, count):
    """Tables 7.6-7.11: rows of `address value`."""
    i = find(title) + 1
    out = {}
    while len(out) < count:
        toks = lines[i].split()
        if len(toks) == 2 and re.fullmatch(r"\d+", toks[0]) and toks[1].lower().startswith("0x"):
            out[int(toks[0])] = int(toks[1], 16)
        i += 1
    return [out[k] for k in range(count)]


def two_group_table(title, count, ncols):
    """Tables with two side-by-side column groups (`idx v.. idx v..`)."""
    i = find(title) + 1
    out = {}
    while len(out) < count:
        toks = lines[i].split()
        if toks and all(re.fullmatch(r"0x[0-9a-fA-F]+|\d+", t) for t in toks):
            vals = [num(t) for t in toks]
            if len(vals) == 2 * (ncols + 1):
                groups = [vals[: ncols + 1], vals[ncols + 1 :]]
            elif len(vals) == ncols + 1:
                groups = [vals]
            else:
                groups = []
            for g in groups:
                out[g[0]] = g[1:]
        i += 1
    return [out[k] for k in range(count)]


def grid_table(title, count):
    """Tables laid out as rows `A=n v0 .. v9` with index 10*A + B."""
    i = find(title) + 1
    out = []
    while len(out) < count:
        m = re.match(r"\s*A=(\d+)\s+(.*)", lines[i])
        if m:
            out.extend(num(t) for t in m.group(2).split())
        i += 1
    return out[:count]


def frame_size_table(title, column):
    i = find(title) + 1
    out = []
    while len(out) < 38:
        m = re.match(r"\s*‘([01]{6})’\s+\((\d+)\)\s+(\d+) kbps\s+(\d+)\s+(\d+)\s+(\d+)", lines[i])
        if m:
            out.append(int(m.group(3 + column)))
        i += 1
    return out


def emit(name, values):
    print(f"{name}: " + " ".join(str(v) for v in values))


emit("slowdec", address_table("Table 7.6 Slow Decay Table", 4))
emit("fastdec", address_table("Table 7.7 Fast Decay Table", 4))
emit("slowgain", address_table("Table 7.8 Slow Gain Table", 4))
emit("dbpbtab", address_table("Table 7.9 dB/Bit Table", 4))
emit("floortab", address_table("Table 7.10 Floor Table", 8))
emit("fastgain", address_table("Table 7.11 Fast Gain Table", 8))
bands = two_group_table("Table 7.12 Banding Structure Tables", 50, 2)
emit("bndtab", [b[0] for b in bands])
emit("bndsz", [b[1] for b in bands])
emit("masktab", grid_table("Table 7.13 Bin Number to Band Number Table", 256))
emit("latab", grid_table("Table 7.14 Log-Addition Table", 256))
hth = two_group_table("Table 7.15 Hearing Threshold Table", 50, 3)
emit("hth48", [h[0] for h in hth])
emit("baptab", [b[0] for b in two_group_table("Table 7.16 Bit Allocation Pointer Table", 64, 1)])
emit("window", grid_table("Table 7.33 Transform Window Sequence", 256))
# Table 5.18, fs = 48 kHz column: words per syncframe for frmsizecod 0..37.
emit("kbps", frame_size_table("Table 5.18 Frame Size Code Table", 0))
emit("words48", frame_size_table("Table 5.18 Frame Size Code Table", 3))
# Table 7.34, fs = 48 kHz column: words in the first 5/8 of the syncframe.
emit("words58_48", frame_size_table("Table 7.34 5/8_frame Size Table", 3))
