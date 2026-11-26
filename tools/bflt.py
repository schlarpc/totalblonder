#!/usr/bin/env python3
"""bFLT (uClinux flat binary, v4) unpacker for the EQAM-420B Nios II userland.

For each input bFLT file writes, into OUTDIR:
  <name>.img   raw "link space" image: file bytes from offset 0x40 (text, then data),
               decompressed if FLAT_FLAG_GZIP/GZDATA, with relocations applied for --base
  <name>.elf   minimal ELF32-LE (e_machine=113 EM_ALTERA_NIOS2) with text/data/bss
               program headers at --base, entry = base + (entry - 0x40)
  <name>.json  header fields + relocation list

Layout notes (from linux-2.6 fs/binfmt_flat.c, MAX_SHARED_LIBS=1):
  * Relocation entries are big-endian u32 "r-space" offsets: r-space 0 == file offset 0x40.
  * Text and data are contiguous in r-space (data r = data_start-0x40 ...), so the file
    minus its 64-byte header is a valid image linked at address 0. The kernel inserts a
    4-byte gap before data at runtime, but r-space pointers already account for that.
  * Nios II encodes a type in the top 4 bits of each reloc entry (arch/nios2 flat.h):
    0 = R_32 (LE word), 1 = R_HI_LO (movhi/ori pair), 2 = R_HIADJ_LO (movhi/addi pair),
    4 = R_CALL26 (call insn). Values are r-space addresses; we add --base to them.
    --base must be 256MB aligned so CALL26 targets stay valid unchanged.
  * GOTPIC (flag 0x2) binaries additionally have a GOT at start of data terminated by -1;
    entries there are relocated too.

Usage: python3 -I bflt.py [--base 0x...] OUTDIR FILE...
"""
import argparse, gzip, json, os, struct, zlib

HDR = struct.Struct(">4sIIIIIIIII I")  # magic rev entry data_start data_end bss_end stack reloc_start reloc_count flags build_date
FLAG_RAM, FLAG_GOTPIC, FLAG_GZIP, FLAG_GZDATA = 1, 2, 4, 8


def gunzip_stream(buf):
    d = zlib.decompressobj(31)
    out = d.decompress(buf)
    return out, d.unused_data


def unpack(path):
    raw = open(path, "rb").read()
    magic, rev, entry, data_start, data_end, bss_end, stack, reloc_start, reloc_count, flags, bdate = HDR.unpack_from(raw, 0)
    if magic != b"bFLT":
        raise ValueError("not bFLT")
    if flags & FLAG_GZIP:
        body, rest = gunzip_stream(raw[0x40:])
        full = raw[:0x40] + body
    elif flags & FLAG_GZDATA:
        body, rest = gunzip_stream(raw[data_start:])
        full = raw[:data_start] + body
    else:
        full = raw
    h = dict(rev=rev, entry=entry, data_start=data_start, data_end=data_end, bss_end=bss_end,
             stack=stack, reloc_start=reloc_start, reloc_count=reloc_count, flags=flags, build_date=bdate)
    relocs = [struct.unpack_from(">I", full, reloc_start + 4 * i)[0] for i in range(reloc_count)]
    return h, full, relocs


def build(h, full, relocs, base):
    img = bytearray(full[0x40:h["data_end"]])
    n_text = h["data_start"] - 0x40
    bad = []
    for rel in relocs:
        typ, r = rel >> 28, rel & 0x0fffffff
        if r + (8 if typ in (1, 2) else 4) > len(img):
            bad.append(rel); continue
        if typ == 0:  # FLAT_NIOS2_R_32: plain LE word
            v = struct.unpack_from("<I", img, r)[0]
            struct.pack_into("<I", img, r, (v + base) & 0xffffffff)
        elif typ in (1, 2):  # R_HI_LO (movhi+ori) / R_HIADJ_LO (movhi+addi): IMM16 at bits 21:6
            i1, i2 = struct.unpack_from("<II", img, r)
            hi, lo = (i1 >> 6) & 0xffff, (i2 >> 6) & 0xffff
            if typ == 1:
                v = ((hi << 16) | lo) + base
                hi, lo = (v >> 16) & 0xffff, v & 0xffff
            else:
                v = ((hi << 16) + (lo - 0x10000 if lo & 0x8000 else lo) + base) & 0xffffffff
                lo = v & 0xffff
                hi = ((v >> 16) + (1 if lo & 0x8000 else 0)) & 0xffff
            i1 = (i1 & ~(0xffff << 6)) | (hi << 6)
            i2 = (i2 & ~(0xffff << 6)) | (lo << 6)
            struct.pack_into("<II", img, r, i1, i2)
        elif typ == 4:  # R_CALL26: target keeps PC's top nibble; nothing to do for 256MB-aligned base
            pass
        else:
            bad.append(rel)
    got = 0
    if h["flags"] & FLAG_GOTPIC:
        o = n_text
        while o + 4 <= len(img):
            v = struct.unpack_from("<I", img, o)[0]
            if v == 0xffffffff: break
            if v: struct.pack_into("<I", img, o, (v + base) & 0xffffffff)
            o += 4; got += 1
    return bytes(img), n_text, bad, got


