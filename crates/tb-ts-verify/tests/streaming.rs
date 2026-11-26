//! The checker is incremental: any chunking of the input gives the same report, a report can
//! be taken mid-stream, and findings can be drained as they happen.

mod common;

use common::*;
use tb_ts_verify::{Checker, Options};

/// A stream with a few faults spread over it, so that the comparison is not of empty reports.
fn faulty() -> (Vec<u8>, Options) {
    let spec = Spec::default();
    let mut s = build(&spec);
    let a = s.of_pid(0x34)[150];
    s.null(a);
    let i = s.null_after(9000);
    s.packets[i][0] = 0x46;
    let p = s.pcr_packets(0x31)[50];
    let pcr = get_pcr(&s.packets[p][6..12]) + 100;
    put_pcr(&mut s.packets[p][6..12], pcr);
    (s.bytes(), opts(&spec))
}

#[test]
fn chunking_does_not_matter() {
    let (bytes, o) = faulty();
    let whole = tb_ts_verify::check(&bytes, &o);
    assert!(!whole.counts.is_empty());
    for chunk in [1usize, 7, 187, 188, 189, 1000, 65536] {
        let mut c = Checker::new(o.clone());
        for part in bytes.chunks(chunk) {
            c.push(part);
        }
        let r = c.finish();
        assert_eq!(r.counts, whole.counts, "chunk {chunk}");
        assert_eq!(r.findings, whole.findings, "chunk {chunk}");
        assert_eq!(r.stats.packets, whole.stats.packets, "chunk {chunk}");
    }
}

#[test]
fn report_mid_stream_and_events() {
    let (bytes, mut o) = faulty();
    o.events_capacity = 64;
    let mut c = Checker::new(o);
    let half = bytes.len() / 2 / 188 * 188;
    c.push(&bytes[..half]);
    let r = c.report();
    assert!(r.stats.packets > 0);
    let mut events = c.drain_events();
    c.push(&bytes[half..]);
    events.extend(c.drain_events());
    let total: u64 = c.counts().values().sum();
    assert_eq!(events.len() as u64, total);
    let end = c.finish();
    assert!(end.stats.packets as usize * 188 == bytes.len());
}

#[test]
fn garbage_before_the_stream_is_skipped() {
    let spec = Spec::default();
    let s = build(&spec);
    let mut bytes = vec![0x47u8, 0x00, 0x11, 0x47, 0x47];
    bytes.extend(s.bytes());
    let r = tb_ts_verify::check(&bytes, &opts(&spec));
    // The leading bytes shift positions but nothing is wrong with the stream itself.
    assert_eq!(r.stats.bytes_out_of_sync, 5);
    assert!(serious(&r).is_empty(), "{}", r.summary());
}

/// Writes the synthetic streams to `$TB_EXPORT_DIR` for the validation with TSDuck
/// (notes/ts-verify-log.md). Run with
/// `TB_EXPORT_DIR=/some/dir cargo nextest run -p tb-ts-verify --run-ignored only export`.
#[test]
#[ignore = "writes files; run by hand for the validation log"]
fn export_synthetic() {
    let Ok(dir) = std::env::var("TB_EXPORT_DIR") else {
        return;
    };
    let mut one = Spec {
        rate: 38_800_000,
        secs: 10.0,
        ..Spec::default()
    };
    one.programs[0].video_pid = 0x101;
    one.programs[0].pcr_pid = 0x101;
    one.programs[0].audio_pid = Some(0x104);
    std::fs::write(format!("{dir}/synthetic.ts"), build(&one).bytes()).unwrap();
    let mut two = one.clone();
    two.programs.push(Prog::new(2));
    two.eit1 = true;
    std::fs::write(format!("{dir}/synthetic2.ts"), build(&two).bytes()).unwrap();
}
