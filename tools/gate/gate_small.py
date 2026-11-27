#!/usr/bin/env python3
"""The gate at small size: the golden cases (crates/tb-golden/vectors/generate.sh) through an encoder binary,
checked on their own merits with the gate's machinery (check.py):

  decode   ffmpeg with -xerror -err_detect explode: no message at warning level or above
  struct   frame count, picture types in display order, ffprobe stream fields
  dec2     libmpeg2 (mpeg2dec) decodes the stream to nearly the same pictures
  vbv      Annex C simulation, normative and with the rate schedule of --stats (vbv_check.py)
  drift    the encoder's reconstruction of every picture against ffmpeg's decode
  cc       captions come back in display order
  conform  the conformance verifier on every stream (H.262 alone: the pictures are test sizes,
           no broadcast format; S1/S2 fail, S3 fails unless gate-allow.txt allows it)
  golden   with --golden: SHA-256 of every stream equals crates/tb-golden/vectors/<case>.sha256

Usage: gate_small.py --enc tb-m2venc [--device ref|cuda|hip] [--golden] [--work DIR]
Needs ffmpeg, ffprobe, mpeg2dec on PATH and the verifier (M2V_VERIFY). No GPU is needed with
--device ref: this is the gate that runs in `nix flake check` (check gate/ref-small).
Exit status 0 only if every case passes.
"""
import argparse
import glob
import hashlib
import json
import os
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
sys.path.insert(0, HERE)
import formats as F  # noqa: E402
import check as C  # noqa: E402
import conform  # noqa: E402
import vbv_check  # noqa: E402
import film_check  # noqa: E402

# name: (width, height, frames, seed, kind)
CLIPS = {"qcif": (176, 144, 30, 1, ""), "qcif-noisy": (176, 144, 24, 2, "noisy"), "w184": (184, 144, 12, 3, ""),
         "qcif-i": (176, 144, 24, 4, "interlaced")}
# The cases of crates/tb-golden/vectors/generate.sh: name, clip, encoder arguments, what the checks need to know.
CASES = [
    dict(id="cbr", clip="qcif", args="--size 176x144 --gop 6 --bframes 2 --bitrate 300000 --max-bitrate 600000 --cc "
         "--rate-plan 2:150000,3:500000", cc=True),
    dict(id="ladder", clip="qcif-noisy", args="--size 176x144 --gop 6 --bframes 2 --bitrate 135000 --vbv 16384", ladder=True),
    dict(id="fixedq", clip="w184", args="--size 184x144 --gop 6 --bframes 2 --q 6 --force-key 7", force=(7,), fixed=True),
    dict(id="interlaced", clip="qcif-i", args="--size 176x144 --interlaced --gop 6 --bframes 2 --bitrate 400000 "
         "--max-bitrate 500000 --overlap --cc --rate-now 8:250000", cc=True, interlaced=True),
    dict(id="shed", clip="qcif", args="--size 176x144 --gop 6 --bframes 2 --bitrate 300000 --sessions 2 --stagger --write 2 "
         "--force-shed 0.3:7 --hold 0:1:10:16", sessions=2, repeats=True),
    # the cases added with the Rust recorder (crates/tb-golden/vectors/generate.sh); every stream checked on its own
    # (generic): strict decode in two decoders, the normative buffer model (and the operating one
    # with the rate schedule for the first stream), the verifier
    dict(id="skip", clip="qcif-noisy", args="--size 176x144 --gop 2 --bframes 0 --bitrate 132000 --vbv 16384",
         generic=True),
    dict(id="deadline", clip="qcif", args="--size 176x144 --gop 6 --bframes 2 --bitrate 300000 --sessions 3 --stagger --write 3 "
         "--test-deadline 1025000", generic=True, ref_only=True),
    dict(id="churn", clip="qcif", args="--size 176x144 --gop 6 --bframes 2 --bitrate 300000 --frames 24 --group --input {work}/qcif-i.yuv "
         "--size 176x144 --interlaced --gop 6 --bframes 2 --bitrate 300000 --sessions 3 --pool --churn 5:15 --churn-chunk 2 "
         "--reap-after 3 --write 3", generic=True, clips=("qcif-i",)),
    dict(id="devin", clip="w184", args="--size 184x144 --gop 6 --bframes 2 --bitrate 300000 --input-mode dev", generic=True),
    dict(id="twofmt", clip="qcif", args="--size 176x144 --gop 6 --bframes 2 --bitrate 300000 --cc --group --input {work}/qcif-i.yuv "
         "--size 176x144 --interlaced --gop 6 --bframes 2 --bitrate 400000 --frames 24", generic=True, clips=("qcif-i",)),
    # film mode (frames shown for other than one frame period): the cadence on both decoders,
    # the displayed fields, each picture against its source frame (film_check.py)
    dict(id="film", clip="qcif", args="--size 176x144 --interlaced --gop 15 --bframes 2 --bitrate 400000 --max-bitrate 600000 "
         "--frames 40 --cc --film 3,2,3,2,3,2,2,i,i,3,3,2 --rate-now 17:300000 --hold 0:0:26:30",
         film=(3, 2, 3, 2, 3, 2, 2, 2, 2, 3, 3, 2), interlaced=True),
    dict(id="film60", clip="qcif", args="--size 176x144 --fps 60000/1001 --gop 30 --bframes 2 --bitrate 400000 --max-bitrate 600000 "
         "--frames 40 --cc --film 3,2,3,2,3,2,1,1,2,3 --force-shed 0.2:3 --overlap",
         film=(3, 2, 3, 2, 3, 2, 1, 1, 2, 3), fps=(60000, 1001)),
]


