#!/usr/bin/env python3
"""Streams from encoders that share nothing with ours, through the verifier.

Usage: python3 -I independent.py --verify BIN --out DIR [--ffmpeg BIN] [--mpeg2enc BIN]
                                 [--mencoder BIN] [--x262 BIN] [--list]

Each case names an encoder command and the rules that encoder is known to break in that
configuration (ALLOWED). The run fails if a stream cannot be produced, if the verifier reports
a rule outside the case's list, or if a stream is empty. The lists are findings in
themselves: notes/m2v-conformance-findings.md discusses each. Their second use is to show
that the verifier's tables and decoding process agree with other people's encoders: any
error in a table of Annex B shows at once as BLK-COEF-VLC or the like.

The repeat_first_field family (film mode) has its own groups: soft-telecined streams of
mpeg2enc and mencoder, other encoders' streams with their display flags rewritten by
esflags.py (FLAGGED), and one deliberate violation each (MUTATED), which the verifier must
report by the named rule.
"""
import argparse
import json
import os
import subprocess
import sys

# the script runs isolated (-I): its own directory is named explicitly
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import esflags  # noqa: E402

# ffmpeg's mpeg2video writes no sequence_end_code into a raw elementary stream.
FF_ALWAYS = {"SEQ-END-MISSING"}
# Without -maxrate ffmpeg writes bit_rate_value 0x3FFFF (104.8 Mbit/s), above every level, and
# a vbv_buffer_size from its default bit rate that its own pictures exceed.
FF_NO_RC = {"PL-BIT-RATE", "VBV-PICTURE-SIZE", "VBV-UNDERFLOW"}

# mpeg2enc and mencoder count the 24 coded frames of each second of film in the GOP time code
# of a 29.97 Hz sequence (time_code_pictures 0 to 23): neither the coded frames nor the
# display time at the sequence's frame rate.
MJ_FILM_TC = {"GOP-TIMECODE-CONTINUITY"}
# What ffmpeg's and mpeg2enc's streams lack under a broadcast profile (not the subject here).
FF_BROADCAST = {"BC-FORMAT", "BC-BIT-RATE", "BC-VBV-SIZE", "BC-SEQEXT-FIELDS", "BC-LEVEL-LOWEST",
                "BC-COLOR-EXPLICIT", "BC-VIDEO-FORMAT", "BC-COLOR-PREFERRED", "BC-VBV-DELAY", "BC-SEQ-INTERVAL",
                "BC-BUFFER-DELAY", "BC-PROFILE", "SDE-MISSING", "CC-STATE"}

SRC = "testsrc2=size={size}:rate={rate}"
RC = ["-b:v", "2M", "-maxrate", "4M", "-bufsize", "1M"]

