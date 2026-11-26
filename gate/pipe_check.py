#!/usr/bin/env python3
"""Gate for the input path, dynamic sessions and the deadline-miss policy (`nix run .#gate-pipe`).

  devin    frames written on the device (`tb-m2venc --input-mode dev`): streams byte-identical to the
           host-memory path, every upload shape (same size, padded rows, padded columns, mixed rates)
  shed     pictures replaced by host-made repeats (random, 10 % and 50 %; a session on hold):
           strict ffmpeg decode without messages, libmpeg2 agrees, frame count, both buffer
           models hold, the encoder's reconstruction of every picture it really coded equals
           the decoder's (no drift after repeats), missed key frames are made up
  churn    sessions opened, closed and reconfigured at random while others run: the undisturbed
           sessions' streams are byte-identical to a run without the churn; every stream of a
           churned session decodes strictly, starts with a sequence header and an I picture,
           holds its buffer model
  pipe     decode -> convert/scale -> encode on the device (tb-pipe --offline): the converted
           frames against the same conversion on the CPU (ffmpeg: zscale Lanczos / interlace
           filter) within a tolerance per case; the coded stream decodes strictly and holds
           its buffer model
  conform  every coded stream a case writes goes through the conformance verifier (conform.py:
           SCTE 43 at 256-QAM, operating delivery model; S1/S2 fail, S3 fails unless
           gate-allow.txt allows it for that stream)

Results: $M2V_RESULTS/check/pipe.txt and pipe.json. Exit status 0 only when every case passes.
"""
import argparse, glob, json, os, re, subprocess, sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import formats as F  # noqa: E402
import check as C  # noqa: E402
import conform  # noqa: E402
import vbv_check  # noqa: E402

ROOT = F.ROOT
OUT = os.path.join(F.OUT, "pipecheck")
RES = os.path.join(F.RESULTS, "check")
ENC = F.M2VENC
PIPE = F.M2VPIPE
IN = os.path.join(F.MEDIA, "in")


def sh(cmd, **kw):
    return subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, **kw)


def group(fmt, clip, n, frames, extra=()):
    a = F.enc_args(fmt, clip)[1:]
    return a + ["--sessions", str(n), "--frames", str(frames), "--stagger"] + list(extra)


# ---------------------------------------------------------------- devin
def devin():
    r = dict(id="devin-identical", fails=[])
    lineups = {
        "480i": group("480i", "crowd_run", 6, 40),
        "1080i": group("1080i", "crowd_run", 4, 30),
        "crop": ["--input", F.clip_path("crowd_run", "480i"), "--size", "696x470", "--fps", "30000/1001", "--interlaced", "--gop", "15", "--bframes", "2",
                 "--bitrate", "4000000", "--vbv", "1835008", "--sessions", "4", "--frames", "30", "--stagger",
                 # 696x470 is in no format table: a test of cropped input, legal H.262 only
                 "--conformance", "h262"],
        "mixed": group("1080i", "crowd_run", 2, 30) + ["--group"] + group("720p", "crowd_run", 2, 60) + ["--group"] + group("480i", "crowd_run", 5, 30),
    }
    n = 0
    for name, args in lineups.items():
        for mode in ("host", "dev"):
            p = sh([ENC] + args + ["--write", "99", "--output", f"{OUT}/devin-{name}-{mode}", "--input-mode", mode] + (["--overlap"] if mode == "host" else []))
            if p.returncode:
                r["fails"].append(f"{name} {mode}: encoder failed {p.stderr[-200:]}")
        for a in sorted(glob.glob(f"{OUT}/devin-{name}-host*.m2v")):
            b = a.replace("-host", "-dev")
            n += 1
            if not os.path.exists(b) or open(a, "rb").read() != open(b, "rb").read():
                r["fails"].append(f"{os.path.basename(b)} differs from the host path")
    r["streams"] = n
    r["note"] = f"{n} streams identical" if not r["fails"] else ""
    return r


# ---------------------------------------------------------------- shed
def frame_psnr(fmt, a, b):
    """PSNR per frame of raw a against raw b."""
    st = a + ".psnr"
    p = sh(["ffmpeg", "-hide_banner", "-nostats"] + C.raw_in(fmt, a) + C.raw_in(fmt, b) + ["-lavfi", f"psnr=stats_file={st}", "-f", "null", "-"])
    out = []
    for line in open(st):
        m = re.search(r"psnr_avg:(\S+)", line)
        out.append(99.0 if m.group(1) == "inf" else float(m.group(1)))
    os.remove(st)
    return out


