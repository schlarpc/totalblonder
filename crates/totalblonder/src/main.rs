//! `totalblonder --config FILE [--offline SECONDS --out DIR] [--duration SECONDS] [--check]`
#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "a command-line program"
)]

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, bail};
use totalblonder::{config, runtime};

const USAGE: &str =
    "totalblonder --config FILE [--check] [--offline SECONDS --out DIR] [--duration SECONDS]";

fn main() -> ExitCode {
    match real_main() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("totalblonder: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn real_main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let mut cfg: Option<PathBuf> = std::env::var_os("TOTALBLONDER_CONFIG").map(PathBuf::from);
    let mut offline: Option<f64> = None;
    let mut out: Option<PathBuf> = None;
    let mut duration: Option<f64> = None;
    let mut check = false;
    while let Some(a) = args.next() {
        let mut val = || args.next().with_context(|| format!("{a} needs a value"));
        match a.as_str() {
            "--config" => cfg = Some(PathBuf::from(val()?)),
            "--offline" => offline = Some(val()?.parse().context("--offline SECONDS")?),
            "--out" => out = Some(PathBuf::from(val()?)),
            "--duration" => duration = Some(val()?.parse().context("--duration SECONDS")?),
            "--check" => check = true,
            "--help" | "-h" => {
                println!("{USAGE}");
                return Ok(());
            }
            x => bail!("unknown option {x}\n{USAGE}"),
        }
    }
    let path = cfg.with_context(|| format!("no configuration\n{USAGE}"))?;
    let text = std::fs::read_to_string(&path).with_context(|| path.display().to_string())?;
    let plant = config::load(&text).with_context(|| path.display().to_string())?;
    if check {
        println!(
            "{}: valid ({} carriers)",
            path.display(),
            plant.carriers.len()
        );
        return Ok(());
    }
    // Telemetry starts once the configuration says where it goes; nothing logs before.
    let tel = tb_telemetry::init(&plant.telemetry.export)?;
    let run = tb_telemetry::wide!(
        "server.run",
        config = path.as_path(),
        mode = if offline.is_some() {
            "offline"
        } else {
            "realtime"
        },
        carriers = plant.carriers.len(),
        programs = plant
            .carriers
            .iter()
            .map(|c| c.programs.len())
            .sum::<usize>(),
        inputs = plant.inputs.len(),
        "video.device" = format!("{:?}", plant.video),
        statmux = plant.statmux.is_some(),
        device = plant.device.is_some(),
    );
    let _run = run.enter();
    tb_telemetry::wide!("server.start").emit();
    let mode = match offline {
        Some(s) => runtime::Mode::Offline {
            duration: Duration::from_secs_f64(s),
            dir: out.context("--offline needs --out DIR")?,
        },
        None => runtime::Mode::Realtime {
            duration: duration.map(Duration::from_secs_f64),
        },
    };
    let r = runtime::run(plant, mode, &tel);
    if let Err(e) = &r {
        run.error(format!("{e:#}"));
    }
    r
}