FFMPEG = [
    # name, size, rate, frames, extra options, allowed rules
    ("ff-ipp", "352x288", "30000/1001", 40, RC, set()),
    ("ff-b2-open", "352x288", "30000/1001", 40, RC + ["-bf", "2", "-g", "12"], set()),
    ("ff-b2-closed", "352x288", "30000/1001", 40,
     RC + ["-bf", "2", "-g", "12", "-flags", "+cgop", "-sc_threshold", "1000000000"], set()),
    ("ff-interlaced", "704x480", "30000/1001", 24,
     ["-b:v", "4M", "-maxrate", "8M", "-bufsize", "1835k", "-bf", "2", "-flags", "+ildct+ilme"], set()),
    ("ff-interlaced-bff", "704x480", "30000/1001", 24,
     ["-b:v", "4M", "-maxrate", "8M", "-bufsize", "1835k", "-bf", "1", "-flags", "+ildct+ilme"], set()),
    ("ff-altscan-ivlc-nonlinear", "352x288", "30000/1001", 40,
     RC + ["-bf", "2", "-alternate_scan", "1", "-intra_vlc", "1", "-non_linear_quant", "1", "-qmax", "28"], set()),
    ("ff-dc9", "352x288", "30000/1001", 30, RC + ["-dc", "9", "-bf", "1"], set()),
    ("ff-dc10", "352x288", "30000/1001", 30, RC + ["-dc", "10"], set()),
    ("ff-dc11", "352x288", "30000/1001", 30, RC + ["-dc", "11"], {"PL-INTRA-DC"}),
    ("ff-cbr", "352x288", "30000/1001", 60,
     ["-b:v", "1M", "-minrate", "1M", "-maxrate", "1M", "-bufsize", "500k", "-bf", "2"], set()),
    ("ff-q1", "352x288", "30000/1001", 20,
     ["-q:v", "1", "-maxrate", "15M", "-bufsize", "1835k", "-bf", "2"], set()),
    ("ff-q31", "352x288", "30000/1001", 30,
     ["-q:v", "31", "-maxrate", "15M", "-bufsize", "1835k", "-bf", "2"], set()),
    ("ff-qscale-no-rc", "352x288", "30000/1001", 20, ["-q:v", "2", "-bf", "2"], FF_NO_RC),
    ("ff-422", "352x288", "30000/1001", 20, RC + ["-pix_fmt", "yuv422p", "-bf", "2"], {"PL-PROFILE"}),
    ("ff-intra-only", "352x288", "30000/1001", 20, ["-b:v", "4M", "-maxrate", "8M", "-bufsize", "1835k", "-g", "1"], set()),
    ("ff-mbd-rd", "352x288", "30000/1001", 30, RC + ["-bf", "2", "-mbd", "rd", "-trellis", "2", "-cmp", "2", "-subcmp", "2"], set()),
    ("ff-slices", "352x288", "30000/1001", 30, RC + ["-bf", "2", "-ps", "300"], set()),
    ("ff-24p", "352x288", "24000/1001", 30, RC + ["-bf", "2"], set()),
    ("ff-25p", "352x288", "25", 30, RC + ["-bf", "2"], set()),
    ("ff-60p-sd", "704x480", "60000/1001", 20, ["-b:v", "6M", "-maxrate", "12M", "-bufsize", "1835k", "-bf", "2"],
     {"PL-FRAME-RATE", "PL-SAMPLE-RATE"}),
    ("ff-720p60", "1280x720", "60000/1001", 12, ["-b:v", "12M", "-maxrate", "20M", "-bufsize", "7M", "-bf", "2"],
     {"PL-SAMPLE-RATE"}),
    ("ff-1080i", "1920x1080", "30000/1001", 8,
     ["-b:v", "15M", "-maxrate", "30M", "-bufsize", "7M", "-bf", "2", "-flags", "+ildct+ilme"], set()),
    ("ff-drop-frame", "352x288", "30000/1001", 40,
     RC + ["-bf", "2", "-g", "12", "-drop_frame_timecode", "1", "-gop_timecode", "00:00:59;20"], set()),
    ("ff-seq-disp", "704x480", "30000/1001", 12,
     ["-b:v", "4M", "-maxrate", "8M", "-bufsize", "1835k", "-seq_disp_ext", "always", "-video_format", "ntsc",
      "-color_primaries", "smpte170m", "-color_trc", "smpte170m", "-colorspace", "smpte170m"], set()),
    ("ff-low-delay", "352x288", "30000/1001", 30, RC + ["-bf", "0", "-flags", "+low_delay"], set()),
    ("ff-wide-motion", "704x480", "30000/1001", 20,
     ["-b:v", "4M", "-maxrate", "8M", "-bufsize", "1835k", "-bf", "2", "-me_range", "1023", "-me_method", "epzs"], set()),
]

# field order of the interlaced ffmpeg cases (set on the source frames)
FIELD_ORDER = {"ff-interlaced": "tff", "ff-interlaced-bff": "bff", "ff-1080i": "tff"}

