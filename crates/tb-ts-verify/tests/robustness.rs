//! Whatever the bytes, the checker does not panic: random damage to a valid stream, random
//! packets with valid sync bytes, and noise.

mod common;

use common::*;
use tb_ts_verify::{Checker, Options};

/// xorshift64*: deterministic, no dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn run_all(bytes: &[u8], o: &Options) {
    let mut c = Checker::new(o.clone());
    for part in bytes.chunks(1000) {
        c.push(part);
    }
    let _ = c.report();
    let r = c.finish();
    let _ = r.to_json();
    let _ = r.summary();
    let _ = r.details();
}

#[test]
fn random_byte_damage() {
    let spec = Spec {
        secs: 1.5,
        ..Spec::default()
    };
    let clean = build(&spec).bytes();
    let mut rng = Rng(0x1234_5678_9ABC_DEF1);
    // TB_FUZZ_ROUNDS=n runs longer (by hand); the default keeps the test quick.
    let rounds = std::env::var("TB_FUZZ_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(40);
    for round in 0..rounds {
        let mut b = clean.clone();
        let hits = 1 + rng.below(2000);
        for _ in 0..hits {
            let i = rng.below(b.len());
            // Mostly header and early payload bytes, where the structure is.
            let i = if round % 2 == 0 {
                i / 188 * 188 + rng.below(24)
            } else {
                i
            };
            b[i] = rng.next() as u8;
        }
        run_all(&b, &opts(&spec));
        let mut o = opts(&spec);
        o.rate = None;
        run_all(&b, &o);
    }
}

#[test]
fn random_packets_with_sync() {
    let rounds: u64 = std::env::var("TB_FUZZ_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);
    for seed in 0..rounds {
        random_packets(0x0BAD_C0DE_1234_5678 ^ seed);
    }
}

fn random_packets(seed: u64) {
    let mut rng = Rng(seed | 1);
    let mut b = Vec::new();
    for _ in 0..20_000 {
        let mut p = [0u8; 188];
        for x in p.iter_mut() {
            *x = rng.next() as u8;
        }
        p[0] = 0x47;
        // A small set of PIDs so that tables and streams build up.
        let pid = [0u16, 1, 0x30, 0x31, 0x34, 0x1FFB, 0x1D00, 0x1E00][rng.below(8)];
        p[1] = (p[1] & 0xE0) | (pid >> 8) as u8;
        p[2] = pid as u8;
        if rng.below(4) == 0 {
            // A plausible section start.
            p[1] |= 0x40;
            p[4] = 0;
            p[5] = [0x00, 0x02, 0xC7, 0xC9, 0xCB, 0xCC, 0xCD][rng.below(7)];
            p[6] = 0xB0 | (p[6] & 0x03);
        }
        b.extend_from_slice(&p);
    }
    run_all(&b, &Options::default());
    run_all(
        &b,
        &Options {
            rate: Some(38_800_000.0),
            ..Options::default()
        },
    );
}

#[test]
fn noise() {
    let mut rng = Rng(42);
    let b: Vec<u8> = (0..500_000).map(|_| rng.next() as u8).collect();
    run_all(&b, &Options::default());
}
