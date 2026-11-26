//! `tb-pipe`: the pipeline harness as a program. Options: `tb_pipe::opts::USAGE`.

use std::io::Write;
use std::process::ExitCode;

use anyhow::Context;
use tb_device_cuda::{CudaDevice, IMAGE};
use tb_device_ref::RefDevice;
use tb_pipe::opts::{DeviceKind, OptError, Opts, USAGE};

fn real_main() -> anyhow::Result<ExitCode> {
    // `tb-pipe --golden-taps FILE`: golden/taps.jsonl, the conversion path's tap tables and
    // color matrices (tb_convert::golden)
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let [flag, path] = args.as_slice()
        && flag == "--golden-taps"
    {
        std::fs::write(path, tb_convert::golden::taps_records())
            .with_context(|| format!("writing {path}"))?;
        return Ok(ExitCode::SUCCESS);
    }
    let o = match Opts::parse(std::env::args().skip(1)) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("{e}");
            if !matches!(e, OptError::NoChannels) {
                eprintln!("{USAGE}");
            }
            return Ok(ExitCode::from(2));
        }
    };
    let arenas = tb_pipe::arena_sizes(&o).context("sizing the arenas")?;
    let mut out = std::io::stdout().lock();
    let r = match o.device {
        DeviceKind::Cuda => {
            let dev = CudaDevice::new(o.gpu, arenas, &IMAGE).context("making the CUDA device")?;
            tb_pipe::run(&o, dev, &mut out)
        }
        DeviceKind::Ref => {
            let dev = RefDevice::new(arenas).context("making the reference device")?;
            tb_pipe::run(&o, dev, &mut out)
        }
    };
    out.flush().ok();
    match r {
        Ok(_) => Ok(ExitCode::SUCCESS),
        Err(tb_pipe::RunError::Usage(m)) => {
            eprintln!("{m}");
            Ok(ExitCode::from(2))
        }
        Err(e) => Err(e).context("the run"),
    }
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_max_level(tracing::Level::WARN)
        .init();
    match real_main() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("tb-pipe: {e:#}");
            ExitCode::FAILURE
        }
    }
}
