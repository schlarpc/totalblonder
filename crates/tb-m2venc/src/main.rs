//! `tb-m2venc`: command-line driver of the Rust encoder.
//!
//! The counterpart of the C++ `m2venc` of the CUDA spike: the correctness gate and the benchmarks
//! run this binary with the options the C++ driver had.
//!
//! ```sh
//! tb-m2venc --input clip.yuv --size 704x480 --interlaced --bitrate 4000000 --output out
//! tb-m2venc --device ref ...           # the kernels compiled for the host (small pictures)
//! tb-m2venc plumb ref|cuda             # the plumbing proof: k_pyr through an adapter
//! tb-m2venc --golden-tables f.jsonl    # the table record the kernels read
//! tb-m2venc --make-clip 176 144 30 1 > qcif.yuv
//! ```
//!
//! Exit status: 0 the run is complete; 1 a file or the device could not be used; 2 the command
//! line or a session's configuration was refused; 3 the device was lost (the streams are
//! complete and valid: repeats from the loss on); 4 plans failed (the streams are complete and
//! valid: their pictures are repeats); 5 the device stopped answering (the streams end there).
#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "a command-line program"
)]

use std::io::Write;
use std::process::ExitCode;

use anyhow::{Context, bail};
use tb_device::{Device, selftest};
use tb_engine::device::Guarded;
use tb_m2venc::clip::{ClipKind, make_clip};
use tb_m2venc::opts::{self, DeviceKind, Opts};
use tb_m2venc::{Outcome, RunError, Status, arena_sizes, run};

