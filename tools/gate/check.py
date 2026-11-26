#!/usr/bin/env python3
"""Correctness gate for the encoder (`nix run .#gate-check -- [--quick]`, on the card).

For every case (format x clip x rate) it encodes one session and checks:
  decode    ffmpeg with -xerror -err_detect explode: no message at warning level or above
  struct    frame count, picture types in display order, ffprobe stream fields
  dec2      libmpeg2 (mpeg2dec) decodes the same stream to nearly the same pictures
  vbv       independent Annex C simulation (vbv_check.py): no underflow, no overflow
  drift     the encoder's own reconstruction of every picture against ffmpeg's decode
  rate      mean bit rate against the target
  quality   PSNR (Y, U, V) and SSIM against the source, compared with the stored baseline
  conform   every coded stream through the conformance verifier (conform.py: SCTE 43 at 256-QAM,
            operating delivery model; S1/S2 fail, S3 fails unless gate-allow.txt allows it)
and records the ffmpeg mpeg2video encode of the same clip at the same rate as reference.

Files: $M2V_RESULTS/check/latest.json (this run) and latest.txt (the table); the stored quality
tools/gate/baselines/baseline.json (--update-baseline rewrites it) and the cached ffmpeg reference
encodes tools/gate/baselines/ref.json (read from there, kept up to date in $M2V_RESULTS/check/ref.json).
Exit status 0 only if every case passes.
"""
import argparse
import concurrent.futures as cf
import glob
import json
import os
import re
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import formats as F  # noqa: E402
import conform  # noqa: E402
import vbv_check  # noqa: E402

ROOT = F.ROOT
OUT = os.path.join(F.OUT, "check")
RES = os.path.join(F.RESULTS, "check")

# Tolerances.
TOL_PSNR = 0.10      # dB below baseline, any plane
TOL_SSIM = 0.002
MIN_DRIFT_AVG, MIN_DRIFT_MIN = 48.0, 40.0   # dB, encoder reconstruction against ffmpeg's decode
MIN_DEC2_AVG = 48.0                          # dB, mpeg2dec against ffmpeg
RATE_HI = 1.02       # mean rate may not exceed target by more than 2 %


def case(fmt, clip, frac=1.0, tag="", extra=(), kind="std", **kw):
    f = F.FORMATS[fmt]
    br = int(f["bitrate"] * frac)
    cid = f"{fmt}-{clip}" + (f"-{tag}" if tag else "")
    return dict(id=cid, fmt=fmt, clip=clip, bitrate=br, nominal=frac == 1.0, extra=list(extra), kind=kind, **kw)


