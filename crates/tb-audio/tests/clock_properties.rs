//! Property tests of the sample clock over seeded random input: block sizes, gaps, timestamp
//! jitter, overlaps, restarts, non-finite samples, source changes. No property-testing crate
//! (as in `tb-timeline`): SplitMix64 with fixed seeds, so a failure names its seed.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

use tb_audio::clock::{AudioFrame, ClockConfig, FADE, FRAME, FRAME_90K, SampleClock, XFADE};
use tb_audio::layout::OutputLayout;
use tb_audio::signal::Rng;
use tb_timeline::{Source, Ticks27};

/// What a scenario may do.
#[derive(Clone, Copy)]
struct Chaos {
    /// Restarts and jumps may land anywhere, including at the play head.
    anywhere: bool,
    /// Non-finite samples in the input.
    nonfinite: bool,
}

/// What a scenario exercised: discontinuities, replaced, late, switches, underruns.
type Exercised = [u64; 5];

/// Runs `frames` frames of a random scenario and checks the invariants frame by frame.
fn scenario(seed: u64, frames: u64, chaos: Chaos, layout: OutputLayout) -> Exercised {
    let mut r = Rng(seed);
    // P0 near the 33-bit wrap of the 90 kHz PTS (2^33 · 300 ticks of 27 MHz).
    let p0 = Ticks27((1i64 << 33) * 300 - 864_000 * 50);
    let mut c = SampleClock::new(ClockConfig::new(p0, layout)).unwrap();
    let ch = layout.count();
    let mut f = AudioFrame::new(layout);
    // Per source: the next sample position its producer believes in, and its DC level.
    let mut pos = [0i64, 0i64];
    let level = [0.5f32, -0.375f32];
    let mut prev_pts90: Option<u64> = None;
    let mut prev = 0.0f32;
    for j in 0..frames {
        // Producers run about 1 s ahead of the play head, in blocks of random size, with
        // jitter, gaps and restarts.
        let head = (j as i64) * FRAME as i64;
        for (i, src) in [Source::Live, Source::Standby].into_iter().enumerate() {
            while pos[i] < head + 48_000 {
                let n = 1 + r.below(3000) as usize;
                let jitter = r.below(97) as i64 - 48;
                let mut stamp = pos[i] + jitter;
                match r.below(200) {
                    0 => pos[i] += r.below(20_000) as i64, // a gap
                    1 => {
                        // a restart, backwards
                        c.end(src);
                        let back = r.below(40_000) as i64;
                        pos[i] = if chaos.anywhere {
                            pos[i] - back
                        } else {
                            (pos[i] - back).max(head + (FRAME + FADE) as i64 * 2)
                        };
                    }
                    2 if chaos.anywhere => stamp = head + r.below(200_000) as i64 - 50_000,
                    3 => {
                        // a stall: this producer sends nothing for a while
                        pos[i] = head + 48_000 + r.below(96_000) as i64;
                        break;
                    }
                    _ => {}
                }
                if r.below(2) == 0 && i == 1 {
                    // the standby is often absent
                    pos[i] += n as i64;
                    continue;
                }
                let mut planes = vec![vec![level[i]; n]; ch];
                if chaos.nonfinite && r.below(50) == 0 {
                    planes[0][n / 2] = f32::NAN;
                    planes[ch - 1][0] = f32::INFINITY;
                }
                let refs: Vec<&[f32]> = planes.iter().map(|p| p.as_slice()).collect();
                c.push(src, c.time_of(stamp), &refs).unwrap();
                pos[i] += n as i64;
            }
        }
        if r.below(100) == 0 {
            let to = [Source::Live, Source::Standby, Source::Slate][r.below(3) as usize];
            let at = head + r.below(4000) as i64;
            c.switch(to, c.time_of(at));
        }
        let info = c.next_frame(&mut f);
        // Cadence: frame j, PTS = P0 + 2880·j at 90 kHz, strictly 2880 apart (mod 2^33).
        assert_eq!(info.index, j, "seed {seed}");
        assert_eq!(info.pts.0, p0.0 + 864_000 * j as i64, "seed {seed}");
        let p90 = info.pts90();
        if let Some(pp) = prev_pts90 {
            assert_eq!((p90 + (1 << 33) - pp) % (1 << 33), FRAME_90K, "seed {seed}");
        }
        prev_pts90 = Some(p90);
        assert_eq!(f.channels(), ch);
        let planes = f.planes();
        assert_eq!(planes.len(), ch);
        for p in planes.iter() {
            assert_eq!(p.len(), FRAME);
            for &v in p.iter() {
                assert!(v.is_finite(), "seed {seed}");
                assert!(v.abs() <= 0.5, "seed {seed}: {v}");
            }
        }
        if !chaos.anywhere {
            // No clicks: inputs are DC levels, so the output may only move by a fade step
            // (1/480 of a level) plus a crossfade step (π/2/960 of the difference).
            let bound = 0.5 / FADE as f32 + 0.875 * std::f32::consts::FRAC_PI_2 / XFADE as f32;
            for &v in planes[0].iter() {
                assert!(
                    (v - prev).abs() <= bound * 1.01,
                    "seed {seed} frame {j}: step {} > {bound}",
                    (v - prev).abs()
                );
                prev = v;
            }
        }
    }
    let lc = c.source_counters(Source::Live);
    let sc = c.source_counters(Source::Standby);
    [
        lc.discontinuities + sc.discontinuities,
        lc.replaced + sc.replaced,
        lc.late + sc.late,
        c.counters().switches,
        c.counters().underruns,
    ]
}