MPEG2ENC = [
    # name, source (size, rate, interlaced), options, allowed rules
    ("mj-progressive", ("352x288", "30000/1001", False, 40), ["-b", "2000", "-I", "0"], set()),
    ("mj-interlaced-frames", ("704x480", "30000/1001", True, 24), ["-b", "4000", "-I", "1", "-g", "6", "-G", "6", "-R", "2"], set()),
    ("mj-kvcd-dc10", ("352x288", "30000/1001", False, 40), ["-b", "1500", "-K", "kvcd", "-D", "10", "-s"], set()),
    ("mj-hires-cbr", ("352x288", "30000/1001", False, 40), ["-b", "3000", "--cbr", "-K", "hi-res", "-D", "9", "-c", "-R", "1"], set()),
    ("mj-no-b", ("352x288", "30000/1001", False, 40), ["-b", "2000", "-R", "0", "-g", "6", "-G", "6"], set()),
    # 3:2 pulldown flags on frames it marks as interlaced: repeat_first_field with
    # progressive_frame 0 (6.3.10)
    ("mj-pulldown", ("704x480", "24000/1001", False, 40), ["-b", "2500", "-p", "-n", "n", "-F", "4", "-I", "1"],
     {"PCE-RFF-INTERLACED"} | MJ_FILM_TC),
    # the same on frames it marks progressive: a correct soft telecine (T3 B2 B3 T2)
    ("mj-pulldown-progressive", ("704x480", "24000/1001", False, 40), ["-b", "2500", "-p", "-n", "n", "-F", "4", "-I", "0"],
     MJ_FILM_TC),
    ("mj-pulldown-b", ("704x480", "24000/1001", False, 48),
     ["-b", "2500", "-p", "-n", "n", "-F", "4", "-I", "0", "-R", "2", "-g", "12", "-G", "12"], MJ_FILM_TC),
    # constant bit rate (mpeg2enc writes vbv_delay 0xFFFF all the same: C.3.2)
    ("mj-pulldown-cbr", ("704x480", "24000/1001", False, 60),
     ["-b", "3000", "--cbr", "-p", "-n", "n", "-F", "4", "-I", "0", "-R", "2", "-g", "12", "-G", "12"], MJ_FILM_TC),
    ("mj-pulldown-cbr-nob", ("704x480", "24000/1001", False, 60),
     ["-b", "3000", "--cbr", "-p", "-n", "n", "-F", "4", "-I", "0", "-R", "0", "-g", "12", "-G", "12"], MJ_FILM_TC),
    # a noisy source at a rate that binds: the buffer holds only with the removal times of
    # C.11 for three-field pictures (the mutation "all-two" underflows it)
    ("mj-pulldown-tight", ("704x480", "24000/1001", False, 96, ",noise=alls=20:allf=t"),
     ["-b", "1500", "--cbr", "-V", "112", "-p", "-n", "n", "-F", "4", "-I", "0", "-R", "2", "-g", "12", "-G", "12"],
     MJ_FILM_TC),
    # its "ATSC 1080i" format with progressive frames writes progressive_sequence 1 at 29.97 Hz
    # and keeps the 3:2 flags: repeat_first_field where Table 8-7 has none, and top_field_first
    # 1 without it
    ("mj-pulldown-1080", ("1920x1080", "24000/1001", False, 16),
     ["-f", "13", "-b", "15000", "-p", "-n", "n", "-F", "4", "-I", "0", "-R", "2", "-g", "12", "-G", "12"],
     {"PL-RFF", "PCE-TFF-PROGRESSIVE", "SDE-DISPLAY-SIZE"} | MJ_FILM_TC),
]

# mplayer's mencoder: libavcodec's mpeg2video at 23.976 Hz, the 3:2 flags written by
# mencoder's own MPEG muxer (-mpegopts telecine), the elementary stream taken out again with
# ffmpeg. With B-pictures the muxer applies the pattern in coded order, so the fields of the
# display order do not alternate: a real stream that breaks 7.12.
MENCODER = [
    # name, size, frames, lavcopts, allowed rules
    ("men-telecine", "704x480", 40, "vmax_b_frames=0:keyint=12", {"SEQ-END-MISSING"} | MJ_FILM_TC),
    ("men-telecine-b", "704x480", 40, "vmax_b_frames=2:keyint=12",
     {"SEQ-END-MISSING", "OUT-FIELD-PARITY"} | MJ_FILM_TC),
]
# x262 (the MPEG-2 branch of x264, github.com/kierank/x262, branch x262-legacy; not in nixpkgs:
# tools/x262.nix builds it) with --pulldown 32: soft telecine with adaptive B-pictures and
# closed GOPs. Only with --x262 BIN (the flake check does not fetch it).
X262 = [
    # name, size, frames, options, allowed rules
    ("x262-pulldown", "704x480", 40,
     ["--bitrate", "2500", "--vbv-maxrate", "4000", "--vbv-bufsize", "1835", "--keyint", "12", "--bframes", "2"],
     {"SEQ-END-MISSING"} | MJ_FILM_TC),
    ("x262-pulldown-nob", "704x480", 40,
     ["--bitrate", "2500", "--vbv-maxrate", "4000", "--vbv-bufsize", "1835", "--keyint", "15", "--bframes", "0"],
     {"SEQ-END-MISSING"} | MJ_FILM_TC),
]
# rules a case must trip (a negative sample that stops being one is a failure too)
REQUIRED = {"mj-pulldown": {"PCE-RFF-INTERLACED"}, "men-telecine-b": {"OUT-FIELD-PARITY"},
            "mj-pulldown-1080": {"PL-RFF", "PCE-TFF-PROGRESSIVE"}}

