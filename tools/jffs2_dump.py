#!/usr/bin/env python3
"""Offline JFFS2 (little-endian) dumper that keeps every node version, including
obsolete ones, so overwritten/deleted file contents can be recovered.

Usage: python3 -I tools/jffs2_dump.py IMAGE OUTDIR

Writes:
  OUTDIR/nodes.txt                    every node (offset, type, obsolete flag, fields)
  OUTDIR/current/<path>               files as the latest versions describe them
  OUTDIR/versions/<name>.ino<I>.v<V>  each inode node's own data fragment, uncompressed
  OUTDIR/history/<name>.ino<I>.upto_v<V>  file reconstructed from all versions <= V
Handles compression: none, zero, rtime, zlib.
"""
import os, struct, sys, zlib

JFFS2_MAGIC = 0x1985
DIRENT, INODE, CLEANMARKER, PADDING = 0xE001, 0xE002, 0x2003, 0x2004
ACCURATE = 0x2000
COMPR = {0: "none", 1: "zero", 2: "rtime", 3: "rubinmips", 4: "copy", 5: "dynrubin", 6: "zlib", 7: "lzo", 8: "lzma"}


def rtime_decompress(src, dlen):
    out = bytearray(); positions = [0] * 256; pos = 0
    while len(out) < dlen and pos < len(src):
        value = src[pos]; out.append(value); pos += 1
        repeat = src[pos]; pos += 1
        backoffs = positions[value]; positions[value] = len(out)
        for _ in range(repeat):
            out.append(out[backoffs]); backoffs += 1
    return bytes(out[:dlen])


def decompress(compr, data, dsize):
    if compr == 0: return data
    if compr == 1: return b"\0" * dsize
    if compr == 2: return rtime_decompress(data, dsize)
    if compr == 6: return zlib.decompress(data)
    raise ValueError(f"unsupported compression {COMPR.get(compr, compr)}")


def main():
    img = open(sys.argv[1], "rb").read(); out = sys.argv[2]
    os.makedirs(out, exist_ok=True)
    log = open(os.path.join(out, "nodes.txt"), "w")
    dirents, inodes = [], []
    off = 0
    while off + 12 <= len(img):
        magic, ntype, totlen, hcrc = struct.unpack_from("<HHII", img, off)
        if magic != JFFS2_MAGIC or totlen < 12 or off + totlen > len(img):
            off += 4; continue
        obsolete = not (ntype & ACCURATE)
        base = ntype | ACCURATE
        if base == DIRENT:
            pino, ver, ino, mctime, nsize, typ = struct.unpack_from("<IIIIBB", img, off + 12)
            name = img[off + 40: off + 40 + nsize].decode("latin-1")
            dirents.append(dict(off=off, pino=pino, ver=ver, ino=ino, name=name, mctime=mctime, obsolete=obsolete, typ=typ))
            log.write(f"{off:#08x} DIRENT {'OBS ' if obsolete else ''}pino={pino} ino={ino} ver={ver} type={typ} mctime={mctime} name={name!r}\n")
        elif base == INODE:
            (ino, ver, mode, uid, gid, isize, atime, mtime, ctime, doff, csize, dsize,
             compr, ucompr, flags, dcrc, ncrc) = struct.unpack_from("<IIIHHIIIIIIIBBHII", img, off + 12)
            raw = img[off + 68: off + 68 + csize]
            try: data = decompress(compr, raw, dsize); err = ""
            except Exception as e: data = b""; err = f" DECOMP-ERR {e}"
            inodes.append(dict(off=off, ino=ino, ver=ver, mode=mode, isize=isize, mtime=mtime, doff=doff,
                               dsize=dsize, data=data, obsolete=obsolete))
            log.write(f"{off:#08x} INODE  {'OBS ' if obsolete else ''}ino={ino} ver={ver} mode={mode:o} isize={isize} "
                      f"mtime={mtime} off={doff} dsize={dsize} csize={csize} compr={COMPR.get(compr, compr)}{err}\n")
        elif base in (CLEANMARKER, PADDING):
            log.write(f"{off:#08x} {'CLEANMARKER' if base == CLEANMARKER else 'PADDING'} len={totlen}\n")
        else:
            log.write(f"{off:#08x} type={ntype:#06x} len={totlen}\n")
        off += (totlen + 3) & ~3

    # names: latest dirent per (pino, name); ino 0 means unlinked
    names = {}
    for d in sorted(dirents, key=lambda d: d["ver"]):
        if d["obsolete"]:
            continue  # superseded or deleted; contents still go to versions/ and history/
        names[(d["pino"], d["name"])] = d
    allnames = {}
    for d in dirents:
        if d["ino"]: allnames.setdefault(d["ino"], d["name"])

    def path_of(ino, seen=()):
        for (pino, name), d in names.items():
            if d["ino"] == ino:
                return name if pino == 1 else os.path.join(path_of(pino, seen + (ino,)), name)
        return f"orphan_ino{ino}_{allnames.get(ino, 'noname')}"

    by_ino = {}
    for n in inodes: by_ino.setdefault(n["ino"], []).append(n)
    for sub in ("current", "versions", "history"): os.makedirs(os.path.join(out, sub), exist_ok=True)
    live = {d["ino"] for d in names.values() if d["ino"]}
    for ino, ns in sorted(by_ino.items()):
        ns.sort(key=lambda n: n["ver"])
        name = allnames.get(ino, f"ino{ino}")
        safe = name.replace("/", "_")
        for i, n in enumerate(ns):
            open(os.path.join(out, "versions", f"{safe}.ino{ino}.v{n['ver']}"), "wb").write(n["data"])
            buf = bytearray()
            for m in ns[: i + 1]:
                end = m["doff"] + len(m["data"])
                if len(buf) < end: buf.extend(b"\0" * (end - len(buf)))
                buf[m["doff"]:end] = m["data"]
            del buf[n["isize"]:]
            open(os.path.join(out, "history", f"{safe}.ino{ino}.upto_v{n['ver']}"), "wb").write(bytes(buf))
        if ino in live and (ns[-1]["mode"] & 0o170000) == 0o100000:
            p = os.path.join(out, "current", path_of(ino))
            os.makedirs(os.path.dirname(p) or ".", exist_ok=True)
            open(p, "wb").write(open(os.path.join(out, "history", f"{safe}.ino{ino}.upto_v{ns[-1]['ver']}"), "rb").read())
    print(f"{len(dirents)} dirents, {len(inodes)} inode nodes, {len(by_ino)} inodes")


if __name__ == "__main__":
    main()