def shed_case(cid, fmt, clip, extra):
    f = F.FORMATS[fmt]
    r = dict(id=cid, fails=[])
    fail = r["fails"].append
    frames = min(F.clip_frames(clip, fmt), 150 if F.fps(fmt) < 40 else 300)
    base = f"{OUT}/{cid}"
    m2v, dec, dec2, recon, stats = base + ".m2v", base + ".dec.yuv", base + ".dec2.yuv", base + ".recon.yuv", base + ".jsonl"
    p = sh(F.enc_args(fmt, clip) + ["--frames", str(frames), "--output", base, "--recon", recon, "--json", base + ".json", "--stats", stats] + extra)
    if p.returncode or not os.path.exists(m2v):
        fail("encoder failed: " + (p.stderr or p.stdout)[-200:]); return r
    ej = json.load(open(base + ".json"))
    rows = [json.loads(l) for l in open(stats)]
    r["repeats"] = sum(x["repeat"] for x in rows); r["keys_missed"] = sum(x["key_missed"] for x in rows)
    r["off_grid"] = sum(x["off_grid"] for x in rows)
    r["pictures"] = len(rows)
    if ej.get("underflows"): fail(f"encoder reports {ej['underflows']} buffer violations")
    rc, msgs = C.decode_ffmpeg(m2v, dec)
    if rc or msgs: fail(f"ffmpeg decode: rc {rc}, {len(msgs)} messages: " + " / ".join(msgs[:2])[:200])
    got = os.path.getsize(dec) // F.frame_bytes(fmt) if os.path.exists(dec) else 0
    if got != frames:
        fail(f"decoded {got} frames, expected {frames}"); return r
    n2, _ = C.decode_mpeg2dec(fmt, m2v, dec2)
    if n2 != frames: fail(f"mpeg2dec decoded {n2} frames")
    else:
        d2 = C.compare(fmt, dec2, dec); r["dec2_db"] = d2["psnr_avg"]
        if d2["psnr_avg"] < C.MIN_DEC2_AVG: fail(f"mpeg2dec differs from ffmpeg: {d2['psnr_avg']:.1f} dB")
    v = vbv_check.check(m2v, schedule=stats, quiet=True)
    r["vbv_ok"] = v["ok"]
    if not v["ok"]: fail(f"VBV: normative {v['underflows']} underflows; operating {v.get('op', {}).get('underflows')}")
    # drift on the pictures that were really coded
    rep = {x["disp"] for x in rows if x["repeat"]}
    ps = frame_psnr(fmt, recon, dec)
    real = [ps[d] for d in range(frames) if d not in rep]
    r["drift_min_db"] = min(real) if real else 0
    if real and min(real) < C.MIN_DRIFT_MIN: fail(f"drift after repeats: worst coded picture {min(real):.1f} dB against the decoder")
    # every missed key frame is followed by an off-grid key frame; the grid itself is kept
    # (a missed key frame gets one attempt to be made up, at the next frame submitted after the repeat was
    # coded: a key frame off the grid; if that is given up too, the next key frame on the grid follows)
    missed = [x["disp"] for x in rows if x["key_missed"]]
    allkeys = [x["disp"] for x in rows if x["key"]]
    span = f["gop"] + 6
    if missed and max(missed) < frames - span and not any(k > max(missed) and k <= max(missed) + span for k in allkeys):
        fail(f"no key frame within {span} frames of the last missed one")
    keys = [x["disp"] for x in rows if x["key"] and not x["off_grid"]]
    if any(k % f["gop"] for k in keys): fail("a key frame on the grid moved")
    q = C.compare(fmt, dec, F.clip_path(clip, fmt)); r["psnr_y"] = q["psnr_y"]
    r["note"] = f"{r['repeats']} of {r['pictures']} pictures repeated, {r['keys_missed']} key frames missed and {r['off_grid']} made up, drift min {r['drift_min_db']:.1f} dB, PSNR-Y {q['psnr_y']:.2f}"
    for x in (dec, dec2, recon):
        if os.path.exists(x): os.remove(x)
    return r


