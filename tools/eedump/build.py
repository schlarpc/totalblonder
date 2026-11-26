#!/usr/bin/env python3
"""Build tools/eedump/eedump.S into a bFLT v4 (RAM-loaded, no relocations) and check it.
Usage: python3 -I build.py OUTFILE      (needs work/tools-build/inst/bin/nios2-elf-{as,ld,objcopy,objdump})
Checks: disassembly contains only the expected mnemonics, the only absolute addresses are
0x12000810 (ldwio) and 0x13010000 (ldw), no store goes anywhere but the stack buffer, and a
small interpreter run with fake hardware produces the right syscalls and file contents."""
import os, struct, subprocess, sys, tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
BIN = os.path.join(HERE, "..", "..", "work", "tools-build", "inst", "bin")
out = sys.argv[1]
tmp = tempfile.mkdtemp()
run = lambda *a: subprocess.run(a, check=True, capture_output=True, text=True).stdout
run(f"{BIN}/nios2-elf-as", "-o", f"{tmp}/e.o", f"{HERE}/eedump.S")
run(f"{BIN}/nios2-elf-ld", "-Ttext=0", "-e", "_start", "-o", f"{tmp}/e.elf", f"{tmp}/e.o")
run(f"{BIN}/nios2-elf-objcopy", "-O", "binary", "-j", ".text", f"{tmp}/e.elf", f"{tmp}/e.bin")
text = open(f"{tmp}/e.bin", "rb").read()
assert len(text) % 4 == 0
data = bytes(16)                                   # dummy data segment, as stock binaries have one
ds = 0x40 + len(text)
de = ds + len(data)
hdr = struct.pack(">4s9I", b"bFLT", 4, 0x40, ds, de, de + 16, 0x4000, de, 0, 1) + bytes(24)
assert len(hdr) == 0x40
open(out, "wb").write(hdr + text + data)

dis = run(f"{BIN}/nios2-elf-objdump", "-d", f"{tmp}/e.elf")
print(dis)
allowed = {"addi", "movhi", "ldwio", "andi", "beq", "mov", "movi", "ldw", "stw", "bne", "nextpc",
           "trap", "blt", "cmpnei", "br"}
ncode = text.index(b"/tmp/ee.bin")
for line in dis.splitlines():
    p = line.split("\t")
    if len(p) >= 3 and p[0].strip().endswith(":") and int(p[0].strip()[:-1], 16) < ncode:
        assert p[2].split()[0] in allowed, line
        if p[2].startswith("stw"):
            assert p[3].strip() == "r7,0(r4)", line   # the one store: into the stack buffer

# ---- tiny interpreter for exactly these instructions, with fake hardware ----
MIRROR = bytes((i * 7 + 3) & 0xFF for i in range(0x2000))
TEXT, STACK = 0x00800040, 0x00900000
mem = {}
reg = [0] * 32
reg[27] = STACK
pc = TEXT
polls, files, status, steps = 0, {}, None, 0
s16 = lambda v: v - 0x10000 if v & 0x8000 else v
u32 = lambda v: v & 0xFFFFFFFF

def load(a):
    global polls
    if a == 0x12000810:
        polls += 1
        return 1 if polls > 3 else 0              # not ready for the first three polls
    if 0x13010000 <= a < 0x13012000:
        return struct.unpack_from("<I", MIRROR, a - 0x13010000)[0]
    if STACK - 8208 <= a < STACK:
        return mem.get(a, 0)
    raise SystemExit(f"emulation: unexpected load from {a:#x}")

def rdbytes(a, n):
    b = bytearray()
    for i in range(n):
        x = a + i
        if TEXT <= x < TEXT + len(text):
            b.append(text[x - TEXT])
        else:
            b.append((mem.get(x & ~3, 0) >> (8 * (x & 3))) & 0xFF)
    return bytes(b)

while status is None:
    steps += 1
    assert steps < 100000 and TEXT <= pc < TEXT + ncode, hex(pc)
    w = struct.unpack_from("<I", text, pc - TEXT)[0]
    op, a, b, imm = w & 0x3F, w >> 27, (w >> 22) & 31, (w >> 6) & 0xFFFF
    npc = pc + 4
    if op == 0x3A:
        c, opx = (w >> 17) & 31, (w >> 11) & 0x3F
        if opx == 0x31: val = u32(reg[a] + reg[b])                      # add / mov
        elif opx == 0x1C: val = npc                                     # nextpc
        elif opx == 0x2D:                                               # trap
            assert reg[2] == 0
            n, c = reg[3], 2
            if n == 5:
                name = rdbytes(reg[4], 12)
                assert name == b"/tmp/ee.bin\0" and reg[5] == 0x241 and reg[6] == 0x1A4
                val = 3
            elif n == 4:
                assert reg[4] == 3 and reg[6] == 0x2000
                files["ee"] = rdbytes(reg[5], reg[6]); val = reg[6]
            elif n == 6: assert reg[4] == 3; val = 0
            elif n == 1: status = reg[4]; val = 0
            else: raise SystemExit(f"emulation: unexpected syscall {n}")
        else: raise SystemExit(f"emulation: opx {opx:#x}")
        if c: reg[c] = val
    elif op == 0x04: reg[b] = u32(reg[a] + s16(imm))                    # addi
    elif op == 0x34: reg[b] = imm << 16                                 # orhi (movhi)
    elif op == 0x0C: reg[b] = reg[a] & imm                              # andi
    elif op in (0x37, 0x17): reg[b] = load(u32(reg[a] + s16(imm)))      # ldwio / ldw
    elif op == 0x15:                                                    # stw
        addr = u32(reg[a] + s16(imm))
        assert STACK - 8208 <= addr < STACK - 16, f"store to {addr:#x}"
        mem[addr] = reg[b]
    elif op == 0x26: npc = npc + s16(imm) if reg[a] == reg[b] else npc  # beq
    elif op == 0x1E: npc = npc + s16(imm) if reg[a] != reg[b] else npc  # bne
    elif op == 0x16:                                                    # blt
        sa = reg[a] - (1 << 32) if reg[a] >> 31 else reg[a]
        sb = reg[b] - (1 << 32) if reg[b] >> 31 else reg[b]
        npc = npc + s16(imm) if sa < sb else npc
    elif op == 0x18: reg[b] = int(reg[a] != u32(s16(imm)))              # cmpnei
    elif op == 0x06: npc = npc + s16(imm)                               # br
    else: raise SystemExit(f"emulation: opcode {op:#x} at {pc:#x}")
    reg[0] = 0
    pc = npc
assert status == 0 and files["ee"] == MIRROR and polls == 4, (status, polls)
print(f"OK: {out}: {0x40 + len(text) + 16} bytes, text {len(text)} bytes; emulation wrote the "
      f"8192 mirror bytes, exit 0, {steps} steps, stores only into the stack buffer")
