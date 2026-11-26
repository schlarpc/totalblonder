# The gate: checks that run in the sandbox on the reference adapter, and apps that run the
# gate's tools on the machine with the card (nixark).
#
# The machinery is tools/gate/ (Python and shell): strict ffmpeg and libmpeg2 decode, the two
# buffer-model simulations, reconstruction against decode, quality baselines, the conformance
# verifier on every stream (tools/gate/conform.py), the capacity benchmark, the pipeline's line-ups,
# the contention harness, the corpus recipes, the Nsight helpers. The encoder under test is
# tb-m2venc (M2VENC), the pipeline tb-pipe (M2VPIPE); tools/gate/README.md has the list.
{
  pkgs,
  lib,
  root,
  cuda,
  nvflags,
  tb-m2venc,
  tb-pipe,
  tb-m2v-verify,
}:
let
  fs = lib.fileset;
  tools = [
    pkgs.ffmpeg
    pkgs.libmpeg2
    pkgs.python3
    pkgs.coreutils
    pkgs.findutils
    pkgs.diffutils
    pkgs.gnugrep
    pkgs.bash
  ];
  enc = "${tb-m2venc}/bin/tb-m2venc";
  pipe = "${tb-pipe}/bin/tb-pipe";
  # Every stream of every case goes through the conformance verifier (conform.py).
  verify = "${tb-m2v-verify}/bin/tb-m2v-verify";
  src = fs.toSource {
    inherit root;
    fileset = fs.unions [
      (root + "/crates/tb-golden/vectors")
      (root + "/tools/gate")
      (root + "/crates/tb-m2v-verify/tools/gate-allow.txt")
      # the independent reader of the display flags (tools/gate/film_check.py)
      (root + "/crates/tb-m2v-verify/tools/esflags.py")
    ];
  };

  # The competitor of the contention runs (tools/gate/contend.sh, optlog experiment 40): a second CUDA
  # context that launches a trivial kernel and/or a copy per period and times its round trip.
  gpucomp = pkgs.stdenv.mkDerivation {
    name = "gpucomp";
    src = fs.toSource {
      inherit root;
      fileset = root + "/tools/gate/gpucomp.cu";
    };
    nativeBuildInputs = [
      cuda.cuda_nvcc
      pkgs.autoAddDriverRunpath
    ];
    buildInputs = [
      cuda.cuda_cudart
      cuda.cccl
    ];
    dontConfigure = true;
    buildPhase = ''
      runHook preBuild
      nvcc ${nvflags} -o gpucomp tools/gate/gpucomp.cu
      runHook postBuild
    '';
    installPhase = ''
      mkdir -p $out/bin
      cp gpucomp $out/bin/
    '';
    meta.mainProgram = "gpucomp";
  };

  script =
    name: text:
    pkgs.writeShellApplication {
      inherit name text;
      runtimeInputs = tools;
    };
  # Run from the repository tree on the machine with the card. The clips: tools/gate/env.sh.
  preamble = ''
    [ -f tools/gate/check.py ] || { echo "run from the repository root" >&2; exit 2; }
    if [ -z "''${M2V_MEDIA:-}" ]; then
      if [ -d media ]; then M2V_MEDIA=$PWD/media; else M2V_MEDIA=$HOME/totalblonder-spike/media; fi
    fi
    export M2V_MEDIA
    export M2V_VERIFY=${verify}
    export M2VENC=${enc} M2VPIPE=${pipe} GPUCOMP=${lib.getExe gpucomp}
    export LD_LIBRARY_PATH=/run/opengl-driver/lib''${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}
  '';
  scripts = lib.mapAttrs script {
    # The correctness gate (tools/gate/check.py: the corpus at broadcast sizes, torture cases and
    # features): nix run .#gate-check -- [--quick] [--only REGEX] [--out NAME]
    gate-check = ''
      ${preamble}
      exec python3 -I tools/gate/check.py "$@"
    '';
    # The gate of the pipeline (device input, repeats, run-time sessions, conversions against
    # CPU references and analytic patterns): nix run .#gate-pipe -- [--only REGEX]. Needs the
    # compressed inputs of tools/gate/prep_inputs.sh under $M2V_MEDIA/in.
    gate-pipe = ''
      ${preamble}
      exec python3 -I tools/gate/pipe_check.py "$@"
    '';
    # The film-mode gate (tools/gate/film_gate.py: tb-m2venc --film and tb-pipe film=auto at broadcast
    # sizes, the cadence on both decoders, the displayed fields): nix run .#gate-film --
    # [--only REGEX] [--measure]. Needs the film material of tools/gate/prep_film.sh under $M2V_FILM.
    gate-film = ''
      ${preamble}
      exec python3 -I tools/gate/film_gate.py "$@"
    '';
    # The golden cases through the binary on either device: nix run .#gate-small -- --device cuda --golden
    gate-small = ''
      export M2V_VERIFY=${verify}
      export LD_LIBRARY_PATH=/run/opengl-driver/lib''${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}
      exec python3 -I tools/gate/gate_small.py --enc ${enc} "$@"
    '';
    # The golden cases on the card: every record (but the measured times) and every stream as
    # the reference adapter recorded them: nix run .#gate-golden -- DIR
    gate-golden = ''
      [ -f crates/tb-golden/vectors/generate.sh ] || { echo "run from the repository root" >&2; exit 2; }
      export LD_LIBRARY_PATH=/run/opengl-driver/lib''${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}
      TB_M2VENC=${enc} TB_PIPE=${pipe} exec bash crates/tb-golden/vectors/generate.sh --device cuda --check "''${1:-golden-cuda}"
    '';
    # Capacity (tools/gate/bench_capacity.py): nix run .#gate-bench -- --mode free|paced|both --label NAME
    # (more builds or options to compare: --bin NAME="PATH --option ...", run interleaved)
    gate-bench = ''
      ${preamble}
      exec python3 -I tools/gate/bench_capacity.py "$@" --bin rust=${enc}
    '';
    # Paced line-ups of the pipeline (tools/gate/pipe_lineup.py): nix run .#gate-lineup -- --label L --lineup a --n 27
    gate-lineup = ''
      ${preamble}
      exec python3 -I tools/gate/pipe_lineup.py "$@"
    '';
    # The encoder next to other users of the card (tools/gate/contend.sh, with gpucomp):
    # nix run .#gate-contend -- LABEL FORMAT N COMPETITOR...
    gate-contend = ''
      ${preamble}
      exec bash tools/gate/contend.sh "$@"
    '';
    # The corpus: nix run .#gate-prep -- media|inputs (raw clips under $M2V_MEDIA, compressed
    # inputs under $M2V_MEDIA/in)
    gate-prep = ''
      ${preamble}
      case "''${1:-}" in
        media) exec bash tools/gate/prep_media.sh ;;
        inputs) exec bash tools/gate/prep_inputs.sh ;;
        film) exec bash tools/gate/prep_film.sh ;;
        *) echo "usage: gate-prep media|inputs|film" >&2; exit 2 ;;
      esac
    '';
    # The pipeline harness itself: nix run .#gate-tb-pipe -- --in ... --ch ...
    gate-tb-pipe = ''
      ${preamble}
      exec ${pipe} "$@"
    '';
  };