def cases():
    c = []
    # Standard: nominal rate, representative clips per format.
    for clip in ["crowd_run", "pedestrian_area", "park_joy", "fourpeople", "cuts", "fade", "grain"]:
        c.append(case("1080i", clip))
    for clip in ["crowd_run", "football", "mobcal", "cuts", "testsrc2", "park_joy"]:
        c.append(case("480i", clip))
    for clip in ["crowd_run", "fourpeople", "park_joy", "cuts", "fade", "still"]:
        c.append(case("720p", clip))
    for clip in ["crowd_run", "pedestrian_area", "cuts"]:
        c.append(case("1080p", clip))
    # Torture: the buffer model must hold whatever the content and the rate.
    for fmt in ["1080i", "720p", "480i"]:
        c.append(case(fmt, "crowd_run", 0.5, "half", kind="torture"))
        c.append(case(fmt, "crowd_run", 0.25, "quarter", kind="torture"))
        c.append(case(fmt, "noise", 1.0, kind="torture"))
        c.append(case(fmt, "noise", 0.25, "quarter", kind="torture"))
        c.append(case(fmt, "cuts", 0.25, "quarter", kind="torture"))
    c.append(case("1080i", "park_joy", 0.25, "quarter", kind="torture"))
    c.append(case("1080i", "still", 1.0, kind="torture"))
    c.append(case("480i", "still", 1.0, kind="torture"))
    c.append(case("1080p", "noise", 0.25, "quarter", kind="torture"))
    # Features.
    # rate change at every GOP over a wide range, constant sequence header (bit_rate = the ceiling)
    for fmt, clip, lo in [("1080i", "crowd_run", 0.2), ("720p", "cuts", 0.2), ("480i", "football", 0.25)]:
        f = F.FORMATS[fmt]
        c.append(case(fmt, clip, 0.8, "ratewalk", kind="feature", feature="rate",
                      extra=["--max-bitrate", str(f["bitrate"]), "--vbv", str(f["vbv"]), "--rate-walk",
                             f"{int(f['bitrate'] * lo)}:{f['bitrate']}:3"]))
    c.append(case("1080i", "cuts", 1.0, "ratestep", kind="feature", feature="rate",
                  extra=["--rate-plan", "1:3750000,3:15000000,5:5000000,6:15000000,7:4000000,8:12000000"]))
    # rate changes in the middle of GOPs (effective at the next coded picture)
    c.append(case("720p", "crowd_run", 0.5, "ratenow", kind="feature", feature="rate",
                  extra=["--max-bitrate", "12000000", "--vbv", "5996544", "--rate-now",
                         "17:12000000,50:3000000,71:9000000,100:2500000,131:12000000,200:4000000,247:8000000"]))
    # captions: cc_data in picture user data, returned by a decoder in display order
    c.append(case("1080i", "pedestrian_area", 1.0, "cc", kind="feature", feature="cc", extra=["--cc"]))
    c.append(case("720p", "fourpeople", 1.0, "cc", kind="feature", feature="cc", extra=["--cc"]))
    # key frames on demand (closed GOP, sequence header) at arbitrary frames
    c.append(case("480i", "crowd_run", 1.0, "forcekey", kind="feature", force=[7, 20, 41, 42, 100],
                  extra=["--force-key", "7,20,41,42,100"]))
    c.append(case("720p", "crowd_run", 1.0, "forcekey", kind="feature", force=[5, 64, 65, 200], extra=["--force-key", "5,64,65,200"]))
    # bottom field first
    c.append(case("480i", "football_bff", 1.0, "bff", kind="feature", field_order="bb", extra=["--bff"]))
    # several sessions of one format at different GOP phases, all streams checked
    c.append(case("1080i", "crowd_run", 1.0, "stagger4", kind="feature", feature="multi", sessions=4,
                  extra=["--sessions", "4", "--stagger", "--write", "4"]))
    c.append(case("480i", "cuts", 1.0, "stagger7-overlap", kind="feature", feature="multi", sessions=7,
                  extra=["--sessions", "7", "--stagger", "--write", "7", "--overlap"]))
    # three formats in one process (two frame rates), staggered, rate walks; every stream checked
    c.append(dict(id="mixed-formats", kind="feature", feature="mixed", fmt="1080i", clip="crowd_run", extra=[], nominal=True,
                  bitrate=F.FORMATS["1080i"]["bitrate"]))
    return c


def plan_types(frames, gop, m=3, phase=0, force=()):
    """Picture types in display order, by the rules of the encoder's GOP structure (tb_m2v_encoder::gop)."""
    out, pend, gop_first, next_key = {}, [], 0, 0

    def close():
        if pend:
            out[pend.pop()] = "P"
            for b in pend:
                out[b] = "B"
            pend.clear()

    for d in range(frames):
        if d == next_key or d in force:
            close()
            gop_first = d
            next_key = gop - phase % gop if (d == 0 and phase > 0) else d + gop
            out[d] = "I"
        elif (d - gop_first) % m == 0 or d + 1 == next_key:
            out[d] = "P"
            for b in pend:
                out[b] = "B"
            pend.clear()
        else:
            pend.append(d)
    close()
    return "".join(out[d] for d in range(frames))


# The subset run after every change: each format, interlaced motion, a cut, and the buffer
# model under stress.
QUICK = {"1080i-crowd_run", "1080i-cuts", "480i-football", "480i-crowd_run", "720p-crowd_run", "720p-fourpeople",
         "1080p-pedestrian_area", "1080i-crowd_run-quarter", "720p-noise-quarter", "480i-cuts-quarter", "480i-still",
         "1080i-crowd_run-ratewalk", "720p-fourpeople-cc", "480i-crowd_run-forcekey", "mixed-formats"}


def run(cmd, **kw):
    return subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, **kw)


def raw_in(fmt, path):
    w, h = F.FORMATS[fmt]["size"]
    return ["-f", "rawvideo", "-pix_fmt", "yuv420p", "-s", f"{w}x{h}", "-i", path]


