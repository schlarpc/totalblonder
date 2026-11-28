# Contributing

Thanks for looking. This file says how to build and test the code, the rules the code
follows, and how the docs are written.

## Building and testing

Everything builds with Nix. A development shell has the pinned Rust toolchain and the tools:

```sh
nix develop                      # then cargo works as usual
cargo test -p tb-hls             # one crate
cargo clippy --workspace --all-targets
nix flake check                  # everything that needs no GPU: build, tests, lints, golden cases
```

`nix flake check` is what a change must pass. It runs the tests in the Nix sandbox with no GPU,
on the reference kernels.

The GPU gate in [`tools/gate/`](tools/gate/README.md) checks the encoder and the pipeline on
a real card: decoder conformance, buffer models, film mode, capacity. Run it for changes to
the encoder, the kernels or the input pipeline.

New files must be tracked by git (`git add`) before Nix sees them.

## Rules for the code

These come from [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) §1, and `nix flake check`
enforces most of them.

- **Sans-io libraries.** A `tb-*` logic crate takes inputs and returns outputs as plain data:
  no clock, thread, socket, file or async runtime. Time is an argument.
  `#![forbid(unsafe_code)]`. The I/O lives in the server (`crates/totalblonder`) and in a few
  adapter crates.
- **libav stays in the sandbox.** Only `tb-avworker`, the sandboxed decoder worker, links
  libav. Do not add ffmpeg or libav to the server, and do not widen the worker's seccomp filter
  for convenience: fix the code that needs the call.
- **Errors are values.** No `unwrap` or `expect` outside tests (clippy denies both);
  `thiserror` enums at crate boundaries.
- **`unsafe` only in adapter crates**, with a `SAFETY` comment on every block.
- **Formatting:** `rustfmt` with `max_width = 100`; clippy warnings are errors; every public
  item is documented.
- **Standards first.** Output must conform. Cite the clause a piece of code implements
  (for example `A/65 §6.2`). The stream checkers (`tb-ts-verify`, `tb-m2v-verify`) must stay
  independent of the code that writes the streams.

## Tests

- Unit tests next to the code; integration tests in each crate's `tests/`.
- Pages captured from a real modulator go in `crates/tb-eqam/tests/fixtures/`. Remove session
  ids and anything secret first.
- A feature that touches a live path should be checked end to end, and the evidence (what ran,
  on what, what was measured) recorded in the commit message and in
  [`docs/STATUS.md`](docs/STATUS.md).

## Working with a real modulator

Read [`docs/EQAM-420B.md`](docs/EQAM-420B.md) first. Some GET URLs on the box act at once (reboot,
flush, delete), and some saves corrupt its configuration. The modulator client in `tb-eqam`
sends only requests its refusal screen allows. Keep it that way: a new request type needs a
rule in `refusal.rs` and a test that the dangerous neighbors are still refused.

## Commits

- One self-contained change per commit.
- The first line says what changed, in under about 70 characters.
- The body says why, and what was checked.

## Writing docs

The docs are written in plain, direct English, so that they read well for people whose first
language is not English:

- Short sentences, active voice, one idea per sentence.
- Define a term the first time it is used.
- American spelling. Only the syntax element names of H.262 keep their British spelling.
- Say what is known and how: measured, verified in code, or inferred. The notes mark this as
  `[V]`, `[I]` and so on.
- Usage belongs in [`docs/GUIDE.md`](docs/GUIDE.md); design and its reasons in
  [`docs/SPEC.md`](docs/SPEC.md); the state of things in [`docs/STATUS.md`](docs/STATUS.md).