in
{
  checks = {
    # The clip generator of the driver is crates/tb-golden/vectors/make_clip.py.
    "gate/make-clip" = pkgs.runCommand "gate-make-clip" { nativeBuildInputs = tools; } ''
      for c in "176 144 30 1" "176 144 24 2 noisy" "184 144 12 3" "176 144 24 4 interlaced" "176 144 3 7 white" "176 144 4 8 patch"; do
        # shellcheck disable=SC2086
        ${enc} --make-clip $c > rust.yuv
        python3 -I ${src}/crates/tb-golden/vectors/make_clip.py $c > py.yuv
        cmp rust.yuv py.yuv
      done
      touch $out
    '';
    # The committed golden vectors are what the tree's encoder records (crates/tb-golden/vectors/generate.sh on
    # the reference adapter): every record and every stream digest, byte for byte.
    "golden/fresh" = pkgs.runCommand "golden-fresh" { nativeBuildInputs = tools; } ''
      export HOME=$TMPDIR
      TB_M2VENC=${enc} TB_PIPE=${pipe} bash ${src}/crates/tb-golden/vectors/generate.sh --check $TMPDIR/fresh | tee log.txt
      mkdir -p $out
      cp log.txt $out/
    '';
    # The gate at small size on the reference adapter: the golden cases through the binary,
    # each stream decoded strictly by ffmpeg and by libmpeg2, both buffer models, the
    # reconstruction against the decode, captions, the verifier, and crates/tb-golden/vectors/*.sha256.
    "gate/ref-small" = pkgs.runCommand "gate-ref-small" { nativeBuildInputs = tools; } ''
      set -o pipefail
      export HOME=$TMPDIR
      M2V_VERIFY=${verify} python3 -I ${src}/tools/gate/gate_small.py --enc ${enc} --device ref --golden --work $TMPDIR/work | tee log.txt
      mkdir -p $out
      cp log.txt $out/
    '';
    # The pipeline harness at small scale on the reference adapter: software-decoded inputs
    # made in the sandbox, frame selection, the host path, the slate; streams decoded strictly
    # by two decoders, buffer model, the verifier, the frame clock's counters.
    "gate/pipe-ref-small" = pkgs.runCommand "gate-pipe-ref-small" { nativeBuildInputs = tools; } ''
      set -o pipefail
      export HOME=$TMPDIR
      M2V_VERIFY=${verify} python3 -I ${src}/tools/gate/pipe_small.py --pipe ${pipe} --work $TMPDIR/work | tee log.txt
      mkdir -p $out
      cp log.txt $out/
    '';
    # The contention harness's competitor builds.
    "gate/gpucomp" = gpucomp;
  };
  apps = lib.mapAttrs (name: drv: {
    type = "app";
    program = lib.getExe drv;
    meta.description = name;
  }) scripts;
  inherit gpucomp;
  # All of them in one store path (to copy to the machine with the card and run from there).
  tools = pkgs.symlinkJoin {
    name = "gate-tools";
    paths = lib.attrValues scripts ++ [ gpucomp ];
  };
}
