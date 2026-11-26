//! Command line of the verifier: `tb-m2v-verify [options] FILE|DIR...`.
#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use tb_m2v_verify::{Delivery, Options, Profile, Report, Rule, Severity, rules, verify};

const USAGE: &str = "\
usage: tb-m2v-verify [options] FILE|DIR...
       tb-m2v-verify --rules-md | --rules-json

Verifies MPEG-2 video elementary streams against H.262 and, with --profile, against
A/53 Part 4 or SCTE 43. A directory stands for every *.m2v, *.mpv and *.mpg2 file in it,
recursively.

options:
  --profile P        h262-only (default), a53, scte43-64qam, scte43-256qam
  --delivery M       normative (default): Annex C as coded
                     operating[:RATE]: also the project's operating model (pictures become
                     available at their coding time and are sent at RATE bit/s, default the
                     bit_rate of the sequence header)
  --max-examples N   findings kept per rule and stream, with position and message (8)
  --details          print every kept finding
  --json             print one JSON document instead of text
  --quiet            print only streams with findings and the totals
  --allow ID         do not count this rule against the exit status (repeatable)
  --allow-file F     a file of rule identifiers to allow, one per line, '#' starts a comment
  --fail-on S        S1, S2 (default), S3, S4 or none: the least severe finding that fails
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
    json: bool,
    quiet: bool,
    allow: BTreeSet<Rule>,
    fail_on: Option<Severity>,
    paths: Vec<PathBuf>,
}

fn rule_arg(id: &str) -> Result<Rule, CliError> {
    rules::rule_by_id(id).ok_or_else(|| CliError::Usage(format!("unknown rule identifier {id}")))
}

