#!/usr/bin/env python3
"""Host-side re-implementation of the EQAM-420B (fw 1.1.13) config writers that damage the
8 KB EEPROM image. Static-analysis aid only: it never talks to the device.

Routines emulated (addresses are in the bflt.py images, base 0x10000000):
  qam      qamconfig.cgi  0x100002a8..0x10000344  QAM Config "Save" RF-channel fan-out
  csv      update_config  0x100000ec (main)       boot-time CSV reconcile (Automatic mode)
  kv       config.cgi     0x10004fcc              Key=Value upload parser (string fields)
  patch    writes a patched copy of qamconfig.cgi (11 words), see docs/notes/corruption-48.md

Examples:
  eqam_corrupt_sim.py qam --mode spts --slot 4 --channel 140
  eqam_corrupt_sim.py qam --replay-live
  eqam_corrupt_sim.py csv --mode spts work/device/ftp/mirror/fs/mnt/config_file.dat
  eqam_corrupt_sim.py patch work/firmware/epcs1/rootfs/home/httpd/cgi-bin/qamconfig.cgi out.cgi
"""
import argparse
import struct
import sys

SIZE = 0x2000
SPTS, MPTS = 0x80, 0x880
REGIONS = [
    (0x0080, 0x0880, "SPTS stream records (64 x 0x20)"),
    (0x0880, 0x0A80, "MPTS stream records (16 x 0x20)"),
    (0x0A80, 0x0A88, "EAS test duration / trigger port / eth"),
    (0x0A88, 0x0AC4, "EAS 1 record"),
    (0x0AC4, 0x0B00, "EAS 2 record"),
    (0x0B00, 0x0B20, "Pro:Idiom enables"),
    (0x0B20, 0x0E90, "unmapped (no descriptor; use unknown)"),
    (0x0E90, 0x0ED0, "Unit Name"),
    (0x0ED0, 0x0F10, "Unit Location"),
    (0x0F3C, 0x105C, "MPTS-mode VCT records"),
    (0x10E0, 0x1100, "SNMP communities"),
    (0x1100, 0x1128, "web users / passwords"),
    (0x1128, 0x1134, "mgmt IP / mask / gateway (0x1128..B = corruption guard)"),
    (0x1140, 0x1160, "Local 1 / GigE IPs"),
    (0x1160, 0x15E0, "SPTS VCT records (64 x 0x12)"),
]
# "next channel up" exceptions of the CEA-542 table used by the CGIs (rest is n+1)
NEXT = {6: 95, 99: 14, 94: 100, 13: 23, 22: 7, 158: 0}


def nxt(ch):
    return NEXT.get(ch, ch + 1)


def region(off):
    for lo, hi, name in REGIONS:
        if lo <= off < hi:
            return name
    return "?"


def blank(mode):
    img = bytearray(SIZE)
    img[0x7B] = 1 if mode == "mpts" else 0
    img[0xE90:0xE90 + 13] = b"Ball Holdings"
    img[0xED0:0xED0 + 6] = b"Rack 3"
    img[0x1128:0x112C] = bytes([172, 16, 70, 1])
    return img


def count(img):
    return 16 if img[0x7B] == 1 else 64


def diff(a, b, limit=40):
    n = 0
    for i in range(SIZE):
        if a[i] != b[i]:
            n += 1
            if n <= limit:
                print(f"  0x{i:04X}: {a[i]:02X} -> {b[i]:02X}   {region(i)}")
    if n > limit:
        print(f"  ... {n - limit} more")
    print(f"  {n} byte(s) changed")


def qam_save(img, slot, ch_a, fixed=False):
    """slot is 0-based. Original: one offset slot*(count>>2)*0x20 is used for BOTH tables."""
    b, c = nxt(ch_a), nxt(nxt(ch_a))
    d = nxt(c)
    off = slot * (count(img) >> 2) * 0x20
    s_off = slot * 0x200 if fixed else off
    m_off = slot * 0x80 if fixed else off
    for q, ch in enumerate((ch_a, b, c, d)):
        for p in range(4):
            img[0x91 + s_off + (q * 4 + p) * 0x20] = ch & 0xFF       # SPTS RF CH
        img[0x891 + m_off + q * 0x20] = ch & 0xFF                    # MPTS RF CH (no bound)


