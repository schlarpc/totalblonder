#!/usr/bin/env python3
"""Independent buffer checks of an MPEG-2 video elementary stream (ITU-T H.262 Annex C).

Usage: vbv_check.py stream.m2v [stats.jsonl]

Shares no code with the encoder. Reads bit_rate (Rmax), vbv_buffer_size (B) and the frame rate
from the first sequence header and extension, checks that every repeated sequence header and
extension equals the first (6.1.1.6), splits the stream into coded pictures (C.5: a picture's
data includes the headers before it and any stuffing after it), and simulates:

 1. The normative model the stream signals.
    vbv_delay = 0xFFFF in every picture (C.3.2): data enters at Rmax whenever the buffer is not
    full; decoding starts when it is full; a picture is removed every frame period. The only
    possible violation is an underflow.
    Otherwise (C.3.1, constant rate assumed): fill at Rmax, first removal after vbv_delay;
    underflow and overflow are both checked, and each coded vbv_delay against the simulation.
    The time between two removals is that of C.9 (progressive sequence) and C.11 (interlaced
    sequence), read from top_field_first and repeat_first_field of each picture: after a
    B-picture its own display time (one, two or three frame periods; two or three field
    periods), after an I- or P-picture the display time of the I- or P-picture before it.
    Without repeat_first_field that is one frame period every time.

 2. With stats.jsonl (one JSON object per coded picture, written by `tb-m2venc --stats`, giving
    the delivery rate `rate` the encoder assumed for that picture): the operating model.
      - coded picture n enters the mux queue at e_n, its removal time of the normative model
        counted from the first picture's (n * T without repeat_first_field), and is removed
        from the decoder buffer at e_n + D, with D = B / Rmax;
      - rho(t) = rate of the picture coded last before t; the mux sends queued data at exactly
        rhobar(t) = max{rho(s) : t - D <= s <= t}, the slowest delivery it is allowed
        (it must keep the old rate for D after a decrease, and raise before the encoder does);
      - no underflow: all of picture n has been delivered at e_n + D;
      - no overflow: the bits delivered and not yet removed never exceed B.
    A mux that delivers faster than rhobar (up to Rmax) only helps the underflow condition, and
    cannot overflow a buffer of B = Rmax * D.
"""
import json
import re
import sys

RATES = {1: 24000 / 1001, 2: 24, 3: 25, 4: 30000 / 1001, 5: 30, 6: 50, 7: 60000 / 1001, 8: 60}


def parse(data):
    seq = data.find(b"\x00\x00\x01\xb3")
    assert seq == 0, "stream must start with a sequence header"
    bits = int.from_bytes(data[seq + 4: seq + 12], "big")
    frame_rate = RATES[(bits >> 32) & 0xF]
    bit_rate_value = (bits >> 14) & 0x3FFFF
    vbv_value = (bits >> 3) & 0x3FF
    ext = data.find(b"\x00\x00\x01\xb5", seq)
    e = int.from_bytes(data[ext + 4: ext + 10], "big")  # 48 bits
    assert (e >> 44) == 1, "sequence extension expected"
    R = ((((e >> 17) & 0xFFF) << 18) | bit_rate_value) * 400
    B = ((((e >> 8) & 0xFF) << 10) | vbv_value) * 16384
    # every sequence header (first 8 bytes after the start code: sizes, rate, vbv; no matrices are
    # loaded) and sequence extension (6 bytes) must repeat the first one
    seqs = [m.start() for m in re.finditer(b"\x00\x00\x01\xb3", data)]
    first_h, first_e = data[seq + 4: seq + 12], data[ext + 4: ext + 10]
    seq_mismatch = 0
    for s in seqs[1:]:
        x = data.find(b"\x00\x00\x01\xb5", s)
        if data[s + 4: s + 12] != first_h or data[x + 4: x + 10] != first_e:
            seq_mismatch += 1
    # Access units: from a picture's first header (sequence header if present, else GOP header,
    # else picture header) up to the next one's.
    starts = [m.start() for m in re.finditer(b"\x00\x00\x01[\x00\xb3\xb8]", data)]
    aus, pics, pending = [], [], None
    for s in starts:
        if pending is None:
            pending = s
        if data[s + 3] == 0x00:
            aus.append(pending)
            pics.append(s)
            pending = None
    aus.append(len(data))
    sizes = [8 * (aus[i + 1] - aus[i]) for i in range(len(pics))]
    vbv_delays = [((int.from_bytes(data[p + 4: p + 8], "big") >> 3) & 0xFFFF) for p in pics]
    types = "".join(" IPB"[(int.from_bytes(data[p + 4: p + 8], "big") >> 19) & 7] for p in pics)
    # Removal times (C.9, C.11): display units per picture from its picture coding extension
    # (6.3.10), units being field periods of an interlaced sequence and frame periods of a
    # progressive one.
    prog_seq = (e >> 35) & 1
    upf = 1 if prog_seq else 2
    shown = []
    for k, pic in enumerate(pics):
        x = data.find(b"\x00\x00\x01\xb5", pic, aus[k + 1])
        assert x >= 0 and data[x + 4] >> 4 == 8, "picture coding extension expected"
        v = int.from_bytes(data[x + 4: x + 9], "big")
        assert (v >> 16) & 3 == 3, "frame pictures only"
        tff, rff = (v >> 15) & 1, (v >> 9) & 1
        shown.append(((3 if tff else 2) if rff else 1) if prog_seq else (3 if rff else 2))
    units, at, prev_anchor = [], 0, upf
    for t, u in zip(types, shown):
        units.append(at)
        if t == "B":
            at += u
        else:
            at += prev_anchor
            prev_anchor = u
    T = 1.0 / frame_rate
    # removal time of each picture from the first one's, and the time of the removal after the last
    times = [u * (T / upf) for u in units + [at]]
    return dict(R=R, B=B, T=T, sizes=sizes, vbv_delays=vbv_delays, types=types, sequence_headers=len(seqs),
                seq_mismatch=seq_mismatch, times=times, shown=shown, upf=upf)