fn parse_args(args: &[String]) -> Result<Cli, CliError> {
    let mut cli = Cli {
        options: Options::default(),
        details: false,
        json: false,
        quiet: false,
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
    while let Some(a) = it.next() {
        match a.as_str() {
            "--profile" => {
                let v = value(&mut it, a)?;
                cli.options.profile = Profile::from_name(&v)
                    .ok_or_else(|| CliError::Usage(format!("unknown profile {v}")))?;
            }
            "--delivery" => {
                let v = value(&mut it, a)?;
                cli.options.delivery = match v.as_str() {
                    "normative" => Delivery::Normative,
                    "operating" => Delivery::Operating { rate: None },
                    other => match other.strip_prefix("operating:").map(str::parse::<f64>) {
                        Some(Ok(r)) if r > 0.0 => Delivery::Operating { rate: Some(r) },
                        _ => return Err(CliError::Usage(format!("unknown delivery mode {v}"))),
                    },
                };
            }
            "--max-examples" => {
                let v = value(&mut it, a)?;
                cli.options.max_examples = v
                    .parse()
                    .map_err(|_| CliError::Usage(format!("--max-examples {v}: not a number")))?;
            }
            "--details" => cli.details = true,
            "--json" => cli.json = true,
            "--quiet" => cli.quiet = true,
            "--allow" => {
                let v = value(&mut it, a)?;
                cli.allow.insert(rule_arg(&v)?);
            }
            "--allow-file" => {
                let v = value(&mut it, a)?;
                let path = PathBuf::from(&v);
                let text = std::fs::read_to_string(&path)
                    .map_err(|source| CliError::Io { path, source })?;
                for line in text.lines() {
                    let id = line.split('#').next().unwrap_or("").trim();
                    if !id.is_empty() {
                        cli.allow.insert(rule_arg(id)?);
                    }
                }
            }
            "--fail-on" => {
                let v = value(&mut it, a)?;
                cli.fail_on = match v.as_str() {
                    "S1" => Some(Severity::S1),
                    "S2" => Some(Severity::S2),
                    "S3" => Some(Severity::S3),
                    "S4" => Some(Severity::S4),
                    "none" => None,
                    _ => {
                        return Err(CliError::Usage(format!(
                            "--fail-on {v}: S1, S2, S3, S4 or none"
                        )));
                    }
                };
            }
            s if s.starts_with("--") => return Err(CliError::Usage(format!("unknown option {s}"))),
            s => cli.paths.push(PathBuf::from(s)),
        }
    }
    if cli.paths.is_empty() {
        return Err(CliError::Usage("no stream given".to_string()));
    }
    Ok(cli)
}

fn collect(path: &Path, out: &mut Vec<PathBuf>) -> Result<(), CliError> {
    let io = |source| CliError::Io {
        path: path.to_path_buf(),
        source,
    };
    if path.is_dir() {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(path)
            .map_err(io)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .collect();
        entries.sort();
        for e in entries {
            if e.is_dir() {
                collect(&e, out)?;
            } else if matches!(
                e.extension().and_then(|x| x.to_str()),
                Some("m2v" | "mpv" | "mpg2")
            ) {
                out.push(e);
            }
        }
    } else {
        out.push(path.to_path_buf());
    }
    Ok(())
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
    let mut files = Vec::new();
    for p in &cli.paths {
        collect(p, &mut files)?;
    }
    let mut totals: BTreeMap<Rule, (u64, u64)> = BTreeMap::new();
    let mut features: BTreeMap<&'static str, (u64, u64)> = BTreeMap::new();
    let mut json_streams = Vec::new();
    let mut failed = 0u64;
    for path in &files {
        let data = std::fs::read(path).map_err(|source| CliError::Io {
            path: path.clone(),
            source,
        })?;
        let report: Report = verify(&data, &cli.options);
        let fails = report
            .counts
            .keys()
            .any(|r| !cli.allow.contains(r) && cli.fail_on.is_some_and(|s| r.info().severity <= s));
        if fails {
            failed += 1;
        }
        for (r, n) in &report.counts {
            let e = totals.entry(*r).or_insert((0, 0));
            e.0 += n;
            e.1 += 1;
        }
        for (k, n) in &report.features {
            let e = features.entry(k).or_insert((0, 0));
            if k.starts_with("max ") {
                e.0 = e.0.max(*n);
            } else {
                e.0 += n;
            }
            e.1 += 1;
        }
        if cli.json {
            let mut v = report.to_json();
            if let Some(o) = v.as_object_mut() {
                o.insert(
                    "path".to_string(),
                    serde_json::Value::from(path.display().to_string()),
                );
                o.insert("pass".to_string(), serde_json::Value::from(!fails));
            }
            json_streams.push(v);
        } else if !(cli.quiet && report.counts.is_empty()) {
            writeln!(
                out,
                "{}: {}",
                path.display(),
                if fails { "FAIL" } else { "ok" }
            )?;
            write!(
                out,
                "  {}",
                report.summary().replace('\n', "\n  ").trim_end_matches(' ')
            )?;
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
        let by_rule: serde_json::Map<String, serde_json::Value> = totals
            .iter()
            .map(|(r, (n, f))| {
                (
                    r.info().id.to_string(),
                    serde_json::json!({ "severity": r.info().severity.name(), "count": n, "streams": f, "allowed": cli.allow.contains(r) }),
                )
            })
            .collect();
        let feats: serde_json::Map<String, serde_json::Value> = features
            .iter()
            .map(|(k, (n, f))| {
                (
                    (*k).to_string(),
                    serde_json::json!({ "count": n, "streams": f }),
                )
            })
            .collect();
        let doc = serde_json::json!({ "streams": json_streams, "totals": by_rule, "features": feats, "files": files.len(), "failed": failed });
        let text = serde_json::to_string_pretty(&doc).map_err(std::io::Error::other)?;
        writeln!(out, "{text}")?;
    } else {
        writeln!(out, "== {} streams, {} failed", files.len(), failed)?;
        if !totals.is_empty() {
            writeln!(
                out,
                "{:<26} {:<3} {:>10} {:>8}  rule",
                "rule", "sev", "count", "streams"
            )?;
            for (r, (n, f)) in &totals {
                let i = r.info();
                writeln!(
                    out,
                    "{:<26} {:<3} {:>10} {:>8}  {}{}",
                    i.id,
                    i.severity.name(),
                    n,
                    f,
                    if cli.allow.contains(r) {
                        "(allowed) "
                    } else {
                        ""
                    },
                    i.statement
                )?;
            }
        }
    }
    out.flush()?;
    Ok(failed == 0)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(e) => {
            let _ = writeln!(std::io::stderr(), "tb-m2v-verify: {e}");
            if matches!(e, CliError::Usage(_)) {
                let _ = writeln!(std::io::stderr(), "{USAGE}");
            }
            ExitCode::from(2)
        }
    }
}
