#!/usr/bin/env python3
"""Dump the 0x44-byte settings descriptor table ("Event Log Lines" ... "!End of Settings")
found in EQAM-420B userland binaries (unpacked .img from bflt.py).
Record layout (inferred, 0x44 bytes): u32 zero, size, type, eeprom_off, stride; char name[0x30]
Usage: python3 -I settings_table.py IMG
"""
import struct, sys
d = open(sys.argv[1], "rb").read()
o = d.find(b"Event Log Lines\0")
if o < 0: sys.exit("table not found")
i = 0
while True:
    name = d[o:o + 0x30].split(b"\0")[0].decode("latin1")
    z, size, typ, off, stride = struct.unpack_from("<5I", d, o - 0x14)
    print(f"{i:3d} rec@{o-0x14:#07x} {name:24s} size={size:#5x} type={typ:#4x} eeprom={off:#06x} stride={stride:#5x}")
    if name.startswith("!End") or not name: break
    o += 0x44; i += 1
