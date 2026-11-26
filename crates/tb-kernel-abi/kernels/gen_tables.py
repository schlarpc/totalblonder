#!/usr/bin/env python3
"""Generate crates/tb-kernel-abi/kernels/vlc_tables.h from the text of ITU-T H.262 (02/2000) Annex B.

Usage: gen_tables.py work/standards/ITU-T_H.262-200002-S.txt > crates/tb-kernel-abi/kernels/vlc_tables.h
       gen_tables.py --rust work/standards/ITU-T_H.262-200002-S.txt > crates/tb-m2v-syntax/src/vlc.rs
The text comes from `pdftotext -layout docs/external/standards/2026-11-24-itu-atsc-2/ITU-T_H.262-200002-S.pdf`.
The tables are parsed from the standard, not typed from memory; the script checks each table
for size and for the prefix-free property before writing it.
"""
import re
import sys

args = sys.argv[1:]
rust = bool(args) and args[0] == "--rust"
if rust:
    args = args[1:]
txt = open(args[0], encoding="utf-8", errors="replace").read().replace("–", "-").replace("−", "-")
lines = txt.split("\n")


def find(pat, start=0):
    for i in range(start, len(lines)):
        if re.search(pat, lines[i]):
            return i
    raise SystemExit("not found: " + pat)


# Annex B starts after the table of contents; anchor on the table captions.
b1 = find(r"^\s+Table B\.1 . Variable length codes for macroblock_address_increment")
b2 = find(r"Table B\.2 ", b1)
b9 = find(r"^\s+Table B\.9 . Variable length codes for coded_block_pattern", b2)
b10 = find(r"^\s+Table B\.10 . Variable length codes for motion_code", b9)
b11 = find(r"Table B\.11 ", b10)
b12 = find(r"^\s+Table B\.12 ", b11)
b13 = find(r"^\s+Table B\.13 ", b12)
b14 = find(r"^\s+Table B\.14 . DCT coefficients Table zero\s*$", b13)
b15 = find(r"^\s+Table B\.15 . DCT coefficients Table one\s*$", b14)
b16 = find(r"^\s+Table B\.16 ", b15)


def code(s):
    s = s.replace(" ", "")
    return int(s, 2), len(s)


def prefix_free(codes, name):
    strs = sorted(format(c, "0%db" % n) for c, n in codes)
    for a, b in zip(strs, strs[1:]):
        if b.startswith(a):
            raise SystemExit("%s: %s is a prefix of %s" % (name, a, b))


def pairs(lo, hi):
    """Two-column tables: '<code> <value>   <code> <value>'."""
    out = {}
    for ln in lines[lo:hi]:
        for m in re.finditer(r"((?:[01]+ )*[01]+)\s{2,}(-?\d+)(?= |$)", ln):
            if m.group(2) in out.values() and False:
                pass
            out[int(m.group(2))] = code(m.group(1))
    return out


mbai = pairs(b1, b2)
assert sorted(mbai) == list(range(1, 34)), sorted(mbai)
prefix_free(list(mbai.values()) + [code("00000001000")], "B.1")

cbp = pairs(b9, b10)
assert sorted(cbp) == list(range(0, 64)), sorted(cbp)
prefix_free(cbp.values(), "B.9")

mot = pairs(b10, b11)
assert sorted(mot) == list(range(-16, 17)), sorted(mot)
prefix_free(mot.values(), "B.10")

dcl = pairs(b12, b13)
dcc = pairs(b13, b14)
assert sorted(dcl) == list(range(12)) and sorted(dcc) == list(range(12))
prefix_free(dcl.values(), "B.12")
prefix_free(dcc.values(), "B.13")


def dct(lo, hi, name, eob):
    t = {}
    for ln in lines[lo:hi]:
        ln = re.sub(r"\(Note \d\)", "", ln)
        m = re.match(r"^\s*((?:[01]+ ?)+?)\s*s\s+(\d+)\s+(\d+)\s*$", ln)
        if not m:
            continue
        c, n = code(m.group(1))
        run, level = int(m.group(2)), int(m.group(3))
        if name == "B.14" and (c, n) == (1, 1):
            continue  # Note 3: '1 s' is only for the first coefficient of a non-intra block
        assert (run, level) not in t, (name, run, level)
        t[(run, level)] = (c, n)  # without the sign bit
    prefix_free([(c * 2, n + 1) for c, n in t.values()] + [code(eob), code("000001")], name)
    return t