def sh(cmd):
    return subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)


def run_case(c, a, work):
    fails = []
    w, h, frames, seed, kind = CLIPS[c["clip"]]
    fmt = "small-" + c["id"]
    # the gate's helpers look formats up by name
    F.FORMATS[fmt] = dict(size=(w, h), fps=c.get("fps", (30000, 1001)), gop=6, bitrate=0, vbv=0, interlaced=bool(c.get("interlaced")))
    for name in (c["clip"],) + c.get("clips", ()):
        cw, ch, cn, cs, ck = CLIPS[name]
        path = os.path.join(work, name + ".yuv")
        if not os.path.exists(path):
            with open(path, "wb") as f:
                p = subprocess.run([a.enc, "--make-clip", str(cw), str(ch), str(cn), str(cs)] + ([ck] if ck else []), stdout=f)
            if p.returncode:
                return ["could not make the clip " + name]
    clip = os.path.join(work, c["clip"] + ".yuv")
    base = os.path.join(work, c["id"])
    recon, stats = base + ".recon.yuv", base + ".stats.jsonl"
    cmd = [a.enc, "--device", a.device, "--conformance", "h262", "--input", clip] + c["args"].format(work=work).split() + [
        "--output", base, "--recon", recon, "--stats", stats, "--json", base + ".json"]
    for old in glob.glob(glob.escape(base) + ".*m2v"):
        os.remove(old)
    p = sh(cmd)
    if p.returncode:
        return [f"encoder failed (status {p.returncode}): " + (p.stderr or p.stdout)[-300:].replace("\n", " ")]
    if c.get("generic"):
        return generic(c, a, base, fmt, stats)
    if c.get("film"):
        return film(c, a, base, fmt, stats, clip)
    ns = c.get("sessions", 1)
    names = [f"{base}.{s}.m2v" for s in range(ns)] if ns > 1 else [base + ".m2v"]
    for s, m2v in enumerate(names):
        tag = f"session {s}: " if ns > 1 else ""
        dec, dec2 = m2v + ".dec.yuv", m2v + ".dec2.yuv"
        rc, msgs = C.decode_ffmpeg(m2v, dec)
        if rc != 0 or msgs:
            fails.append(f"{tag}ffmpeg decode: rc {rc}, {len(msgs)} messages: " + " / ".join(msgs[:2])[:200])
        got = os.path.getsize(dec) // F.frame_bytes(fmt) if os.path.exists(dec) else 0
        if got != frames:
            fails.append(f"{tag}decoded {got} frames, expected {frames}")
            continue
        st, types = C.probe(m2v)
        if not c.get("repeats"):
            exp = C.plan_types(frames, 6, 3, 0, c.get("force", ()))
            if c.get("ladder"):   # the last rung replaces an I picture by an all-skipped P picture
                types = "".join("I" if (b == "I" and t == "P" and i > 0) else t for i, (t, b) in enumerate(zip(types, exp)))
            if types != exp:
                fails.append(f"{tag}picture types {types} expected {exp}")
        want = dict(codec_name="mpeg2video", profile="Main", width=w, height=h, r_frame_rate="30000/1001", pix_fmt="yuv420p",
                    field_order="tt" if c.get("interlaced") else "progressive")
        for k, v in want.items():
            if st.get(k) != v:
                fails.append(f"{tag}stream {k} = {st.get(k)}, expected {v}")
        n2, _ = C.decode_mpeg2dec(fmt, m2v, dec2)
        if n2 != frames:
            fails.append(f"{tag}mpeg2dec decoded {n2} frames, expected {frames}")
        else:
            d2 = C.compare(fmt, dec2, dec)
            if d2["psnr_avg"] < C.MIN_DEC2_AVG:
                fails.append(f"{tag}mpeg2dec differs from ffmpeg: {d2['psnr_avg']:.1f} dB")
        if not c.get("fixed"):
            v = vbv_check.check(m2v, schedule=stats if s == 0 else None, quiet=True)
            op = v.get("op")
            if not v["ok"]:
                fails.append(f"{tag}VBV: normative {v['underflows']} underflows, {v['overflows']} overflows; operating "
                             f"{op['underflows'] if op else '-'} underflows; {v['seq_mismatch']} changed sequence headers")
            if s == 0 and not op:
                fails.append("no rate schedule: operating model not checked")
        if s == 0:
            # (a repeated picture has no reconstruction of its own: no drift figure with repeats)
            if c.get("repeats"):
                pass
            elif os.path.exists(recon) and os.path.getsize(recon) == frames * F.frame_bytes(fmt):
                d = C.compare(fmt, recon, dec)
                if d["psnr_avg"] < C.MIN_DRIFT_AVG or d["psnr_min"] < C.MIN_DRIFT_MIN:
                    fails.append(f"drift: reconstruction vs decode avg {d['psnr_avg']:.1f} dB, worst {d['psnr_min']:.1f} dB")
            else:
                fails.append("no reconstruction dump")
            if c.get("cc"):
                e = C.check_cc(m2v, frames, 29.97)
                if e:
                    fails.append(e)
    fl, _ = conform.verify(names, conform.TEST_SIZE)
    fails += fl
    if a.golden:
        fails += golden_digests(c, names, a.device)
    return fails


