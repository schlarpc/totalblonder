# The kernel builds: everything that is compiled from the CUDA kernel sources or from the
# kernel ABI, as derivations of their own. The Rust units take them as inputs through
# breakbulk package settings (nix/rust.nix); nothing here knows about Cargo.
#
#   abiHeader    tb_kernel_abi.h, generated from crates/tb-kernel-abi by cbindgen
#   abiLayout    the layout of every ABI record as C++ sees that header (a text file)
#   deviceImage  tb_kernels.cubin (nvcc, sm_89, nvflags below) and tb_kernels.sym
#   refLib       lib/libtbkernels_ref.a: the same kernels compiled for the host
#   refLibAsan   the same, with AddressSanitizer (for the tests of tb-device-ref)
#   checks       abi-header-fresh: the committed header equals the generated one;
#                cuda-api: the CUDA adapter's own driver bindings agree with cuda.h
#
# The kernel sources are crates/tb-kernel-abi/kernels/ (one copy, compiled by these derivations only).
{ pkgs, root }:
let
  inherit (pkgs) lib;
  fs = lib.fileset;
  # The toolkit release the card's driver (595.71.05) was built for.
  cuda = pkgs.cudaPackages_13_2;

  kernelFiles = root + "/crates/tb-kernel-abi/kernels";
  kernelSrc = fs.toSource {
    inherit root;
    fileset = kernelFiles;
  };
  includes = "-Icrates/tb-kernel-abi/kernels";
  # The flags the C++ spike built its kernels with. -fmad=false: no implicit fused
  # multiply-add; the kernels write each fused operation as fmaf, so the image is the machine
  # code nvcc's default contraction made of them before, and refLib (-ffp-contract=off, fmaf
  # from libm, correctly rounded) computes the same bits (docs/ARCHITECTURE.md section 7.3).
  nvflags = "-O3 -std=c++17 -arch=sm_89 -lineinfo -fmad=false";

  abiHeader =
    pkgs.runCommand "tb_kernel_abi.h"
      {
        nativeBuildInputs = [ pkgs.rust-cbindgen ];
        src = fs.toSource {
          root = root + "/crates/tb-kernel-abi";
          fileset = fs.unions [
            (root + "/crates/tb-kernel-abi/src/lib.rs")
            (root + "/crates/tb-kernel-abi/cbindgen.toml")
          ];
        };
      }
      ''
        cbindgen --config $src/cbindgen.toml $src/src/lib.rs --output $out
      '';

  abiLayout =
    pkgs.runCommandCC "tb-kernel-abi-layout.txt"
      {
        src = root + "/crates/tb-kernel-abi/kernels/abi_layout.cc";
      }
      ''
        mkdir inc
        cp ${abiHeader} inc/tb_kernel_abi.h
        $CXX -std=c++17 -Wall -Werror -Iinc $src -o layout
        ./layout > $out
      '';

  deviceImage = pkgs.stdenv.mkDerivation {
    name = "tb-kernels-sm89";
    src = kernelSrc;
    nativeBuildInputs = [ cuda.cuda_nvcc ];
    buildInputs = [
      cuda.cuda_cudart
      cuda.cccl
    ];
    dontConfigure = true;
    dontFixup = true;
    buildPhase = ''
      runHook preBuild
      nvcc ${nvflags} ${includes} -cubin -o tb_kernels.cubin crates/tb-kernel-abi/kernels/tb_kernels.cu
      # The kernels' symbols: "<name> <mangled symbol>" per line, for cuModuleGetFunction.
      readelf -sW tb_kernels.cubin \
        | awk '$4 == "FUNC" && $5 == "GLOBAL" { print $NF }' \
        | while read -r sym; do
            name=$(c++filt "$sym" | sed 's/(.*//')
            echo "$name $sym"
          done | sort > tb_kernels.sym
      # Every kernel of the list is in the image.
      for k in $(sed -n 's/^ *X(\([a-z_0-9]*\),.*/\1/p' crates/tb-kernel-abi/kernels/tb_kernel_list.h); do
        grep -q "^$k " tb_kernels.sym || { echo "kernel $k is not in the image"; exit 1; }
      done
      runHook postBuild
    '';
    installPhase = ''
      mkdir -p $out
      cp tb_kernels.cubin tb_kernels.sym $out/
    '';
  };

  # The host build. `extraFlags`: the AddressSanitizer variant adds its flags; the plain
  # library has none, so its derivation is what it was before the variant existed.
  mkRefLib =
    {
      name,
      extraFlags ? "",
    }:
    pkgs.stdenv.mkDerivation {
      inherit name;
      src = kernelSrc;
      dontConfigure = true;
      # -fno-strict-aliasing: the kernels read and write pixel rows as 32-bit words.
      # -ffp-contract=off: no fused multiply-add the source does not ask for; the ones it asks
      # for (fmaf) are libm's, a single correctly rounded operation (ARCHITECTURE §7.3).
      # No exceptions, no RTTI, no C++ runtime library: the archive links into Rust with libc
      # and libm only (the AddressSanitizer variant also with the sanitizer's runtime).
      buildPhase = ''
        runHook preBuild
        $CXX -std=c++17 -O2 -fPIC -D__CUDACC__ -fno-strict-aliasing -ffp-contract=off \
          -fno-exceptions -fno-rtti -Wall -Wno-unknown-pragmas -Wno-maybe-uninitialized ${extraFlags} \
          -Icrates/tb-kernel-abi/kernels/shim ${includes} -c crates/tb-kernel-abi/kernels/tb_kernels_ref.cc -o tb_kernels_ref.o
        $AR rcs libtbkernels_ref.a tb_kernels_ref.o
        runHook postBuild
      '';
      installPhase = ''
        mkdir -p $out/lib $out/include
        cp libtbkernels_ref.a $out/lib/
        cp crates/tb-kernel-abi/kernels/tb_kernels_ref.h $out/include/
      '';
    };
  refLib = mkRefLib { name = "tb-kernels-ref"; };
  # The same kernels with AddressSanitizer: a kernel that reads or writes outside its arrays,
  # its stack or the arena is silent on the card and on refLib. tb-device-ref links it with its
  # feature `asan`; the flake's variant `asan` runs that crate's tests so (nix/rust.nix).
  refLibAsan = mkRefLib {
    name = "tb-kernels-ref-asan";
    extraFlags = "-g -fsanitize=address -fno-omit-frame-pointer";
  };

  abiHeaderFresh = pkgs.runCommand "abi-header-fresh" { } ''
    if ! diff -u ${root + "/crates/tb-kernel-abi/kernels/tb_kernel_abi.h"} ${abiHeader}; then
      echo "crates/tb-kernel-abi/kernels/tb_kernel_abi.h is stale: run 'nix run .#abi-header'"
      exit 1
    fi
    touch $out
  '';
  # The hand-written driver bindings of tb-device-cuda against cuda.h of this toolkit:
  # crates/tb-device-cuda/api_check.cc is generated from the bindings (its freshness is a test of
  # that crate) and is a list of static_asserts on symbol names, function types, constants and
  # the layout of CUDA_MEMCPY2D.
  cudaApiCheck =
    pkgs.runCommandCC "cuda-api-check"
      {
        src = root + "/crates/tb-device-cuda/api_check.cc";
        buildInputs = [ cuda.cuda_cudart ];
      }
      ''
        $CXX -std=c++17 -Wall -Werror -fsyntax-only $src
        touch $out
      '';
in
{
  inherit
    cuda
    nvflags
    abiHeader
    abiLayout
    deviceImage
    refLib
    refLibAsan
    ;
  checks = {
    "kernels/abi-header-fresh" = abiHeaderFresh;
    "kernels/device-image" = deviceImage;
    "kernels/ref-lib" = refLib;
    "kernels/ref-lib-asan" = refLibAsan;
    "kernels/cuda-api" = cudaApiCheck;
  };
}