t14 = dct(b14, b15, "B.14", "10")
t15 = dct(b15, b16, "B.15", "0110")
assert len(t14) == 111 and len(t15) == 111, (len(t14), len(t15))
assert set(t14) == set(t15)

# Dense layout: index = run * 41 + level (level 1..40, run 0..31); len 0 means "use escape".
MAXRUN, MAXLEV = 32, 41


def dense(t):
    arr = [(0, 0)] * (MAXRUN * MAXLEV)
    for (run, level), (c, n) in t.items():
        arr[run * MAXLEV + level] = (c, n)
    return arr


def emit(name, arr, ctype="VlcCode"):
    print("M2V_TABLE %s %s[%d] = {" % (ctype, name, len(arr)))
    for i in range(0, len(arr), 8):
        print("  " + " ".join("{0x%x,%d}," % cn for cn in arr[i:i + 8]))
    print("};")


def emit_rust(name, arr, doc):
    print()
    print("/// %s" % doc)
    print("pub const %s: [VlcCode; %d] = [" % (name.upper(), len(arr)))
    for c, n in arr:
        print("    v(0x%x, %d)," % (c, n))
    print("];")


TABLES = [
    ("vlc_mbai", [(0, 0)] + [mbai[i] for i in range(1, 34)],       # index = increment 1..33
     "macroblock_address_increment, Table B.1, by increment (1..=33; entry 0 unused)."),
    ("vlc_cbp", [cbp[i] for i in range(64)],                       # index = cbp
     "coded_block_pattern, Table B.9, by pattern."),
    ("vlc_motion", [mot[i] for i in range(-16, 17)],               # index = motion_code + 16
     "motion_code, Table B.10, by code + 16."),
    ("vlc_dc_lum", [dcl[i] for i in range(12)],                    # index = dct_dc_size
     "dct_dc_size_luminance, Table B.12, by size."),
    ("vlc_dc_chr", [dcc[i] for i in range(12)],
     "dct_dc_size_chrominance, Table B.13, by size."),
    ("vlc_dct0", dense(t14),                                       # Table B.14, no sign bit
     "DCT coefficients table zero (B.14) without the sign bit, by `run * %d + level`; len 0: escape." % MAXLEV),
    ("vlc_dct1", dense(t15),                                       # Table B.15, no sign bit
     "DCT coefficients table one (B.15) without the sign bit, by `run * %d + level`; len 0: escape." % MAXLEV),
]

if rust:
    print("//! Variable-length code tables of H.262 (02/2000) Annex B.")
    print("//!")
    print("//! Generated by `crates/tb-kernel-abi/kernels/gen_tables.py --rust` from the text of the standard. Do not edit.")
    print()
    print("use tb_kernel_abi::{VLC_DCT_CODES, VLC_DCT_MAXLEV, VLC_DCT_MAXRUN, VlcCode};")
    print()
    print("const _: () = assert!(VLC_DCT_MAXRUN == %d && VLC_DCT_MAXLEV == %d && VLC_DCT_CODES == %d);"
          % (MAXRUN, MAXLEV, MAXRUN * MAXLEV))
    print()
    print("const fn v(code: u16, len: u8) -> VlcCode {")
    print("    VlcCode { code, len, pad: 0 }")
    print("}")
    for name, arr, doc in TABLES:
        emit_rust(name[4:], arr, doc)
    sys.exit(0)

print("// Generated by gen_tables.py from ITU-T H.262 (02/2000) Annex B. Do not edit.")
print("#pragma once")
print("// VlcCode, VLC_DCT_MAXRUN (%d) and VLC_DCT_MAXLEV (%d) come from tb_kernel_abi.h." % (MAXRUN, MAXLEV))
print("#include \"tb_kernel_abi.h\"")
print("static_assert(VLC_DCT_MAXRUN == %d && VLC_DCT_MAXLEV == %d, \"vlc_tables.h and tb_kernel_abi.h disagree\");" % (MAXRUN, MAXLEV))
for name, arr, _ in TABLES:
    emit(name, arr)
