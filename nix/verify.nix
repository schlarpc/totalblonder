# The conformance verifier (crates/tb-m2v-verify, work package V1) on streams made in the
# sandbox: our encoder on the reference adapter, and encoders that share nothing with ours.
{
  pkgs,
  lib,
  root,
  tb-m2venc,
  tb-m2v-verify,
}:
let
  fs = lib.fileset;
  tools = "${
    fs.toSource {
      inherit root;
      fileset = fs.unions [
        (root + "/crates/tb-m2v-verify/tools")
        (root + "/docs/notes/m2v-conformance-rules.md")
      ];
    }
  }/crates/tb-m2v-verify/tools";
  rules = "${
    fs.toSource {
      inherit root;
      fileset = root + "/docs/notes/m2v-conformance-rules.md";
    }
  }/docs/notes/m2v-conformance-rules.md";
  verify = "${tb-m2v-verify}/bin/tb-m2v-verify";
  enc = "${tb-m2venc}/bin/tb-m2venc";
  # nix run .#verify-streams -- --profile scte43-256qam --delivery operating DIR...
  verify-streams = pkgs.writeShellApplication {
    name = "verify-streams";
    text = ''exec ${verify} "$@"'';
  };
in
{
  checks = {
    # Our encoder's streams on the CPU reference adapter (tools/ref_cases.sh). The group
    # "clean" (small pictures, profile h262) must have no finding of any severity and must hold
    # the operating model; the group "broadcast" (formats of A/53 Part 4 Table 6.2 under the
    # default profile) nothing above a note; every configuration of the group "refused" must be
    # refused when the session opens, with the clause in the message. There is no list of
    # known findings any more: the gaps of docs/notes/post-port-worklist.md are closed.
    "verify/ref-streams" =
      pkgs.runCommand "verify-ref-streams"
        {
          nativeBuildInputs = [
            pkgs.bash
            pkgs.coreutils
            pkgs.gnugrep
            pkgs.gnused
          ];
        }
        ''
          set -o pipefail
          export HOME=$TMPDIR
          bash ${tools}/ref_cases.sh ${enc} $TMPDIR/s
          mkdir -p $out
          ${verify} --delivery operating --fail-on S4 $TMPDIR/s/clean | tee $out/clean.txt
          # under the cable profile the clean group adds only format-table and color notes
          # (the test pictures are 176x144 and 352x240), nothing about captions or headers
          ${verify} --profile scte43-256qam --fail-on S3 --allow BC-FORMAT --allow BC-LEVEL-LOWEST $TMPDIR/s/clean > $out/clean-scte43.txt
          # the broadcast formats under the default profile: nothing at all outside notes
          ${verify} --profile scte43-256qam --delivery operating --fail-on S3 $TMPDIR/s/broadcast | tee $out/broadcast.txt
          cp $TMPDIR/s/refused.txt $out/
        '';
    # ffmpeg's mpeg2video, mjpegtools' mpeg2enc and mplayer's mencoder: the verifier must decode
    # everything they write and report only the deviations listed per case in
    # tools/independent.py; the soft-telecined streams, the streams flagged by tools/esflags.py
    # and their mutations exercise the rules of repeat_first_field.
    "verify/independent" =
      pkgs.runCommand "verify-independent"
        {
          nativeBuildInputs = [
            pkgs.ffmpeg
            pkgs.mjpegtools
            pkgs.mplayer
            pkgs.python3
          ];
        }
        ''
          set -o pipefail
          export HOME=$TMPDIR
          mkdir -p $out
          python3 -I ${tools}/independent.py --verify ${verify} --out $TMPDIR/s --mpeg2enc mpeg2enc --mencoder mencoder | tee $out/log.txt
        '';
    # docs/notes/m2v-conformance-rules.md carries the catalog the binary prints.
    "verify/rules-catalog" = pkgs.runCommand "verify-rules-catalog" { } ''
      ${verify} --rules-md > generated.md
      sed -n '/^<!-- generated: tb-m2v-verify --rules-md -->$/,$p' ${rules} | tail -n +2 > committed.md
      cmp generated.md committed.md
      touch $out
    '';
  };
  apps.verify-streams = {
    type = "app";
    program = lib.getExe verify-streams;
    meta.description = "verify-streams";
  };
}