fn plumb(dev: &mut impl Device, what: &str) -> ExitCode {
    match selftest::run(dev) {
        Ok(out) => {
            let ok = selftest::digest(&out) == selftest::EXPECTED_DIGEST;
            println!(
                "plumb {what}: digest {:016x} ({}), {} ns",
                selftest::digest(&out),
                if ok { "as expected" } else { "WRONG" },
                out.completion
                    .finished
                    .0
                    .saturating_sub(out.completion.submitted.0)
            );
            if ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(e) => {
            eprintln!("plumb {what}: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `--golden-tables FILE`: the `tables` record of crates/tb-golden/vectors/README.md, as the encoder uploads it.
fn golden_tables(path: &str) -> anyhow::Result<()> {
    let t = tb_m2v_syntax::tables::tables();
    let bytes = bytemuck::bytes_of(&t);
    let mut s = String::with_capacity(2 * bytes.len() + 128);
    s.push_str("{\"k\":\"golden\",\"version\":1,\"case\":\"tables\"}\n");
    s.push_str(&format!(
        "{{\"k\":\"tables\",\"size\":{},\"bytes\":\"",
        bytes.len()
    ));
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s.push_str("\"}\n");
    std::fs::write(path, s).with_context(|| path.to_owned())
}

/// `--make-clip W H FRAMES SEED [interlaced|noisy|white|patch]`: the clip on standard output.
fn make_clip_cmd(a: &[&str]) -> anyhow::Result<()> {
    let (dims, kind) = match a {
        [w, h, n, seed] => ([w, h, n, seed], ClipKind::Plain),
        [w, h, n, seed, "interlaced"] => ([w, h, n, seed], ClipKind::Interlaced),
        [w, h, n, seed, "noisy"] => ([w, h, n, seed], ClipKind::Noisy),
        [w, h, n, seed, "white"] => ([w, h, n, seed], ClipKind::White),
        [w, h, n, seed, "patch"] => ([w, h, n, seed], ClipKind::Patch),
        _ => bail!("usage: tb-m2venc --make-clip W H FRAMES SEED [interlaced|noisy|white|patch]"),
    };
    let num = |s: &str| {
        s.parse::<u32>()
            .with_context(|| format!("--make-clip: {s:?}"))
    };
    let (w, h, n) = (num(dims[0])?, num(dims[1])?, num(dims[2])?);
    if w <= 48 || h == 0 {
        bail!("--make-clip: the width must be above 48 and the height above 0");
    }
    let clip = make_clip(w, h, n, u64::from(num(dims[3])?), kind);
    std::io::stdout().lock().write_all(&clip)?;
    Ok(())
}

/// Make the adapter the options name and run.
fn encode(o: &Opts) -> anyhow::Result<Result<Outcome, RunError>> {
    let arenas = match arena_sizes(o) {
        Ok(a) => a,
        Err(e) => return Ok(Err(e)),
    };
    tracing::info!(
        device = arenas.device,
        host = arenas.host,
        "arena sizes for this line-up"
    );
    let mut console = std::io::stdout().lock();
    Ok(match (o.device, o.check_arena) {
        (DeviceKind::Ref, check) => {
            let d = tb_device_ref::RefDevice::new(arenas).context("reference device")?;
            if check {
                run(o, Guarded::new(d), &mut console)
            } else {
                run(o, d, &mut console)
            }
        }
        (DeviceKind::Cuda, check) => {
            let d = tb_device_cuda::CudaDevice::new(o.gpu, arenas, &tb_device_cuda::IMAGE)
                .context("CUDA device")?;
            if check {
                run(o, Guarded::new(d), &mut console)
            } else {
                run(o, d, &mut console)
            }
        }
    })
}

fn real_main() -> anyhow::Result<ExitCode> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["plumb", "ref"] => {
            let mut d =
                tb_device_ref::RefDevice::new(selftest::ARENAS).context("reference device")?;
            return Ok(plumb(&mut d, "ref"));
        }
        ["plumb", "cuda"] => {
            let mut d =
                tb_device_cuda::CudaDevice::new(0, selftest::ARENAS, &tb_device_cuda::IMAGE)
                    .context("CUDA device")?;
            return Ok(plumb(&mut d, "cuda"));
        }
        ["--golden-tables", path] => {
            golden_tables(path)?;
            return Ok(ExitCode::SUCCESS);
        }
        ["--make-clip", rest @ ..] => {
            make_clip_cmd(rest)?;
            return Ok(ExitCode::SUCCESS);
        }
        [] | ["--help"] | ["-h"] => {
            eprintln!("{}", opts::USAGE);
            return Ok(ExitCode::from(2));
        }
        _ => {}
    }
    let o = match opts::parse(&args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("tb-m2venc: {e}");
            eprintln!("{}", opts::USAGE);
            return Ok(ExitCode::from(2));
        }
    };
    let out = match encode(&o)? {
        Ok(out) => out,
        Err(e @ (RunError::Encoder(_) | RunError::FrameRates)) => {
            // a session the encoder refuses: the C++ exits with 2
            eprintln!("tb-m2venc: {e}");
            return Ok(ExitCode::from(2));
        }
        Err(e) => return Err(e.into()),
    };
    print!("{}", out.report.text());
    if let Some(p) = &o.json {
        std::fs::write(p, out.report.json()).with_context(|| p.display().to_string())?;
    }
    Ok(match out.status {
        Status::Complete => ExitCode::SUCCESS,
        Status::DeviceLost => {
            eprintln!(
                "tb-m2venc: the device was lost. The streams are complete: every picture after \
                 the loss is a repeat made on the host. Start the process again to use the card."
            );
            ExitCode::from(3)
        }
        Status::FailedPlans(n) => {
            eprintln!(
                "tb-m2venc: {n} device plans failed. The streams are complete: the pictures of \
                 those ticks are repeats made on the host."
            );
            ExitCode::from(4)
        }
        Status::Hung(why) => {
            eprintln!("tb-m2venc: {why}. The streams end at the last complete picture.");
            ExitCode::from(5)
        }
    })
}

fn main() -> ExitCode {
    // Adapters and the driver report through tracing; TB_LOG=debug (or trace) shows more.
    let level = match std::env::var("TB_LOG").as_deref() {
        Ok("trace") => tracing::Level::TRACE,
        Ok("debug") => tracing::Level::DEBUG,
        Ok("info") => tracing::Level::INFO,
        Ok("error") => tracing::Level::ERROR,
        _ => tracing::Level::WARN,
    };
    tracing_subscriber::fmt()
        .with_max_level(level)
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();
    match real_main() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("tb-m2venc: {e:#}");
            ExitCode::FAILURE
        }
    }
}
