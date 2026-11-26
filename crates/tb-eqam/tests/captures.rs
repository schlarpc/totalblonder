//! Parse a directory of pages captured from the live device (opt-in; no network).
//!
//! ```text
//! TB_EQAM_CAPTURES=/path/to/dir cargo nextest run -p tb-eqam --run-ignored only captures
//! ```
//!
//! File names pick the parser by prefix: `status`, `statusedit`, `qamconfig*1` … `qamconfig*4`
//! (the digit before the extension is the module), `streaminfo`, `diagnostic`, `debug`,
//! `errorlog`, `logged_out`, `login`. Each file must parse; a summary goes to stderr.
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

use tb_eqam::ReadPage;
use tb_eqam::client::Observation;
use tb_eqam::html::Document;
use tb_eqam::pages::{PageClass, classify};
use tb_eqam::types::RfModule;

#[test]
#[ignore = "needs TB_EQAM_CAPTURES"]
fn captures() {
    let Ok(dir) = std::env::var("TB_EQAM_CAPTURES") else {
        panic!("set TB_EQAM_CAPTURES to a directory of saved pages");
    };
    let mut n = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let body = std::fs::read(&path).unwrap();
        let stem = name.split('.').next().unwrap_or("");
        let page = if name.starts_with("statusedit") {
            Some(ReadPage::StatusEdit)
        } else if name.starts_with("status") {
            Some(ReadPage::Status)
        } else if name.starts_with("qamconfig") {
            let digit = name
                .rsplit('.')
                .nth(1)
                .and_then(|s| s.chars().last())
                .and_then(|c| c.to_digit(10))
                .unwrap_or(1);
            Some(ReadPage::QamConfig(
                RfModule::new(u8::try_from(digit).unwrap()).unwrap(),
            ))
        } else if name.starts_with("streaminfo") {
            Some(ReadPage::StreamInfo)
        } else if name.starts_with("diagnostic") {
            Some(ReadPage::Diagnostic)
        } else if name.starts_with("debug") {
            Some(ReadPage::Debug)
        } else if name.starts_with("errorlog") {
            Some(ReadPage::EventLog)
        } else {
            None
        };
        match page {
            Some(p) => {
                let o = Observation::parse(p, &body).unwrap_or_else(|e| panic!("{name}: {e}"));
                eprintln!("{name}: {o:#?}");
            }
            None if stem.starts_with("logged_out") => {
                assert_eq!(
                    classify(&Document::parse(&body)),
                    PageClass::LoggedOut,
                    "{name}"
                );
                eprintln!("{name}: Logged Out page recognized");
            }
            None if stem.starts_with("login") => {
                eprintln!("{name}: {:?}", classify(&Document::parse(&body)));
            }
            None => {
                eprintln!("{name}: skipped (unknown prefix)");
                continue;
            }
        }
        n += 1;
    }
    assert!(n > 0, "no recognized files in {dir}");
}
