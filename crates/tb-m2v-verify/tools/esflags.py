#!/usr/bin/env python3
"""Read and rewrite the display flags of an MPEG-2 video elementary stream.

Written from H.262 6.2.3 and 6.2.3.1 only; it shares nothing with the verifier or the
encoder. Three uses:

- `list`: the pictures in display order with top_field_first, repeat_first_field and
  progressive_frame, and whether the output fields alternate (7.12). The gate uses it to check
  the cadence a stream asks a decoder for (tools/gate/film_check.py).
- `telecine`: set the flags of another encoder's stream to a pulldown pattern (what DGPulldown
  does to a 23.976 Hz stream): the coded data stay that encoder's, the flags become a legal
  soft telecine. Gives the verifier repeat_first_field streams it has not seen, including
  progressive sequences shown for two and three frame periods.
- `mutate`: one deliberate violation of a rule of this family, for the verifier's mutation
  cases (independent.py).

Usage: esflags.py list FILE
       esflags.py telecine IN OUT [--pattern 3,2] [--progressive-frame] [--cc]
       esflags.py mutate IN OUT KIND [--at N]
"""
import argparse
import sys

PIC, USER, SEQ, EXT, GOP = 0x00, 0xB2, 0xB3, 0xB5, 0xB8
# bit offsets in the payload of picture_coding_extension() (6.2.3.1), after the start code
TFF, FPFD, RFF, C420, PROG = 24, 25, 30, 31, 32


def start_codes(data):
    i = data.find(b"\x00\x00\x01")
    while i >= 0 and i + 3 < len(data):
        yield i, data[i + 3]
        i = data.find(b"\x00\x00\x01", i + 3)