def cmd_qam(a):
    if a.replay_live:
        # history that reproduces the live export (work/device/config_file.dat)
        img = blank("spts")
        ref = bytes(img)
        for mode, slot, ch in (("spts", 1, 2), ("spts", 2, 10), ("spts", 3, 6),
                               ("mpts", 1, 2), ("mpts", 4, 140)):
            img[0x7B] = 1 if mode == "mpts" else 0
            qam_save(img, slot - 1, ch)
        print("SPTS RF CH:", [img[0x91 + i * 0x20] for i in range(64)])
        print("MPTS RF CH:", [img[0x891 + i * 0x20] for i in range(16)])
        print("bytes outside both tables:")
        for i in range(0xA80, SIZE):
            if img[i] != ref[i]:
                print(f"  0x{i:04X} = {img[i]}   {region(i)}")
        return
    img = blank(a.mode)
    before = bytes(img)
    qam_save(img, a.slot - 1, a.channel, a.fixed)
    print(f"QAM Config save, {a.mode.upper()} mode, RF module {a.slot}, channel A {a.channel}"
          f"{' (patched)' if a.fixed else ''}")
    diff(before, img, 80)
    print("Unit Name    :", bytes(img[0xE90:0xED0]).split(b"\0")[0])
    print("Unit Location:", bytes(img[0xED0:0xF10]).split(b"\0")[0])


def strtok_all(line, delims=b",-."):
    out, cur = [], bytearray()
    for ch in line:
        if ch in delims:
            if cur:
                out.append(bytes(cur))
                cur = bytearray()
        else:
            cur.append(ch)
    if cur:
        out.append(bytes(cur))
    return out


def atoi(tok):
    s = tok.lstrip(b" \t\n\r")
    n = 0
    neg = s[:1] == b"-"
    for ch in s[1:] if s[:1] in (b"-", b"+") else s:
        if not 48 <= ch <= 57:
            break
        n = n * 10 + ch - 48
    return -n if neg else n


def update_config(img, text, null_bytes=b":p\x01\x00"):
    """Boot-time reconcile. null_bytes = what a read of address 0 returns (strtok ran dry and the
    code passes NULL to atoi/strncpy; on this no-MMU CPU that reads RAM at 0, here the first bytes
    of vmlinux.bin). Returns the number of differing fields."""
    if img[0x40] != 0:
        return 0
    n, mpts = count(img), img[0x7B] == 1
    base = MPTS if mpts else SPTS
    recs = [dict(ip=bytes(4), port=0, udp=0, name=bytes(10), rf=0, prog=0) for _ in range(n + 1)]
    for i, line in enumerate(text.split(b"\n")[: n + 1]):       # reads n+1 lines (off by one)
        if not line and i == len(text.split(b"\n")) - 1:
            break
        t = strtok_all(line + b"\n")
        get = lambda k: t[k] if k < len(t) else null_bytes
        name = get(6).split(b"\0")[0][:10]
        recs[i] = dict(udp=atoi(get(0)) & 0xFF, ip=bytes(atoi(get(k)) & 0xFF for k in (1, 2, 3, 4)),
                       port=atoi(get(5)), name=name.ljust(10, b"\0"), rf=atoi(get(7)),
                       prog=0 if mpts else atoi(get(8)))
    diffs = 0
    for i in range(n):
        o, r = base + i * 0x20, recs[i]
        ee = dict(ip=bytes(img[o:o + 4]), port=img[o + 4] << 8 | img[o + 5], udp=img[o + 6],
                  name=bytes(img[o + 7:o + 17]), rf=img[o + 0x11],
                  prog=0 if mpts else img[o + 0x12] << 8 | img[o + 0x13])
        diffs += sum(ee[k] != r[k] for k in ee if k != "name")
        diffs += ee["name"].split(b"\0")[0] != r["name"].split(b"\0")[0]
    if diffs:
        for i in range(n):
            o, r = base + i * 0x20, recs[i]
            img[o:o + 4] = r["ip"]
            img[o + 4], img[o + 5], img[o + 6] = (r["port"] >> 8) & 0xFF, r["port"] & 0xFF, r["udp"]
            img[o + 7:o + 17] = r["name"]
            img[o + 0x11] = r["rf"] & 0xFF
            img[o + 0x12], img[o + 0x13] = (r["prog"] >> 8) & 0xFF, r["prog"] & 0xFF
        img[0x10DE] = 0
    return diffs


