#!/usr/bin/env python3
"""Generate src/tables_gen.rs from the text extraction of ITU-T H.262 (02/2000).

Usage: python3 -I tools/gen_tables.py PATH/ITU-T_H.262-200002-S.txt > src/tables_gen.rs

The tables are transcribed from the standard's text only (Annex B, Figures 7-2 and 7-3,
clause 6.3.11, Table 7-6). Nothing is taken from any encoder. The script checks every
variable length code table for the properties the standard implies: the number of codes,
prefix-freeness, and (where the table is meant to be complete) the Kraft sum.
"""
import re
import sys
from fractions import Fraction

CODE = r"((?:[01]{1,4} )*[01]{1,4})"


def load(path):
    with open(path, encoding="utf-8", errors="replace") as f:
        return f.read().replace("–", "-").replace("−", "-")


def section(text, start_pat, end_pat):
    """The text from the first match of start_pat to the next match of end_pat."""
    m = re.search(start_pat, text)
    if not m:
        sys.exit(f"gen_tables: start not found: {start_pat}")
    e = re.search(end_pat, text[m.end():])
    if not e:
        sys.exit(f"gen_tables: end not found: {end_pat}")
    return text[m.end(): m.end() + e.start()]


def bits(code):
    return code.replace(" ", "")


def check_prefix_free(name, codes):
    cs = sorted(codes)
    for a, b in zip(cs, cs[1:]):
        if b.startswith(a):
            sys.exit(f"gen_tables: {name}: {a} is a prefix of {b}")


def kraft(codes):
    return sum(Fraction(1, 2 ** len(c)) for c in codes)


def two_column(body, value_pat):
    """Tables printed as two (code, value) columns side by side."""
    out = []
    for line in body.splitlines():
        for m in re.finditer(CODE + r"\s{2,}" + value_pat + r"(?=\s|$)", line):
            out.append((bits(m.group(1)), m.group(2)))
    return out


def table_b1(text):
    body = section(text, r"Table B\.1 - Variable length codes for macroblock_address_increment", r"NOTE")
    rows = two_column(body, r"(\d+|macroblock_escape)")
    inc = [(c, int(v)) for c, v in rows if v != "macroblock_escape"]
    esc = [c for c, v in rows if v == "macroblock_escape"]
    assert len(inc) == 33 and sorted(v for _, v in inc) == list(range(1, 34)), inc
    assert esc == ["00000001000"], esc
    check_prefix_free("B.1", [c for c, _ in inc] + esc)
    return inc, esc[0]


def mb_type(text, n, end):
    body = section(text, rf"Table B\.{n} - Variable length codes for macroblock_type in", end)
    rows = []
    for line in body.splitlines():
        m = re.match(r"\s*" + CODE + r"\s+([01])\s+([01])\s+([01])\s+([01])\s+([01])\s+([01])\s+\S", line)
        if m:
            rows.append((bits(m.group(1)),) + tuple(int(m.group(i)) for i in range(2, 8)))
    check_prefix_free(f"B.{n}", [r[0] for r in rows])
    return rows


def table_b9(text):
    body = section(text, r"Table B\.9 - Variable length codes for coded_block_pattern", r"NOTE")
    rows = [(c, int(v)) for c, v in two_column(body, r"(\d+)(?: \(Note\))?")]
    assert sorted(v for _, v in rows) == list(range(64)), sorted(v for _, v in rows)
    check_prefix_free("B.9", [c for c, _ in rows])
    return rows


def table_b10(text):
    body = section(text, r"Table B\.10 - Variable length codes for motion_code", r"Table B\.11")
    rows = []
    for line in body.splitlines():
        m = re.match(r"\s*" + CODE + r"\s{2,}(-?\d+)\s*$", line)
        if m:
            rows.append((bits(m.group(1)), int(m.group(2))))
    assert sorted(v for _, v in rows) == list(range(-16, 17)), rows
    check_prefix_free("B.10", [c for c, _ in rows])
    return rows


def table_b11(text):
    body = section(text, r"Table B\.11 - Variable length codes for dmvector", r"B\.5\s+DCT coefficients")
    rows = []
    for line in body.splitlines():
        m = re.match(r"\s*([01]{1,2})\s{2,}(-?\d)\s*$", line)
        if m:
            rows.append((m.group(1), int(m.group(2))))
    assert sorted(v for _, v in rows) == [-1, 0, 1], rows
    check_prefix_free("B.11", [c for c, _ in rows])
    assert kraft([c for c, _ in rows]) == 1
    return rows


def dc_size(text, n, end):
    body = section(text, rf"Table B\.{n} - Variable length codes for dct_dc_size", end)
    rows = []
    for line in body.splitlines():
        m = re.match(r"\s*" + CODE + r"\s{2,}(\d+)\s*$", line)
        if m:
            rows.append((bits(m.group(1)), int(m.group(2))))
    assert sorted(v for _, v in rows) == list(range(12)), rows
    check_prefix_free(f"B.{n}", [c for c, _ in rows])
    assert kraft([c for c, _ in rows]) == 1, f"B.{n} is not complete"
    return rows


