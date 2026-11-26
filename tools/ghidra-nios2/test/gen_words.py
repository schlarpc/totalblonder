# Generate a raw blob of Nios II R1 instruction words covering every OP/OPX with
# random and edge-case fields (incl. undefined encodings) for objdump-vs-Ghidra diffing.
# Usage: python3 -I gen_words.py > words.bin
import random, struct, sys
random.seed(1234)
SPECIAL = [0, 0, 0x1d, 0x1e, 0x1f, 27, 26]
def fld():
    return random.choice(SPECIAL) if random.random() < 0.5 else random.randrange(32)
words = []
for op in range(64):
    for _ in range(600 if op != 0x3a else 0):
        w = op | random.getrandbits(26) << 6
        if random.random() < 0.5:
            a, b = fld(), fld()
            w = (w & ~(0x3ff << 22)) | a << 27 | b << 22
        words.append(w)
for opx in range(64):
    for _ in range(600):
        a, b, c = fld(), fld(), fld()
        imm5 = 0 if random.random() < 0.6 else random.randrange(32)
        w = 0x3a | imm5 << 6 | opx << 11 | c << 17 | b << 22 | a << 27
        words.append(w)
for opx in range(64):
    for a in (0, 0x1d, 0x1e, 0x1f, 5):
        for b in (0, 0x1d, 0x1e, 0x1f, 6):
            for c in (0, 0x1d, 0x1e, 0x1f, 7):
                for imm5 in (0, random.randrange(1, 32)):
                    words.append(0x3a | imm5 << 6 | opx << 11 | c << 17 | b << 22 | a << 27)
for op in range(64):
    for a in (0, 5, 27):
        for b in (0, 6, 31):
            for imm in (0, 0x7fff, 0x8000, 0xffff, random.getrandbits(16)):
                words.append(op | imm << 6 | b << 22 | a << 27)
for _ in range(20000):
    words.append(random.getrandbits(32))
sys.stdout.buffer.write(b"".join(struct.pack("<I", w) for w in words))
