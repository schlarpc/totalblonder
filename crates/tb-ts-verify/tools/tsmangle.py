#!/usr/bin/env python3
"""Deliberate damage to an MPEG-2 transport stream, for validating tb-ts-verify against
TSDuck. Also rewrites STT system_time so that it advances with the
byte position (TSDuck's inject plugin repeats a fixed STT when run offline).

usage: python3 -I tsmangle.py IN OUT OP [options]

OP (options in brackets):
  copy                                    no change
  stt-advance --rate R --gps0 G           STT system_time = G + seconds at R bit/s
  drop --pid P --nth N [--count K]        replace K packets of P from its N-th by nulls
  cc --pid P --nth N [--delta D]          add D to the continuity_counter of one packet
  crc --pid P --nth N                     flip a bit inside the section of the N-th PUSI
                                          packet of P
  delay-pat --from T --dur D --rate R     nulls instead of PAT packets for D s from T s
  pcr-jitter --pid P --ns X [--every N]   add +X / -X ns alternately to every N-th PCR
  pcr-jump --pid P --nth N --ms M         add M ms to the N-th PCR of P and all after it
  burst --pid P [--every N]               every N-th PES of P sent back to back (its
                                          packets swapped forward over other PIDs')
  tei --nth N                             set transport_error_indicator in packet N
  sync --nth N [--count C]                corrupt the sync byte of C packets from N
  scramble --pid P --nth N [--count C]    set transport_scrambling_control 10

Packet numbers count from 0; --nth counts packets of the PID (PCRs for the pcr ops).
"""

import argparse
import struct
import sys

NULL = bytes([0x47, 0x1F, 0xFF, 0x10]) + bytes([0xFF] * 184)


def crc32(data):
    c = 0xFFFFFFFF
    for b in data:
        c ^= b << 24
        for _ in range(8):
            c = ((c << 1) ^ 0x04C11DB7) if c & 0x80000000 else (c << 1)
            c &= 0xFFFFFFFF
    return c


def pid_of(p):
    return ((p[1] & 0x1F) << 8) | p[2]


def has_pcr(p):
    return p[3] & 0x20 and p[4] >= 7 and p[5] & 0x10


def get_pcr(p):
    b = p[6:12]
    base = (b[0] << 25) | (b[1] << 17) | (b[2] << 9) | (b[3] << 1) | (b[4] >> 7)
    return base * 300 + (((b[4] & 1) << 8) | b[5])


def put_pcr(p, pcr):
    pcr %= (1 << 33) * 300
    base, ext = divmod(pcr, 300)
    p[6] = (base >> 25) & 0xFF
    p[7] = (base >> 17) & 0xFF
    p[8] = (base >> 9) & 0xFF
    p[9] = (base >> 1) & 0xFF
    p[10] = ((base & 1) << 7) | 0x7E | ((ext >> 8) & 1)
    p[11] = ext & 0xFF


def payload_off(p):
    afc = (p[3] >> 4) & 3
    return 5 + p[4] if afc & 2 else 4


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("inp")
    ap.add_argument("out")
    ap.add_argument("op")
    ap.add_argument("--pid", type=lambda s: int(s, 0))
    ap.add_argument("--nth", type=int, default=0)
    ap.add_argument("--count", type=int, default=1)
    ap.add_argument("--delta", type=int, default=5)
    ap.add_argument("--every", type=int, default=1)
    ap.add_argument("--ns", type=float)
    ap.add_argument("--ms", type=float)
    ap.add_argument("--rate", type=float)
    ap.add_argument("--gps0", type=int)
    ap.add_argument("--from", dest="frm", type=float)
    ap.add_argument("--dur", type=float)
    a = ap.parse_args()

    data = open(a.inp, "rb").read()
    n = len(data) // 188
    pk = [bytearray(data[i * 188:(i + 1) * 188]) for i in range(n)]
    of_pid = lambda pid: [i for i in range(n) if pid_of(pk[i]) == pid]

    if a.op == "copy":
        pass
    elif a.op == "stt-advance":
        for i in of_pid(0x1FFB):
            p = pk[i]
            if not p[1] & 0x40 or payload_off(p) != 4:
                continue
            off = 5 + p[4]
            if off + 16 > 188 or p[off] != 0xCD:
                continue
            slen = ((p[off + 1] & 0x0F) << 8) | p[off + 2]
            end = off + 3 + slen
            if end > 188:
                continue
            secs = int((i * 188 + end - 1) * 8 / a.rate)
            struct.pack_into(">I", p, off + 9, a.gps0 + secs)
            struct.pack_into(">I", p, end - 4, crc32(p[off:end - 4]))
    elif a.op == "drop":
        for i in of_pid(a.pid)[a.nth:a.nth + a.count]:
            pk[i] = bytearray(NULL)
    elif a.op == "cc":
        i = of_pid(a.pid)[a.nth]
        pk[i][3] = (pk[i][3] & 0xF0) | ((pk[i][3] + a.delta) & 0x0F)
    elif a.op == "crc":
        i = [k for k in of_pid(a.pid) if pk[k][1] & 0x40][a.nth]
        off = payload_off(pk[i]) + 1 + pk[i][payload_off(pk[i])]
        pk[i][off + 9] ^= 0x01
    elif a.op == "delay-pat":
        lo = int(a.frm * a.rate / 1504)
        hi = int((a.frm + a.dur) * a.rate / 1504)
        for i in of_pid(0):
            if lo <= i < hi:
                pk[i] = bytearray(NULL)
    elif a.op == "pcr-jitter":
        k = 0
        sign = 1
        for i in of_pid(a.pid):
            if not has_pcr(pk[i]):
                continue
            if k % a.every == 0:
                put_pcr(pk[i], get_pcr(pk[i]) + sign * round(a.ns * 27 / 1000))
                sign = -sign
            k += 1
    elif a.op == "pcr-jump":
        k = 0
        for i in of_pid(a.pid):
            if not has_pcr(pk[i]):
                continue
            if k >= a.nth:
                put_pcr(pk[i], get_pcr(pk[i]) + round(a.ms * 27000))
            k += 1
    elif a.op == "burst":
        idx = of_pid(a.pid)
        starts = [j for j, i in enumerate(idx) if pk[i][1] & 0x40]
        for s_no, j in enumerate(starts):
            if s_no % a.every:
                continue
            nxt = starts[s_no + 1] if s_no + 1 < len(starts) else len(idx)
            members = idx[j:nxt]
            # Move the later members into the null packets that follow the first one, so
            # that they arrive back to back while every other PID's packets (and their
            # PCRs) keep their positions.
            nulls = [k for k in range(members[0] + 1, members[-1]) if pid_of(pk[k]) == 0x1FFF]
            for m, k in zip(members[1:], nulls):
                if k < m:
                    pk[k], pk[m] = pk[m], pk[k]
    elif a.op == "tei":
        pk[a.nth][1] |= 0x80
    elif a.op == "sync":
        for i in range(a.nth, a.nth + a.count):
            pk[i][0] = 0x00
    elif a.op == "scramble":
        for i in of_pid(a.pid)[a.nth:a.nth + a.count]:
            pk[i][3] = (pk[i][3] & 0x3F) | 0x80
    else:
        sys.exit("unknown op " + a.op)
    with open(a.out, "wb") as f:
        for p in pk:
            f.write(p)
        f.write(data[n * 188:])


if __name__ == "__main__":
    main()