# ---------------------------------------------------------------- churn
def churn():
    r = dict(id="churn-identical", fails=[])
    fail = r["fails"].append
    stable = group("1080i", "crowd_run", 3, 120) + ["--group"] + group("720p", "fourpeople", 2, 240) + ["--group"] + group("480i", "football", 4, 120)
    pools = (["--group"] + group("1080i", "cuts", 4, 1)[:-1] + ["--pool", "--group"] + group("720p", "crowd_run", 3, 1)[:-1] + ["--pool", "--group"] +
             group("480i", "crowd_run", 6, 1)[:-1] + ["--pool", "--group"] + group("1080p", "pedestrian_area", 2, 1)[:-1] + ["--pool"])
    common = ["--write", "99", "--churn-chunk", "2", "--reap-after", "20"]
    runs = {"ref": ["--churn", "1:0"], "reserve": ["--churn", "5:6:reserve"], "naive": ["--churn", "9:6:naive"], "overlap": ["--churn", "3:8:reserve", "--overlap"],
            "refov": ["--churn", "1:0", "--overlap"], "arena": ["--churn", "7:6:naive", "--arena", "900"]}
    txt = {}
    for name, extra in runs.items():
        p = sh([ENC] + stable + pools + common + extra + ["--output", f"{OUT}/churn-{name}"])
        txt[name] = p.stdout
        if p.returncode: fail(f"{name}: encoder failed {p.stderr[-300:]}")
    n = nch = 0
    for name, ref in (("reserve", "ref"), ("naive", "ref"), ("overlap", "refov"), ("arena", "ref")):
        for gi, cnt in ((0, 3), (1, 2), (2, 4)):
            for s in range(cnt):
                a, b = f"{OUT}/churn-{ref}.g{gi}.{s}.m2v", f"{OUT}/churn-{name}.g{gi}.{s}.m2v"
                n += 1
                if not os.path.exists(b) or open(a, "rb").read() != open(b, "rb").read(): fail(f"{os.path.basename(b)} differs from the run without churn")
        # the churned sessions' own streams
        for m2v in sorted(glob.glob(f"{OUT}/churn-{name}.g[3-6].*.c*.m2v")):
            gi = int(re.search(r"\.g(\d)\.", m2v).group(1))
            fmt = ["1080i", "720p", "480i", "1080p"][gi - 3]
            data = open(m2v, "rb").read()
            if len(data) <= 4: continue   # opened and closed before a frame was submitted
            nch += 1
            if not data.startswith(b"\x00\x00\x01\xb3"): fail(f"{os.path.basename(m2v)} does not start with a sequence header")
            if not data.endswith(b"\x00\x00\x01\xb7"): fail(f"{os.path.basename(m2v)} does not end with sequence_end_code")
            rc, msgs = C.decode_ffmpeg(m2v, m2v + ".yuv")
            if rc or msgs: fail(f"{os.path.basename(m2v)}: ffmpeg decode rc {rc}, {len(msgs)} messages: " + " / ".join(msgs[:1])[:160])
            st, types = C.probe(m2v)
            if not types.startswith("I"): fail(f"{os.path.basename(m2v)} starts with {types[:3]}")
            v = vbv_check.check(m2v, quiet=True)
            if not v["ok"]: fail(f"{os.path.basename(m2v)}: VBV {v['underflows']} underflows")
            if os.path.exists(m2v + ".yuv"): os.remove(m2v + ".yuv")
    ops = " | ".join(l.strip() for l in txt.get("reserve", "").splitlines() if l.startswith("churn:"))
    r["note"] = f"{n} undisturbed streams identical in 4 churn runs, {nch} streams of churned sessions decode strictly; {ops[:150]}"
    r["streams"] = n
    return r


