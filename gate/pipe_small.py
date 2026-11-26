#!/usr/bin/env python3
"""pipe_small.py --pipe TB_PIPE [--work DIR]

The pipeline harness at small scale on the reference adapter, for the sandbox (flake check
`gate/pipe-ref-small`; no GPU): software-decoded inputs made here with ffmpeg go through frame
selection, the host path and the encoder's kernels compiled for the host. Checked per case with
the gate's machinery: the coded stream decodes strictly in ffmpeg and in libmpeg2, the frame
count is what the frame clock says it submitted, the normative buffer check holds, and the
frame-rate conversion counted what it must (29.97 into 29.97: every frame new; 59.94 into
29.97: every second input frame dropped; an input that ends: the slate). Every stream also
goes through the conformance verifier (conform.py; the output is 480i, a broadcast format).

Film mode (`film=auto`): a 23.976 Hz source into 480i and into 720p59.94 must come out as
pictures shown for three and two fields (frame periods) and nothing else, without a cadence
break; a 25 Hz source as an irregular but legal mix; a 29.97 Hz source as progressive pictures
of two fields; an interlaced source as it is. Each stream: the cadence on both decoders and
the displayed fields (film_check.py), the display time against what the frame clock pushed,
the picture log against the stream's flags, both decoders, the buffer check with the removal
times of the flags, the verifier.
"""
import argparse
import json
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import formats as F  # noqa: E402
import check as C  # noqa: E402
import conform  # noqa: E402
import vbv_check  # noqa: E402
import film_check  # noqa: E402


def clip(path, rate, frames, size="352x240", interlaced=False):
    opts = ["-flags", "+ildct+ilme"] if interlaced else []
    src = f"testsrc2=size={size}:rate={rate}" + (",setfield=tff" if interlaced else "")
    subprocess.run(["ffmpeg", "-hide_banner", "-v", "error", "-y", "-f", "lavfi", "-i", src, "-frames:v", str(frames),
                    "-c:v", "mpeg2video", "-b:v", "1M", "-g", "10"] + opts + [path], check=True, stdin=subprocess.DEVNULL)