def normative(p):
    R, B, sizes, d, times = p["R"], p["B"], p["sizes"], p["vbv_delays"], p["times"]
    vbr = d[0] == 0xFFFF
    mixed = any((x == 0xFFFF) != vbr for x in d)     # 6.3.9: all or none
    full = B if vbr else min(B, R * d[0] / 90000.0)
    under = over = 0
    lo, hi, worst = float("inf"), 0.0, 0.0
    for i, sz in enumerate(sizes):
        if not vbr:
            worst = max(worst, abs(R * d[i] / 90000.0 - full))
            if full > B + 1:
                over += 1
        hi = max(hi, full)
        if sz > full:
            under += 1
        full -= sz
        lo = min(lo, full)
        full += R * (times[i + 1] - times[i])
        if vbr:
            full = min(full, B)
    return dict(model="C.3.2 (vbv_delay 0xFFFF)" if vbr else "C.3.1 constant rate", underflows=under, overflows=over, min_occ=lo / B,
                max_occ=hi / B, delay_err_bits=worst, mixed_vbv_delay=mixed)


def operating(p, rates):
    """Fluid simulation of the mux queue and the decoder buffer under the slowest allowed delivery."""
    R, B, sizes, e = p["R"], p["B"], p["sizes"], p["times"]
    D = B / R
    n = len(sizes)
    assert len(rates) == n, f"schedule has {len(rates)} pictures, stream has {n}"
    chg = [(0.0, rates[0])] + [(e[i], rates[i]) for i in range(1, n) if rates[i] != rates[i - 1]]

    def rbar(t):
        m = 0.0
        for i, (c, r) in enumerate(chg):
            end = chg[i + 1][0] if i + 1 < len(chg) else float("inf")
            if c <= t and end + D > t:       # rate r was in force at some time in [t - D, t]
                m = max(m, r)
        return m

    ev = set()
    for i in range(n):
        ev.add(e[i])
        ev.add(e[i] + D)
    for c, _ in chg:
        ev.add(c)
        ev.add(c + D)
    ev = sorted(ev)
    arrive = {e[i]: i for i in range(n)}
    queue = delivered = arrived = removed = 0.0
    under = over = 0
    worst_under = float("inf")     # smallest (delivered - needed) at a removal, bits
    max_dec = 0.0
    removals = sorted((e[i] + D, i) for i in range(n))
    ri = 0
    fin = 0                        # next picture whose last bit has not left the queue yet
    min_margin = float("inf")      # smallest (removal time - time the last bit was delivered), seconds
    need = 0.0
    cum = []
    acc = 0.0
    for sz in sizes:
        acc += sz
        cum.append(acc)
    prev = ev[0]
    for t in ev:
        dt = t - prev
        if dt > 0:
            rate = rbar(0.5 * (prev + t))
            send = min(queue, rate * dt)
            while fin < n and cum[fin] <= delivered + send + 1e-6 and e[fin] <= prev + 1e-12:
                tf = prev + max(0.0, cum[fin] - delivered) / rate if rate > 0 else prev
                min_margin = min(min_margin, e[fin] + D - tf)
                fin += 1
            queue -= send
            delivered += send
        prev = t
        # removals due at t (before any arrival at the same instant)
        while ri < n and removals[ri][0] <= t + 1e-12:
            i = removals[ri][1]
            dec = delivered - removed
            max_dec = max(max_dec, dec)
            if dec > B + 1:
                over += 1
            slack = delivered - cum[i]
            worst_under = min(worst_under, slack)
            if slack < -1:
                under += 1
            removed += sizes[i]
            ri += 1
        if t in arrive:
            queue += sizes[arrive[t]]
    return dict(model="operating (queue drained at the largest rate of the last D)", D=D, rate_changes=len(chg) - 1,
                rate_min=min(rates), rate_max=max(rates), underflows=under, overflows=over,
                min_slack_bits=worst_under, min_margin_s=min_margin, max_decoder_bits=max_dec, max_dec_frac=max_dec / B,
                rate_above_max=sum(1 for r in rates if r > R))