def get(data, unit, bit, n=1):
    v = 0
    for k in range(bit, bit + n):
        v = (v << 1) | ((data[unit + 4 + k // 8] >> (7 - k % 8)) & 1)
    return v


def put(data, unit, bit, value, n=1):
    for j in range(n):
        k = bit + j
        b = (value >> (n - 1 - j)) & 1
        m = 0x80 >> (k % 8)
        o = unit + 4 + k // 8
        data[o] = (data[o] & ~m) | (m if b else 0)


class Picture:
    def __init__(self, hdr, coded):
        self.hdr = hdr            # offset of the picture start code
        self.coded = coded        # coded index
        self.pce = None           # offset of its picture_coding_extension start code
        self.first_slice = None   # offset of its first slice start code
        self.gop = False          # a GOP header precedes it
        self.type = None
        self.tref = 0

    def flags(self, data):
        return dict(tff=get(data, self.pce, TFF), rff=get(data, self.pce, RFF),
                    prog=get(data, self.pce, PROG), fpfd=get(data, self.pce, FPFD),
                    structure=get(data, self.pce, 22, 2))


def parse(data):
    """(pictures in coded order, progressive_sequence, frame_rate_code, sequence header offsets)."""
    pics, prog_seq, frc, seqs, gop = [], 0, 0, [], False
    for off, code in start_codes(data):
        if code == SEQ:
            frc = data[off + 7] & 15
            seqs.append(off)
        elif code == GOP:
            gop = True
        elif code == EXT:
            ident = data[off + 4] >> 4
            if ident == 1:
                prog_seq = (data[off + 5] >> 3) & 1
            elif ident == 8 and pics and pics[-1].pce is None:
                pics[-1].pce = off
        elif code == PIC:
            p = Picture(off, len(pics))
            p.tref = (data[off + 4] << 2) | (data[off + 5] >> 6)
            p.type = " IPB"[(data[off + 5] >> 3) & 7]
            p.gop, gop = gop, False
            pics.append(p)
        elif 1 <= code <= 0xAF and pics and pics[-1].first_slice is None:
            pics[-1].first_slice = off
    return pics, prog_seq, frc, seqs


def display_order(pics):
    """Frame pictures only: a B-picture is output at once, an anchor when the next one comes."""
    out, held = [], None
    for p in pics:
        if p.type == "B":
            out.append(p)
        else:
            if held is not None:
                out.append(held)
            held = p
    if held is not None:
        out.append(held)
    return out


def units(f, prog_seq):
    """Display duration: fields of an interlaced sequence, frames of a progressive one."""
    if prog_seq:
        return (3 if f["tff"] else 2) if f["rff"] else 1
    return 3 if f["rff"] else 2


def describe(data):
    """Display-order list of (coded index, type, first field 'T'/'B'/'-', units, progressive_frame,
    gop) and the number of parity breaks."""
    pics, prog_seq, frc, _ = parse(data)
    rows, last, breaks = [], None, 0
    for p in display_order(pics):
        f = p.flags(data)
        u = units(f, prog_seq)
        first = "-" if prog_seq else ("T" if f["tff"] else "B")
        if not prog_seq:
            if last is not None and last == first:
                breaks += 1
            # two fields end on the other parity, three on the first again
            last = first if u == 3 else ("B" if first == "T" else "T")
        rows.append((p.coded, p.type, first, u, f["prog"], p.gop))
    return dict(progressive_sequence=prog_seq, frame_rate_code=frc, pictures=rows, parity_breaks=breaks)


def cc_user_data(count, lead):
    """A/53 Part 4 picture user data with cc_data(): `lead` CEA-608 constructs (cc_type list,
    valid null pairs with odd parity), then an end-of-packet DTVCC construct and padding."""
    body = bytearray(b"\x00\x00\x01\xb2GA94\x03")
    body.append(0xC0 | count)
    body.append(0xFF)
    for ty in lead:
        body += bytes([0xFC | ty, 0x80, 0x80])
    for _ in range(count - len(lead)):
        body += bytes([0xFA, 0x00, 0x00])
    body.append(0xFF)
    return bytes(body)


def telecine(data, pattern, progressive_frame, cc):
    """Durations `pattern` (cyclic, in display order) written into the flags; parity kept
    alternating from the first picture's top_field_first."""
    data = bytearray(data)
    pics, prog_seq, _, _ = parse(data)
    order = display_order(pics)
    top = bool(order[0].flags(data)["tff"]) if order else True
    inserts, shown = [], 0
    for k, p in enumerate(order):
        u = pattern[k % len(pattern)]
        if prog_seq:
            put(data, p.pce, RFF, int(u > 1))
            put(data, p.pce, TFF, int(u == 3))
            # one CEA-608 field per frame period of 1/59.94 s, the two fields in turn
            lead = [(shown + n) % 2 for n in range(u)]
            shown += u
        else:
            put(data, p.pce, RFF, int(u == 3))
            put(data, p.pce, TFF, int(top))
            if u == 3 or progressive_frame:
                put(data, p.pce, PROG, 1)
                put(data, p.pce, C420, 1)
            # CTA-708-E Annex C: cc_type 01 with a top field, 00 with a bottom field
            lead = [int(top) ^ (n % 2) for n in range(u)]
            if u == 3:
                top = not top
        if cc:
            per_unit = 10
            inserts.append((p.first_slice, cc_user_data(per_unit * u, lead)))
    for off, blob in sorted(inserts, reverse=True):
        data[off:off] = blob
    return bytes(data)


MUTATIONS = {
    # name: what is done to the picture at display index --at
    "rff-clear": "repeat_first_field 1 -> 0 on a three-field picture: the next picture starts on the wrong parity",
    "tff-flip": "top_field_first inverted on one picture",
    "prog-clear": "progressive_frame (and chroma_420_type) 1 -> 0 on a picture with repeat_first_field 1",
    "rate-24": "frame_rate_code 1 (23.976 Hz) in every sequence header of a stream with repeat_first_field",
    "tref-skip": "temporal_reference + 1 on one picture",
    "all-two": "every picture two fields, top field first: the coded vbv_delay values no longer fit the removal times",
    "tff-no-rff": "progressive sequence: top_field_first 1 on a picture with repeat_first_field 0",
    "cc-10": "cc_count 10 written into a three-field picture's cc_data() (20 constructs dropped): neither its own\n"
             "duration nor the picture before it gives that count",
}


def mutate(data, kind, at):
    data = bytearray(data)
    pics, prog_seq, _, seqs = parse(data)
    order = display_order(pics)

    def pick(want):
        hits = [p for p in order if want(p.flags(data))]
        if not hits:
            raise SystemExit(f"esflags: no picture for mutation {kind}")
        return hits[at % len(hits)]

    if kind == "rff-clear":
        put(data, pick(lambda f: f["rff"]).pce, RFF, 0)
    elif kind == "tff-flip":
        p = order[at % len(order)]
        put(data, p.pce, TFF, 1 - get(data, p.pce, TFF))
    elif kind == "prog-clear":
        p = pick(lambda f: f["rff"])
        put(data, p.pce, PROG, 0)
        put(data, p.pce, C420, 0)
    elif kind == "rate-24":
        for s in seqs:
            data[s + 7] = (data[s + 7] & 0xF0) | 1
    elif kind == "tref-skip":
        p = order[at % len(order)]
        t = ((data[p.hdr + 4] << 2) | (data[p.hdr + 5] >> 6)) + 1
        data[p.hdr + 4] = (t >> 2) & 0xFF
        data[p.hdr + 5] = (data[p.hdr + 5] & 0x3F) | ((t & 3) << 6)
    elif kind == "all-two":
        for p in order:
            put(data, p.pce, RFF, 0)
            put(data, p.pce, TFF, 1)
    elif kind == "tff-no-rff":
        put(data, pick(lambda f: not f["rff"]).pce, TFF, 1)
    elif kind == "cc-10":
        p = pick(lambda f: f["rff"])
        end = p.first_slice
        u = data.rfind(b"\x00\x00\x01\xb2GA94\x03", p.pce, end)
        if u < 0:
            raise SystemExit("esflags: no caption user data in that picture")
        count = data[u + 9] & 0x1F
        data[u + 9] = (data[u + 9] & 0xE0) | 10
        # drop the last constructs so that the length agrees with the new cc_count
        cut = 3 * (count - 10)
        del data[end - 1 - cut:end - 1]
    else:
        raise SystemExit(f"esflags: unknown mutation {kind}")
    return bytes(data)


def main():
    ap = argparse.ArgumentParser()
    sub = ap.add_subparsers(dest="cmd", required=True)
    ls = sub.add_parser("list")
    ls.add_argument("file")
    te = sub.add_parser("telecine")
    te.add_argument("src")
    te.add_argument("dst")
    te.add_argument("--pattern", default="3,2")
    te.add_argument("--progressive-frame", action="store_true",
                    help="interlaced sequence: progressive_frame 1 on two-field pictures too")
    te.add_argument("--cc", action="store_true", help="insert caption user data of the right cc_count")
    mu = sub.add_parser("mutate")
    mu.add_argument("src")
    mu.add_argument("dst")
    mu.add_argument("kind", choices=sorted(MUTATIONS))
    mu.add_argument("--at", type=int, default=3)
    a = ap.parse_args()
    if a.cmd == "list":
        d = describe(open(a.file, "rb").read())
        print(" ".join(f"{t}{c}:{first}{u}{'p' if prog else 'i'}{'G' if gop else ''}"
                       for c, t, first, u, prog, gop in d["pictures"]))
        print(f"progressive_sequence {d['progressive_sequence']} frame_rate_code {d['frame_rate_code']} "
              f"pictures {len(d['pictures'])} parity breaks {d['parity_breaks']}")
        return 0
    data = open(a.src, "rb").read()
    if a.cmd == "telecine":
        out = telecine(data, [int(x) for x in a.pattern.split(",")], a.progressive_frame, a.cc)
    else:
        out = mutate(data, a.kind, a.at)
    open(a.dst, "wb").write(out)
    return 0


if __name__ == "__main__":
    sys.exit(main())