def generic(c, a, base, fmt, stats):
    """Every stream the case wrote, on its own merits."""
    fails = []
    names = sorted(glob.glob(glob.escape(base) + ".m2v") + glob.glob(glob.escape(base) + ".*.m2v"))
    if not names:
        return ["no stream written"]
    for i, m2v in enumerate(names):
        tag = os.path.basename(m2v) + ": "
        dec, dec2 = m2v + ".dec.yuv", m2v + ".dec2.yuv"
        rc, msgs = C.decode_ffmpeg(m2v, dec)
        if rc != 0 or msgs:
            fails.append(f"{tag}ffmpeg decode: rc {rc}, {len(msgs)} messages: " + " / ".join(msgs[:2])[:200])
        _, types = C.probe(m2v)
        got = os.path.getsize(dec) // F.frame_bytes(fmt) if os.path.exists(dec) else 0
        if got != len(types) or got == 0:
            fails.append(f"{tag}decoded {got} frames, {len(types)} pictures")
        n2, _ = C.decode_mpeg2dec(fmt, m2v, dec2)
        if n2 != got:
            fails.append(f"{tag}mpeg2dec decoded {n2} frames, ffmpeg {got}")
        elif got:
            d2 = C.compare(fmt, dec2, dec)
            if d2["psnr_avg"] < C.MIN_DEC2_AVG:
                fails.append(f"{tag}mpeg2dec differs from ffmpeg: {d2['psnr_avg']:.1f} dB")
        # the rate schedule (--stats) is the first session's: the operating model for it
        v = vbv_check.check(m2v, schedule=stats if i == 0 else None, quiet=True)
        op = v.get("op")
        if not v["ok"]:
            fails.append(f"{tag}VBV: normative {v['underflows']} underflows, {v['overflows']} overflows; operating "
                         f"{op['underflows'] if op else '-'} underflows; {v['seq_mismatch']} changed sequence headers")
        for x in (dec, dec2):
            if os.path.exists(x):
                os.remove(x)
    fails += conform.verify(names, conform.TEST_SIZE)[0]
    if a.golden:
        fails += golden_digests(c, names, a.device)
    return fails


