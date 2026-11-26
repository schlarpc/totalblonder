//! Cost of the encoder's stages in isolation (per call, nanoseconds), for profiling without a
//! profiler.
#![allow(clippy::print_stdout)]

use std::hint::black_box;
use std::time::Instant;
use tb_ac3::bitalloc::{self, Mask, NOMINAL_FAST_GAIN, Params, SnrOffset};
use tb_ac3::exponents;
use tb_ac3::mdct::{Mdct, Scratch};
use tb_ac3::transient::TransientDetector;

fn time(name: &str, n: usize, mut f: impl FnMut(usize)) {
    let t = Instant::now();
    for i in 0..n {
        f(i);
    }
    println!("{name}: {:.0} ns", t.elapsed().as_nanos() as f64 / n as f64);
}

fn main() {
    let mdct = Mdct::new();
    let mut s = Scratch::default();
    let input: [f32; 512] = std::array::from_fn(|i| ((i * 7919) % 1000) as f32 / 1000.0 - 0.5);
    let mut out = [0.0f32; 256];
    time("mdct long", 200_000, |_| {
        mdct.forward(black_box(&input), false, &mut out, &mut s);
        black_box(&out);
    });
    time("mdct short", 200_000, |_| {
        mdct.forward(black_box(&input), true, &mut out, &mut s);
        black_box(&out);
    });
    let mut d = TransientDetector::new();
    let blk: [f32; 256] = std::array::from_fn(|i| input[i]);
    time("transient detect", 200_000, |_| {
        black_box(d.detect(black_box(&blk)));
    });
    let mut raw = [0u8; 256];
    exponents::extract(&out, 253, &mut raw);
    time("exponent extract", 200_000, |_| {
        exponents::extract(black_box(&out), 253, &mut raw);
    });
    let mut e = raw;
    time("exponent process D15", 200_000, |_| {
        e = raw;
        exponents::process(&mut e, 253, exponents::ExpStrategy::D15);
    });
    let mut mask = Mask::default();
    time("compute_mask", 200_000, |_| {
        bitalloc::compute_mask(
            black_box(&e),
            253,
            NOMINAL_FAST_GAIN,
            &Params::NOMINAL,
            &mut mask,
        );
    });
    let mut bap = [0u8; 256];
    time("compute_bap", 200_000, |i| {
        let snr = SnrOffset::from_index((i % 1024) as u16);
        black_box(bitalloc::compute_bap(&mask, snr.value(), 496, &mut bap));
    });
}
