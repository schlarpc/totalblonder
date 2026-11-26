#!/usr/bin/env python3
"""The film-mode gate on the card (`nix run .#gate-film`), and its measurements (`--measure`).

Film mode codes one picture per film frame and says how long it is shown (repeat_first_field);
`tools/gate/film_check.py` checks what a decoder makes of that. Two families of cases, every stream
also through the conformance verifier under the default broadcast profile:

  enc    `tb-m2venc --film` on film frames at broadcast sizes (tools/gate/prep_film.sh): 3:2 on 1080i
         and 480i, frames shown three times and twice on 720p59.94, cadences with cuts, video
         pictures and stills in between, bottom field first, captions, holds, pictures given
         up at random and by a real deadline, forced key frames, rate changes. Per stream: the
         cadence on both decoders and the displayed fields; the durations in display order
         against what was asked; libmpeg2 against ffmpeg; both buffer models with the removal
         times of the flags and the rate schedule; the encoder's reconstruction of every
         picture it really coded against the decoder's; the rate against display time.
  pipe   `tb-pipe ... film=auto` (decode on the card, frame clock, assembler, conversion,
         encoder): 23.976, 24.000, 25 and 29.97 Hz and interlaced sources into 1080i, 480i and
         720p; a source that stalls (the slate and back); a film source with captions. Per
         stream as above, and: the display time against what the frame clock pushed; the
         picture log against the stream's flags; every decoded picture against the converted
         frame the encoder was given; for film at 23.976 Hz the converted frames byte for byte
         against those of the same run without film mode (the frames shown whole there).

`--measure`: film mode against today's coding of the pulldown (film=off) on the same inputs,
per format: rate and quality of what is displayed (results/check/film-measure.txt).

Results: $M2V_RESULTS/check/film.txt and film.json. Exit status 0 only when every case passes.
"""
import argparse, glob, json, math, os, re, subprocess, sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import formats as F  # noqa: E402
import check as C  # noqa: E402
import conform  # noqa: E402
import vbv_check  # noqa: E402
import film_check  # noqa: E402

OUT = os.path.join(F.OUT, "filmcheck")
RES = os.path.join(F.RESULTS, "check")
FILM = os.environ.get("M2V_FILM") or os.path.join(F.WORK, "film")
ENC, PIPE = F.M2VENC, F.M2VPIPE


def sh(cmd, **kw):
    return subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, stdin=subprocess.DEVNULL, **kw)


def film_clip(clip, fmt):
    """The film frames of a clip at a format's picture size."""
    if fmt == "1080i":
        return os.path.join(F.MEDIA, f"{clip}_1080p.yuv")
    return os.path.join(FILM, f"{clip}_film{'480' if fmt == '480i' else '720'}.yuv")


def frame_stats(fmt, a, b):
    """Per frame: (mean squared error of the luminance, SSIM is not kept) of raw a against raw b."""
    st = a + ".psnr"
    p = sh(["ffmpeg", "-hide_banner", "-nostats"] + C.raw_in(fmt, a) + C.raw_in(fmt, b) + ["-lavfi", f"psnr=stats_file={st}", "-f", "null", "-"])
    out = []
    if p.returncode == 0:
        for line in open(st):
            out.append(float(re.search(r"mse_y:(\S+)", line).group(1)))
        os.remove(st)
    return out


def db(mse):
    return 99.0 if mse <= 0 else 10 * math.log10(255.0 * 255.0 / mse)


