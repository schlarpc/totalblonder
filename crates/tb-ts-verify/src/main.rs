//! Command line of the checker: `tb-ts-verify [options] FILE...` (`-` for standard input).
#![forbid(unsafe_code)]

use std::collections::BTreeSet;
use std::io::{Read as _, Write as _};
use std::path::PathBuf;
use std::process::ExitCode;

use tb_ts_verify::{Checker, Options, Report, Rule, Severity, passes, rules};

const USAGE: &str = "\
usage: tb-ts-verify [options] FILE...
       tb-ts-verify --rules-md | --rules-json

Checks MPEG-2 transport streams (188-byte packets) against TR 101 290, H.222.0, SCTE 54,
ATSC A/53 Part 3, A/52 Annex A and A/65 (PSIP on cable). FILE '-' reads standard input,
which is checked as it arrives.

options:
  --rate R            nominal TS rate in bit/s (e.g. 38800000): the time base, and the PCR
                      accuracy against it (default: the rate the PCRs give)
  --tsid N            the transport_stream_id the PAT must carry (decimal or 0x hex)
  --pcr-target-ms M   PCR interval target for PCR-INTERVAL-TARGET (30)
  --pid-timeout S     PID-MISSING period for elementary and PCR PIDs, s (1)
  --psip-pid-timeout S  PID-MISSING period for EIT and ETT PIDs, s (60)
  --no-buffer-model   do not run the T-STD and PSIP smoothing-buffer model
  --max-examples N    findings kept per rule with position and message (8)
  --details           print every kept finding
  --tables            print table repetition and buffer statistics
  --json              print one JSON document (schema tb-ts-verify/1)
  --allow ID          do not count this rule against the exit status (repeatable)
  --fail-on S         S1, S2 (default), S3, S4 or none: the least severe finding that fails
exit status: 0 no finding at or above --fail-on (allowed rules apart), 1 findings, 2 error";

#[derive(Debug, thiserror::Error)]
enum CliError {
    #[error("{0}")]
    Usage(String),
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("writing output: {0}")]
    Out(#[from] std::io::Error),
}

struct Cli {
    options: Options,
    details: bool,
    tables: bool,
    json: bool,
    allow: BTreeSet<Rule>,
    fail_on: Option<Severity>,
    paths: Vec<PathBuf>,
}

fn number(s: &str) -> Option<u64> {
    match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(h) => u64::from_str_radix(h, 16).ok(),
        None => s.parse().ok(),
    }
}

fn parse_args(args: &[String]) -> Result<Cli, CliError> {
    let mut cli = Cli {
        options: Options::default(),
        details: false,
        tables: false,
        json: false,
        allow: BTreeSet::new(),
        fail_on: Some(Severity::S2),
        paths: Vec::new(),
    };
    let mut it = args.iter();
    let value = |it: &mut std::slice::Iter<'_, String>, name: &str| -> Result<String, CliError> {
        it.next()
            .cloned()
            .ok_or_else(|| CliError::Usage(format!("{name} needs a value")))
    };
    let float = |v: &str, name: &str| -> Result<f64, CliError> {
        v.parse::<f64>()
            .ok()
            .filter(|x| *x > 0.0 && x.is_finite())
            .ok_or_else(|| CliError::Usage(format!("{name} {v}: not a positive number")))
    };
    while let Some(a) = it.next() {
        match a.as_str() {
            "--rate" => {
                let v = value(&mut it, a)?;
                cli.options.rate = Some(float(&v, a)?);
            }
            "--tsid" => {
                let v = value(&mut it, a)?;
                cli.options.tsid = Some(
                    number(&v)
                        .and_then(|n| u16::try_from(n).ok())
                        .ok_or_else(|| {
                            CliError::Usage(format!("--tsid {v}: not a 16-bit number"))
                        })?,
                );
            }
            "--pcr-target-ms" => {
                let v = value(&mut it, a)?;
                cli.options.pcr_target_ms = float(&v, a)?;
            }
            "--pid-timeout" => {
                let v = value(&mut it, a)?;
                cli.options.pid_timeout_s = float(&v, a)?;
            }
            "--psip-pid-timeout" => {
                let v = value(&mut it, a)?;
                cli.options.psip_pid_timeout_s = float(&v, a)?;
            }
            "--no-buffer-model" => cli.options.buffer_model = false,
            "--max-examples" => {
                let v = value(&mut it, a)?;
                cli.options.max_examples = v
                    .parse()
                    .map_err(|_| CliError::Usage(format!("--max-examples {v}: not a number")))?;
            }
            "--details" => cli.details = true,
            "--tables" => cli.tables = true,
            "--json" => cli.json = true,
            "--allow" => {
                let v = value(&mut it, a)?;
                cli.allow.insert(
                    rules::rule_by_id(&v)
                        .ok_or_else(|| CliError::Usage(format!("unknown rule identifier {v}")))?,
                );
            }
            "--fail-on" => {
                let v = value(&mut it, a)?;
                cli.fail_on = match v.as_str() {
                    "none" => None,
                    s => Some(Severity::from_name(s).ok_or_else(|| {
                        CliError::Usage(format!("--fail-on {v}: S1, S2, S3, S4 or none"))
                    })?),
                };
            }
            "-" => cli.paths.push(PathBuf::from("-")),
            s if s.starts_with("--") => return Err(CliError::Usage(format!("unknown option {s}"))),
            s => cli.paths.push(PathBuf::from(s)),
        }
    }
    if cli.paths.is_empty() {
        return Err(CliError::Usage("no stream given".to_string()));
    }
    Ok(cli)
}

