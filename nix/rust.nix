# breakbulk configuration of the Cargo workspace (one derivation per Cargo unit).
# `kernels` is nix/kernels.nix, passed through mkRepository's specialArgs.
{
  pkgs,
  root,
  breakbulk,
  kernels,
  ...
}:
let
  toolchain = pkgs.rust-bin.fromRustupToolchainFile (root + "/rust-toolchain.toml");
  # libav for tb-decode (see packages.ffmpeg-sys-the-third below).
  ffmpeg = pkgs.ffmpeg-headless;
in
{
  workspaces.default = {
    path = ".";
    toolchain = {
      rustc = toolchain;
      cargo = toolchain;
    };
    fmt.enable = true;
    imports = [
      (breakbulk.variants.clippy {
        args = [
          "-D"
          "warnings"
        ];
      })
    ];
    memberDefaults.test.runner = breakbulk.testRunners.nextest { };
    # The bench plan (release profile, so its units are the products'): the GPU adapter's
    # protocol-cost bench, run on the card by `nix run .#gpu-bench`. In `nix flake check` it
    # runs once with --test, which measures nothing.
    variants.default.benches = [ "tb-device-gpu" ];
    # The tests of the reference adapter once more, with the kernels built with
    # AddressSanitizer (kernels.refLibAsan, linked by the crate's feature `asan`).
    variants.asan = {
      packages = [ "tb-device-ref" ];
      tests = [ "tb-device-ref" ];
      features = [ "asan" ];
      checks.products = false;
    };
  };

  # The kernel builds enter the Rust units as store paths in the environment of the one
  # package that reads each.
  packages.tb-kernel-abi.env.TB_ABI_LAYOUT = kernels.abiLayout;
  packages.tb-device-ref = {
    env = {
      TB_KERNELS_REF = kernels.refLib;
      TB_KERNELS_REF_ASAN = kernels.refLibAsan;
    };
    # For the runs of the variant `asan` (and harmless in the others): the sanitizer's runtime
    # is linked, not preloaded, so it is not the first library loaded; the fiber stacks of the
    # launcher live until the process ends, by design.
    test.env.ASAN_OPTIONS = "verify_asan_link_order=0:detect_leaks=0:abort_on_error=1";
  };
  packages.tb-device-gpu = {
    env = {
      TB_KERNEL_IMAGE_CUDA = "${kernels.deviceImage}/tb_kernels.cubin";
      TB_KERNEL_SYMBOLS_CUDA = "${kernels.deviceImage}/tb_kernels.sym";
      TB_KERNEL_IMAGE_HIP = "${kernels.deviceImageHip}/tb_kernels.hsaco";
      TB_KERNEL_SYMBOLS_HIP = "${kernels.deviceImageHip}/tb_kernels.sym";
    };
    # tests/plumbing.rs needs a card and its driver. Its tests are `#[ignore]`d, so the run
    # in `nix flake check` (and a bare `cargo test`) builds the binary and runs nothing; on the
    # machine with the card it runs outside the sandbox as the test app
    # default/x86_64-unknown-linux-gnu/tb-device-gpu/test/plumbing with --include-ignored
    # (alias: nix run .#gpu-plumbing; its tests are `cuda::...` and `hip::...`, one set per
    # runtime, so a filter picks the vendor of the machine).
    # benches/protocol.rs has no libtest harness, which nextest cannot list.
    bench.runner = breakbulk.testRunners.direct;
  };
  # libav for tb-decode. The -sys crate's build script finds the libraries with pkg-config and
  # runs bindgen on their headers (bindgenHook: libclang and the C include paths). ffmpeg-headless
  # of this nixpkgs is built with nv-codec-headers: `--enable-nvdec --enable-cuvid`, `ffmpeg
  # -hwaccels` lists cuda (the spike's C++ used pkgs.ffmpeg of the same nixpkgs revision: the same
  # libraries plus the desktop outputs). The driver's libraries (libnvcuvid.so.1, libcuda.so.1)
  # are loaded by name at run time and are not in the closure.
  packages.ffmpeg-sys-the-third = {
    nativeInputs = p: [
      p.pkg-config
      p.rustPlatform.bindgenHook
    ];
    inputs = p: [ p.ffmpeg-headless ];
  };
  # tb-decode declares AVCUDADeviceContext itself (libavutil/hwcontext_cuda.h needs NVIDIA's
  # headers, so bindgen does not see it). This compiles static assertions on the declaration
  # against that header, with the nv-codec-headers this ffmpeg was built with.
  checks."tb-decode/cuda-layout" =
    pkgs.runCommandCC "tb-decode-cuda-layout"
      {
        src = root + "/crates/tb-decode/shim/cuda_layout.c";
        buildInputs = [
          ffmpeg
        ]
        ++ builtins.filter (i: pkgs.lib.hasPrefix "nv-codec-headers" (i.pname or "")) ffmpeg.buildInputs;
      }
      ''
        $CC -std=c11 -Wall -Werror -fsyntax-only $src
        touch $out
      '';
  # The CUDA kernels live in this crate's directory but are built by nix/kernels.nix, not by
  # Cargo: keep them out of the crate's source view so a kernel edit does not rebuild every
  # crate that depends on the ABI.
  packages.tb-kernel-abi.sources.noKernels =
    { default, root, packageDir, ... }:
    pkgs.lib.fileset.difference default (root + "/${packageDir}/kernels");
}
