//! Host cost of executing plans through the protocol against the same driver calls made
//! directly (docs/ARCHITECTURE.md §8.1), and the hybrid wait against the blocking one. Needs the
//! card; prints a note and exits 0 where there is none. Run it on nixark with the card idle
//! (look at `nvidia-smi pmon` first):
//!
//! ```sh
//! nix run .#gpu-bench            # or: cargo bench -p tb-device-cuda --bench protocol
//! ```
#![allow(
    clippy::print_stdout,
    missing_docs,
    reason = "a benchmark program: the table is its output"
)]

use tb_device_cuda::{CudaDevice, IMAGE, cost};

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
    let mut dev = match CudaDevice::new(0, cost::ARENAS, &IMAGE) {
        Ok(d) => d,
        Err(e) => {
            println!("no CUDA device ({e}); nothing measured");
            return Ok(());
        }
    };
    println!(
        "device: {}; {reps} repetitions per shape, medians",
        dev.name()
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
    for w in cost::measure_wait(&mut dev, 400)? {
        println!(
            "| {:?} | {:.0} | {:.0} | {:.0} |",
            w.mode, w.loop_us, w.device_us, w.cpu_us
        );
    }
    Ok(())
}