fn add(a: &mut Exercised, b: Exercised) {
    for (x, y) in a.iter_mut().zip(b) {
        *x += y;
    }
}

#[test]
fn cadence_bounds_and_no_clicks_under_jitter_gaps_and_restarts() {
    let mut seen = Exercised::default();
    for seed in 0..12 {
        let e = scenario(
            seed,
            1_500,
            Chaos {
                anywhere: false,
                nonfinite: false,
            },
            OutputLayout::Stereo,
        );
        add(&mut seen, e);
    }
    // The scenarios did exercise restarts, overlaps, switches and running dry.
    assert!(
        seen[0] > 10 && seen[1] > 0 && seen[3] > 10 && seen[4] > 0,
        "{seen:?}"
    );
}

#[test]
fn cadence_and_bounds_under_chaos_and_nonfinite_input() {
    let mut seen = Exercised::default();
    for seed in 100..110 {
        let e = scenario(
            seed,
            1_000,
            Chaos {
                anywhere: true,
                nonfinite: true,
            },
            if seed % 2 == 0 {
                OutputLayout::FivePointOne
            } else {
                OutputLayout::Stereo
            },
        );
        add(&mut seen, e);
    }
    assert!(seen.iter().all(|&v| v > 0), "{seen:?}");
}

/// Contiguous input with jittered stamps, always ahead of the play head, comes out exactly,
/// sample for sample, after the initial fade.
#[test]
fn contiguous_input_is_reproduced_exactly() {
    let mut r = Rng(42);
    let p0 = Ticks27(5_000_000);
    let mut c = SampleClock::new(ClockConfig::new(p0, OutputLayout::Stereo)).unwrap();
    c.switch(Source::Live, p0);
    let total = 48_000 * 20;
    let x: Vec<f32> = (0..total).map(|_| (r.uniform() - 0.5) as f32).collect();
    let mut f = AudioFrame::new(OutputLayout::Stereo);
    let mut out = Vec::new();
    let mut sent = 0usize;
    let frames = total / FRAME;
    for j in 0..frames {
        while sent < total && sent < (j + 30) * FRAME {
            let n = (1 + r.below(2047) as usize).min(total - sent);
            // The first block's stamp is the anchor; the rest are jittered.
            let jitter = if sent == 0 {
                0
            } else {
                r.below(97) as i64 - 48
            };
            let b = &x[sent..sent + n];
            c.push(Source::Live, c.time_of(sent as i64 + jitter), &[b, b])
                .unwrap();
            sent += n;
        }
        c.next_frame(&mut f);
        out.extend_from_slice(f.plane(0));
    }
    assert_eq!(
        out[XFADE..frames * FRAME - FADE],
        x[XFADE..frames * FRAME - FADE]
    );
    assert_eq!(c.counters().underruns, 0);
    assert_eq!(c.source_counters(Source::Live).discontinuities, 0);
}

/// The cadence is arithmetic, not accumulated: frame j's PTS is exact after 30 days.
#[test]
fn cadence_is_exact_after_30_days() {
    let p0 = Ticks27(123);
    let c = SampleClock::new(ClockConfig::new(p0, OutputLayout::Stereo)).unwrap();
    let j = 30 * 86_400 * 48_000 / 1536; // 81 million frames
    assert_eq!(c.pts_of(j).0, 123 + 864_000 * j as i64);
    assert_eq!(c.time_of(j as i64 * 1536), c.pts_of(j));
}
