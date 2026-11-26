#!/usr/bin/env python3
"""Reproducible extraction of the EQAM-420B EPCS dumps (docs/external/eqam-420b.zip).

Usage: python3 -I extract_firmware.py RAWDIR OUTDIR
  RAWDIR contains EPCS_1_1113.bin and EPCS_2_1113.bin (unzipped).
Writes:
  OUTDIR/epcs1/fpga1.rbf               FPGA1 (Nios host) bitstream, file bytes [0, sw_start)
  OUTDIR/epcs1/rec<N>_<addr>.bin       Nios II boot-copier records (bit-reversed back to CPU byte order)
  OUTDIR/epcs1/vmlinux.bin             gunzipped kernel image (raw, link/load address 0x00000000)
  OUTDIR/epcs1/initramfs.cpio          gunzipped initramfs (newc) embedded in the kernel
  OUTDIR/epcs2/fpga2_img0.rbf, fpga2_img1.rbf   the two bitstreams in EPCS_2 (@0x0, @0x800000)
Unpack the cpio yourself:  (cd OUTDIR/epcs1/rootfs && cpio -idm --no-absolute-filenames < ../initramfs.cpio)
and the bFLT userland with tools/bflt.py.

Facts used (see notes/firmware.md):
  * Dumps are in .rbf byte orientation (FF.. 6A at 0x20). Everything that is not FPGA config
    data must be bit-reversed per byte to get CPU-visible bytes (EPCS is read LSB-first).
  * FPGA config length (bits) is a 24/26-bit LSB-first field carried in bit 2 of the header bytes
    starting at 0x30 (Cyclone III/IV-style header). Software starts at ceil(bits/8) rounded to 2.
  * Nios II EPCS boot-copier records: u32 LE length, u32 LE dest address, payload; a record with
    length 0 carries the entry point.
"""
import os, sys, zlib

REV = bytes(int(f"{i:08b}"[::-1], 2) for i in range(256))


def cfg_len_bits(img, base=0, start_bit=15, nbits=24):
    hdr = img[base + 0x21: base + 0x49]
    bits = [(b >> 2) & 1 for b in hdr][start_bit:start_bit + nbits]
    return sum(b << i for i, b in enumerate(bits))


def last_non_ff(buf, start, end):
    i = end
    while i > start and buf[i - 1] == 0xFF:
        i -= 1
    return i


def main():
    raw, out = sys.argv[1], sys.argv[2]
    e1 = open(os.path.join(raw, "EPCS_1_1113.bin"), "rb").read()
    e2 = open(os.path.join(raw, "EPCS_2_1113.bin"), "rb").read()
    o1 = os.path.join(out, "epcs1"); o2 = os.path.join(out, "epcs2")
    os.makedirs(o1, exist_ok=True); os.makedirs(o2, exist_ok=True)

    bits = cfg_len_bits(e1)
    sw = (bits + 7) // 8
    sw += sw & 1
    print(f"EPCS_1: config length {bits:#x} bits -> software at {sw:#x}")
    open(os.path.join(o1, "fpga1.rbf"), "wb").write(e1[:sw])
    r = e1.translate(REV)
    off, i = sw, 0
    recs = []
    while True:
        ln = int.from_bytes(r[off:off + 4], "little"); addr = int.from_bytes(r[off + 4:off + 8], "little")
        print(f"  record {i} @ {off:#x}: len {ln:#x} -> {addr:#010x}")
        if ln == 0:
            print(f"  entry {addr:#010x}"); break
        p = r[off + 8: off + 8 + ln]
        recs.append((addr, p))
        open(os.path.join(o1, f"rec{i}_{addr:08x}.bin"), "wb").write(p)
        off += 8 + ln; i += 1
    # record 1 = u32 piggy length + gzip(vmlinux.bin)
    piggy = recs[1][1]
    vm = zlib.decompressobj(31).decompress(piggy[4:])
    open(os.path.join(o1, "vmlinux.bin"), "wb").write(vm)
    print(f"  vmlinux.bin {len(vm):#x} bytes")
    gz = vm.find(b"\x1f\x8b\x08\x00")
    while gz >= 0:
        try:
            cp = zlib.decompressobj(31).decompress(vm[gz:])
            if cp.startswith(b"070701"):
                open(os.path.join(o1, "initramfs.cpio"), "wb").write(cp)
                print(f"  initramfs at vmlinux+{gz:#x}: {len(cp):#x} bytes"); break
        except zlib.error:
            pass
        gz = vm.find(b"\x1f\x8b\x08", gz + 1)

    for n, base in enumerate((0, 0x800000)):
        end = last_non_ff(e2, base, base + 0x800000)
        print(f"EPCS_2 image {n} @ {base:#x}: data ends {end:#x} (len {end - base:#x}); "
              f"header len field (24b) {cfg_len_bits(e2, base):#x}")
        open(os.path.join(o2, f"fpga2_img{n}.rbf"), "wb").write(e2[base:end])


main()