def check(path, schedule=None, quiet=False):
    """Returns a dict; ok is True only if every model simulated holds."""
    say = (lambda *a: None) if quiet else print
    p = parse(open(path, "rb").read())
    nm = normative(p)
    dur = p["times"][-1]
    res = dict(pictures=len(p["sizes"]), bit_rate=p["R"], vbv_bits=p["B"], mean_rate=sum(p["sizes"]) / dur,
               largest_picture=max(p["sizes"]), types=p["types"], model=nm["model"], underflows=nm["underflows"],
               overflows=nm["overflows"], min_occ=nm["min_occ"], max_occ=nm["max_occ"], seq_mismatch=p["seq_mismatch"],
               sequence_headers=p["sequence_headers"])
    ok = nm["underflows"] == 0 and nm["overflows"] == 0 and not nm["mixed_vbv_delay"] and p["seq_mismatch"] == 0
    say(f"pictures {res['pictures']} ({p['types'][:15]}...), {1 / p['T']:.3f} frames/s, bit_rate {p['R']} bit/s, vbv_buffer_size {p['B']} bits, "
        f"{p['sequence_headers']} sequence headers ({p['seq_mismatch']} differ from the first)")
    say(f"mean rate {res['mean_rate'] / 1e6:.3f} Mbit/s; largest picture {max(p['sizes'])} bits, smallest {min(p['sizes'])} bits")
    say(f"normative model {nm['model']}: occupancy before removal max {100 * nm['max_occ']:.1f}%, after removal min {100 * nm['min_occ']:.1f}%; "
        f"underflows {nm['underflows']}, overflows {nm['overflows']}")
    if schedule:
        rows = [json.loads(l) for l in open(schedule) if l.strip()]
        rows.sort(key=lambda r: r["n"])
        op = operating(p, [float(r["rate"]) for r in rows])
        res["op"] = op
        ok = ok and op["underflows"] == 0 and op["overflows"] == 0 and op["rate_above_max"] == 0
        say(f"operating model: D {op['D']:.4f} s, rate {op['rate_min'] / 1e6:.3f}..{op['rate_max'] / 1e6:.3f} Mbit/s, {op['rate_changes']} changes; "
            f"underflows {op['underflows']}, overflows {op['overflows']}; a picture's last bit arrives at least {1e3 * op['min_margin_s']:.2f} ms before its removal, "
            f"largest decoder occupancy {100 * op['max_dec_frac']:.1f}% of vbv_buffer_size")
    res["ok"] = ok
    say("VBV RESULT:", "PASS" if ok else "FAIL")
    return res


if __name__ == "__main__":
    sys.exit(0 if check(sys.argv[1], sys.argv[2] if len(sys.argv) > 2 else None)["ok"] else 1)