# ---------------------------------------------------------------- pipe
def pipe_case(cid, src, fmt, ref_vf, min_y, min_c, chan="", secs=3.0, in_opt="", dump_w=None, extra=()):
    """ref_vf: ffmpeg filter chain giving the reference frames (yuv420p at the output size)."""
    f = F.FORMATS[fmt]
    r = dict(id=cid, fails=[])
    fail = r["fails"].append
    base = f"{OUT}/{cid}"
    dump, ref = base + ".conv.yuv", base + ".ref.yuv"
    p = sh([PIPE, "--in", f"{IN}/{src}{in_opt}", "--ch", f"fmt={fmt},src=0{chan}", "--offline", "--secs", str(secs), "--dump-conv", dump, "--output", base, "--write", "1",
            "--json", base + ".json", "--ptslog", base + ".pts"] + list(extra))
    if p.returncode or not os.path.exists(dump):
        fail("pipeline failed: " + (p.stderr or p.stdout)[-300:]); return r
    j = json.load(open(base + ".json"))
    if not j["all_on_device"]: fail("decode fell back to software")
    if j["violations"]: fail(f"{j['violations']} buffer violations")
    n = os.path.getsize(dump) // F.frame_bytes(fmt)
    if ref_vf is not None:
        q = sh(["ffmpeg", "-hide_banner", "-nostats", "-v", "error", "-i", f"{IN}/{src}", "-vf", ref_vf, "-fps_mode", "passthrough", "-frames:v", str(n), "-f", "rawvideo", "-pix_fmt", "yuv420p", "-y", ref])
        if q.returncode: fail("reference failed: " + q.stderr[-200:]); return r
        nr = os.path.getsize(ref) // F.frame_bytes(fmt)
        if nr < n - 2: fail(f"reference has {nr} frames, pipeline {n}")
        c = C.compare(fmt, dump, ref)
        r.update(psnr_y=c["psnr_y"], psnr_u=c["psnr_u"], psnr_v=c["psnr_v"], ssim=c["ssim"], psnr_min=c["psnr_min"])
        if c["psnr_y"] < min_y: fail(f"luma {c['psnr_y']:.2f} dB against the CPU reference, need {min_y}")
        if min(c["psnr_u"], c["psnr_v"]) < min_c: fail(f"chroma {min(c['psnr_u'], c['psnr_v']):.2f} dB against the CPU reference, need {min_c}")
    # the coded stream
    m2v = base + ".0.m2v"
    rc, msgs = C.decode_ffmpeg(m2v, base + ".dec.yuv")
    if rc or msgs: fail(f"ffmpeg decode: rc {rc}, {len(msgs)} messages: " + " / ".join(msgs[:2])[:200])
    got = os.path.getsize(base + ".dec.yuv") // F.frame_bytes(fmt)
    if got != j["frames"]: fail(f"decoded {got} frames, {j['frames']} submitted")
    n2, _ = C.decode_mpeg2dec(fmt, m2v, base + ".dec2.yuv")
    if n2 != got: fail(f"mpeg2dec decoded {n2} frames")
    v = vbv_check.check(m2v, quiet=True)
    if not v["ok"]: fail(f"VBV: {v['underflows']} underflows")
    st, _ = C.probe(m2v)
    if st.get("width") != f["size"][0] or st.get("height") != f["size"][1]: fail("stream size")
    r["note"] = (f"{n} converted frames; " + (f"Y {r['psnr_y']:.2f} U {r['psnr_u']:.2f} V {r['psnr_v']:.2f} dB, worst frame {r['psnr_min']:.2f}, SSIM {r['ssim']:.4f} (need Y {min_y}, C {min_c}); " if "psnr_y" in r else "") +
                 f"new {j['fresh']} (PsF {j['psf']}, woven {j['weaves']}), repeated {j['again']}, dropped {j['drops']}")
    r["j"] = {k: j[k] for k in ("frames", "fresh", "psf", "weaves", "again", "drops", "slates")}
    for x in (ref, base + ".dec.yuv", base + ".dec2.yuv", dump):
        if os.path.exists(x): os.remove(x)
    return r


# ---------------------------------------------------------------- geometry against an analytic pattern
def geom_src(w, h, il, tag):
    """A still pattern of sinusoids along both axes in luma and chroma, coded nearly losslessly (H.264)."""
    name = f"{IN}/geom_{w}x{h}{'i' if il else 'p'}_{tag}.mkv"
    if os.path.exists(name) and os.path.getsize(name) > 1000: return name
    lum = "128+60*sin(2*PI*Y/16)+40*sin(2*PI*X/24)"
    cb = "128+50*sin(2*PI*(2*Y+0.5)/32)+40*sin(2*PI*(2*X)/48)"
    cr = "128-50*sin(2*PI*(2*Y+0.5)/32)-40*sin(2*PI*(2*X)/48)"
    # (the color description is set on the frames: as an encoder option it makes ffmpeg convert the matrix)
    vf = f"format=yuv420p,geq=lum='{lum}':cb='{cb}':cr='{cr}',setparams=colorspace={'bt709' if tag == '709' else 'smpte170m'}:range=tv" + (",setfield=tff" if il else "")
    p = sh(["ffmpeg", "-hide_banner", "-v", "error", "-y", "-f", "lavfi", "-i", f"color=gray:s={w}x{h}:r=30000/1001", "-vf", vf, "-frames:v", "24",
            "-c:v", "libx264", "-preset", "veryfast", "-profile:v", "high", "-qp", "2", "-bf", "0"] + (["-flags", "+ildct+ilme"] if il else []) +
           [name])
    if p.returncode: raise RuntimeError("pattern source failed: " + p.stderr[-300:])
    return name