fn check_path(path: &PathBuf, options: &Options) -> Result<Report, CliError> {
    let io = |source| CliError::Io {
        path: path.clone(),
        source,
    };
    let mut checker = Checker::new(options.clone());
    let mut buf = vec![0u8; 1 << 20];
    let mut reader: Box<dyn std::io::Read> = if path.as_os_str() == "-" {
        Box::new(std::io::stdin().lock())
    } else {
        Box::new(std::fs::File::open(path).map_err(io)?)
    };
    loop {
        let n = reader.read(&mut buf).map_err(io)?;
        if n == 0 {
            break;
        }
        checker.push(&buf[..n]);
    }
    Ok(checker.finish())
}

fn run(args: &[String]) -> Result<bool, CliError> {
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    match args.first().map(String::as_str) {
        None | Some("--help" | "-h") => {
            writeln!(out, "{USAGE}")?;
            return Ok(true);
        }
        Some("--rules-md") => {
            write!(out, "{}", rules::catalog_markdown())?;
            return Ok(true);
        }
        Some("--rules-json") => {
            let all: Vec<&rules::RuleInfo> = rules::ALL_RULES.iter().map(|r| r.info()).collect();
            let text = serde_json::to_string_pretty(&all).map_err(std::io::Error::other)?;
            writeln!(out, "{text}")?;
            return Ok(true);
        }
        _ => {}
    }
    let cli = parse_args(args)?;
    let mut all_pass = true;
    let mut docs = Vec::new();
    for path in &cli.paths {
        let report = check_path(path, &cli.options)?;
        let pass = passes(&report, cli.fail_on, &cli.allow);
        all_pass &= pass;
        if cli.json {
            let mut v = report.to_json();
            if let Some(o) = v.as_object_mut() {
                o.insert(
                    "path".into(),
                    serde_json::Value::from(path.display().to_string()),
                );
                o.insert("pass".into(), serde_json::Value::from(pass));
                o.insert(
                    "fail_on".into(),
                    serde_json::Value::from(cli.fail_on.map(|s| s.name())),
                );
                let allowed: Vec<&str> = cli.allow.iter().map(|r| r.info().id).collect();
                o.insert("allowed".into(), serde_json::Value::from(allowed));
            }
            docs.push(v);
        } else {
            writeln!(
                out,
                "{}: {}",
                path.display(),
                if pass { "ok" } else { "FAIL" }
            )?;
            write!(
                out,
                "  {}",
                report.summary().replace('\n', "\n  ").trim_end_matches(' ')
            )?;
            if cli.tables {
                write!(
                    out,
                    "  {}",
                    report
                        .tables_text()
                        .replace('\n', "\n  ")
                        .trim_end_matches(' ')
                )?;
            }
            if cli.details {
                write!(
                    out,
                    "  {}",
                    report.details().replace('\n', "\n  ").trim_end_matches(' ')
                )?;
            }
        }
    }
    if cli.json {
        let doc = if docs.len() == 1 {
            docs.pop().unwrap_or_default()
        } else {
            serde_json::Value::from(docs)
        };
        let text = serde_json::to_string_pretty(&doc).map_err(std::io::Error::other)?;
        writeln!(out, "{text}")?;
    }
    out.flush()?;
    Ok(all_pass)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(e) => {
            let _ = writeln!(std::io::stderr(), "tb-ts-verify: {e}");
            if matches!(e, CliError::Usage(_)) {
                let _ = writeln!(std::io::stderr(), "{USAGE}");
            }
            ExitCode::from(2)
        }
    }
}