def film(c, a, base, fmt, stats, clip):
    """A film-mode case: the cadence the stream asks for on both decoders, the displayed
    fields, every picture against the clip frame it was coded from, both buffer models with
    the removal times of the flags, the verifier."""
    fails = []
    w, h = F.FORMATS[fmt]["size"]
    fb = F.frame_bytes(fmt)
    m2v = base + ".m2v"
    frames = 40
    dec, dec2 = m2v + ".dec.yuv", m2v + ".dec2.yuv"
    fl, info = film_check.check(m2v, w, h, units=sum(c["film"][k % len(c["film"])] for k in range(frames)), pattern=c["film"], keep=dec)
    fails += fl
    if os.path.exists(dec) and os.path.getsize(dec) == frames * fb:
        n2, _ = C.decode_mpeg2dec(fmt, m2v, dec2)
        if n2 != frames:
            fails.append(f"mpeg2dec decoded {n2} frames, expected {frames}")
        else:
            d2 = C.compare(fmt, dec2, dec)
            if d2["psnr_avg"] < C.MIN_DEC2_AVG:
                fails.append(f"mpeg2dec differs from ffmpeg: {d2['psnr_avg']:.1f} dB")
        # each picture is the clip frame it was made from (frame n of the clip, in display
        # order), and no other: closer to it than to its neighbors
        src = open(clip, "rb").read()
        nclip = len(src) // fb
        got = open(dec, "rb").read()
        # (a repeat made on the host shows the picture before it: the rows of --stats say which)
        rows = [json.loads(l) for l in open(stats) if l.strip()]
        skip = {r["disp"] for r in rows if r["repeat"]}
        if [r["units"] for r in sorted(rows, key=lambda r: r["disp"])] != [c["film"][k % len(c["film"])] for k in range(frames)]:
            fails.append("the access units' display durations are not the stream's")
        wrong = 0
        for n in range(frames):
            if n in skip:
                continue
            pic = got[n * fb:(n + 1) * fb]
            own = film_check.psnr_y(pic, src[(n % nclip) * fb:(n % nclip + 1) * fb], w, h)
            other = max(film_check.psnr_y(pic, src[(m % nclip) * fb:(m % nclip + 1) * fb], w, h) for m in (n - 1, n + 1))
            wrong += own < other + 3.0
        if wrong:
            fails.append(f"{wrong} pictures are not clearly the clip frame they were coded from (within 3 dB of a neighbor)")
    v = vbv_check.check(m2v, schedule=stats, quiet=True)
    op = v.get("op")
    if not v["ok"] or not op:
        fails.append(f"VBV: normative {v['underflows']} underflows, {v['overflows']} overflows; operating "
                     f"{op['underflows'] if op else '-'} underflows; {v['seq_mismatch']} changed sequence headers")
    for x in (dec, dec2):
        if os.path.exists(x):
            os.remove(x)
    fails += conform.verify([m2v], conform.TEST_SIZE)[0]
    if a.golden:
        fails += golden_digests(c, [m2v], a.device)
    return fails


def golden_digests(c, names, device="ref"):
    if c.get("ref_only") and device != "ref":
        return []   # the deadline follows the device's clock: steps on the reference adapter only
    want = {}
    for line in open(os.path.join(ROOT, "crates", "tb-golden", "vectors", c["id"] + ".sha256")):
        digest, name = line.split()
        want[name] = digest
    fails = []
    if sorted(want) != sorted(os.path.basename(n) for n in names):
        fails.append(f"streams {sorted(os.path.basename(n) for n in names)}, crates/tb-golden/vectors/{c['id']}.sha256 has {sorted(want)}")
    for m2v in names:
        got = hashlib.sha256(open(m2v, "rb").read()).hexdigest()
        if want.get(os.path.basename(m2v)) != got:
            fails.append(f"{os.path.basename(m2v)}: SHA-256 {got[:16]}... is not crates/tb-golden/vectors/{c['id']}.sha256's {want.get(os.path.basename(m2v), '?')[:16]}...")
    return fails


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--enc", required=True, help="the encoder binary (tb-m2venc)")
    ap.add_argument("--device", default="ref", choices=["ref", "cuda", "hip"])
    ap.add_argument("--golden", action="store_true", help="also require the streams of crates/tb-golden/vectors/*.sha256")
    ap.add_argument("--work", help="directory for clips and outputs (default: a temporary one)")
    a = ap.parse_args()
    tmp = None
    if not a.work:
        tmp = tempfile.TemporaryDirectory()
        a.work = tmp.name
    os.makedirs(a.work, exist_ok=True)
    nfail = 0
    for c in CASES:
        try:
            fails = run_case(c, a, a.work)
        except Exception as e:  # noqa: BLE001
            fails = [f"exception {e!r}"[:300]]
        nfail += bool(fails)
        print(f"{c['id']:12} {'ok' if not fails else 'FAIL: ' + '; '.join(fails)}", flush=True)
    print(f"{len(CASES)} cases, {nfail} failed")
    return 1 if nfail else 0


if __name__ == "__main__":
    sys.exit(main())