def csv_line(udp, ip, port, name, rf, prog=None):
    """Line as admin.cgi 'Commit Changes to Config File' writes it (fw 1.1.13)."""
    s = f"{udp},{ip},{port},{name if name else '0'},{rf}"
    return (s + (f"-{prog}" if prog is not None else "")).encode()


def cmd_csv(a):
    img = blank(a.mode)
    n = count(img)
    base = MPTS if a.mode == "mpts" else SPTS
    for i in range(n):                                   # a fully configured unit
        o = base + i * 0x20
        img[o:o + 4] = bytes([239, 1, 1, i + 1])
        img[o + 4], img[o + 5] = 5000 >> 8, 5000 & 0xFF
        nm = (a.name or f"CH{i + 1}").encode()[:10]
        img[o + 7:o + 7 + len(nm)] = nm
        img[o + 0x11] = 2 + i // (n // 16)
        img[o + 0x13] = i % 4 + 1
    if a.file:
        text = open(a.file, "rb").read()
        print(f"config file: {a.file} ({len(text.splitlines())} lines)")
    else:                                                # round trip through the firmware's writer
        text = b"\n".join(
            csv_line(img[base + i * 0x20 + 6], ".".join(map(str, img[base + i * 0x20:base + i * 0x20 + 4])),
                     5000, bytes(img[base + i * 0x20 + 7:base + i * 0x20 + 17]).split(b"\0")[0].decode(),
                     img[base + i * 0x20 + 0x11], None if a.mode == "mpts" else img[base + i * 0x20 + 0x13])
            for i in range(n)) + b"\n"
        print("config file: committed by admin.cgi from the current settings (round trip)")
    before = bytes(img)
    d = update_config(img, text)
    print(f"update_config at boot, {a.mode.upper()} mode, Automatic: {d} differing field(s)")
    for i in sorted(set([0, 1, 47, 48, n - 1]) & set(range(n))):
        o = base + i * 0x20
        show = lambda b: (f"{'.'.join(map(str, b[o:o + 4]))}:{b[o + 4] << 8 | b[o + 5]} "
                          f"name={bytes(b[o + 7:o + 17]).split(bytes(1))[0]!r} rf={b[o + 0x11]} "
                          f"prog={b[o + 0x12] << 8 | b[o + 0x13]}")
        print(f"  stream {i + 1:2d}: {show(before)}\n          -> {show(img)}")
    wiped = sum(1 for i in range(n) if not any(img[base + i * 0x20:base + i * 0x20 + 4])
                and any(before[base + i * 0x20:base + i * 0x20 + 4]))
    print(f"  streams whose group IP was zeroed: {wiped}")