# Another encoder's stream with its display flags rewritten by tools/esflags.py (as DGPulldown
# does): name, base case, esflags arguments, profile, allowed rules.
FLAGGED = [
    # ffmpeg 720p59.94 shown as film: frames for three and two frame periods (C.9)
    ("pf-720p-film", "ff-720p60", ["--pattern", "3,2"], None, {"PL-SAMPLE-RATE"} | FF_ALWAYS),
    ("pf-720p-mixed", "ff-720p60", ["--pattern", "3,2,1,1,2,3,3,1"], None, {"PL-SAMPLE-RATE"} | FF_ALWAYS),
    # ffmpeg interlaced frames flagged progressive with 3:2 (legal in H.262: field tools in a
    # progressive frame are only forbidden by A/53 and SCTE 43)
    ("pf-480i-film", "ff-interlaced", ["--pattern", "3,2", "--progressive-frame"], None, FF_ALWAYS),
    ("pf-480i-mixed", "ff-interlaced", ["--pattern", "3,2,2,3,3,2,2,2"], None, FF_ALWAYS),
    ("pf-1080i-film", "ff-1080i", ["--pattern", "3,2", "--progressive-frame"], None, FF_ALWAYS),
    # with captions of the right count per picture, under the broadcast profile
    ("pf-480i-film-cc", "ff-interlaced", ["--pattern", "3,2", "--progressive-frame", "--cc"], "scte43-256qam",
     FF_ALWAYS | FF_BROADCAST | {"BC-PROGRESSIVE-FPFD"}),
    ("pf-1080i-film-cc", "ff-1080i", ["--pattern", "3,2", "--progressive-frame", "--cc"], "scte43-256qam",
     FF_ALWAYS | FF_BROADCAST | {"BC-PROGRESSIVE-FPFD"}),
    ("pf-720p-film-cc", "ff-720p60", ["--pattern", "3,2", "--cc"], "scte43-256qam",
     FF_ALWAYS | FF_BROADCAST | {"PL-SAMPLE-RATE"}),
    ("mj-pulldown-b-cc", "mj-pulldown-b", ["--pattern", "3,2", "--cc"], "scte43-256qam",
     MJ_FILM_TC | FF_BROADCAST),
    # repeat_first_field where Table 8-7 has none: a progressive sequence at 29.97 Hz
    ("pf-30p-rff", "ff-ipp", ["--pattern", "2,1"], None, FF_ALWAYS | {"PL-RFF"}),
]
REQUIRED["pf-30p-rff"] = {"PL-RFF"}

# One violation written into a stream that is clean in this respect: name, base, mutation of
# tools/esflags.py, profile, the rule the verifier must add to what the base gives, rules that
# may come with it.
MUTATED = [
    ("mut-rff-clear", "mj-pulldown-b", "rff-clear", None, "OUT-FIELD-PARITY", {"VBV-UNDERFLOW", "VBV-DELAY-RATE", "VBV-OVERFLOW"}),
    ("mut-tff-flip", "mj-pulldown-b", "tff-flip", None, "OUT-FIELD-PARITY", set()),
    ("mut-tff-flip-x262", "x262-pulldown", "tff-flip", None, "OUT-FIELD-PARITY", set()),
    ("mut-rff-clear-x262", "x262-pulldown", "rff-clear", None, "OUT-FIELD-PARITY", {"VBV-UNDERFLOW"}),
    ("mut-prog-clear-x262", "x262-pulldown", "prog-clear", None, "PCE-RFF-INTERLACED", set()),
    ("mut-tff-flip-pf", "pf-1080i-film", "tff-flip", None, "OUT-FIELD-PARITY", set()),
    ("mut-prog-clear", "mj-pulldown-b", "prog-clear", None, "PCE-RFF-INTERLACED", set()),
    ("mut-rate-24", "mj-pulldown-b", "rate-24", None, "PL-RFF",
     {"VBV-UNDERFLOW", "VBV-DELAY-RATE", "VBV-OVERFLOW", "GOP-TIMECODE-PICTURES", "GOP-TIMECODE-CONTINUITY"}),
    ("mut-rate-24-720p", "pf-720p-film", "rate-24", None, "PL-RFF", {"VBV-UNDERFLOW", "VBV-OVERFLOW", "PL-LEVEL-LOWEST"}),
    ("mut-tref", "mj-pulldown-b", "tref-skip", None, "PIC-TEMPORAL-REF", set()),
    ("mut-tff-no-rff", "pf-720p-mixed", "tff-no-rff", None, "PCE-TFF-PROGRESSIVE", set()),
    # the coded vbv_delay values belong to the 3:2 removal times: with every picture two
    # fields long the input rate of C.3.1 no longer comes out at the coded bit rate
    # every picture two fields long: pictures leave a quarter faster than the stream was coded
    # for, and the buffer of C.3.2 runs empty
    ("mut-vbv-all-two", "mj-pulldown-tight", "all-two", None, "VBV-UNDERFLOW", set()),
    ("mut-cc-10", "pf-1080i-film-cc", "cc-10", "scte43-256qam", "CC-COUNT", set()),
    ("mut-cc-10-mj", "mj-pulldown-b-cc", "cc-10", "scte43-256qam", "CC-COUNT", set()),
]