def make_elf(img, n_text, h, base):
    bss_len = h["bss_end"] - h["data_end"]
    ehsz, phsz = 52, 32
    off_text = 0x1000
    off_data = off_text + n_text
    data = img[n_text:]
    phdrs = [
        (1, off_text, base, base, n_text, n_text, 5, 4),
        (1, off_data, base + n_text, base + n_text, len(data), len(data) + bss_len, 6, 4),
    ]
    shstr = b"\0.text\0.data\0.bss\0.shstrtab\0"
    out = bytearray(ehsz + phsz * len(phdrs))
    for i, p in enumerate(phdrs):
        struct.pack_into("<IIIIIIII", out, ehsz + i * phsz, *p)
    out += b"\0" * (off_text - len(out))
    out += img
    off_shstr = len(out)
    out += shstr
    while len(out) % 4: out += b"\0"
    off_sh = len(out)
    # name, type, flags, addr, offset, size, link, info, align, entsize
    shdrs = [
        (0, 0, 0, 0, 0, 0, 0, 0, 0, 0),
        (1, 1, 6, base, off_text, n_text, 0, 0, 4, 0),                       # .text  AX
        (7, 1, 3, base + n_text, off_data, len(data), 0, 0, 4, 0),           # .data  WA
        (13, 8, 3, base + len(img), off_data + len(data), bss_len, 0, 0, 4, 0),  # .bss NOBITS
        (18, 3, 0, 0, off_shstr, len(shstr), 0, 0, 1, 0),                    # .shstrtab
    ]
    for s in shdrs:
        out += struct.pack("<10I", *s)
    eh = struct.pack("<16sHHIIIIIHHHHHH",
                     b"\x7fELF\x01\x01\x01" + b"\0" * 9, 2, 113, 1,
                     base + h["entry"] - 0x40, ehsz, off_sh, 0, ehsz, phsz, len(phdrs), 40, len(shdrs), 4)
    out[:ehsz] = eh
    return bytes(out)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--base", type=lambda s: int(s, 0), default=0x10000000)
    ap.add_argument("outdir")
    ap.add_argument("files", nargs="+")
    a = ap.parse_args()
    os.makedirs(a.outdir, exist_ok=True)
    for f in a.files:
        try:
            h, full, relocs = unpack(f)
        except Exception as e:
            print(f"{f}: skip ({e})"); continue
        img, n_text, bad, got = build(h, full, relocs, a.base)
        name = os.path.basename(f)
        open(os.path.join(a.outdir, name + ".img"), "wb").write(img)
        open(os.path.join(a.outdir, name + ".elf"), "wb").write(make_elf(img, n_text, h, a.base))
        meta = dict(h, base=a.base, text_len=n_text, relocs=relocs, bad_relocs=bad, got_entries=got)
        json.dump(meta, open(os.path.join(a.outdir, name + ".json"), "w"), indent=1)
        print(f"{name:28s} flags={h['flags']:#x} text={n_text:#8x} data={h['data_end']-h['data_start']:#7x} "
              f"bss={h['bss_end']-h['data_end']:#7x} relocs={len(relocs):5d} bad={len(bad)} entry={a.base + h['entry'] - 0x40:#x}")


main()
