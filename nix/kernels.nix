# The kernel builds: everything that is compiled from the CUDA kernel sources or from the
# kernel ABI, as derivations of their own. The Rust units take them as inputs through
# breakbulk package settings (nix/rust.nix); nothing here knows about Cargo.
#
#   abiHeader    tb_kernel_abi.h, generated from crates/tb-kernel-abi by cbindgen
#   abiLayout    the layout of every ABI record as C++ sees that header (a text file)
#   deviceImage  tb_kernels.cubin (nvcc, sm_89, nvflags below) and tb_kernels.sym
#   deviceImageHip  tb_kernels.hsaco (hipcc, the AMD targets of hipArchs, hipflags below) and
#                tb_kernels.sym
#   refLib       lib/libtbkernels_ref.a: the same kernels compiled for the host
#   refLibAsan   the same, with AddressSanitizer (for the tests of tb-device-ref)
#   checks       abi-header-fresh: the committed header equals the generated one;
#                cuda-api: the GPU adapter's own CUDA driver bindings agree with cuda.h
#                hip-api: its HIP bindings agree with hip_runtime_api.h
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

  # AMD: ROCm of this nixpkgs (7.2; the runtime the adapter loads is the system's libamdhip64,
  # ROCm 6.2 or later). The same kernel sources through shim/hip/cuda_runtime.h, the HIP
  # counterpart of the host shim (shuffles 32 lanes wide, the SIMD-in-a-word intrinsics).
  rocm = pkgs.rocmPackages;
  # The RDNA targets, all wave32 (the kernels are written for 32-lane warps; the shim's shuffles
  # would keep them right on wave64 parts too): RDNA2, RDNA3 (gfx1100: RX 7900, the card of
  # bagel), RDNA3.5, RDNA4. One offload bundle; the runtime loads the code object of its card.
  hipArchs = [
    "gfx1030"
    "gfx1100"
    "gfx1101"
    "gfx1102"
    "gfx1150"
    "gfx1151"
    "gfx1200"
    "gfx1201"
  ];
  # -ffp-contract=off: no fused multiply-add the source does not write (as nvcc's -fmad=false);
  # the source's fmaf is v_fma_f32, correctly rounded. Division and denormals are IEEE by
  # default in HIP (no -ffast-math, no -fgpu-flush-denormals-to-zero), so the image computes the
  # bits of the cubin and of refLib (docs/ARCHITECTURE.md section 7.3).
  # -Wno-unused-command-line-argument: the hipcc wrapper adds --hip-link to every call.
  hipflags = "-x hip -O3 -std=c++17 -ffp-contract=off -Wall -Werror -Wno-unused-function -Wno-unused-command-line-argument";

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

  deviceImageHip = pkgs.stdenv.mkDerivation {
    name = "tb-kernels-hip";
    src = kernelSrc;
    nativeBuildInputs = [
      rocm.clr
      rocm.llvm.bintools
    ];
    dontConfigure = true;
    dontFixup = true;
    buildPhase = ''
      runHook preBuild
      dl=--rocm-device-lib-path=${rocm.rocm-device-libs}/amdgcn/bitcode
      inc="-Icrates/tb-kernel-abi/kernels/shim/hip ${includes}"
      tu=crates/tb-kernel-abi/kernels/tb_kernels.cu
      hipcc ${hipflags} $dl $inc --genco --no-offload-compress \
        ${lib.concatMapStringsSep " " (a: "--offload-arch=${a}") hipArchs} -o tb_kernels.hsaco $tu
      # The symbols are the same in every target's code object: read them from the first one.
      hipcc ${hipflags} $dl $inc --offload-device-only --no-gpu-bundle-output \
        --offload-arch=${builtins.head hipArchs} -o first.co $tu
      llvm-readelf -sW first.co \
        | awk '$4 == "FUNC" && $5 == "GLOBAL" { print $NF }' \
        | sort -u | while read -r sym; do
            name=$(llvm-cxxfilt "$sym" | sed 's/(.*//')
            echo "$name $sym"
          done | sort > tb_kernels.sym
      for k in $(sed -n 's/^ *X(\([a-z_0-9]*\),.*/\1/p' crates/tb-kernel-abi/kernels/tb_kernel_list.h); do
        grep -q "^$k " tb_kernels.sym || { echo "kernel $k is not in the image"; exit 1; }
      done
      # Every kernel is compiled for 32-lane waves.
      if llvm-readelf --notes first.co | grep -E '\.wavefront_size:' | grep -vqE ':\s+32$'; then
        echo "a kernel is not wave32"; exit 1
      fi
      runHook postBuild
    '';
    installPhase = ''
      mkdir -p $out
      cp tb_kernels.hsaco tb_kernels.sym $out/
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
  # The hand-written driver bindings of tb-device-gpu against cuda.h of this toolkit:
  # crates/tb-device-gpu/cuda_api_check.cc is generated from the bindings (its freshness is a
  # test of that crate) and is a list of static_asserts on symbol names, function types,
  # constants and the layout of CUDA_MEMCPY2D.
  cudaApiCheck =
    pkgs.runCommandCC "cuda-api-check"
      {
        src = root + "/crates/tb-device-gpu/cuda_api_check.cc";
        buildInputs = [ cuda.cuda_cudart ];
      }
      ''
        $CXX -std=c++17 -Wall -Werror -fsyntax-only $src
        touch $out
      '';
  # The same for its HIP bindings against hip_runtime_api.h of the pinned ROCm.
  hipApiCheck =
    pkgs.runCommandCC "hip-api-check"
      {
        src = root + "/crates/tb-device-gpu/hip_api_check.cc";
      }
      ''
        $CXX -std=c++17 -Wall -Werror -Wno-deprecated-declarations -fsyntax-only \
          -I${rocm.clr}/include $src
        touch $out
      '';
in
{
  inherit
    cuda
    nvflags
    rocm
    hipArchs
    hipflags
    deviceImageHip
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
    "kernels/device-image-hip" = deviceImageHip;
    "kernels/cuda-api" = cudaApiCheck;
    "kernels/hip-api" = hipApiCheck;
  };
}