def compare(fmt, a, b):
    """PSNR and SSIM of raw file a against raw file b."""
    p = run(["ffmpeg", "-hide_banner", "-nostats"] + raw_in(fmt, a) + raw_in(fmt, b) + [
        "-filter_complex", "[0:v]split[a][b];[1:v]split[c][d];[a][c]psnr=shortest=1[o1];[b][d]ssim=shortest=1[o2]",
        "-map", "[o1]", "-f", "null", "-", "-map", "[o2]", "-f", "null", "-"])
    m = re.search(r"PSNR y:([\d.inf]+) u:([\d.inf]+) v:([\d.inf]+) average:([\d.inf]+) min:([\d.inf]+)", p.stderr)
    s = re.search(r"SSIM Y:([\d.]+) .*All:([\d.]+)", p.stderr)
    if not m or not s:
        raise RuntimeError("compare failed: " + p.stderr[-400:])
    g = [float(x) if x != "inf" else 99.0 for x in m.groups()]
    return dict(psnr_y=g[0], psnr_u=g[1], psnr_v=g[2], psnr_avg=g[3], psnr_min=g[4], ssim_y=float(s.group(1)),
                ssim=float(s.group(2)))


def decode_ffmpeg(m2v, out):
    # -f mpegvideo: an elementary stream; without it a stream of a picture or two is probed with
    # a "low score" warning that says nothing about the stream
    p = run(["ffmpeg", "-hide_banner", "-nostats", "-v", "warning", "-xerror", "-err_detect", "explode", "-f", "mpegvideo", "-i", m2v,
             "-f", "rawvideo", "-pix_fmt", "yuv420p", "-y", out])
    # Not about the stream: the duration estimate of a raw elementary stream, and the decoder's own
    # thread count on a machine with more cores than a small picture has slices.
    benign = ("Estimating duration from bitrate", "too many threads/slices")
    msgs = [l for l in p.stderr.splitlines() if l.strip() and not any(b in l for b in benign)]
    return p.returncode, msgs


