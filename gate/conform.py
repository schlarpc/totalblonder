"""The conformance verifier in the gate (notes/m2v-conformance-findings.md section 8).

Every coded stream a gate case writes goes through tb-m2v-verify (M2V_VERIFY: the binary;
nix/gate.nix sets it) with the profile of the case and the operating delivery model:

  - any S1 or S2 finding fails the case;
  - an S3 finding fails the case unless crates/tb-m2v-verify/tools/gate-allow.txt allows that
    rule for that stream (each line: the rule, globs on the stream's file name, and the entry of
    notes/post-port-worklist.md that will remove it);
  - S4 notes are counted and reported, never fail.

Profiles: the gate's broadcast formats (1080i, 1080p, 720p, 480i and the pipeline's outputs,
pipe_small.py's 480i among them) are checked against SCTE 43 at 256-QAM, the plant the server
feeds (scte43-256qam); the small test pictures of gate_small.py (176x144 and the like, no
broadcast format) against H.262 alone (h262-only).
"""
import fnmatch
import json
import os
import subprocess

HERE = os.path.dirname(os.path.abspath(__file__))
BROADCAST = "scte43-256qam"
TEST_SIZE = "h262-only"
# Streams of the broadcast gates that are run under the encoder's profile h262 on purpose (a
# picture size of no format table) and are therefore verified against H.262 alone.
TEST_SIZE_STREAMS = ["devin-crop-*"]


def test_size(path):
    return any(fnmatch.fnmatch(os.path.basename(path), g) for g in TEST_SIZE_STREAMS)


def _default_allow():
    # gate/ -> repository root
    root = os.path.dirname(HERE)
    return os.path.join(root, "crates", "tb-m2v-verify", "tools", "gate-allow.txt")


VERIFY = os.environ.get("M2V_VERIFY")
ALLOW_FILE = os.environ.get("M2V_GATE_ALLOW") or _default_allow()


def load_allow(path=None):
    """[(rule, [globs], comment)] from the allow file."""
    out = []
    with open(path or ALLOW_FILE) as f:
        for line in f:
            body, _, comment = line.partition("#")
            words = body.split()
            if words:
                out.append((words[0], words[1:] or ["*"], comment.strip()))
    return out


def allowed(rule, stream, allow):
    name = os.path.basename(stream)
    return any(r == rule and any(fnmatch.fnmatch(name, g) for g in globs) for r, globs, _ in allow)


def verify(paths, profile=BROADCAST, allow=None):
    """Verify streams. Returns (fails, info): fails is a list of messages (empty: the streams
    pass), info a dict with the profile, the number of streams, the S3 findings allowed and
    the S4 notes, both as {rule: streams}."""
    paths = sorted(p for p in paths if not p.endswith(".ref.m2v"))
    info = dict(profile=profile, streams=len(paths), allowed={}, notes={})
    if not paths:
        return [], info
    if not VERIFY:
        return ["conformance: M2V_VERIFY is not set (the gate must run the verifier)"], info
    allow = load_allow() if allow is None else allow
    p = subprocess.run([VERIFY, "--json", "--profile", profile, "--delivery", "operating", "--fail-on", "none", *paths],
                       stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    if p.returncode != 0:
        return [f"conformance: the verifier failed (status {p.returncode}): {p.stderr.strip()[-300:]}"], info
    doc = json.loads(p.stdout)
    sev = {rule: t["severity"] for rule, t in doc["totals"].items()}
    fails = []
    for s in doc["streams"]:
        name = os.path.basename(s["path"])
        for rule, n in sorted(s["counts"].items()):
            level = sev.get(rule, "S1")
            if level in ("S1", "S2") or (level == "S3" and not allowed(rule, s["path"], allow)):
                fails.append(f"conformance: {name}: {rule} ({level}) x{n}")
            elif level == "S3":
                info["allowed"].setdefault(rule, []).append(name)
            else:
                info["notes"].setdefault(rule, []).append(name)
    return fails, info


def note(info):
    """A short text for a case's line in the gate's table."""
    parts = [f"verified {info['streams']} ({info['profile']})"]
    if info["allowed"]:
        parts.append("allowed " + ", ".join(f"{r} x{len(v)}" for r, v in sorted(info["allowed"].items())))
    return "; ".join(parts)


def unused(allow, infos):
    """Allow-list entries that matched no finding in these runs (after a full run: delete them)."""
    seen = {(r, n) for i in infos for r, names in i["allowed"].items() for n in names}
    return [(r, globs, c) for r, globs, c in allow
            if not any(r == sr and any(fnmatch.fnmatch(n, g) for g in globs) for sr, n in seen)]
