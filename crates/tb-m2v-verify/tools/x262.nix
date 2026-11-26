# x262 for tools/independent.py --x262: the MPEG-2 branch of x264 (not in nixpkgs).
#   nix build --impure -f crates/tb-m2v-verify/tools/x262.nix --no-link --print-out-paths
# Not part of the flake: it fetches the branch head from GitHub (impure), and builds it without
# assembly with the flags an old C code base needs on a current compiler. Obtained and run on
# 2026-11-26 (commit bb887aa of branch x262-legacy); docs/notes/m2v-conformance-findings.md, section 6.
let
  flake = builtins.getFlake (toString ../../..);
  pkgs = import flake.inputs.nixpkgs { system = "x86_64-linux"; };
in
pkgs.stdenv.mkDerivation {
  name = "x262";
  src = builtins.fetchGit {
    url = "https://github.com/kierank/x262.git";
    ref = "x262-legacy";
  };
  nativeBuildInputs = [
    pkgs.bash
    pkgs.perl
  ];
  configureFlags = [
    "--disable-asm"
    "--disable-opencl"
    "--disable-avs"
    "--disable-swscale"
    "--disable-lavf"
    "--disable-ffms"
    "--disable-gpac"
    "--disable-lsmash"
  ];
  postPatch = "patchShebangs .";
  env.NIX_CFLAGS_COMPILE = "-fcommon -Wno-error -Wno-incompatible-pointer-types -Wno-implicit-function-declaration -Wno-int-conversion -std=gnu99";
  installPhase = "mkdir -p $out/bin; cp x264 $out/bin/x262";
}