def decode_mpeg2dec(fmt, m2v, out):
    """libmpeg2: `-o pgmpipe` writes one PGM per frame, Y on top, then U and V side by side."""
    w, h = F.FORMATS[fmt]["size"]
    p = subprocess.Popen(["mpeg2dec", "-c", "-o", "pgmpipe", m2v], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    inp, n = p.stdout, 0
    with open(out, "wb") as o:
        while True:
            magic = inp.readline()
            if not magic:
                break
            if magic.strip() != b"P5":
                break
            line = inp.readline()
            while line.startswith(b"#"):
                line = inp.readline()
            pw, ph = map(int, line.split())
            inp.readline()
            data = inp.read(pw * ph)
            ch = (ph * 2) // 3
            mv = memoryview(data)
            o.write(b"".join(mv[r * pw: r * pw + w] for r in range(h)))
            o.write(b"".join(mv[(ch + r) * pw: (ch + r) * pw + w // 2] for r in range(h // 2)))
            o.write(b"".join(mv[(ch + r) * pw + pw // 2: (ch + r) * pw + pw // 2 + w // 2] for r in range(h // 2)))
            n += 1
    err = p.stderr.read().decode(errors="replace")
    p.wait()
    return n, err.strip().splitlines()


def expected_types(frames, gop, m=3):
    t = []
    for d in range(frames):
        g = d % gop
        t.append("I" if g == 0 else ("P" if (g % m == 0 or g == gop - 1 or d == frames - 1) else "B"))
    return "".join(t)


def probe(m2v):
    p = run(["ffprobe", "-hide_banner", "-v", "error", "-select_streams", "v:0", "-show_entries",
             "stream=codec_name,profile,level,width,height,r_frame_rate,field_order,pix_fmt:frame=pict_type",
             "-of", "json", m2v])
    j = json.loads(p.stdout)
    return j["streams"][0], "".join(f["pict_type"] for f in j["frames"])


def check_cc(m2v, frames, fps):
    """Captions as ffmpeg returns them (A53 side data of the decoded frames, display order)."""
    out = m2v + ".cc"
    p = run(["ffmpeg", "-hide_banner", "-nostats", "-v", "error", "-f", "lavfi", "-i", f"movie={m2v}[out0+subcc]", "-map", "0:1",
             "-c", "copy", "-f", "data", "-y", out])
    if p.returncode or not os.path.exists(out):
        return "caption extraction failed: " + p.stderr[-200:]
    data = open(out, "rb").read()
    per = 3 * (10 if fps > 40 else 20)
    if len(data) != per * frames:
        return f"caption bytes {len(data)}, expected {per * frames}"
    for d in range(frames):
        t = data[d * per: (d + 1) * per]
        # the field 1 pair (cc_type 00) carries the display index; in a picture with
        # top_field_first the field 2 pair (cc_type 01) comes before it (CTA-708-E 4.4.2)
        if fps <= 40 and t[0] == 0xFD:
            t = t[3:6] + t[0:3] + t[6:]
        if t[0] != 0xFC or ((t[1] & 0x7F) << 7 | (t[2] & 0x7F)) != d:
            return f"caption of display frame {d} carries index {((t[1] & 0x7F) << 7 | (t[2] & 0x7F))} (marker {t[0]:#x})"
        k = 3 if fps > 40 else 6
        if t[k] != 0xFF or t[k + 1] != (d & 0xFF) or t[k + 2] != 0x55:
            return f"DTVCC triple of display frame {d} wrong"
    return None


def check_stream(fmt, m2v, frames, exp_types, field_order, tmp):
    """Strict decode, structure and normative buffer check of one stream. Returns a list of failures."""
    f, fails = F.FORMATS[fmt], []
    rc, msgs = decode_ffmpeg(m2v, tmp)
    if rc != 0 or msgs:
        fails.append(f"ffmpeg decode: rc {rc}, {len(msgs)} messages: " + " / ".join(msgs[:2])[:200])
    got = os.path.getsize(tmp) // F.frame_bytes(fmt) if os.path.exists(tmp) else 0
    if got != frames:
        fails.append(f"decoded {got} frames, expected {frames}")
        return fails
    st, types = probe(m2v)
    if types != exp_types:
        fails.append(f"picture types {types[:32]}... expected {exp_types[:32]}...")
    want = dict(codec_name="mpeg2video", profile="Main", width=f["size"][0], height=f["size"][1],
                r_frame_rate="%d/%d" % f["fps"], pix_fmt="yuv420p", field_order=field_order)
    for k, v in want.items():
        if st.get(k) != v:
            fails.append(f"stream {k} = {st.get(k)}, expected {v}")
    v = vbv_check.check(m2v, quiet=True)
    if not v["ok"]:
        fails.append(f"VBV: {v['underflows']} underflows, {v['overflows']} overflows, {v['seq_mismatch']} changed sequence headers")
    return fails


def run_mixed(c):
    """1080i x2, 720p x2, 480i x4 in one process: staggered GOPs, a rate walk in every group."""
    r = dict(id=c["id"], kind=c["kind"], fmt="mixed", clip="crowd_run", fails=[])
    base = os.path.join(OUT, c["id"])
    groups = [("1080i", 2, 60), ("720p", 2, 120), ("480i", 4, 60)]
    cmd = []
    for i, (fmt, n, frames) in enumerate(groups):
        f = F.FORMATS[fmt]
        a = F.enc_args(fmt, "crowd_run")
        cmd += (a if i == 0 else ["--group"] + a[1:]) + ["--sessions", str(n), "--frames", str(frames), "--stagger",
                                                         "--rate-walk", f"{f['bitrate'] // 3}:{f['bitrate']}:{i + 1}"]
    cmd += ["--output", base, "--write", "4", "--json", base + ".json"]
    r["cmd"] = " ".join(cmd[1:])
    p = run(cmd)
    if p.returncode != 0:
        r["fails"].append("encoder failed: " + (p.stderr or p.stdout)[-200:].replace("\n", " "))
        return r
    nst = 0
    for gi, (fmt, n, frames) in enumerate(groups):
        f = F.FORMATS[fmt]
        for s in range(n):
            m2v = f"{base}.g{gi}.{s}.m2v"
            phase = (s * f["gop"] // n) % f["gop"]
            fl = check_stream(fmt, m2v, frames, plan_types(frames, f["gop"], 3, phase),
                              "tt" if f["interlaced"] else "progressive", m2v + ".yuv")
            r["fails"] += [f"{fmt} session {s}: {x}" for x in fl]
            if os.path.exists(m2v + ".yuv"):
                os.remove(m2v + ".yuv")
            nst += 1
    r["streams"] = nst
    return r


def ffmpeg_ref(c, frames, refcache):
    """ffmpeg mpeg2video at the same rate (default options, +ildct+ilme when interlaced). Cached."""
    fmt, f = c["fmt"], F.FORMATS[c["fmt"]]
    key = f"{c['fmt']}|{c['clip']}|{frames}|{c['bitrate']}|v2"
    if key in refcache:
        return refcache[key]
    src = F.clip_path(c["clip"], fmt)
    out = os.path.join(OUT, c["id"] + ".ref.m2v")
    il = f["interlaced"]
    vbv = F.vbv_for(fmt, c["bitrate"]) if not c["nominal"] else f["vbv"]
    cmd = ["ffmpeg", "-hide_banner", "-nostats", "-v", "error", "-r", "%d/%d" % f["fps"]] + raw_in(fmt, src) + [
        "-frames:v", str(frames), "-vf", "setfield=tff" if il else "null", "-c:v", "mpeg2video", "-threads", "2",
        "-b:v", str(c["bitrate"]), "-minrate", str(c["bitrate"]), "-maxrate", str(c["bitrate"]), "-bufsize", str(vbv),
        "-g", str(f["gop"]), "-bf", "2", "-flags", "+cgop" + ("+ildct+ilme" if il else ""), "-sc_threshold", "1000000000",
        "-y", out]
    p = run(cmd)
    if p.returncode:
        raise RuntimeError("reference encode failed: " + p.stderr[-300:])
    dec = out + ".yuv"
    decode_ffmpeg(out, dec)
    q = compare(fmt, dec, src)
    q["mbps"] = os.path.getsize(out) * 8 / (frames / F.fps(fmt)) / 1e6
    os.remove(dec)
    os.remove(out)
    refcache[key] = q
    return q


def run_case(c, refcache, keep):
    if c.get("feature") == "mixed":
        return run_mixed(c)
    fmt, f = c["fmt"], F.FORMATS[c["fmt"]]
    r = dict(id=c["id"], kind=c["kind"], fmt=fmt, clip=c["clip"], target_mbps=c["bitrate"] / 1e6, fails=[])
    fail = r["fails"].append
    src = F.clip_path(c["clip"], fmt)
    if not os.path.exists(src):
        fail("no source clip")
        return r
    frames = min(F.clip_frames(c["clip"], fmt), 150 if F.fps(fmt) < 40 else 300)
    r["frames"] = frames
    base = os.path.join(OUT, c["id"])
    nsess = c.get("sessions", 1)
    m2v, dec, dec2, recon = base + (".0.m2v" if nsess > 1 else ".m2v"), base + ".dec.yuv", base + ".dec2.yuv", base + ".recon.yuv"
    stats = base + ".jsonl"
    cmd = F.enc_args(fmt, c["clip"], bitrate=None if c["nominal"] else c["bitrate"]) + [
        "--frames", str(frames), "--output", base, "--recon", recon, "--json", base + ".json", "--stats", stats] + c["extra"]
    # M2V_EXTRA="--range 4": extra encoder options for every case, to measure a speed/quality knob
    # against the baseline (group options; the mixed-format case applies them to its last group only)
    cmd += os.environ.get("M2V_EXTRA", "").split()
    r["cmd"] = " ".join(cmd[1:])
    t0 = time.time()
    p = run(cmd)
    r["enc_s"] = round(time.time() - t0, 2)
    if p.returncode != 0 or not os.path.exists(m2v):
        fail("encoder failed: " + (p.stderr or p.stdout)[-200:].replace("\n", " "))
        return r
    try:
        ej = json.load(open(base + ".json"))
        r["enc"] = {k: ej[k] for k in ej if k in ("underflows", "retries_per_picture", "ladder_pictures", "skipped_pictures",
                                                  "i_replaced", "room_min_frac")}
        if ej.get("underflows"):
            fail(f"encoder reports {ej['underflows']} buffer violations")
    except Exception:
        pass
    r["mbps"] = (os.path.getsize(m2v) - 4) * 8 / (frames / F.fps(fmt)) / 1e6
    # decode, strict
    rc, msgs = decode_ffmpeg(m2v, dec)
    r["decode_msgs"] = len(msgs)
    if rc != 0 or msgs:
        fail(f"ffmpeg decode: rc {rc}, {len(msgs)} messages: " + " / ".join(msgs[:2])[:200])
    got = os.path.getsize(dec) // F.frame_bytes(fmt) if os.path.exists(dec) else 0
    if got != frames:
        fail(f"decoded {got} frames, expected {frames}")
        return r
    # structure
    st, types = probe(m2v)
    exp = plan_types(frames, f["gop"], 3, 0, c.get("force", ()))
    if c["kind"] == "torture" or c.get("feature") == "rate":
        # the last step of the buffer ladder replaces an I picture by an all-skipped P picture
        r["i_replaced"] = sum(1 for a_, b_ in zip(types, exp) if b_ == "I" and a_ == "P")
        types = "".join("I" if (b_ == "I" and a_ == "P" and i > 0) else a_ for i, (a_, b_) in enumerate(zip(types, exp)))
    if types != exp:
        fail(f"picture types {types[:32]}... expected {exp[:32]}...")
    want = dict(codec_name="mpeg2video", profile="Main", width=f["size"][0], height=f["size"][1],
                r_frame_rate="%d/%d" % f["fps"], pix_fmt="yuv420p",
                field_order=c.get("field_order", "tt" if f["interlaced"] else "progressive"))
    for k, v in want.items():
        if st.get(k) != v:
            fail(f"stream {k} = {st.get(k)}, expected {v}")
    # second decoder
    n2, err2 = decode_mpeg2dec(fmt, m2v, dec2)
    if n2 != frames:
        fail(f"mpeg2dec decoded {n2} frames, expected {frames}")
    else:
        d2 = compare(fmt, dec2, dec)
        r["dec2_db"] = d2["psnr_avg"]
        if d2["psnr_avg"] < MIN_DEC2_AVG:
            fail(f"mpeg2dec differs from ffmpeg: {d2['psnr_avg']:.1f} dB")
    # buffer model
    try:
        v = vbv_check.check(m2v, schedule=stats if os.path.exists(stats) else None, quiet=True)
        r["vbv"] = {k: v[k] for k in ("ok", "underflows", "overflows", "min_occ", "max_occ", "bit_rate", "vbv_bits",
                                      "largest_picture", "model", "seq_mismatch", "sequence_headers") if k in v}
        op = v.get("op")
        if op:
            r["vbv"]["op"] = {k: op[k] for k in ("underflows", "overflows", "min_margin_s", "max_dec_frac", "rate_changes",
                                                 "rate_min", "rate_max", "rate_above_max")}
        else:
            fail("no rate schedule: operating model not checked")
        if not v["ok"]:
            fail(f"VBV: normative {v['underflows']} underflows, {v['overflows']} overflows; operating "
                 f"{op['underflows'] if op else '-'} underflows, {op['overflows'] if op else '-'} overflows; "
                 f"{v['seq_mismatch']} changed sequence headers")
    except Exception as e:  # noqa: BLE001
        fail(f"vbv_check: {e!r}"[:200])
    # drift
    if os.path.exists(recon) and os.path.getsize(recon) == frames * F.frame_bytes(fmt):
        d = compare(fmt, recon, dec)
        r["drift_avg_db"], r["drift_min_db"] = d["psnr_avg"], d["psnr_min"]
        if d["psnr_avg"] < MIN_DRIFT_AVG or d["psnr_min"] < MIN_DRIFT_MIN:
            fail(f"drift: reconstruction vs decode avg {d['psnr_avg']:.1f} dB, worst picture {d['psnr_min']:.1f} dB")
    else:
        fail("no reconstruction dump")
    # rate
    # the stream may be ahead of the delivery by at most the buffer: bits <= rate * duration + vbv
    dur = frames / F.fps(fmt)
    vbvb = (F.vbv_for(fmt, c["bitrate"]) if not c["nominal"] else f["vbv"]) / 1e6
    feat = c.get("feature")
    if feat == "rate":
        # per GOP: the rate that was set and the rate the GOP used
        rows = [json.loads(l) for l in open(stats)]
        gops = []
        for x in rows:
            if x["key"]:
                gops.append([x["rate"], 0, 0])
            gops[-1][1] += x["bits"]
            gops[-1][2] += 1
        r["gops"] = [[g[0] / 1e6, round(g[1] / g[2] * F.fps(fmt) / 1e6, 3)] for g in gops]
        setmean = sum(g[0] * g[2] for g in gops) / sum(g[2] for g in gops) / 1e6
        r["target_mbps"] = round(setmean, 3)
        # (no bound on the mean here: what may be sent is the integral of the delivery schedule,
        # which the operating-model simulation checks; the GOP figures show how the rate lands)
    elif r["mbps"] > (r["target_mbps"] * dur + vbvb) / dur * 1.002:
        fail(f"rate {r['mbps']:.3f} Mbit/s above target {r['target_mbps']:.3f} by more than the buffer")
    if feat == "cc":
        e = check_cc(m2v, frames, F.fps(fmt))
        r["captions"] = "ok" if not e else e
        if e:
            fail(e)
    if feat == "multi":
        for sx in range(1, nsess):
            phase = (sx * f["gop"] // nsess) % f["gop"]
            fl = check_stream(fmt, f"{base}.{sx}.m2v", frames, plan_types(frames, f["gop"], 3, phase),
                              "tt" if f["interlaced"] else "progressive", f"{base}.{sx}.yuv")
            for x in fl:
                fail(f"session {sx}: {x}")
            if os.path.exists(f"{base}.{sx}.yuv"):
                os.remove(f"{base}.{sx}.yuv")
        r["streams"] = nsess
    # quality and reference
    r.update(compare(fmt, dec, src))
    try:
        r["ref"] = ffmpeg_ref(c, frames, refcache)
    except Exception as e:  # noqa: BLE001
        r["ref_error"] = repr(e)[:200]
    if not keep:
        for x in (dec, dec2, recon):
            if os.path.exists(x):
                os.remove(x)
    return r


def table(results, base):
    lines = []
    hdr = (f"{'case':28} {'Mbit/s':>7} {'PSNR-Y':>7} {'U':>6} {'V':>6} {'SSIM':>6} | {'ref Mb':>6} {'ref Y':>6} {'SSIM':>6} {'dY':>6} {'dSSIM':>7} |"
           f" {'vbv min%':>8} {'op ms':>6} {'drift':>6} {'dec2':>5} | {'dY base':>7} result")
    lines.append(hdr)
    for r in results:
        ref = r.get("ref") or {}
        b = base.get(r["id"]) or {}
        v = r.get("vbv") or {}
        def g(d, k, fmt="%.2f", w=6):
            return (fmt % d[k]).rjust(w) if k in d and d[k] is not None else "-".rjust(w)
        dy = ("%+.2f" % (r["psnr_y"] - ref["psnr_y"])).rjust(6) if "psnr_y" in r and "psnr_y" in ref else "-".rjust(6)
        ds = ("%+.4f" % (r["ssim"] - ref["ssim"])).rjust(7) if "ssim" in r and "ssim" in ref else "-".rjust(7)
        db = ("%+.2f" % (r["psnr_y"] - b["psnr_y"])).rjust(7) if "psnr_y" in r and "psnr_y" in b else "-".rjust(7)
        vmin = ("%.1f" % (100 * v["min_occ"])).rjust(8) if "min_occ" in v else "-".rjust(8)
        vmax = ("%.1f" % (1e3 * v["op"]["min_margin_s"])).rjust(6) if "op" in v else "-".rjust(6)
        res = "ok" if not r["fails"] else "FAIL: " + "; ".join(r["fails"])
        lines.append(f"{r['id']:28} {g(r, 'mbps', '%.3f', 7)} {g(r, 'psnr_y', '%.2f', 7)} {g(r, 'psnr_u')} {g(r, 'psnr_v')} "
                     f"{g(r, 'ssim', '%.4f')} | {g(ref, 'mbps', '%.2f')} {g(ref, 'psnr_y')} {g(ref, 'ssim', '%.4f')} {dy} {ds} | {vmin} {vmax} "
                     f"{g(r, 'drift_avg_db', '%.1f')} {g(r, 'dec2_db', '%.1f', 5)} | {db} {res}")
    return "\n".join(lines)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--only", help="regular expression on case ids")
    ap.add_argument("--quick", action="store_true", help="the subset for use after every change (about a minute)")
    ap.add_argument("--kind", choices=["std", "torture", "feature"], help="run one kind of case only")
    ap.add_argument("--update-baseline", action="store_true")
    ap.add_argument("--jobs", type=int, default=3)
    ap.add_argument("--keep", action="store_true", help="keep decoded and reconstructed pictures in out/check")
    ap.add_argument("--enc-arg", action="append", default=[], help="extra encoder argument for every case")
    ap.add_argument("--out", default="latest", help="name of the result file under $M2V_RESULTS/check")
    a = ap.parse_args()
    os.makedirs(OUT, exist_ok=True)
    os.makedirs(RES, exist_ok=True)
    cs = [c for c in cases() if (not a.only or re.search(a.only, c["id"])) and (not a.kind or c["kind"] == a.kind)
          and (not a.quick or c["id"] in QUICK)]
    if a.quick and a.out == "latest":
        a.out = "quick"
    for c in cs:
        c["extra"] += a.enc_arg
    refpath, basepath = os.path.join(RES, "ref.json"), os.path.join(F.BASELINES, "baseline.json")
    refstored = refpath if os.path.exists(refpath) else os.path.join(F.BASELINES, "ref.json")
    refcache = json.load(open(refstored)) if os.path.exists(refstored) else {}
    base = json.load(open(basepath)) if os.path.exists(basepath) else {}
    t0 = time.time()
    with cf.ThreadPoolExecutor(a.jobs) as ex:
        results = list(ex.map(lambda c: run_case(c, refcache, a.keep), cs))
    json.dump(refcache, open(refpath, "w"), indent=1, sort_keys=True)
    # every coded stream of every case through the conformance verifier
    allow = conform.load_allow()
    infos = []
    for r in results:
        streams = glob.glob(os.path.join(OUT, glob.escape(r["id"]) + ".m2v")) + \
            glob.glob(os.path.join(OUT, glob.escape(r["id"]) + ".*.m2v"))
        fl, info = conform.verify(streams, conform.BROADCAST, allow)
        if not info["streams"] and not r["fails"]:
            fl = ["conformance: no coded stream to verify"]
        r["fails"] += fl
        r["conform"] = info
        infos.append(info)
    # compare with the stored baseline
    for r in results:
        b = base.get(r["id"])
        if not b or "psnr_y" not in r:
            continue
        for k in ("psnr_y", "psnr_u", "psnr_v"):
            if r[k] < b[k] - TOL_PSNR:
                r["fails"].append(f"{k} {r[k]:.2f} below baseline {b[k]:.2f}")
        if r["ssim"] < b["ssim"] - TOL_SSIM:
            r["fails"].append(f"ssim {r['ssim']:.4f} below baseline {b['ssim']:.4f}")
    txt = table(results, base)
    nfail = sum(1 for r in results if r["fails"])
    summary = f"{len(results)} cases, {nfail} failed, {time.time() - t0:.0f} s"
    print(txt)
    nver = sum(i["streams"] for i in infos)
    allowed = {}
    for i in infos:
        for rule, names in i["allowed"].items():
            allowed.setdefault(rule, []).extend(names)
    conf = f"conformance: {nver} streams verified, " + (
        "allowed S3: " + ", ".join(f"{k} in {len(v)}" for k, v in sorted(allowed.items())) if allowed else "no allowed S3 finding")
    print(conf)
    print(summary)
    json.dump(dict(results=results, summary=summary), open(os.path.join(RES, a.out + ".json"), "w"), indent=1)
    open(os.path.join(RES, a.out + ".txt"), "w").write(txt + "\n" + conf + "\n" + summary + "\n")
    if a.update_baseline:
        # accept the current quality: every case that fails nothing but the comparison with the old baseline
        n = 0
        for r in results:
            if "psnr_y" in r and all("below baseline" in f for f in r["fails"]):
                base[r["id"]] = {k: round(r[k], 4) for k in ("psnr_y", "psnr_u", "psnr_v", "ssim", "mbps")}
                n += 1
        json.dump(base, open(basepath, "w"), indent=1, sort_keys=True)
        print(f"baseline updated for {n} cases (those with no failure other than the baseline comparison)")
    return 1 if nfail else 0


if __name__ == "__main__":
    sys.exit(main())
