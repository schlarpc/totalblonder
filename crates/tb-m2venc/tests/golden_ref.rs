//! Work package G1, the sandbox end-to-end proof: the whole encoder (`tb-m2venc`'s run loop,
//! the core, the kernels compiled for the host through `tb-device-ref`) on the recorded
//! command lines of the first five golden cases. No recorded device results are used: the kernels
//! run.
//!
//! For each case the test takes the command line from the case's `golden` record, as the
//! recorder was given it (only the file names are replaced), and requires:
//! - every written stream byte-identical to the recorded one: equal to the `au.data` records of
//!   its session followed by `sequence_end_code`, and with the SHA-256 of `crates/tb-golden/vectors/<case>.sha256`;
//! - every access unit's fields (types, flags, times, sizes, quantizer, complexity, buffer
//!   state, rung, passes, macroblock counts) equal to the golden `au` record, through the
//!   driver's own `--golden` output;
//! - the host-arena rule holds for every plan (`Guarded`).
//!
//! Floating point: the device image and the host build of the kernels differ in the last bit
//! of four kernels until work package K3 (docs/ARCHITECTURE.md §7.3). On these five cases the
//! difference never reaches a coded byte or a host-side decision: all six streams match
//! exactly, so nothing here is ignored or weakened for K3.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers outside #[test] functions"
)]

mod common;

use common::{Scratch, sha256};
use tb_golden::Record;
use tb_m2venc::Status;

/// The recorded command line with this test's file names.
fn command(recs: &[Record], dir: &Scratch, case: &str) -> String {
    let args = recs[0].text("args").unwrap();
    let mut out = Vec::new();
    let mut it = args.split(' ');
    while let Some(w) = it.next() {
        out.push(w.to_owned());
        match w {
            "--input" => {
                let clip = it.next().unwrap();
                let name = clip.trim_start_matches("../").trim_end_matches(".yuv");
                out.push(dir.clip(name));
            }
            "--output" => {
                assert_eq!(it.next(), Some(case));
                out.push(dir.path(case));
            }
            "--golden" => {
                it.next().unwrap();
                out.push(dir.path("au.jsonl"));
            }
            _ => {}
        }
    }
    out.join(" ")
}

fn run_case(case: &str) {
    let recs = tb_golden::case(case).unwrap();
    let dir = Scratch::new(case);
    let out = common::encode_args(&command(&recs, &dir, case));
    assert_eq!(out.status, Status::Complete, "{case}");

    // Per session in coded order: inside a tick the golden records are in the order the
    // pictures were made (repeats first), the driver's in the order it takes them.
    let by_session = |v: &mut Vec<&Record>| {
        v.sort_by_key(|r| (r.int("s").unwrap(), r.int("coded").unwrap()));
    };
    let mut gold: Vec<&Record> = recs.iter().filter(|r| r.kind() == "au").collect();
    by_session(&mut gold);
    let sessions = recs.iter().filter(|r| r.kind() == "open").count();
    let digests = tb_golden::digests(case);
    assert_eq!(digests.len(), sessions, "{case}: every session is written");
    assert_eq!(out.report.pictures as usize, gold.len(), "{case}: pictures");

    // The access units, field by field.
    let ours = tb_golden::parse(case, &String::from_utf8(dir.read("au.jsonl")).unwrap()).unwrap();
    let mut ours: Vec<&Record> = ours.iter().filter(|r| r.kind() == "au").collect();
    by_session(&mut ours);
    assert_eq!(ours.len(), gold.len(), "{case}: access units");
    for (i, (a, g)) in ours.iter().zip(&gold).enumerate() {
        for f in [
            "tick",
            "s",
            "coded",
            "disp",
            "type",
            "key",
            "off_grid",
            "repeat",
            "key_missed",
            "len",
            "pts",
            "dts",
            "bits",
            "rate",
            "ticket",
            "rung",
            "tries",
            "intra",
            "field",
        ] {
            assert_eq!(a.int(f).unwrap(), g.int(f).unwrap(), "{case}: au {i}: {f}");
        }
        for f in ["avg_q", "complexity", "queue", "room", "cap"] {
            assert_eq!(a.num(f).unwrap(), g.num(f).unwrap(), "{case}: au {i}: {f}");
        }
        assert_eq!(
            a.text("fnv").unwrap(),
            g.text("fnv").unwrap(),
            "{case}: au {i} (session {}, coded {}): the bytes differ",
            g.int("s").unwrap(),
            g.int("coded").unwrap()
        );
        assert_eq!(a.bytes("data").unwrap(), g.bytes("data").unwrap());
    }

    // The streams as files.
    for (s, digest) in digests.iter().enumerate() {
        let name = if sessions > 1 {
            format!("{case}.{s}.m2v")
        } else {
            format!("{case}.m2v")
        };
        let got = dir.read(&name);
        let mut want: Vec<u8> = gold
            .iter()
            .filter(|r| r.int("s").unwrap() == s as i64)
            .flat_map(|r| r.bytes("data").unwrap())
            .collect();
        want.extend_from_slice(&[0, 0, 1, 0xB7]);
        if got != want {
            let at = got.iter().zip(&want).position(|(a, b)| a != b);
            panic!(
                "{name}: {} bytes, the recorded {}; first difference at {at:?}",
                got.len(),
                want.len()
            );
        }
        assert_eq!(sha256(&got), *digest, "{name}");
    }
}

#[test]
fn cbr_is_byte_identical_on_the_reference_adapter() {
    run_case("cbr");
}

#[test]
fn ladder_is_byte_identical_on_the_reference_adapter() {
    run_case("ladder");
}

#[test]
fn fixedq_is_byte_identical_on_the_reference_adapter() {
    run_case("fixedq");
}

#[test]
fn interlaced_is_byte_identical_on_the_reference_adapter() {
    run_case("interlaced");
}

#[test]
fn shed_is_byte_identical_on_the_reference_adapter() {
    run_case("shed");
}

#[test]
fn film_is_byte_identical_on_the_reference_adapter() {
    run_case("film");
    run_case("film60");
}

/// The table record the kernels read, as `tb-m2venc --golden-tables` writes it, is the
/// recorded one (`crates/tb-golden/vectors/tables.jsonl`).
#[test]
fn the_tables_are_the_recorded_ones() {
    let t = tb_m2v_syntax::tables::tables();
    let gold = tb_golden::case("tables").unwrap();
    assert_eq!(gold[1].bytes("bytes").unwrap(), bytemuck::bytes_of(&t));
}