def dct_table(text, n, end):
    """Returns (rows, eob, escape, first): rows are (code-without-sign, run, level)."""
    body = section(text, rf"Table B\.{n} - DCT coefficients Table (?:zero|one)\n", end)
    rows, eob, esc, first = [], None, None, None
    for line in body.splitlines():
        m = re.match(r"\s*" + CODE + r"(?: ?s)?(?: \(Note (\d)\))?\s{2,}(End of Block|Escape|\d+)(?:\s+(\d+))?\s*$", line)
        if not m:
            continue
        code, note, a, b = bits(m.group(1)), m.group(2), m.group(3), m.group(4)
        if a == "End of Block":
            eob = code
        elif a == "Escape":
            esc = code
        elif n == 14 and note == "3":
            assert (int(a), int(b)) == (0, 1)
            first = code
        else:
            rows.append((code, int(a), int(b)))
    assert eob and esc, (eob, esc)
    # 111 (run, level) codes besides End of Block and Escape, the first-coefficient
    # code of Table B.14 not counted.
    assert len(rows) == 111, (n, len(rows))
    assert len(set((r, l) for _, r, l in rows)) == 111
    full = [c + "s" for c, _, _ in rows] + [eob, esc]
    check_prefix_free(f"B.{n}", [c.replace("s", "0") for c in full] + [c.replace("s", "1") for c in full if "s" in c])
    return rows, eob, esc, first


def matrix(text, start, end):
    body = section(text, start, end)
    rows = []
    for line in body.splitlines():
        m = re.match(r"\s*(?:v\s+)?([0-7])((?:\s+\d+){8})\s*$", line)
        if m:
            rows.append([int(x) for x in m.group(2).split()])
    assert len(rows) == 8, (start, rows)
    return rows


def qscale(text):
    body = section(text, r"Table 7-6 - Relation between quantiser_scale and quantiser_scale_code", r"\f")
    lin, non = [0], [0]
    for line in body.splitlines():
        m = re.match(r"\s*(\d+)\s+(\d+)\s+(\d+)\s*$", line)
        if m:
            assert int(m.group(1)) == len(lin)
            lin.append(int(m.group(2)))
            non.append(int(m.group(3)))
    assert len(lin) == 32 and lin[31] == 62 and non[31] == 112
    return lin, non


def emit_codes(name, doc, rows, fmt):
    out = [f"/// {doc}", f"pub const {name}: &[{fmt[0]}] = &["]
    for r in rows:
        out.append("    " + fmt[1](r) + ",")
    out.append("];")
    return "\n".join(out)


def code_lit(c):
    return f"Code {{ bits: 0b{c}, len: {len(c)} }}"


