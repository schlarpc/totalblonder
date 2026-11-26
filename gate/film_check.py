#!/usr/bin/env python3
"""The cadence a film-mode stream asks a decoder for, checked on the decoders.

A stream with repeat_first_field codes fewer pictures than it displays: the decoder shows a
picture for two or three fields (interlaced sequence) or one, two or three frame periods
(progressive sequence at 59.94 Hz). This module checks what comes out of that, with three
readers that share nothing with the encoder:

  flags    crates/tb-m2v-verify/tools/esflags.py: top_field_first, repeat_first_field and
           progressive_frame of every picture, in display order; the output fields alternate
           (H.262 7.12)
  ffmpeg   ffprobe's view of every frame (repeat_pict, top_field_first), which must be the
           same cadence; the pictures decoded one per coded picture (-fps_mode passthrough);
           and the *displayed* sequence: for an interlaced sequence ffmpeg's `repeatfields`
           filter (the fields as a display gets them, woven two by two), for a progressive
           one ffmpeg's constant-rate output (each frame as often as it is shown). Every
           displayed frame must be, sample for sample, the fields of the decoded pictures
           that the flags name.
  libmpeg2 `mpeg2dec -v`: its own count of fields per picture and its top-field-first flag,
           and its decoded pictures against ffmpeg's

and, when the caller knows them, against the source: the display time covered (in display
units: fields, or frame periods of a progressive sequence), the durations in display order,
and each decoded picture against the source frame it was made from.

Usage: film_check.py stream.m2v WIDTH HEIGHT [--units N] [--pattern 3,2,...]
"""
import argparse
import os
import re
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(os.path.dirname(HERE), "crates", "tb-m2v-verify", "tools"))
import esflags  # noqa: E402


def run(cmd):
    return subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE)


def cadence_flags(data):
    """[(units, first field 'T'/'B'/'-', progressive_frame)] in display order, and parity breaks."""
    d = esflags.describe(data)
    return [(u, first, prog) for _, _, first, u, prog, _ in d["pictures"]], d


