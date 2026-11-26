"""Formats, encoder command lines and the places of things, shared by the gate's tools."""
import os
import shutil

GATE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(GATE)
# The gate's tools take the binaries and directories from the environment (the flake's apps set
# them; gate/env.sh is the same for the shell tools):
#   M2VENC       the encoder (default: tb-m2venc on PATH)
#   M2VPIPE      the pipeline harness (default: tb-pipe on PATH)
#   M2V_MEDIA    the clips (default: media/ in the tree if it exists, else
#                ~/totalblonder-spike/media, where the corpus lives on nixark)
#   M2V_WORK     scratch and results (default work/gate in the tree, not in git)
#   M2V_OUT      coded streams and other scratch (default $M2V_WORK/out)
#   M2V_RESULTS  result files (default $M2V_WORK/results)
# The quality baseline and the cached reference encodes of check.py are gate/baselines/.
M2VENC = os.environ.get("M2VENC") or shutil.which("tb-m2venc") or "tb-m2venc"
M2VPIPE = os.environ.get("M2VPIPE") or shutil.which("tb-pipe") or "tb-pipe"
_tree_media = os.path.join(ROOT, "media")
MEDIA = os.environ.get("M2V_MEDIA") or (_tree_media if os.path.isdir(_tree_media) else os.path.expanduser("~/totalblonder-spike/media"))
WORK = os.environ.get("M2V_WORK") or os.path.join(ROOT, "work", "gate")
OUT = os.environ.get("M2V_OUT") or os.path.join(WORK, "out")
RESULTS = os.environ.get("M2V_RESULTS") or os.path.join(WORK, "results")
BASELINES = os.path.join(GATE, "baselines")

# rate/vbv: nominal operating point per format; vbv = 0.5 s of the rate in units of 16384 bits,
# limited by the level (MP@ML 1,835,008 bits; A/53 limits vbv_buffer_size_value to 488).
FORMATS = {
    "480i": dict(size=(704, 480), fps=(30000, 1001), gop=15, bitrate=4_000_000, vbv=1_835_008, interlaced=True),
    "720p": dict(size=(1280, 720), fps=(60000, 1001), gop=30, bitrate=12_000_000, vbv=5_996_544, interlaced=False),
    "1080i": dict(size=(1920, 1080), fps=(30000, 1001), gop=15, bitrate=15_000_000, vbv=7_487_488, interlaced=True),
    "1080p": dict(size=(1920, 1080), fps=(30000, 1001), gop=15, bitrate=15_000_000, vbv=7_487_488, interlaced=False),
}


def fps(fmt):
    n, d = FORMATS[fmt]["fps"]
    return n / d


def frame_bytes(fmt):
    w, h = FORMATS[fmt]["size"]
    return w * h * 3 // 2


def clip_path(clip, fmt):
    return os.path.join(MEDIA, f"{clip}_{fmt}.yuv")


def clip_frames(clip, fmt):
    return os.path.getsize(clip_path(clip, fmt)) // frame_bytes(fmt)


def vbv_for(fmt, bitrate):
    """Buffer for a rate other than nominal: 0.5 s, whole units of 16384 bits, format maximum."""
    return max(16384, min(FORMATS[fmt]["vbv"], int(bitrate * 0.5) // 16384 * 16384))


def enc_args(fmt, clip, bitrate=None, vbv=None, bframes=2, gop=None):
    f = FORMATS[fmt]
    br = bitrate or f["bitrate"]
    a = [M2VENC, "--input", clip_path(clip, fmt), "--size", "%dx%d" % f["size"],
         "--fps", "%d/%d" % f["fps"], "--gop", str(gop or f["gop"]), "--bframes", str(bframes),
         "--bitrate", str(br), "--vbv", str(vbv or (f["vbv"] if bitrate is None else vbv_for(fmt, br)))]
    if f["interlaced"]:
        a.append("--interlaced")
    return a