def main():
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    text = load(sys.argv[1])
    b1, esc = table_b1(text)
    b2 = mb_type(text, 2, r"Table B\.3")
    b3 = mb_type(text, 3, r"Table B\.4")
    b4 = mb_type(text, 4, r"Table B\.5")
    assert (len(b2), len(b3), len(b4)) == (2, 7, 11), (len(b2), len(b3), len(b4))
    b9 = table_b9(text)
    b10 = table_b10(text)
    b11 = table_b11(text)
    b12 = dc_size(text, 12, r"Table B\.13")
    b13 = dc_size(text, 13, r"Table B\.14")
    b14, eob14, esc14, first14 = dct_table(text, 14, r"NOTE 1")
    b15, eob15, esc15, _ = dct_table(text, 15, r"NOTE 1")
    assert first14 == "1" and eob14 == "10" and eob15 == "0110" and esc14 == esc15 == "000001"
    assert sorted((r, l) for _, r, l in b14) == sorted((r, l) for _, r, l in b15)
    scan0 = matrix(text, r"QF\[v\]\[u\] = QFS\[scan\[alternate_scan\]\[v\]\[u\]\]", r"Figure 7-2")
    scan1 = matrix(text, r"Figure 7-2 - Definition of scan\[0\]\[v\]\[u\]", r"Figure 7-3")
    for s in (scan0, scan1):
        assert sorted(x for row in s for x in row) == list(range(64))
    intra = matrix(text, r"The default matrix for intra blocks \(both luminance and chrominance\) is:", r"\f")
    nonintra = matrix(text, r"The default matrix for non-intra blocks \(both luminance and chrominance\) is:", r"load_intra_quantiser_matrix")
    assert intra[0][0] == 8 and all(x == 16 for row in nonintra for x in row)
    lin, non = qscale(text)

    p = print
    p("// DO NOT EDIT. Generated by tools/gen_tables.py from the text of ITU-T H.262 (02/2000):")
    p("// Annex B (Tables B.1 to B.4 and B.9 to B.15), Figures 7-2 and 7-3, clause 6.3.11 and")
    p("// Table 7-6. Regenerate with:")
    p("//   python3 -I tools/gen_tables.py <ITU-T_H.262-200002-S.txt> > src/tables_gen.rs")
    p("")
    p("use super::{Code, DctCode, MbTypeCode};")
    p("")
    p(emit_codes("MB_ADDR_INC", "Table B.1 (p.121): macroblock_address_increment, as (code, increment).", b1,
                 ("(Code, u8)", lambda r: f"({code_lit(r[0])}, {r[1]})")))
    p("")
    p("/// Table B.1 (p.121): macroblock_escape.")
    p(f"pub const MB_ESCAPE: Code = {code_lit(esc)};")
    for name, n, page, rows in (("MB_TYPE_I", 2, 122, b2), ("MB_TYPE_P", 3, 122, b3), ("MB_TYPE_B", 4, 123, b4)):
        p("")
        p(emit_codes(name, f"Table B.{n} (p.{page}): macroblock_type.", rows,
                     ("MbTypeCode", lambda r: "MbTypeCode { code: %s, quant: %s, motion_forward: %s, motion_backward: %s, pattern: %s, intra: %s }" % (
                         code_lit(r[0]), *("true" if x else "false" for x in r[1:6])))))
    p("")
    p(emit_codes("CBP", "Table B.9 (p.127): coded_block_pattern_420, as (code, cbp).", b9,
                 ("(Code, u8)", lambda r: f"({code_lit(r[0])}, {r[1]})")))
    p("")
    p(emit_codes("MOTION_CODE", "Table B.10 (p.128): motion_code, as (code, value).", b10,
                 ("(Code, i8)", lambda r: f"({code_lit(r[0])}, {r[1]})")))
    p("")
    p(emit_codes("DMVECTOR", "Table B.11 (p.129): dmvector, as (code, value).", b11,
                 ("(Code, i8)", lambda r: f"({code_lit(r[0])}, {r[1]})")))
    p("")
    p(emit_codes("DC_SIZE_LUMA", "Table B.12 (p.129): dct_dc_size_luminance, as (code, size).", b12,
                 ("(Code, u8)", lambda r: f"({code_lit(r[0])}, {r[1]})")))
    p("")
    p(emit_codes("DC_SIZE_CHROMA", "Table B.13 (p.129): dct_dc_size_chrominance, as (code, size).", b13,
                 ("(Code, u8)", lambda r: f"({code_lit(r[0])}, {r[1]})")))
    for name, n, pages, rows, eob in (("DCT_ZERO", 14, "130-133", b14, eob14), ("DCT_ONE", 15, "134-137", b15, eob15)):
        p("")
        p(emit_codes(name, f"Table B.{n} (p.{pages}): (run, level) codes without the sign bit.", rows,
                     ("DctCode", lambda r: f"DctCode {{ code: {code_lit(r[0])}, run: {r[1]}, level: {r[2]} }}")))
        p("")
        p(f"/// Table B.{n}: End of Block.")
        p(f"pub const {name}_EOB: Code = {code_lit(eob)};")
    p("")
    p("/// Tables B.14 and B.15: Escape.")
    p(f"pub const DCT_ESCAPE: Code = {code_lit(esc14)};")
    p("")
    p("/// Table B.14, Note 3: the code of run 0, level 1 for the first coefficient of a non-intra")
    p("/// block, without the sign bit.")
    p(f"pub const DCT_ZERO_FIRST: Code = {code_lit(first14)};")
    for name, doc, m in (("SCAN_ZIGZAG", "Figure 7-2 (p.65): `scan[0][v][u]`.", scan0), ("SCAN_ALTERNATE", "Figure 7-3 (p.65): `scan[1][v][u]`.", scan1),
                         ("DEFAULT_INTRA_MATRIX", "Clause 6.3.11 (p.51): the default intra matrix, by v then u.", intra),
                         ("DEFAULT_NON_INTRA_MATRIX", "Clause 6.3.11 (p.52): the default non-intra matrix, by v then u.", nonintra)):
        p("")
        p(f"/// {doc}")
        p(f"pub const {name}: [[u8; 8]; 8] = [")
        for row in m:
            p("    [" + ", ".join(str(x) for x in row) + "],")
        p("];")
    p("")
    p("/// Table 7-6 (p.68): quantiser_scale for q_scale_type 0, by quantiser_scale_code.")
    p("pub const QUANTIZER_SCALE_LINEAR: [u8; 32] = [" + ", ".join(map(str, lin)) + "];")
    p("")
    p("/// Table 7-6 (p.68): quantiser_scale for q_scale_type 1, by quantiser_scale_code.")
    p("pub const QUANTIZER_SCALE_NON_LINEAR: [u8; 32] = [" + ", ".join(map(str, non)) + "];")
    # Properties, printed to stderr for the log.
    for name, codes in (("B.1", [c for c, _ in b1] + [esc]), ("B.9", [c for c, _ in b9]), ("B.10", [c for c, _ in b10]),
                        ("B.14", [c + "0" for c, _, _ in b14] + [c + "1" for c, _, _ in b14] + [eob14, esc14]),
                        ("B.15", [c + "0" for c, _, _ in b15] + [c + "1" for c, _, _ in b15] + [eob15, esc15])):
        print(f"gen_tables: {name}: {len(codes)} codes, Kraft sum {kraft(codes)}", file=sys.stderr)


if __name__ == "__main__":
    main()