def cadence_ffprobe(m2v, prog_seq):
    """ffmpeg's view: [(units, first field)] in display order."""
    p = run(["ffprobe", "-v", "error", "-f", "mpegvideo", "-select_streams", "v", "-show_entries",
             "frame=top_field_first,repeat_pict", "-of", "csv=p=0", m2v])
    out = []
    for line in p.stdout.decode().split():
        tff, rep = (int(x) for x in line.split(",")[:2])
        if prog_seq:
            # libavcodec: 2 extra field periods for a frame shown twice, 4 for three times
            out.append((1 + rep // 2, "-"))
        else:
            out.append((2 + rep, "T" if tff else "B"))
    return out


def cadence_mpeg2dec(m2v, prog_seq):
    """libmpeg2's view: [(units, first field)] in display order (it lists coded order)."""
    p = run(["mpeg2dec", "-c", "-v", "-o", "null", m2v])
    text = (p.stdout + p.stderr).decode(errors="replace")
    coded = []
    for m in re.finditer(r"PICTURE (\w)(.*?) time_ref", text):
        fields = int(re.search(r"fields (\d+)", m.group(2)).group(1))
        tff = " TFF" in m.group(2)
        coded.append((m.group(1), fields, tff))
    out, held = [], None
    for t, fields, tff in coded:
        # libmpeg2 counts fields; in a progressive sequence it reports 2, 4 or 6
        item = (fields // 2, "-") if prog_seq else (fields, "T" if tff else "B")
        if t == "B":
            out.append(item)
        else:
            if held is not None:
                out.append(held)
            held = item
    if held is not None:
        out.append(held)
    return out


def decode(m2v, out, *opts):
    p = run(["ffmpeg", "-hide_banner", "-nostats", "-v", "warning", "-xerror", "-err_detect", "explode", "-f", "mpegvideo",
             "-i", m2v] + list(opts) + ["-f", "rawvideo", "-pix_fmt", "yuv420p", "-y", out])
    benign = ("Estimating duration from bitrate", "too many threads/slices")
    msgs = [l for l in p.stderr.decode(errors="replace").splitlines() if l.strip() and not any(b in l for b in benign)]
    return p.returncode, msgs


def planes(buf, w, h):
    """The three planes of a 4:2:0 frame as (bytes, width, height)."""
    y, c = w * h, (w // 2) * (h // 2)
    return [(buf[:y], w, h), (buf[y:y + c], w // 2, h // 2), (buf[y + c:y + 2 * c], w // 2, h // 2)]


def weave(top, bot, w, h):
    """A frame with the even rows of `top` and the odd rows of `bot`, plane by plane."""
    out = bytearray()
    for (a, pw, ph), (b, _, _) in zip(planes(top, w, h), planes(bot, w, h)):
        for r in range(ph):
            src = a if r % 2 == 0 else b
            out += src[r * pw:(r + 1) * pw]
    return bytes(out)


def psnr_y(a, b, w, h):
    """PSNR of the luminance of two 4:2:0 frames, dB (on a sample grid of at most about
    100 000 samples: enough to tell one frame from another)."""
    step = max(1, (w * h) // 100_000)
    ya, yb = a[:w * h:step], b[:w * h:step]
    sse = sum((x - y) * (x - y) for x, y in zip(ya, yb))
    if sse == 0:
        return 99.0
    import math
    return 10 * math.log10(255.0 * 255.0 * len(ya) / sse)


def check(m2v, w, h, units=None, pattern=None, keep=None):
    """Every check of the module on one stream. `units`: display units the stream must cover;
    `pattern`: the durations in display order (a list, or a cycle given as a tuple).
    Returns (failures, info); info["decoded"] is the path of the pictures decoded one per coded
    picture (kept when `keep` names a file)."""
    fails = []
    data = open(m2v, "rb").read()
    cad, d = cadence_flags(data)
    prog_seq = bool(d["progressive_sequence"])
    info = dict(pictures=len(cad), units=sum(u for u, _, _ in cad), by_units={k: sum(1 for u, _, _ in cad if u == k) for k in (1, 2, 3)},
                progressive_sequence=prog_seq, parity_breaks=d["parity_breaks"])
    if d["parity_breaks"]:
        fails.append(f"{d['parity_breaks']} places where the output fields do not alternate (H.262 7.12)")
    for k, (u, _, prog) in enumerate(cad):
        if not prog_seq and u == 3 and not prog:
            fails.append(f"picture {k}: three fields of a frame that is not progressive")
            break
    if units is not None and info["units"] != units:
        fails.append(f"the stream displays {info['units']} units, expected {units}")
    if pattern is not None:
        want = [pattern[k % len(pattern)] for k in range(len(cad))] if isinstance(pattern, tuple) else list(pattern)
        got = [u for u, _, _ in cad]
        if got != want:
            k = next((i for i, (a, b) in enumerate(zip(got, want)) if a != b), min(len(got), len(want)))
            fails.append(f"durations differ from the expected at picture {k}: {got[k:k + 8]} against {want[k:k + 8]} "
                         f"({len(got)} pictures, expected {len(want)})")
    flat = [(u, first) for u, first, _ in cad]
    for name, other in (("ffprobe", cadence_ffprobe(m2v, prog_seq)), ("libmpeg2", cadence_mpeg2dec(m2v, prog_seq))):
        if other != flat:
            k = next((i for i, (a, b) in enumerate(zip(other, flat)) if a != b), min(len(other), len(flat)))
            fails.append(f"{name} reports another cadence from picture {k}: {other[k:k + 4]} against the flags' {flat[k:k + 4]} "
                         f"({len(other)} pictures against {len(flat)})")
    # ---- pictures, one per coded picture
    fb = w * h * 3 // 2
    dec = keep or (m2v + ".film.yuv")
    rc, msgs = decode(m2v, dec, "-fps_mode", "passthrough")
    if rc != 0 or msgs:
        fails.append(f"ffmpeg decode: rc {rc}, {len(msgs)} messages: " + " / ".join(msgs[:2])[:200])
    a = open(dec, "rb").read() if os.path.exists(dec) else b""
    n = len(a) // fb
    if n != len(cad):
        fails.append(f"ffmpeg decoded {n} pictures, the stream has {len(cad)}")
        return fails, info
    pic = lambda k: a[k * fb:(k + 1) * fb]  # noqa: E731
    # ---- the displayed sequence
    shown = m2v + ".shown.yuv"
    if not prog_seq and cad and cad[0][1] == "B":
        # ffmpeg's repeatfields filter starts on a top field; a stream that begins with a bottom
        # field (legal: 7.12) is checked by the flags and by the two decoders' reports only
        info["displayed_frames"] = None
        if not keep:
            os.remove(dec)
        return fails, info
    if prog_seq:
        # at the sequence's frame rate (ffmpeg would otherwise take the rate from the durations
        # it sees: a stream in which every frame is shown twice looks like half the rate)
        rate = {1: "24000/1001", 2: "24", 3: "25", 4: "30000/1001", 5: "30", 6: "50", 7: "60000/1001", 8: "60"}[d["frame_rate_code"]]
        rc, msgs = decode(m2v, shown, "-fps_mode", "cfr", "-r", rate)
        want = [k for k, (u, _, _) in enumerate(cad) for _ in range(u)]
    else:
        rc, msgs = decode(m2v, shown, "-vf", "repeatfields", "-fps_mode", "passthrough")
        fields = []
        for k, (u, first, _) in enumerate(cad):
            top = first == "T"
            for i in range(u):
                fields.append((k, top == (i % 2 == 0)))
        # two fields a frame, in display order: (picture behind the top rows, behind the bottom rows)
        want = []
        for j in range(0, len(fields) - 1, 2):
            f0, f1 = fields[j], fields[j + 1]
            if f0[1] == f1[1]:
                want.append(None)       # a parity break: reported above
            else:
                want.append((f0[0], f1[0]) if f0[1] else (f1[0], f0[0]))
    if rc != 0 or msgs:
        fails.append(f"ffmpeg display decode: rc {rc}, {len(msgs)} messages: " + " / ".join(msgs[:2])[:200])
    s = open(shown, "rb").read() if os.path.exists(shown) else b""
    got = len(s) // fb
    info["displayed_frames"] = got
    if got != len(want):
        fails.append(f"ffmpeg displays {got} frames, the flags give {len(want)}")
    else:
        bad = []
        for j, wk in enumerate(want):
            if wk is None:
                continue
            exp = pic(wk) if prog_seq else (pic(wk[0]) if wk[0] == wk[1] else weave(pic(wk[0]), pic(wk[1]), w, h))
            if s[j * fb:(j + 1) * fb] != exp:
                bad.append(j)
        if bad:
            fails.append(f"{len(bad)} of {got} displayed frames are not the fields of the pictures the flags name (first: frame {bad[0]})")
    for x in (shown,) + (() if keep else (dec,)):
        if os.path.exists(x):
            os.remove(x)
    return fails, info


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("m2v")
    ap.add_argument("width", type=int)
    ap.add_argument("height", type=int)
    ap.add_argument("--units", type=int)
    ap.add_argument("--pattern", help="the cycle of durations in display order, e.g. 3,2")
    a = ap.parse_args()
    pattern = tuple(int(x) for x in a.pattern.split(",")) if a.pattern else None
    fails, info = check(a.m2v, a.width, a.height, a.units, pattern)
    print(info)
    for f in fails:
        print("FAIL:", f)
    print("FILM RESULT:", "PASS" if not fails else "FAIL")
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())