def cmd_kv(a):
    """String/short-name handling of the Key=Value parser for one SPTS block."""
    img = blank("spts")
    i = a.stream - 1
    img[0x91 + i * 0x20] = 77
    img[0x1160 + (i + 1) * 0x12:0x1164 + (i + 1) * 0x12] = bytes([9, 0, 4, 0])  # next major/minor
    before = bytes(img)
    val = a.name.encode() + (b"\r\n" if a.crlf else b"\n")     # fgets keeps the line ending
    o = 0x87 + i * 0x20
    img[o:o + 10] = bytes(10)
    img[o:o + len(val)] = val                                  # memcpy(strlen), no clamp to 10
    s = 0x1164 + i * 0x12
    img[s:s + 0x12] = bytes(0x12)                              # memset(size 0x12), field is 14
    for k, ch in enumerate(val[:7]):
        img[s + 2 * k], img[s + 2 * k + 1] = 0, ch
    print(f"Key=Value upload, SPTS_ID={i}, Name/Short Name = {a.name!r}, "
          f"{'CRLF' if a.crlf else 'LF'} line ends")
    diff(before, img)


PATCH = {  # image offset: (original word, patched word); bFLT file offset = 0x40 + image offset
    0x2A8: (0x9805883A, 0xA804927A),  # mov r2,r19          -> slli r2,r21,9
    0x2AC: (0x9800010E, 0x1705883A),  # bge r19,zero,+4     -> add  r2,r2,fp
    0x2B0: (0x988000C4, 0xA81291FA),  # addi r2,r19,3       -> slli r9,r21,7
    0x2C0: (0x1005D0BA, 0x4F13883A),  # srai r2,r2,2        -> add  r9,r9,fp
    0x2CC: (0xA885383A, 0x0001883A),  # mul r2,r21,r2       -> nop
    0x2D8: (0x1004917A, 0x0001883A),  # slli r2,r2,5        -> nop
    0x2E8: (0x1705883A, 0x0001883A),  # add r2,r2,fp        -> nop
    0x2EC: (0x11C2D745, 0x49C2D745),  # stb r7,2909(r2)     -> stb r7,2909(r9)
    0x2F4: (0x1182DF45, 0x4982DF45),  # stb r6,2941(r2)     -> stb r6,2941(r9)
    0x30C: (0x10C2E745, 0x48C2E745),  # stb r3,2973(r2)     -> stb r3,2973(r9)
    0x344: (0x1442CF45, 0x4C42CF45),  # stb r17,2877(r2)    -> stb r17,2877(r9)
}


def cmd_patch(a):
    f = bytearray(open(a.src, "rb").read())
    if f[:4] != b"bFLT" or struct.unpack(">I", f[8:12])[0] != 0x50:
        sys.exit("not the expected uncompressed bFLT")
    for off, (old, new) in PATCH.items():
        if struct.unpack_from("<I", f, 0x40 + off)[0] != old:
            sys.exit(f"unexpected word at image 0x{off:X}; wrong firmware version?")
        struct.pack_into("<I", f, 0x40 + off, new)
    open(a.dst, "wb").write(f)
    print(f"wrote {a.dst}: {len(PATCH)} words changed (UNTESTED on hardware)")


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="cmd", required=True)
    q = sub.add_parser("qam")
    q.add_argument("--mode", choices=["spts", "mpts"], default="spts")
    q.add_argument("--slot", type=int, default=4, help="RF module 1..4")
    q.add_argument("--channel", type=int, default=140)
    q.add_argument("--fixed", action="store_true", help="behavior with the binary patch")
    q.add_argument("--replay-live", action="store_true")
    q.set_defaults(fn=cmd_qam)
    c = sub.add_parser("csv")
    c.add_argument("--mode", choices=["spts", "mpts"], default="spts")
    c.add_argument("--name", help="stream name to give every stream (try 'ESPN-HD')")
    c.add_argument("file", nargs="?")
    c.set_defaults(fn=cmd_csv)
    k = sub.add_parser("kv")
    k.add_argument("--stream", type=int, default=1)
    k.add_argument("--name", default="0123456789")
    k.add_argument("--crlf", action="store_true")
    k.set_defaults(fn=cmd_kv)
    t = sub.add_parser("patch")
    t.add_argument("src")
    t.add_argument("dst")
    t.set_defaults(fn=cmd_patch)
    a = p.parse_args()
    a.fn(a)


if __name__ == "__main__":
    main()
