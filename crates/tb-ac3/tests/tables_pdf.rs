//! Pins every normative table in `src/tables.rs` (and the window of `src/mdct.rs`) to the text
//! of the A/52:2012 PDF, as extracted by `tools/extract-tables.py` into
//! `tests/data/a52_tables.txt`. The extraction is mechanical (pdftotext, then a parser that
//! knows each table's layout); the constants were typed separately. A mismatch means one of the
//! two is wrong.
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

use std::collections::HashMap;
use tb_ac3::config::BitRate;
use tb_ac3::mdct::kbd_window_half;
use tb_ac3::tables::*;

fn pdf() -> HashMap<String, Vec<f64>> {
    include_str!("data/a52_tables.txt")
        .lines()
        .filter_map(|l| l.split_once(": "))
        .map(|(k, v)| {
            (
                k.to_string(),
                v.split_whitespace().map(|t| t.parse().unwrap()).collect(),
            )
        })
        .collect()
}

fn check<T: Copy + Into<f64>>(name: &str, ours: &[T]) {
    let t = pdf();
    let theirs = t
        .get(name)
        .unwrap_or_else(|| panic!("{name} missing from the extraction"));
    assert_eq!(theirs.len(), ours.len(), "{name}: length");
    for (i, (a, b)) in ours.iter().zip(theirs).enumerate() {
        assert_eq!((*a).into(), *b, "{name}[{i}]");
    }
}

#[test]
fn bit_allocation_parameter_tables_7_6_to_7_11() {
    check("slowdec", &SLOW_DECAY);
    check("fastdec", &FAST_DECAY);
    check("slowgain", &SLOW_GAIN);
    check("dbpbtab", &DB_PER_BIT);
    check("fastgain", &FAST_GAIN);
    // floortab[7] is printed 0xf800; we carry it as the signed 16-bit value it is, −2048.
    let floor_u16: Vec<f64> = FLOOR.iter().map(|&f| f64::from(f as u16)).collect();
    check("floortab", &floor_u16);
}

#[test]
fn banding_tables_7_12_and_7_13() {
    check("bndtab", &BAND_START);
    check("bndsz", &BAND_SIZE);
    check("masktab", &MASK_TAB);
}

#[test]
fn log_addition_table_7_14() {
    check("latab", &LOG_ADD);
}

#[test]
fn hearing_threshold_table_7_15() {
    check("hth48", &HEARING_THRESHOLD_48K);
}

#[test]
fn bap_table_7_16() {
    check("baptab", &BAP_TAB);
}

#[test]
fn window_table_7_33() {
    // The table prints five decimals: the computed KBD window must round to every entry.
    let t = pdf();
    let table = &t["window"];
    let w = kbd_window_half();
    assert_eq!(table.len(), 256);
    for (i, (ours, theirs)) in w.iter().zip(table).enumerate() {
        assert!(
            (ours - theirs).abs() <= 0.5e-5 + 1e-12,
            "w[{i}] = {ours} vs {theirs}"
        );
    }
}

#[test]
#[allow(
    clippy::needless_range_loop,
    reason = "frmsizecod is the index of three tables"
)]
fn frame_size_tables_5_18_and_7_34() {
    let t = pdf();
    // Every frmsizecod row (both codes of a pair) at 48 kHz.
    for code in 0..38usize {
        let rate = BitRate::ALL[code / 2];
        assert_eq!(
            f64::from(rate.kbps()),
            t["kbps"][code],
            "kbps of code {code}"
        );
        assert_eq!(
            f64::from(rate.frame_words()),
            t["words48"][code],
            "words of code {code}"
        );
        assert_eq!(
            (rate.five_eighths_bytes() / 2) as f64,
            t["words58_48"][code],
            "5/8 words of code {code}"
        );
    }
}