# ---------------------------------------------------------------- the encoder on film frames
def enc_case(cid, fmt, clip, film, extra=(), frames=120, rate=None, paced=False):
    f = F.FORMATS[fmt]
    w, h = f["size"]
    r = dict(id=cid, fails=[])
    fail = r["fails"].append
    src = film_clip(clip, fmt)
    if not os.path.exists(src):
        fail(f"no film clip {src} (tools/gate/prep_film.sh)"); return r
    frames = min(frames, os.path.getsize(src) // F.frame_bytes(fmt))
    base = f"{OUT}/{cid}"
    m2v, dec, dec2, recon, stats = base + ".m2v", base + ".dec.yuv", base + ".dec2.yuv", base + ".recon.yuv", base + ".jsonl"
    br = rate or f["bitrate"]
    cmd = [ENC, "--input", src, "--size", f"{w}x{h}", "--fps", "%d/%d" % f["fps"], "--gop", str(f["gop"]), "--bframes", "2",
           "--bitrate", str(br), "--max-bitrate", str(f["bitrate"]), "--vbv", str(f["vbv"])] + (["--interlaced"] if f["interlaced"] else []) + [
           "--film", film, "--frames", str(frames), "--output", base, "--recon", recon, "--json", base + ".json", "--stats", stats] + list(extra)
    for old in glob.glob(glob.escape(base) + "*.m2v"):
        os.remove(old)
    p = sh(cmd)
    if not os.path.exists(m2v) and os.path.exists(base + ".0.m2v"):
        m2v = base + ".0.m2v"        # several sessions: the first is checked in full
    if p.returncode or not os.path.exists(m2v):
        fail("encoder failed: " + (p.stderr or p.stdout)[-300:].replace("\n", " ")); return r
    ej = json.load(open(base + ".json"))
    if ej.get("underflows"): fail(f"encoder reports {ej['underflows']} buffer violations")
    cyc = [2 if x == "i" else int(x) for x in film.split(",")]
    want = [cyc[k % len(cyc)] for k in range(frames)]
    fl, info = film_check.check(m2v, w, h, units=sum(want), pattern=want, keep=dec)
    r["fails"] += fl
    r["info"] = info
    rows = [json.loads(l) for l in open(stats)]
    rep = {x["disp"] for x in rows if x["repeat"]}
    if [x["units"] for x in sorted(rows, key=lambda x: x["disp"])] != want: fail("the access units' display durations are not what was asked")
    upf = 2 if f["interlaced"] else 1
    dur = sum(want) / upf / F.fps(fmt)
    r["mbps"] = (os.path.getsize(m2v) - 4) * 8 / dur / 1e6
    r["repeats"] = len(rep)
    if os.path.exists(dec) and os.path.getsize(dec) == frames * F.frame_bytes(fmt):
        n2, _ = C.decode_mpeg2dec(fmt, m2v, dec2)
        if n2 != frames: fail(f"mpeg2dec decoded {n2} frames, expected {frames}")
        else:
            d2 = C.compare(fmt, dec2, dec)
            if d2["psnr_avg"] < C.MIN_DEC2_AVG: fail(f"mpeg2dec differs from ffmpeg: {d2['psnr_avg']:.1f} dB")
        # drift on the pictures that were really coded (the reconstruction is session 0's)
        if os.path.exists(recon) and os.path.getsize(recon) == frames * F.frame_bytes(fmt):
            ms = frame_stats(fmt, recon, dec)
            real = [db(ms[d]) for d in range(frames) if d not in rep]
            r["drift_min_db"] = min(real) if real else 0
            if real and min(real) < C.MIN_DRIFT_MIN: fail(f"drift: worst coded picture {min(real):.1f} dB against the decoder")
        elif not rep:
            # (with repeats before the first coded picture of a ring slot the dump is shorter:
            # the drift of such runs is the shed cases' of gate-pipe)
            fail("no reconstruction dump")
        # each picture against the film frame it was coded from
        ms = frame_stats(fmt, dec, src)
        real = [ms[d] for d in range(min(frames, len(ms))) if d not in rep]
        r["psnr_y"] = db(sum(real) / len(real)) if real else 0
        if not paced and not rep and r["psnr_y"] < 22: fail(f"decoded pictures are {r['psnr_y']:.1f} dB from their film frames")
    v = vbv_check.check(m2v, schedule=stats, quiet=True)
    op = v.get("op")
    if not v["ok"] or not op:
        fail(f"VBV: normative {v['underflows']} underflows, {v['overflows']} overflows; operating {op['underflows'] if op else '-'} underflows")
    # the stream may be ahead of the delivery by at most the buffer, in display time
    if not any(x in extra for x in ("--rate-plan", "--rate-now")) and r["mbps"] > (br / 1e6 * dur + f["vbv"] / 1e6) / dur * 1.002:
        fail(f"rate {r['mbps']:.3f} Mbit/s above {br / 1e6:.3f} by more than the buffer")
    b = info["by_units"]
    r["note"] = (f"{info['pictures']} pictures for {info['units']} display units ({b[1]}/{b[2]}/{b[3]} of 1/2/3), {info.get('displayed_frames')} frames displayed; "
                 f"{r['mbps']:.2f} Mbit/s, PSNR-Y {r.get('psnr_y', 0):.2f}" + (f", {len(rep)} repeats" if rep else "") +
                 (f", shed {ej.get('shed', 0)}" if paced else ""))
    r["shed"] = ej.get("shed", 0)
    for x in (dec, dec2, recon):
        if os.path.exists(x): os.remove(x)
    return r


def enc_cases():
    c = []
    for fmt, clips in (("1080i", ["pedestrian_area", "crowd_run", "cuts", "grain"]), ("480i", ["pedestrian_area", "crowd_run", "cuts"]),
                       ("720p", ["pedestrian_area", "park_joy", "cuts"])):
        for clip in clips:
            c.append((f"film-{fmt}-{clip}", fmt, clip, "3,2"))
    il = "3,2,3,2,3,2,2,i,i,i,3,3,2,i,3,2,2,2,3,i"
    c.append(("film-1080i-mixed", "1080i", "cuts", il))
    c.append(("film-480i-mixed", "480i", "cuts", il))
    c.append(("film-720p-mixed", "720p", "cuts", "3,2,3,2,1,1,1,2,2,3,3,3,1,2,3,2"))
    c.append(("film-1080i-stills", "1080i", "fade", "3,3,3,3,2,3,3,3"))
    c.append(("film-480i-bff", "480i", "pedestrian_area", "3,2", ["--bff"]))
    c.append(("film-1080i-cc", "1080i", "pedestrian_area", "3,2,3,2,2,i,3,2", ["--cc"]))
    c.append(("film-480i-cc", "480i", "crowd_run", "3,2", ["--cc"]))
    c.append(("film-720p-cc", "720p", "pedestrian_area", "3,2,3,2,1,2", ["--cc"]))
    c.append(("film-1080i-hold", "1080i", "crowd_run", "3,2", ["--hold", "0:0:31:67"]))
    c.append(("film-720p-hold", "720p", "park_joy", "3,2", ["--hold", "0:0:20:55"]))
    c.append(("film-1080i-shed", "1080i", "cuts", "3,2", ["--force-shed", "0.2:4"]))
    c.append(("film-480i-shed", "480i", "crowd_run", "3,2,2,3,i", ["--force-shed", "0.5:9"]))
    c.append(("film-720p-shed", "720p", "cuts", "3,2", ["--force-shed", "0.3:5", "--overlap"]))
    c.append(("film-1080i-forcekey", "1080i", "pedestrian_area", "3,2", ["--force-key", "5,6,40,77"]))
    c.append(("film-1080i-ratewalk", "1080i", "crowd_run", "3,2", ["--rate-walk", "3000000:15000000:3"], 120, 12000000))
    c.append(("film-480i-ratenow", "480i", "cuts", "3,2,3,2,i", ["--rate-now", "9:4000000,30:1200000,51:3000000,80:1000000"], 120, 2000000))
    c.append(("film-720p-rateplan", "720p", "park_joy", "3,2", ["--rate-plan", "1:3000000,2:12000000,4:5000000"], 120, 9000000))
    c.append(("film-1080i-devin", "1080i", "park_joy", "3,2,2,3", ["--input-mode", "dev", "--overlap"]))
    c.append(("film-1080i-stagger4", "1080i", "pedestrian_area", "3,2", ["--sessions", "4", "--stagger", "--write", "4"]))
    return c


def overload():
    """Film sessions beyond what the card codes in time, paced, with the deadline policy: the
    pictures it gives up are repeats with the display of their frames."""
    r = enc_case("film-1080i-overload", "1080i", "crowd_run", "3,2", ["--sessions", "110", "--stagger", "--write", "3", "--paced", "--deadline",
                                                                     "--input-mode", "dev"], frames=100, paced=True)
    if not r["fails"] and not r.get("shed"):
        r["fails"].append("the deadline policy gave nothing up: no pressure")
    for sx in (1, 2):
        m2v = f"{OUT}/film-1080i-overload.{sx}.m2v"
        if os.path.exists(m2v):
            fl, info = film_check.check(m2v, 1920, 1080, pattern=[3 if k % 2 == 0 else 2 for k in range(100)])
            r["fails"] += [f"session {sx}: {x}" for x in fl]
            v = vbv_check.check(m2v, quiet=True)
            if not v["ok"]: r["fails"].append(f"session {sx}: VBV {v['underflows']} underflows")
    return r


# ---------------------------------------------------------------- the pipeline
def pipe_run(base, src, fmt, chan, secs, extra=()):
    dump = base + ".conv.yuv"
    for x in (dump, base + ".0.m2v"):
        if os.path.exists(x): os.remove(x)
    # offline (deterministic frame selection), except with a stalling input: there the frame
    # clock must run in real time, or it would wait for the frame that does not come
    mode = "--paced" if "--stall" in extra else "--offline"
    p = sh([PIPE, "--in", src, "--ch", f"fmt={fmt},src=0{chan}", mode, "--secs", str(secs), "--dump-conv", dump, "--output", base, "--write", "1",
            "--json", base + ".json", "--ptslog", base + ".pts"] + list(extra))
    if p.returncode or not os.path.exists(base + ".0.m2v"):
        return None, "pipeline failed: " + (p.stderr or p.stdout)[-300:].replace("\n", " ")
    return json.load(open(base + ".json")), None


def pipe_case(cid, src, fmt, secs, expect, chan="", extra=(), same_as_off=False, min_db=27.0):
    f = F.FORMATS[fmt]
    w, h = f["size"]
    fb = F.frame_bytes(fmt)
    r = dict(id=cid, fails=[])
    fail = r["fails"].append
    path = src if os.path.isabs(src) else os.path.join(FILM, "in", src)
    if not os.path.exists(path):
        fail(f"no input {path} (tools/gate/prep_film.sh)"); return r
    base = f"{OUT}/{cid}"
    j, err = pipe_run(base, path, fmt, ",film=auto" + chan, secs, extra)
    if err:
        fail(err); return r
    if not j["all_on_device"]: fail("decode fell back to software")
    if j["violations"]: fail(f"{j['violations']} buffer violations")
    m2v, dec, dump = base + ".0.m2v", base + ".dec.yuv", base + ".conv.yuv"
    fl, info = film_check.check(m2v, w, h, keep=dec)
    r["fails"] += fl
    if info["units"] != j["film_units_out"] or not 0 <= j["film_units_in"] - j["film_units_out"] <= 4:
        fail(f"display units: stream {info['units']}, assembler out {j['film_units_out']}, in {j['film_units_in']}")
    # (a paced run restarts the frame clock's counters after its warm-up; the film counters run on)
    if [info["by_units"][k] for k in (1, 2, 3)] != j["film_pictures"] or (not j["paced"] and info["pictures"] != j["frames"]):
        fail(f"pictures by duration: stream {info['by_units']}, assembler {j['film_pictures']}, submitted {j['frames']}")
    cad, _ = film_check.cadence_flags(open(m2v, "rb").read())
    log = [l.split() for l in open(base + ".pts")]
    at = 0
    for k, (line, (u, first, _)) in enumerate(zip(log, cad)):
        if (int(line[7]), int(line[8]), line[9]) != (at, u, first):
            fail(f"picture {k}: logged start {line[7]} units {line[8]} first field {line[9]}, the stream has {at} {u} {first}"); break
        at += u
    if len(log) != len(cad): fail(f"{len(log)} pictures logged, {len(cad)} in the stream")
    # every decoded picture against the converted frame the encoder was given (the dump has the
    # frames that were converted: pictures that show the one before again have none)
    got = open(dec, "rb").read() if os.path.exists(dec) else b""
    conv = open(dump, "rb").read() if os.path.exists(dump) else b""
    k, worst, n = 0, 99.0, 0
    for i, line in enumerate(log[:len(got) // fb]):
        if line[4] in "NW":
            if (k + 1) * fb > len(conv): fail("fewer converted frames than pictures made from new frames"); break
            # (sampled: every sixth picture is enough to see a wrong frame, and they are large)
            if k % 6 == 0:
                worst = min(worst, film_check.psnr_y(got[i * fb:(i + 1) * fb], conv[k * fb:(k + 1) * fb], w, h)); n += 1
            k += 1
    r["conv_min_db"] = worst
    if n and worst < min_db: fail(f"a decoded picture is {worst:.1f} dB from the frame the encoder was given (need {min_db})")
    if same_as_off:
        # without film mode the same frames go into pictures of two fields; those that show one
        # source frame whole ('N') were made by the same conversion: the same bytes
        jo, err = pipe_run(base + "-off", path, fmt, chan, secs, extra)
        if err: fail("film=off run: " + err)
        else:
            off = {}
            data = open(base + "-off.conv.yuv", "rb").read()
            k = 0
            for line in (l.split() for l in open(base + "-off.pts")):
                if line[4] in "NW":
                    if line[4] == "N" and line[5] == line[6]: off.setdefault(int(line[5]), data[k * fb:(k + 1) * fb])
                    k += 1
            k = same = diff = 0
            for line in log:
                if line[4] in "NW":
                    if line[4] == "N" and int(line[5]) in off:
                        if conv[k * fb:(k + 1) * fb] == off[int(line[5])]: same += 1
                        else: diff += 1
                    k += 1
            r["same_as_off"] = same
            if diff or same < 10: fail(f"converted frames: {same} equal to the run without film mode, {diff} differ")
            r["off"] = dict(pictures=jo["pictures"], mbps=(os.path.getsize(base + "-off.0.m2v") - 4) * 8 / (jo["frames"] / F.fps(fmt)) / 1e6)
            for x in (base + "-off.conv.yuv",):
                if os.path.exists(x): os.remove(x)
    n2, _ = C.decode_mpeg2dec(fmt, m2v, base + ".dec2.yuv")
    if n2 != info["pictures"]: fail(f"mpeg2dec decoded {n2} frames, the stream has {info['pictures']} pictures")
    elif got:
        d2 = C.compare(fmt, base + ".dec2.yuv", dec)
        if d2["psnr_avg"] < C.MIN_DEC2_AVG: fail(f"mpeg2dec differs from ffmpeg: {d2['psnr_avg']:.1f} dB")
    v = vbv_check.check(m2v, quiet=True)
    if not v["ok"]: fail(f"VBV: {v['underflows']} underflows")
    if not expect(j, info): fail("film counters: " + json.dumps({k: j[k] for k in ("frames", "film_pictures", "film_breaks", "film_idle", "film_again", "psf", "weaves", "slates", "cc_in", "cc_out")}))
    upf = 2 if f["interlaced"] else 1
    b = info["by_units"]
    r["mbps"] = (os.path.getsize(m2v) - 4) * 8 / (info["units"] / upf / F.fps(fmt)) / 1e6
    brk = ", ".join(f"{k} {v}" for k, v in j["film_breaks"].items() if v)
    r["note"] = (f"{info['pictures']} pictures for {info['units']} display units ({b[1]}/{b[2]}/{b[3]} of 1/2/3), breaks: {brk or 'none'}; "
                 f"{r['mbps']:.2f} Mbit/s; decoded against converted frames at least {worst:.1f} dB" +
                 (f"; {r.get('same_as_off')} converted frames identical to film=off ({r['off']['pictures']} pictures, {r['off']['mbps']:.2f} Mbit/s there)" if "off" in r else ""))
    for x in (dec, base + ".dec2.yuv", dump):
        if os.path.exists(x): os.remove(x)
    return r


def pipe_cases():
    quiet = lambda j: not any(j["film_breaks"].values())  # noqa: E731
    film32 = lambda j, i: quiet(j) and i["by_units"][1] == 0 and abs(i["by_units"][2] - i["by_units"][3]) <= 1 and i["by_units"][3] > 20  # noqa: E731
    c = []
    for fmt in ("1080i", "480i", "720p"):
        c.append((f"pfilm-2398-{fmt}", "pedestrian_area_2398.mkv", fmt, 5.0, film32, "", (), True))
    c.append(("pfilm-2398-1080i-crowd", "crowd_run_2398.mkv", "1080i", 5.0, film32, "", (), True))
    c.append(("pfilm-2398-720p-cuts", "cuts_2398.mkv", "720p", 5.0, film32, "", (), True))
    c.append(("pfilm-2398-480i-grain-crop", "grain_2398.mkv", "480i", 4.0, film32, ",fit=crop"))
    # 24.000 Hz: 3:2 that slips by one field in 1001 frames (none within these seconds: the same
    # as 23.976); 25 Hz: 2.4 fields a frame, the phase moves every few pictures
    c.append(("pfilm-24-1080i", "pedestrian_area_24.mkv", "1080i", 5.0, lambda j, i: i["by_units"][3] > 20 and i["by_units"][1] == 0))
    for fmt in ("1080i", "720p"):
        c.append((f"pfilm-25-{fmt}", "pedestrian_area_25.mkv", fmt, 5.0,
                  lambda j, i: i["by_units"][3] > 10 and i["by_units"][2] > i["by_units"][3] and j["film_breaks"]["shift"] > 5 and i["by_units"][1] == 0))
    # 29.97 Hz progressive video on a film channel: two fields of one instant, never three;
    # on 720p every frame twice
    c.append(("pfilm-2997-1080i", "pedestrian_area_2997.mkv", "1080i", 4.0, lambda j, i: quiet(j) and i["by_units"][3] == 0 and j["psf"] == j["frames"]))
    c.append(("pfilm-2997-720p", "pedestrian_area_2997.mkv", "720p", 4.0, lambda j, i: quiet(j) and i["by_units"][2] == i["pictures"]))
    # video on a film channel: interlaced and 59.94 Hz sources go through as before
    c.append(("pfilm-1080i-video-1080i", os.path.join(F.MEDIA, "in", "h264_1080i.mkv"), "1080i", 4.0,
              lambda j, i: quiet(j) and i["by_units"][3] == 0 and j["psf"] == 0 and j["film_idle"] == 0, "", (), False, 24.0))
    c.append(("pfilm-720p60-video-720p", os.path.join(F.MEDIA, "in", "h264_720p60.mkv"), "720p", 3.0,
              lambda j, i: quiet(j) and i["by_units"][1] == i["pictures"]))
    c.append(("pfilm-720p60-video-1080i", os.path.join(F.MEDIA, "in", "h264_720p60.mkv"), "1080i", 3.0,
              lambda j, i: i["by_units"][3] == 0 and j["weaves"] > 50, "", (), False, 24.0))
    # the source stalls for a second: the slate (three units a picture, the copies "again"), and back
    for fmt in ("1080i", "720p"):
        c.append((f"pfilm-stall-{fmt}", "pedestrian_area_2398.mkv", fmt, 6.0,
                  lambda j, i: j["film_again"] > 10 and j["film_breaks"]["jump"] >= 2 and j["film_breaks"]["still"] >= 1 and i["by_units"][3] > 30,
                  "", ("--stall", "0:2.0:3.2")))
    # a film source with captions: nothing lost, 20 and 30 constructs a picture
    for fmt in ("1080i", "720p", "480i"):
        c.append((f"pfilm-cc-{fmt}", "cc_2398.ts", fmt, 5.0, lambda j, i: quiet(j) and j["cc_in"] >= 300 and 0 <= j["cc_in"] - j["cc_out"] < 40))
    return c


# ---------------------------------------------------------------- measurements
def displayed_quality(fmt, base, film):
    """Mean squared error of what is displayed against what the encoder was given, per display
    unit: each picture's error weighs as long as it is shown."""
    f = F.FORMATS[fmt]
    w, h = f["size"]
    fb = F.frame_bytes(fmt)
    m2v, dump, dec = base + ".0.m2v", base + ".conv.yuv", base + ".dec.yuv"
    rc, msgs = film_check.decode(m2v, dec, "-fps_mode", "passthrough")
    log = [l.split() for l in open(base + ".pts")]
    n = os.path.getsize(dec) // fb
    # the converted frame behind each picture (a picture that repeats the one before has its frame)
    order, k = [], -1
    for line in log[:n]:
        if line[4] in "NW": k += 1
        order.append(max(k, 0))
    nd = os.path.getsize(dump) // fb
    ref = base + ".ref.yuv"
    with open(dump, "rb") as d, open(ref, "wb") as o:
        for k in order:
            d.seek(min(k, nd - 1) * fb); o.write(d.read(fb))
    ms = frame_stats(fmt, dec, ref)
    units = [int(l[8]) for l in log[:n]] if film else [1] * n
    for x in (dec, ref): os.remove(x)
    shown = sum(units[:len(ms)])
    # (the slate before the input's first frame has no converted frame: left out of the error)
    units = [u if l[4] in "NWA" else 0 for u, l in zip(units, log[:n])]
    tot = sum(units[:len(ms)])
    upf = 2 if (f["interlaced"] and film) else 1
    secs = shown / upf / F.fps(fmt)
    return sum(m * u for m, u in zip(ms, units)) / max(tot, 1), (os.path.getsize(m2v) - 4) * 8 / secs / 1e6, n, secs


def measure():
    rates = {"1080i": [6, 9, 12, 15], "480i": [1.5, 2.5, 4], "720p": [4, 6, 9, 12]}
    clips = ["pedestrian_area", "crowd_run", "park_joy", "grain"]
    lines = ["| Format | Clip | Mbit/s set | off: Mbit/s, PSNR-Y, pictures | film: Mbit/s, PSNR-Y, pictures | film - off, dB |", "|---|---|---|---|---|---|"]
    summary = ["| Format | Clip | PSNR-Y gain at equal rate, dB (mean over the rates) | Rate for the quality of film=off at the middle rate | Saving |", "|---|---|---|---|---|"]
    res = []
    for fmt in ("1080i", "480i", "720p"):
        for clip in clips:
            src = os.path.join(FILM, "in", f"{clip}_2398.mkv")
            pts = {"off": [], "film": []}
            for mb in rates[fmt]:
                row = {}
                for arm in ("off", "film"):
                    base = f"{OUT}/m-{fmt}-{clip}-{arm}"
                    chan = f",bitrate={int(mb * 1e6)}" + (",film=auto" if arm == "film" else "")
                    j, err = pipe_run(base, src, fmt, chan, 6.0)
                    if err:
                        print("measure:", err, flush=True); continue
                    mse, mbps, n, secs = displayed_quality(fmt, base, arm == "film")
                    row[arm] = (mbps, db(mse), n)
                    pts[arm].append((mbps, db(mse)))
                    for x in glob.glob(base + ".*"): os.remove(x)
                if len(row) == 2:
                    o, fm = row["off"], row["film"]
                    lines.append(f"| {fmt} | {clip} | {mb} | {o[0]:.2f}, {o[1]:.2f}, {o[2]} | {fm[0]:.2f}, {fm[1]:.2f}, {fm[2]} | {fm[1] - o[1]:+.2f} |")
                    print(lines[-1], flush=True)
            if len(pts["off"]) >= 3 and len(pts["film"]) == len(pts["off"]):
                gain = sum(b[1] - a[1] for a, b in zip(pts["off"], pts["film"])) / len(pts["off"])
                # the rate at which film mode reaches the quality film=off has at its middle rate:
                # PSNR against log(rate), linear between the measured points
                mid = pts["off"][len(pts["off"]) // 2]
                eq = None
                fp = pts["film"]
                for (r0, q0), (r1, q1) in zip(fp, fp[1:]):
                    if q0 <= mid[1] <= q1 and q1 > q0:
                        eq = math.exp(math.log(r0) + (mid[1] - q0) / (q1 - q0) * (math.log(r1) - math.log(r0)))
                if eq is None and fp[0][1] > mid[1]:
                    eq = -fp[0][0]      # better than that already at the lowest rate measured
                sv = "below %.2f (the lowest measured)" % -eq if eq and eq < 0 else ("%.2f against %.2f" % (eq, mid[0]) if eq else "n/a")
                pc = "more than %.0f %%" % (100 * (1 + eq / mid[0])) if eq and eq < 0 else ("%.0f %%" % (100 * (1 - eq / mid[0])) if eq else "")
                summary.append(f"| {fmt} | {clip} | {gain:+.2f} | {sv} | {pc} |")
                res.append(dict(fmt=fmt, clip=clip, off=pts["off"], film=pts["film"], gain_db=gain, equal_rate=eq, mid=mid))
                print(summary[-1], flush=True)
    os.makedirs(RES, exist_ok=True)
    open(os.path.join(RES, "film-measure.txt"), "w").write("\n".join(summary) + "\n\n" + "\n".join(lines) + "\n")
    json.dump(res, open(os.path.join(RES, "film-measure.json"), "w"), indent=1)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--only")
    ap.add_argument("--measure", action="store_true")
    a = ap.parse_args()
    os.makedirs(OUT, exist_ok=True); os.makedirs(RES, exist_ok=True)
    if a.measure:
        measure(); return 0
    todo = []
    for ec in enc_cases():
        todo.append((ec[0], lambda ec=ec: enc_case(*ec)))
    todo.append(("film-1080i-overload", overload))
    for pc in pipe_cases():
        todo.append((pc[0], lambda pc=pc: pipe_case(*pc)))
    results, infos = [], []
    allow = conform.load_allow()

    def streams():
        return {p: os.stat(p).st_mtime_ns for p in glob.glob(os.path.join(OUT, "*.m2v"))}

    for cid, fn in todo:
        if a.only and not re.search(a.only, cid): continue
        before = streams()
        try:
            r = fn()
        except Exception as e:  # noqa: BLE001
            r = dict(id=cid, fails=[f"exception {e!r}"[:300]])
        written = [p for p, t in streams().items() if before.get(p) != t]
        fl, info = conform.verify(written, conform.BROADCAST, allow)
        if not info["streams"] and not r["fails"]: fl = ["conformance: no coded stream to verify"]
        r["fails"] += fl
        r["conform"] = info
        infos.append(info)
        if info["streams"]:
            r["note"] = (r.get("note", "") + ("; " if r.get("note") else "") + conform.note(info) + " ")
        results.append(r)
        print(f"{r['id']:30} {'ok  ' if not r['fails'] else 'FAIL'} {r.get('note', '')}{' ; '.join(r['fails'])}", flush=True)
    nfail = sum(1 for r in results if r["fails"])
    summary = f"{len(results)} cases, {nfail} failed; conformance: {sum(i['streams'] for i in infos)} streams verified"
    print(summary)
    if not a.only:
        json.dump(dict(results=results, summary=summary), open(os.path.join(RES, "film.json"), "w"), indent=1, default=str)
        with open(os.path.join(RES, "film.txt"), "w") as o:
            for r in results:
                o.write(f"{r['id']:30} {'ok  ' if not r['fails'] else 'FAIL'} {r.get('note', '')}{' ; '.join(r['fails'])}\n")
            o.write(summary + "\n")
    return 1 if nfail else 0


if __name__ == "__main__":
    sys.exit(main())