def geom_expect(sw, shh, fmt, out):
    """The same pattern as it must appear at the output size: every output sample at its position in the source."""
    w, h = F.FORMATS[fmt]["size"]
    sx, sy = sw / w, shh / h
    lum = f"128+60*sin(2*PI*((Y+0.5)*{sy}-0.5)/16)+40*sin(2*PI*((X+0.5)*{sx}-0.5)/24)"
    cb = f"128+50*sin(2*PI*((2*Y+1)*{sy}-0.5)/32)+40*sin(2*PI*((2*X+0.5)*{sx}-0.5)/48)"
    cr = f"128-50*sin(2*PI*((2*Y+1)*{sy}-0.5)/32)-40*sin(2*PI*((2*X+0.5)*{sx}-0.5)/48)"
    p = sh(["ffmpeg", "-hide_banner", "-v", "error", "-y", "-f", "lavfi", "-i", f"color=gray:s={w}x{h}:r=30", "-vf", f"format=yuv420p,geq=lum='{lum}':cb='{cb}':cr='{cr}'",
            "-frames:v", "1", "-f", "rawvideo", out])
    if p.returncode: raise RuntimeError("expected pattern failed: " + p.stderr[-300:])


def geom_case(cid, sw, shh, il, tag, fmt, min_y, min_c, chan=",fit=stretch", cpu_vf=None):
    r = dict(id=cid, fails=[])
    fail = r["fails"].append
    src = geom_src(sw, shh, il, tag)
    base = f"{OUT}/{cid}"
    dump, exp = base + ".conv.yuv", base + ".exp.yuv"
    p = sh([PIPE, "--in", src, "--ch", f"fmt={fmt},src=0{chan}", "--offline", "--secs", "0.6", "--dump-conv", dump, "--json", base + ".json"])
    if p.returncode or not os.path.exists(dump):
        fail("pipeline failed: " + (p.stderr or p.stdout)[-300:]); return r
    geom_expect(sw, shh, fmt, exp)
    fb = F.frame_bytes(fmt)
    n = os.path.getsize(dump) // fb
    one = open(exp, "rb").read()
    with open(exp, "wb") as o: o.write(one * n)
    c = C.compare(fmt, dump, exp)
    r.update(psnr_y=c["psnr_y"], psnr_u=c["psnr_u"], psnr_v=c["psnr_v"], psnr_min=c["psnr_min"])
    note = f"{n} frames against the analytic pattern: Y {c['psnr_y']:.2f} U {c['psnr_u']:.2f} V {c['psnr_v']:.2f} dB, worst frame {c['psnr_min']:.2f} (need Y {min_y}, C {min_c})"
    if c["psnr_y"] < min_y: fail(f"luma {c['psnr_y']:.2f} dB")
    if min(c["psnr_u"], c["psnr_v"]) < min_c: fail(f"chroma {min(c['psnr_u'], c['psnr_v']):.2f} dB")
    if cpu_vf:
        # what a CPU filter chain gives on the same source, against the same pattern
        w, h = F.FORMATS[fmt]["size"]
        q = sh(["ffmpeg", "-hide_banner", "-v", "error", "-y", "-i", src, "-vf", cpu_vf, "-frames:v", str(n), "-f", "rawvideo", "-pix_fmt", "yuv420p", base + ".cpu.yuv"])
        if not q.returncode and os.path.getsize(base + ".cpu.yuv") >= n * fb:
            k = C.compare(fmt, base + ".cpu.yuv", exp)
            note += f"; ffmpeg `{cpu_vf.split(',')[0][:40]}` on the CPU: Y {k['psnr_y']:.2f} U {k['psnr_u']:.2f} V {k['psnr_v']:.2f} dB"
            r["cpu"] = dict(psnr_y=k["psnr_y"], psnr_u=k["psnr_u"], psnr_v=k["psnr_v"])
        if os.path.exists(base + ".cpu.yuv"): os.remove(base + ".cpu.yuv")
    r["note"] = note
    for x in (dump, exp):
        if os.path.exists(x): os.remove(x)
    return r


