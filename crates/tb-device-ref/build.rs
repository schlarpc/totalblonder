//! Links the host build of the kernels (`libtbkernels_ref.a`), which Nix builds from the sources in
//! `crates/tb-kernel-abi/kernels/` (nix/kernels.nix: `refLib`) and names in `TB_KERNELS_REF`. The flake sets the
//! variable for this package's build and in the dev shell.
//!
//! With the feature `asan`, links the same kernels built with AddressSanitizer instead
//! (`refLibAsan`, named in `TB_KERNELS_REF_ASAN`) and the sanitizer's runtime. The flake's
//! variant `asan` runs this crate's tests that way (nix/rust.nix).
#![allow(
    clippy::print_stdout,
    reason = "a build script talks to Cargo on stdout"
)]

fn main() {
    let asan = std::env::var_os("CARGO_FEATURE_ASAN").is_some();
    let var = if asan {
        "TB_KERNELS_REF_ASAN"
    } else {
        "TB_KERNELS_REF"
    };
    println!("cargo:rerun-if-env-changed={var}");
    println!("cargo:rerun-if-changed=build.rs");
    match std::env::var(var) {
        Ok(dir) => {
            println!("cargo:rustc-link-search=native={dir}/lib");
            println!("cargo:rerun-if-changed={dir}/lib/libtbkernels_ref.a");
            println!("cargo:rustc-link-lib=static=tbkernels_ref");
            if asan {
                // The instrumented code calls into the runtime. It is not the first library
                // the program loads, so runs need ASAN_OPTIONS=verify_asan_link_order=0.
                println!("cargo:rustc-link-lib=dylib=asan");
            }
        }
        Err(_) => {
            println!(
                "cargo:warning={var} is not set: build inside `nix develop` (or through the \
                 flake), which builds the kernels for the host and sets it"
            );
            std::process::exit(1);
        }
    }
}
