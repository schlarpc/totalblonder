# breakbulk: feedback from the first use in this repository

2026-11-25. breakbulk `9a85f41` (`github:schlarpc/breakbulk`), Determinate Nix 3.24.2 (Nix
2.35.3) on bagel and 3.22.2 (2.35.2) on nixark, IFD mode. The workspace: eleven crates, two of
them with native inputs (a static library linked by a build script; a binary blob embedded
with `include_bytes!(env!(…))`), kernel derivations built outside Cargo with an unfree
toolkit, tests that need a GPU. Configuration: `flake.nix`, `nix/rust.nix`,
`nix/kernels.nix`.

Nothing was worked around or patched. Two things were missing; the rest are notes.

## Gaps

### 1. No supported way to add tools to the workspace's dev shell

Needed: the workspace dev shell plus `cargo-nextest` and `cbindgen` on `PATH`
(`nix develop -c cargo nextest run` must work, because the checks run tests under nextest).

Exists: `workspaces.<ws>.devShell.env` and `devShell.target`. No option for packages, and the
documentation does not say whether `overrideAttrs` or `mkShell { inputsFrom = [ … ]; }` on
`repo.workspaces.<ws>.devShell` keeps what the shell sets up (it exports its variables from a
hook that reads `cargo-env.json`).

Used: `ws.devShell.overrideAttrs (old: { nativeBuildInputs = (old.nativeBuildInputs or [ ]) ++
[ pkgs.cargo-nextest pkgs.rust-cbindgen ]; })`. It works here (the `TB_*` variables,
`CARGO_BUILD_TARGET` and the tools are all present), but it relies on the shell's shape.

Minimal reproduction:

```nix
devShells.${system}.default = repo.workspaces.default.devShell;   # how do I add pkgs.cargo-nextest?
```

Wanted: `workspaces.<ws>.devShell.packages` (a function of the package set, like
`test.path`), or a sentence in "The dev shell" that `overrideAttrs` / `inputsFrom` are
supported and what they keep. A workspace that sets `test.runner = testRunners.nextest { }`
could put that cargo-nextest in the shell by default.

### 2. Nothing names the evaluation-time closure, so a second machine rebuilds breakbulk's tools

Needed: build on bagel, copy to nixark, evaluate and run there without compiling anything
(nixark is shared and has little disk).

Did: `nix copy --no-check-sigs --to ssh://nixark` of the runtime closure of every output of
this flake's own derivations (units, test binaries, test runs, checks: 2.4 GB). Then on
nixark `nix run .#gpu-plumbing`.

Happened: evaluation on nixark built `breakbulk-plan`, `breakbulk-instantiate`,
`breakbulk-shim`, `breakbulk-predict`, the vendor directory, the plans and `units.json`
there, because they are inputs of import-from-derivation, not part of any output's runtime
closure. Minutes, once. Everything after evaluation was already present.

Exists: "Use a binary cache" covers `nix copy --to <cache> .#default` and a
`post-build-hook` with `"$DRV_PATH^*"`; "Following your nixpkgs" says the tools "come from a
cache that has breakbulk's pin, or build once". There is no public cache with the tools and no
attribute that holds what evaluation needs.

Minimal reproduction, two machines A and B with the same flake checkout:

```sh
A$ nix flake check && nix copy --no-check-sigs --to ssh://B .#checks.x86_64-linux.all
B$ nix flake check        # builds breakbulk-plan, -instantiate, -shim, -predict, plans, units.json
```

Wanted: an output such as `repo.evalClosure` (a derivation whose closure is everything
import-from-derivation reads for this repository: the tools' outputs, vendor directories,
plans, `units.json`), so that `nix copy --to ssh://B .#evalClosure .#checks.x86_64-linux.all`
makes B's evaluation and build substitution-only. Or a published cache for the tools at
breakbulk's own nixpkgs pin, named in the documentation.

## Notes (worked as documented)

- **Per-package environment with derivations** (`packages.<name>.env`): the route for the
  kernel image, the host library and the ABI layout manifest. A kernel edit rebuilds only the
  package that names the derivation and its dependents. `option_env!` / `env!` /
  `include_bytes!(env!(…))` and a build script printing `rustc-link-search` from the variable
  all work in the units and in the dev shell. A test binary that reads a store path baked in
  at compile time finds it in the sandbox (the path is in the binary's closure).
- **`extraSources`** for `golden/` outside the crate: worked; the error for an entry that does
  not exist (the directory was empty, so git did not have it) named the entry and the
  resolved path. Good message.
- **Unfree packages**: nothing to do in breakbulk; `allowUnfreePredicate` on the `pkgs` passed
  to `mkRepository` is enough, and breakbulk's own nixpkgs is not involved.
- **GPU tests outside the sandbox**: test apps are the mechanism and they work on the GPU
  machine (`repo.apps."default/<triple>/tb-device-cuda/test/plumbing"`). The tests here are
  `#[ignore]`d rather than disabled with `test.binaries.<key>.enable = false`, so that a bare
  `cargo test` on a machine without the card passes too. A test app passes only its own
  command line (documented), so the flake wraps the app in a script that adds
  `--include-ignored --nocapture`. A `test.binaries.<key>.appArgs` would remove the wrapper;
  small.
- **Checks of our own** (`checks = { "kernels/…" = drv; }`): the grouping by `/` gives
  `checks.kernels` for free.
- **`specialArgs`** to hand the kernel derivations to the module: worked.
- **clippy preset, `fmt.enable`, nextest runner** in `memberDefaults`: three lines, no
  surprises. clippy's `-D warnings` reaches test targets and build scripts, as documented.
- **Times**: the first `nix flake check` of the workspace took 25 s on bagel (119 checks,
  planning included; breakbulk's tools and the toolchain were already in the store).
- **Dev shell warning** about `~/.cargo/config.toml` on every entry: correct and useful once;
  an environment variable to acknowledge it would be welcome.
- The dev shell realizes every derivation named in package `env`, here the CUDA image (the
  unfree toolkit download) even for work on pure crates. `devShell.env.X = null` is the
  documented way out; a second, lighter shell was not needed yet.