def geom_cases():
    Z = "zscale=%s:filter=lanczos:dither=none,format=yuv420p"
    return [
        ("geom-1080p-720p", 1920, 1080, False, "709", "720p", 48, 48, ",fit=stretch", Z % "1280:720"),
        ("geom-1080p-480i-psf", 1920, 1080, False, "601", "480i", 48, 48, ",fit=stretch", Z % "704:480"),
        ("geom-720p-1080p", 1280, 720, False, "709", "1080p", 46, 46, ",fit=stretch", Z % "1920:1080"),
        # ratios of 3 and more: the source is halved first (k_half), then resampled
        ("geom-2160p-720p-halved", 3840, 2160, False, "709", "720p", 44, 44, ",fit=stretch", Z % "1280:720"),
        ("geom-2160p-480i-halved", 3840, 2160, False, "601", "480i", 44, 44, ",fit=stretch", Z % "704:480"),
        ("geom-1080i-480i-fields", 1920, 1080, True, "601", "480i", 46, 46, ",fit=stretch", "scale=704:480:interl=1:flags=lanczos"),
        ("geom-480i-1080i-fields", 704, 480, True, "709", "1080i", 44, 44, ",fit=stretch", "scale=1920:1080:interl=1:flags=lanczos"),
        ("geom-1080i-720p-bob", 1920, 1080, True, "709", "720p", 46, 46, ",fit=stretch", "bwdif=mode=send_field,scale=1280:720:flags=lanczos"),
        ("geom-1080i-1080p-bob", 1920, 1080, True, "709", "1080p", 46, 46, ",fit=stretch", "yadif=mode=send_frame"),
    ]


def pipe_cases():
    Z = "zscale=%s:filter=lanczos:dither=none"
    c = []
    # same size: the bytes of the decoder, split into planes
    c.append(("pipe-1080p30-1080i-psf", "h264_1080p30.mkv", "1080i", "format=yuv420p", 99, 99))
    c.append(("pipe-720p60-720p", "h264_720p60.mkv", "720p", "format=yuv420p", 99, 99))
    c.append(("pipe-1080i-1080i-fields", "h264_1080i.mkv", "1080i", "format=yuv420p", 99, 99))
    c.append(("pipe-hevc10-1080p-round", "hevc_1080p30_10.mkv", "1080p", Z % "1920:1080" + ",format=yuv420p", 55, 55))
    # size changes against zimg (Lanczos-3, same siting rules)
    c.append(("pipe-2160p10-1080p", "hevc_2160p24_10.mkv", "1080p", Z % "1920:1080" + ",format=yuv420p", 45, 45))
    c.append(("pipe-2160p-720p", "hevc_2160p30.mkv", "720p", Z % "1280:720" + ",format=yuv420p", 45, 45, "", 2.0))
    c.append(("pipe-1080p-720p", "h264_1080p60.mkv", "720p", Z % "1280:720" + ",format=yuv420p", 45, 45))
    c.append(("pipe-720p-1080p", "h264_720p60.mkv", "1080p", "select='mod(n,2)'," + Z % "1920:1080" + ",format=yuv420p", 45, 45))
    c.append(("pipe-1366x768-720p", "h264_1366x768p30.mkv", "720p", Z % "1280:720" + ",format=yuv420p", 42, 42, ",fit=stretch"))
    # 16:9 HD into 4:3 SD: letterbox and crop, with the color matrix (BT.709 -> BT.601)
    c.append(("pipe-1080p-480i-letterbox", "h264_1080p30.mkv", "480i", "zscale=704:360:filter=lanczos:dither=none:matrixin=709:matrix=170m,format=yuv420p,pad=704:480:0:60:black", 40, 40))
    c.append(("pipe-1080p-480i-crop", "h264_1080p30.mkv", "480i", "crop=1440:1080:240:0,zscale=704:480:filter=lanczos:dither=none:matrixin=709:matrix=170m,format=yuv420p", 40, 40, ",fit=crop"))
    c.append(("pipe-854x480-1080i", "h264_854x480p30.mkv", "1080i", "zscale=1920:1080:filter=lanczos:dither=none:matrixin=170m:matrix=709,format=yuv420p", 40, 40, ",fit=stretch"))
    # interlaced output from 59.94p: fields from successive frames, [1 2 1]/4 across lines
    c.append(("pipe-1080p60-1080i-weave", "h264_1080p60.mkv", "1080i", "format=yuv420p,interlace=scan=tff:lowpass=linear", 42, 42))
    c.append(("pipe-720p60-1080i-weave", "h264_720p60.mkv", "1080i", Z % "1920:1080" + ",format=yuv420p,interlace=scan=tff:lowpass=linear", 40, 40))
    c.append(("pipe-720p60-480i-weave", "h264_720p60.mkv", "480i", "zscale=704:360:filter=lanczos:dither=none:matrixin=709:matrix=170m,format=yuv420p,interlace=scan=tff:lowpass=linear,pad=704:480:0:60:black", 38, 38))
    # interlaced sources: field by field to another size; bob to progressive
    # (ffmpeg's zscale filter scales an interlaced frame as if it were progressive, so these have no CPU reference here; the geom-* cases check the geometry)
    c.append(("pipe-1080i-480i-fields", "h264_1080i.mkv", "480i", None, 0, 0, ",fit=stretch"))
    c.append(("pipe-480i-1080i-fields", "h264_480i.mkv", "1080i", None, 0, 0, ",fit=stretch"))
    c.append(("pipe-1080i-720p-bob", "h264_1080i.mkv", "720p", None, 0, 0))
    # (MPEG-2's inverse transform is not bit-exact between decoders: NVDEC against libavcodec's is about 61 dB)
    c.append(("pipe-mpeg2-480i-480i", "mpeg2_480i.ts", "480i", "format=yuv420p", 50, 50))
    # frame-rate conversion and composition: structure only
    c.append(("pipe-1080p24-1080i-pulldown", "h264_1080p24.mkv", "1080i", None, 0, 0))
    c.append(("pipe-1080p25-720p", "h264_1080p25.mkv", "720p", None, 0, 0))
    c.append(("pipe-720p50-1080i", "h264_720p50.mkv", "1080i", None, 0, 0))
    c.append(("pipe-av1-1080i", "av1_1080p30.mkv", "1080i", "format=yuv420p", 99, 99))
    c.append(("pipe-vp9-720p", "vp9_1080p30.mkv", "720p", None, 0, 0))
    c.append(("pipe-mosaic-2x2-overlay", "h264_720p60.mkv", "1080i", None, 0, 0, ",mosaic=2x2,ov=1"))
    c.append(("pipe-slate", "h264_720p60.mkv", "720p", None, 0, 0, ",slate=1"))
    return c


