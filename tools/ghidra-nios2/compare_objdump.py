#!/usr/bin/env python3
"""Compare a DumpNios2Listing.java dump against `nios2-elf-objdump -d -z` output.

Run with `python3 -I` (inputs are derived from untrusted firmware):

    nios2-elf-objdump -d -z prog.elf > prog.objdump
    python3 -I compare_objdump.py prog.objdump listing.txt [--show N]

Both sides are normalized to (mnemonic, [operand values]) where registers are
canonical names and numbers are integers, so hex-vs-decimal and register alias
differences (r30 = ba = sstatus) are ignored. Anything else counts as a mismatch.
"""
import re
import sys

REG_ALIAS = {
    "r0": "zero", "r1": "at", "r24": "et", "r25": "bt", "r26": "gp", "r27": "sp",
    "r28": "fp", "r29": "ea", "r30": "ba", "sstatus": "ba", "r31": "ra",
}
NUM = re.compile(r"^-?(0x[0-9a-fA-F]+|\d+)$")


def norm_operand(tok):
    tok = tok.strip()
    # objdump prints branch targets as "0x10000018 <sym>" sometimes
    tok = re.sub(r"\s*<[^>]*>$", "", tok)
    if NUM.match(tok):
        return int(tok, 0) & 0xFFFFFFFF
    return REG_ALIAS.get(tok, tok)


def split_ops(s):
    # "r2,-32748(gp)" -> ["r2", "-32748", "gp"]
    s = s.replace("(", ",").replace(")", "")
    return [norm_operand(t) for t in s.split(",") if t.strip()]


def load_objdump(path):
    out = {}
    pat = re.compile(r"^\s*([0-9a-f]+):\s+([0-9a-f]{8})\s+(\S+)(?:\s+(.*))?$")
    with open(path, encoding="utf-8", errors="replace") as f:
        for line in f:
            m = pat.match(line.rstrip("\n"))
            if not m:
                continue
            addr = int(m.group(1), 16)
            mnem = m.group(3)
            ops = m.group(4) or ""
            if mnem.startswith("0x"):  # undefined word
                out[addr] = ("<undef>", [])
            else:
                out[addr] = (mnem, split_ops(ops))
    return out


def load_ghidra(path):
    out = {}
    src = {}
    with open(path, encoding="utf-8", errors="replace") as f:
        for line in f:
            parts = line.rstrip("\n").split("\t")
            if len(parts) < 3:
                continue
            addr = int(parts[0], 16)
            src[addr] = parts[1]
            text = parts[2].strip()
            if not text:
                out[addr] = ("<undef>", [])
                continue
            mnem, _, ops = text.partition(" ")
            out[addr] = (mnem, split_ops(ops))
    return out, src


def main():
    show = 20
    args = [a for a in sys.argv[1:]]
    if "--show" in args:
        i = args.index("--show")
        show = int(args[i + 1])
        del args[i:i + 2]
    od = load_objdump(args[0])
    gh, src = load_ghidra(args[1])
    common = sorted(set(od) & set(gh))
    only_od = sorted(set(od) - set(gh))
    only_gh = sorted(set(gh) - set(od))
    mn_mis, op_mis = [], []
    for a in common:
        if od[a][0] != gh[a][0]:
            mn_mis.append(a)
        elif od[a][1] != gh[a][1]:
            op_mis.append(a)
    in_listing = sum(1 for a in common if src.get(a) == "A")
    print(f"objdump words: {len(od)}  ghidra words: {len(gh)}  compared: {len(common)}")
    print(f"  ghidra listing (auto-analysis) instructions among them: {in_listing}")
    print(f"  only in objdump: {len(only_od)}  only in ghidra: {len(only_gh)}")
    print(f"  mnemonic mismatches: {len(mn_mis)}")
    print(f"  operand mismatches:  {len(op_mis)}")
    for a in (mn_mis + op_mis)[:show]:
        print(f"    {a:08x}  objdump={od[a]}  ghidra={gh[a]}")
    return 1 if (mn_mis or op_mis) else 0


if __name__ == "__main__":
    sys.exit(main())