def run(cmd, **kw):
    r = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, **kw)
    if r.returncode != 0:
        sys.stderr.write(" ".join(cmd) + "\n" + r.stderr.decode(errors="replace")[-2000:] + "\n")
    return r.returncode == 0


def verify(a, path, profile):
    cmd = [a.verify, "--json", "--fail-on", "none"] + (["--profile", profile] if profile else []) + [path]
    r = subprocess.run(cmd, stdout=subprocess.PIPE)
    return json.loads(r.stdout)["streams"][0]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--verify", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--ffmpeg", default="ffmpeg")
    ap.add_argument("--mpeg2enc")
    ap.add_argument("--mencoder")
    ap.add_argument("--x262")
    ap.add_argument("--list", action="store_true", help="print what every stream gives and do not fail")
    a = ap.parse_args()
    os.makedirs(a.out, exist_ok=True)
    # name, file, allowed rules, made, profile, rule that must be added to the base's (mutations)
    cases = []
    paths = {}

    def add(name, out, allowed, ok, profile=None, must=None, base=None):
        paths[name] = out
        cases.append((name, out, allowed, ok, profile, must, base))

    for name, size, rate, frames, opts, allowed in FFMPEG:
        out = os.path.join(a.out, name + ".m2v")
        src = SRC.format(size=size, rate=rate)
        if name in FIELD_ORDER:
            src += ",setfield=" + FIELD_ORDER[name]
        ok = run([a.ffmpeg, "-v", "error", "-y", "-f", "lavfi", "-i", src,
                  "-frames:v", str(frames), "-c:v", "mpeg2video"] + opts + ["-f", "mpeg2video", out])
        add(name, out, allowed | FF_ALWAYS, ok)
    if a.mpeg2enc:
        for name, (size, rate, interlaced, frames, *more), opts, allowed in MPEG2ENC:
            y4m = os.path.join(a.out, name + ".y4m")
            out = os.path.join(a.out, name + ".m2v")
            vf = SRC.format(size=size, rate=rate) + "".join(more) + (",setfield=tff" if interlaced else "")
            ok = run([a.ffmpeg, "-v", "error", "-y", "-f", "lavfi", "-i", vf, "-frames:v", str(frames),
                      "-pix_fmt", "yuv420p", "-f", "yuv4mpegpipe", y4m])
            fmt = [] if "-f" in opts else ["-f", "3"]
            ok = ok and run([a.mpeg2enc, "-v", "0"] + fmt + opts + ["-o", out, y4m])
            if os.path.exists(y4m):
                os.remove(y4m)
            add(name, out, allowed, ok)
    if a.mencoder:
        for name, size, frames, lavc, allowed in MENCODER:
            y4m = os.path.join(a.out, name + ".y4m")
            mpg = os.path.join(a.out, name + ".mpg")
            out = os.path.join(a.out, name + ".m2v")
            ok = run([a.ffmpeg, "-v", "error", "-y", "-f", "lavfi", "-i", SRC.format(size=size, rate="24000/1001"),
                      "-frames:v", str(frames), "-pix_fmt", "yuv420p", "-f", "yuv4mpegpipe", y4m])
            ok = ok and run([a.mencoder, y4m, "-really-quiet", "-ovc", "lavc", "-lavcopts",
                             "vcodec=mpeg2video:vbitrate=2500:vrc_maxrate=8000:vrc_buf_size=1835:" + lavc,
                             "-of", "mpeg", "-mpegopts", "format=dvd:tsaf:telecine", "-ofps", "24000/1001",
                             "-nosound", "-o", mpg])
            ok = ok and run([a.ffmpeg, "-v", "error", "-y", "-i", mpg, "-c", "copy", "-f", "mpeg2video", out])
            for f in (y4m, mpg):
                if os.path.exists(f):
                    os.remove(f)
            add(name, out, allowed, ok)
    if a.x262:
        for name, size, frames, opts, allowed in X262:
            y4m = os.path.join(a.out, name + ".y4m")
            out = os.path.join(a.out, name + ".m2v")
            ok = run([a.ffmpeg, "-v", "error", "-y", "-f", "lavfi", "-i", SRC.format(size=size, rate="24000/1001"),
                      "-frames:v", str(frames), "-pix_fmt", "yuv420p", "-f", "yuv4mpegpipe", y4m])
            ok = ok and run([a.x262, "--mpeg2", "--pulldown", "32"] + opts + ["-o", out, y4m])
            if os.path.exists(y4m):
                os.remove(y4m)
            add(name, out, allowed, ok)
    for name, base, args, profile, allowed in FLAGGED:
        if base not in paths:
            continue
        out = os.path.join(a.out, name + ".m2v")
        ok = os.path.exists(paths[base]) and run(
            [sys.executable, "-I", esflags.__file__, "telecine", paths[base], out] + args)
        add(name, out, allowed, ok, profile)
    for name, base, kind, profile, must, extra in MUTATED:
        if base not in paths:
            continue
        out = os.path.join(a.out, name + ".m2v")
        ok = os.path.exists(paths[base]) and run(
            [sys.executable, "-I", esflags.__file__, "mutate", paths[base], out, kind])
        add(name, out, extra, ok, profile, must, base)
    failed = 0
    seen = {}
    features = {}
    for name, out, allowed, ok, profile, must, base in cases:
        if not ok or not os.path.exists(out) or os.path.getsize(out) == 0:
            print(f"{name}: FAIL: no stream")
            failed += 1
            continue
        s = verify(a, out, profile)
        got = set(s["counts"])
        if base:
            # a mutation: the base's findings, the rule of the mutation, what may come with it
            was = set(verify(a, paths[base], profile)["counts"])
            extra = got - was - allowed - {must}
            if must not in got or must in was:
                extra.add("(missing) " + must)
        else:
            for k in got:
                seen.setdefault(k, []).append(name)
            for k, v in s["features"].items():
                features[k] = max(features.get(k, 0), v) if k.startswith("max ") else features.get(k, 0) + v
            extra = got - allowed
            for k in REQUIRED.get(name, set()) - got:
                extra.add("(missing) " + k)
            if name.startswith(("mj-pulldown", "men-", "pf-", "x262-")) and not name.endswith("-rff"):
                d = esflags.describe(open(out, "rb").read())
                breaks = d["parity_breaks"]
                if (breaks > 0) != ("OUT-FIELD-PARITY" in got):
                    extra.add(f"(esflags counts {breaks} parity breaks)")
                if not any(u != (1 if d["progressive_sequence"] else 2) for _, _, _, u, _, _ in d["pictures"]):
                    extra.add("(no repeat_first_field in the stream)")
        status = "ok" if not extra else "FAIL"
        if extra:
            failed += 1
        print(f"{name}: {status}: {s['pictures']} pictures, rules {sorted(got)}" + (f", not allowed {sorted(extra)}" if extra else ""))
        if extra:
            for f in s["findings"]:
                if f["rule"] in extra:
                    print(f"    {f['severity']} {f['rule']} byte {f['byte']} pic {f.get('picture_coded')}: {f['message']}")
    print("== rules tripped by independent encoders:")
    for k in sorted(seen):
        print(f"  {k}: {', '.join(seen[k])}")
    if a.list:
        print("== features exercised:")
        for k in sorted(features):
            print(f"  {features[k]:>10}  {k}")
        return 0
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