def quality():
    """Filters against zimg's Lanczos-3 on the CPU ($M2V_RESULTS/pipe/quality.txt)."""
    Z = "zscale=%s:filter=lanczos:dither=none"
    rows = [("2160p 10-bit -> 1080p", "hevc_2160p24_10.mkv", "1080p", Z % "1920:1080" + ",format=yuv420p", ""),
            ("2160p -> 720p (source halved first)", "hevc_2160p30.mkv", "720p", Z % "1280:720" + ",format=yuv420p", ""),
            ("2160p -> 720p, `--exact-down`", "hevc_2160p30.mkv", "720p", Z % "1280:720" + ",format=yuv420p", "", ("--exact-down",)),
            ("2160p -> 704x360 in 480i (source halved first)", "hevc_2160p30.mkv", "480i", "zscale=704:360:filter=lanczos:dither=none:matrixin=709:matrix=170m,format=yuv420p,pad=704:480:0:60:black", ""),
            ("2160p -> 704x360 in 480i, `--exact-down`", "hevc_2160p30.mkv", "480i", "zscale=704:360:filter=lanczos:dither=none:matrixin=709:matrix=170m,format=yuv420p,pad=704:480:0:60:black", "", ("--exact-down",)),
            ("1080p -> 720p", "h264_1080p60.mkv", "720p", Z % "1280:720" + ",format=yuv420p", ""),
            ("1080p -> 704x360 in 480i (BT.709 -> 601)", "h264_1080p30.mkv", "480i", "zscale=704:360:filter=lanczos:dither=none:matrixin=709:matrix=170m,format=yuv420p,pad=704:480:0:60:black", ""),
            ("720p -> 1080p", "h264_720p60.mkv", "1080p", "select='mod(n,2)'," + Z % "1920:1080" + ",format=yuv420p", ""),
            ("854x480 -> 1080i (BT.601 -> 709)", "h264_854x480p30.mkv", "1080i", "zscale=1920:1080:filter=lanczos:dither=none:matrixin=170m:matrix=709,format=yuv420p", ",fit=stretch"),
            ("1080p60 -> 1080i, fields woven", "h264_1080p60.mkv", "1080i", "format=yuv420p,interlace=scan=tff:lowpass=linear", "")]
    out = ["| Conversion | Filter | PSNR Y / U / V against the CPU reference, dB | worst frame, dB | SSIM |", "|---|---|---|---|---|"]
    for row in rows:
        name, src, fmt, vf, chan = row[:5]
        for flt in ("lanczos", "bicubic", "bilinear"):
            r = pipe_case("q-" + flt, src, fmt, vf, 0, 0, chan + ",filter=" + flt, 1.5, extra=row[5] if len(row) > 5 else ())
            out.append(f"| {name} | {flt} | {r.get('psnr_y', 0):.2f} / {r.get('psnr_u', 0):.2f} / {r.get('psnr_v', 0):.2f} | {r.get('psnr_min', 0):.2f} | {r.get('ssim', 0):.4f} |" + (" " + "; ".join(r["fails"]) if r["fails"] else ""))
            print(out[-1], flush=True)
    os.makedirs(os.path.join(F.RESULTS, "pipe"), exist_ok=True)
    open(os.path.join(F.RESULTS, "pipe", "quality.txt"), "w").write("\n".join(out) + "\n")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--quality", action="store_true")
    ap.add_argument("--only")
    ap.add_argument("--keep", action="store_true")
    a = ap.parse_args()
    os.makedirs(OUT, exist_ok=True); os.makedirs(RES, exist_ok=True)
    if a.quality:
        quality(); return 0
    todo = []
    todo.append(("devin-identical", devin))
    for fmt, clip in (("1080i", "crowd_run"), ("720p", "cuts"), ("480i", "football")):
        todo.append((f"shed10-{fmt}-{clip}", lambda fmt=fmt, clip=clip: shed_case(f"shed10-{fmt}-{clip}", fmt, clip, ["--force-shed", "0.1:1"])))
        todo.append((f"shed50-{fmt}-{clip}", lambda fmt=fmt, clip=clip: shed_case(f"shed50-{fmt}-{clip}", fmt, clip, ["--force-shed", "0.5:2"])))
    todo.append(("shed90-1080i-park_joy", lambda: shed_case("shed90-1080i-park_joy", "1080i", "park_joy", ["--force-shed", "0.9:3"])))
    todo.append(("hold-720p-crowd_run", lambda: shed_case("hold-720p-crowd_run", "720p", "crowd_run", ["--hold", "0:0:50:170"])))
    todo.append(("hold-480i-mobcal", lambda: shed_case("hold-480i-mobcal", "480i", "mobcal", ["--hold", "0:0:13:47"])))
    todo.append(("churn-identical", churn))
    for gc in geom_cases():
        todo.append((gc[0], lambda gc=gc: geom_case(*gc)))
    for pc in pipe_cases():
        todo.append((pc[0], lambda pc=pc: pipe_case(pc[0], pc[1], pc[2], pc[3], pc[4], pc[5], *(pc[6:]))))
    results = []
    allow = conform.load_allow()
    infos = []

    def streams():
        return {p: os.stat(p).st_mtime_ns for p in glob.glob(os.path.join(OUT, "*.m2v"))}

    for cid, fn in todo:
        if a.only and not re.search(a.only, cid): continue
        before = streams()
        try:
            r = fn()
        except Exception as e:  # noqa: BLE001
            r = dict(id=cid, fails=[f"exception {e!r}"[:300]])
        # the streams this case wrote, whatever their names
        written = [p for p, t in streams().items() if before.get(p) != t]
        # the streams of a deliberately non-broadcast size are held to H.262 alone
        small = [p for p in written if conform.test_size(p)]
        fl, info = conform.verify([p for p in written if p not in small], conform.BROADCAST, allow)
        if small:
            fl2, info2 = conform.verify(small, conform.TEST_SIZE, allow)
            fl += fl2
            info["streams"] += info2["streams"]
        r["fails"] += fl
        r["conform"] = info
        infos.append(info)
        if info["streams"]:
            r["note"] = (r.get("note", "") + ("; " if r.get("note") else "") + conform.note(info) + " ")
        results.append(r)
        print(f"{r['id']:30} {'ok  ' if not r['fails'] else 'FAIL'} {r.get('note', '')}{' ; '.join(r['fails'])}", flush=True)
    nfail = sum(1 for r in results if r["fails"])
    nver = sum(i["streams"] for i in infos)
    summary = f"{len(results)} cases, {nfail} failed; conformance: {nver} streams verified"
    if not a.only:
        for rule, globs, comment in conform.unused(allow, infos):
            print(f"note: gate-allow.txt entry {rule} {' '.join(globs)} matched no finding of this run ({comment})")
    print(summary)
    if not a.only:
        json.dump(dict(results=results, summary=summary), open(os.path.join(RES, "pipe.json"), "w"), indent=1)
        with open(os.path.join(RES, "pipe.txt"), "w") as o:
            for r in results:
                o.write(f"{r['id']:30} {'ok  ' if not r['fails'] else 'FAIL'} {r.get('note', '')}{' ; '.join(r['fails'])}\n")
            o.write(summary + "\n")
    return 1 if nfail else 0


if __name__ == "__main__":
    sys.exit(main())
