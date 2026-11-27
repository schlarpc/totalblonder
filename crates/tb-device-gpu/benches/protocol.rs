//! Host cost of executing plans through the protocol against the same driver calls made
//! directly (docs/ARCHITECTURE.md §8.1), and the hybrid wait against the blocking one, on every
//! runtime whose card is present (`--runtime cuda|hip` picks one); prints a note for a runtime
//! without a card and exits 0. Run it with the card idle (look at `nvidia-smi pmon` or
//! `amd-smi monitor` first):
//!
//! ```sh
//! nix run .#gpu-bench            # or: cargo bench -p tb-device-gpu --bench protocol
//! ```
#![allow(
    clippy::print_stdout,
    missing_docs,
    reason = "a benchmark program: the table is its output"
)]

use tb_device_gpu::{Cuda, GpuDevice, Hip, Runtime, cost};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::args().any(|a| a == "--test") {
        // `nix flake check` runs every bench once in test mode: nothing to time there.
        println!("test mode: nothing measured");
        return Ok(());
    }
    let reps: usize = std::env::var("TB_BENCH_REPS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(2000);
    let only = std::env::args().skip_while(|a| a != "--runtime").nth(1);
    let wants = |name: &str| only.as_deref().is_none_or(|o| o.eq_ignore_ascii_case(name));
    if wants("cuda") {
        bench::<Cuda>(reps)?;
    }
    if wants("hip") {
        bench::<Hip>(reps)?;
    }
    Ok(())
}

fn bench<R: Runtime>(reps: usize) -> Result<(), Box<dyn std::error::Error>> {
    let mut dev = match GpuDevice::<R>::new(0, cost::ARENAS, &R::IMAGE) {
        Ok(d) => d,
        Err(e) => {
            println!("no {} device ({e}); nothing measured", R::NAME);
            return Ok(());
        }
    };
    println!(
        "device: {} ({}); {reps} repetitions per shape, medians",
        dev.name(),
        R::NAME
    );
    println!();
    println!(
        "| Plan | Steps | validate ns/step | submit ns/step | direct ns/step | protocol overhead ns/step |"
    );
    println!("|---|---|---|---|---|---|");
    for r in cost::measure(&mut dev, reps)? {
        println!(
            "| {} | {} | {:.0} | {:.0} | {:.0} | {:.0} |",
            r.name,
            r.steps,
            r.validate_ns,
            r.submit_ns,
            r.direct_ns,
            r.submit_ns - r.direct_ns
        );
    }
    println!();
    println!(
        "| Wait | submit + wait per plan, µs | device time, µs (median) | host CPU per plan, µs |"
    );
    println!("|---|---|---|---|");
    // A short plan (8 launches of k_pyr: about 1.7 ms on the RTX 2000 Ada, 0.15 ms on the RX 7900
    // XTX) and a long one (a tick phase's length on the faster card).
    for (launches, iters) in [(8, 400), (200, 100)] {
        for w in cost::measure_wait(&mut dev, iters, launches)? {
            println!(
                "| {:?}, {launches} launches | {:.0} | {:.0} | {:.0} |",
                w.mode, w.loop_us, w.device_us, w.cpu_us
            );
        }
    }
    println!();
    Ok(())
}