def film_case(a, cid, ins, ch, secs, fmt, expect):
    """One film-mode case; `expect(j, info)` judges the counters and the cadence."""
    base = os.path.join(a.work, cid)
    w, h = F.FORMATS[fmt]["size"]
    cmd = [a.pipe, "--device", "ref", "--offline", "--secs", str(secs), "--ch", ch, "--output", base, "--write", "1", "--json", base + ".json",
           "--ptslog", base + ".pts"]
    for i in ins:
        cmd += ["--in", i]
    p = subprocess.run(cmd, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    if p.returncode:
        return [f"exit {p.returncode}: {(p.stderr or p.stdout)[-300:]}"]
    fails = []
    j = json.load(open(base + ".json"))
    m2v = base + ".0.m2v"
    dec = base + ".dec.yuv"
    fl, info = film_check.check(m2v, w, h, keep=dec)
    fails += fl
    # the stream shows what the assembler gave out, and that is what the frame clock pushed
    # less what was still queued at the end (two frame periods at most)
    if info["units"] != j["film_units_out"] or not 0 <= j["film_units_in"] - j["film_units_out"] <= 4:
        fails.append(f"display units: stream {info['units']}, assembler out {j['film_units_out']}, in {j['film_units_in']}")
    if [info["by_units"][k] for k in (1, 2, 3)] != j["film_pictures"] or info["pictures"] != j["frames"]:
        fails.append(f"pictures by duration: stream {info['by_units']}, assembler {j['film_pictures']}, submitted {j['frames']}")
    # the picture log (one line per picture, in display order: ... start units first-field break)
    # against the flags of the stream
    cad, _ = film_check.cadence_flags(open(m2v, "rb").read())
    log = [l.split() for l in open(base + ".pts")]
    at = 0
    for k, (line, (u, first, _)) in enumerate(zip(log, cad)):
        if (int(line[7]), int(line[8]), line[9]) != (at, u, first):
            fails.append(f"picture {k}: logged start {line[7]} units {line[8]} first field {line[9]}, the stream has {at} {u} {first}")
            break
        at += u
    if len(log) != len(cad):
        fails.append(f"{len(log)} pictures logged, {len(cad)} in the stream")
    n2, _ = C.decode_mpeg2dec(fmt, m2v, base + ".dec2.yuv")
    if n2 != info["pictures"]:
        fails.append(f"mpeg2dec decoded {n2} frames, the stream has {info['pictures']} pictures")
    elif os.path.exists(dec):
        d2 = C.compare(fmt, base + ".dec2.yuv", dec)
        if d2["psnr_avg"] < C.MIN_DEC2_AVG:
            fails.append(f"mpeg2dec differs from ffmpeg: {d2['psnr_avg']:.1f} dB")
    v = vbv_check.check(m2v, quiet=True)
    if not v["ok"]:
        fails.append(f"VBV: {v['underflows']} underflows")
    if j["violations"]:
        fails.append(f"{j['violations']} buffer violations")
    fails += conform.verify([m2v], conform.BROADCAST)[0]
    if not expect(j, info):
        fails.append("film counters: " + json.dumps({k: j[k] for k in ("frames", "film_pictures", "film_breaks", "film_idle", "psf", "weaves")}))
    for x in (dec, base + ".dec2.yuv"):
        if os.path.exists(x):
            os.remove(x)
    return fails


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--pipe", required=True)
    ap.add_argument("--work", default="pipe-small")
    a = ap.parse_args()
    os.makedirs(a.work, exist_ok=True)
    c30, c60 = os.path.join(a.work, "in30.mkv"), os.path.join(a.work, "in60.mkv")
    clip(c30, "30000/1001", 12)
    clip(c60, "60000/1001", 24)
    cases = [
        # id, inputs, channel, seconds, expectation on the counters
        ("sw-30-480i", [c30 + "@sw"], "fmt=480i,src=0", 0.2, lambda j: j["fresh"] == j["frames"] and j["again"] == 0 and j["drops"] == 0),
        ("sw-60-480i-drops", [c60 + "@sw"], "fmt=480i,src=0", 0.2, lambda j: j["fresh"] == j["frames"] and j["drops"] >= j["frames"] - 2),
        ("slate-480i", [], "fmt=480i,slate=1", 0.12, lambda j: j["slates"] == j["frames"] and j["fresh"] == 0),
    ]
    bad = 0
    for cid, ins, ch, secs, expect in cases:
        base = os.path.join(a.work, cid)
        cmd = [a.pipe, "--device", "ref", "--offline", "--secs", str(secs), "--ch", ch, "--output", base, "--write", "1", "--json", base + ".json",
               "--ptslog", base + ".pts"]
        for i in ins:
            cmd += ["--in", i]
        p = subprocess.run(cmd, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        fails = []
        if p.returncode:
            fails.append(f"exit {p.returncode}: {(p.stderr or p.stdout)[-300:]}")
        else:
            j = json.load(open(base + ".json"))
            m2v = base + ".0.m2v"
            rc, msgs = C.decode_ffmpeg(m2v, base + ".dec.yuv")
            if rc or msgs:
                fails.append(f"ffmpeg decode: rc {rc}, {msgs[:2]}")
            got = os.path.getsize(base + ".dec.yuv") // F.frame_bytes("480i")
            if got != j["frames"]:
                fails.append(f"decoded {got} frames, {j['frames']} submitted")
            n2, _ = C.decode_mpeg2dec("480i", m2v, base + ".dec2.yuv")
            if n2 != got:
                fails.append(f"mpeg2dec decoded {n2} frames")
            v = vbv_check.check(m2v, quiet=True)
            if not v["ok"]:
                fails.append(f"VBV: {v['underflows']} underflows")
            if j["violations"]:
                fails.append(f"{j['violations']} buffer violations")
            fl, _ = conform.verify([m2v], conform.BROADCAST)
            fails += fl
            if not expect(j):
                fails.append("frame clock counters: " + json.dumps({k: j[k] for k in ("frames", "fresh", "again", "drops", "slates")}))
            for x in (base + ".dec.yuv", base + ".dec2.yuv"):
                if os.path.exists(x):
                    os.remove(x)
        print(f"{cid:20} {'ok' if not fails else 'FAIL: ' + '; '.join(fails)}", flush=True)
        bad += bool(fails)
    # ---- film mode
    c24, c25, c30i = os.path.join(a.work, "in24.mkv"), os.path.join(a.work, "in25.mkv"), os.path.join(a.work, "in30i.mkv")
    c24hd = os.path.join(a.work, "in24hd.mkv")
    clip(c24, "24000/1001", 24)
    clip(c25, "25", 24)
    clip(c30i, "30000/1001", 12, size="704x480", interlaced=True)
    clip(c24hd, "24000/1001", 12, size="640x360")
    quiet = lambda j: not any(j["film_breaks"].values())  # noqa: E731
    film = [
        # 23.976 Hz into 480i: three fields, two fields, nothing else, no break
        ("film-24-480i", [c24 + "@sw"], "fmt=480i,src=0,film=auto", 0.6, "480i",
         lambda j, i: quiet(j) and i["by_units"][2] > 4 and abs(i["by_units"][3] - i["by_units"][2]) <= 1 and j["film_idle"] > 3),
        # 25 Hz: 2.4 fields a frame, pictures of two and three in an irregular but legal order
        ("film-25-480i", [c25 + "@sw"], "fmt=480i,src=0,film=auto", 0.5, "480i",
         lambda j, i: i["by_units"][3] > 2 and i["by_units"][2] > i["by_units"][3] and j["film_breaks"]["shift"] > 0),
        # 29.97 Hz progressive: every picture two fields of one instant
        ("film-30-480i", [c30 + "@sw"], "fmt=480i,src=0,film=auto", 0.3, "480i",
         lambda j, i: quiet(j) and i["by_units"][3] == 0 and j["psf"] == j["frames"]),
        # an interlaced source: pictures of two fields from two instants, as without film mode
        ("film-30i-480i", [c30i + "@sw"], "fmt=480i,src=0,film=auto", 0.3, "480i",
         lambda j, i: quiet(j) and i["by_units"][3] == 0 and j["psf"] == 0 and j["film_idle"] == 0),
        # 23.976 Hz into 720p59.94: frames shown three times and twice
        ("film-24-720p", [c24hd + "@sw"], "fmt=720p,src=0,film=auto", 0.35, "720p",
         lambda j, i: quiet(j) and i["by_units"][1] == 0 and i["by_units"][3] >= 3 and i["by_units"][2] >= 3),
    ]
    for cid, ins, ch, secs, fmt, expect in film:
        fails = film_case(a, cid, ins, ch, secs, fmt, expect)
        print(f"{cid:20} {'ok' if not fails else 'FAIL: ' + '; '.join(fails)}", flush=True)
        bad += bool(fails)
    print(f"{len(cases) + len(film)} cases, {bad} failed")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
