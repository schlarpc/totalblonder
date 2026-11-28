//! Which size of thread block the transform kernel runs fastest in on the card at hand.
//!
//! `k_xf` exists in four sizes (`tb_device::kernel::XF_BLOCKS`). Two cards were measured and
//! they disagree by a fifth of the encoder's throughput (`tb_kernel_abi::XB`, `XB_HIP`), so
//! the value a device starts from is a guess for every other card. [`xf_blocks`] times the
//! sizes at start: the real encoder on the sessions it is about to run, fed a moving synthetic
//! picture, a few ticks per size, the sizes in turn over several rounds.
//!
//! The measure is the median tick of a size, which a stall of the card does not move (on the
//! AMD card the driver evicts a process's queues for 100 ms now and then). The first round only
//! warms the card up. A size replaces the device's usual one when it is at least 3 % faster:
//! below that two runs of the same size differ as much. Coded streams do not depend on the size.

use std::time::{Duration, Instant};

use tb_device::kernel::XF_BLOCKS;
use tb_m2v_encoder::{EncoderOptions, FrameSource, SessionConfig, SessionId};

use crate::device::TickDevice;
use crate::engine::Engine;

/// Rounds over the sizes; the first is not counted.
const ROUNDS: usize = 4;
/// Ticks per size and round, and how many of them are left out at the start (the first
/// pictures of a session are not what it codes later).
const TICKS: usize = 24;
const SKIP: usize = 8;
/// Macroblocks per tick the sessions of the timing may have together (four 1080-line pictures):
/// more sessions make the timing longer, not better.
const MAX_MBS: u64 = 4 * 8160;
/// No further round is begun that would end later than this (one counted round is always run).
const BUDGET: Duration = Duration::from_millis(2500);
/// A size is chosen over the device's usual one when its median tick is below this share of it.
const GAIN: f64 = 0.97;

/// What a timing found.
#[derive(Clone, Debug, PartialEq)]
pub struct Calibration {
    /// The size to run with.
    pub chosen: u32,
    /// The device's usual size.
    pub usual: u32,
    /// Median tick per size in milliseconds, in the order of `XF_BLOCKS`; empty when nothing
    /// was timed.
    pub median_ms: Vec<(u32, f64)>,
    /// Why nothing was timed, or why the timing was given up.
    pub skipped: Option<String>,
    /// How long the timing took.
    pub took: Duration,
}

/// The synthetic picture: a 256 x 256 tile of blocky texture and noise that moves three pixels
/// to the left and one up per frame (planar 4:2:0, `w` x `h` luma), so that the search finds
/// vectors and the transform has coefficients to store.
struct Tile(Vec<u8>);

impl Tile {
    fn new() -> Self {
        let mut s = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s >> 32) as u8
        };
        let coarse: Vec<u8> = (0..256).map(|_| next()).collect();
        let t = (0..256 * 256)
            .map(|i| {
                let (x, y) = (i & 255, i >> 8);
                let c = u16::from(coarse[(y >> 4) * 16 + (x >> 4)]);
                ((3 * c + u16::from(next())) / 4) as u8
            })
            .collect();
        Self(t)
    }

    fn plane(&self, out: &mut [u8], w: usize, frame: usize, step: usize) {
        if w == 0 {
            return;
        }
        for (y, row) in out.chunks_mut(w).enumerate() {
            let src = &self.0[((y * step + frame) & 255) * 256..][..256];
            let mut x0 = (3 * frame) & 255;
            for chunk in row.chunks_mut(256) {
                for (o, i) in chunk.iter_mut().zip(src.iter().cycle().skip(x0)) {
                    *o = *i;
                }
                x0 = (x0 + chunk.len()) & 255;
            }
        }
    }

    fn frame(&self, out: &mut [u8], w: usize, h: usize, frame: usize) {
        let ysz = (w * h).min(out.len());
        let (y, c) = out.split_at_mut(ysz);
        self.plane(y, w, frame, 1);
        let csz = (ysz / 4).min(c.len());
        let (u, v) = c.split_at_mut(csz);
        self.plane(u, w / 2, frame, 2);
        let vsz = csz.min(v.len());
        self.plane(&mut v[..vsz], w / 2, frame + 64, 2);
    }
}

/// One size's ticks on a fresh encoder; the device back, and the tick times in milliseconds
/// (or why not).
fn time_size<D: TickDevice>(
    dev: D,
    opt: &EncoderOptions,
    cfgs: &[SessionConfig],
    device_timeout: f64,
    blocks: u32,
    tile: &Tile,
) -> (D, Result<Vec<f64>, String>) {
    let mut eng = Engine::with_xf_blocks(dev, *opt, device_timeout, blocks);
    let mut run = || -> Result<Vec<f64>, String> {
        let mut ids: Vec<(SessionId, usize, usize, i64)> = Vec::new();
        for c in cfgs {
            let id = eng.enc_mut().open(c).map_err(|e| e.to_string())?;
            let per = 90_000 * i64::from(c.fps_d) / i64::from(c.fps_n.max(1));
            ids.push((id, c.w as usize, c.h as usize, per));
        }
        let mut ms = Vec::with_capacity(TICKS - SKIP);
        for t in 0..TICKS {
            for &(id, w, h, per) in &ids {
                let buf = eng.enc().frame_buffer(id).map_err(|e| e.to_string())?;
                let out = buf
                    .of_mut(eng.host_mut())
                    .ok_or("a frame buffer outside the host arena")?;
                tile.frame(out, w, h, t);
                eng.enc_mut()
                    .submit(id, FrameSource::Host, t as i64 * per, &[])
                    .map_err(|e| e.to_string())?;
            }
            let began = Instant::now();
            eng.tick(None).map_err(|e| e.to_string())?;
            if t >= SKIP {
                ms.push(1e3 * began.elapsed().as_secs_f64());
            }
            for &(id, ..) in &ids {
                while let Some(au) = eng.enc_mut().take_output(id) {
                    eng.enc_mut().recycle(au.data);
                }
            }
        }
        eng.settle().map_err(|e| e.to_string())?;
        match &eng.first_error {
            Some(e) => Err(e.to_string()),
            None => Ok(ms),
        }
    };
    let r = run();
    (eng.into_dev(), r)
}

/// The size to run with: the fastest, if it beats the usual one by [`GAIN`] (or the usual one
/// was not timed); else the usual one.
fn choose(usual: u32, median_ms: &[(u32, f64)]) -> u32 {
    let Some(best) = median_ms.iter().copied().min_by(|a, b| a.1.total_cmp(&b.1)) else {
        return usual;
    };
    match median_ms.iter().find(|m| m.0 == usual) {
        Some(u) if best.1 >= GAIN * u.1 => usual,
        _ => best.0,
    }
}

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

/// Times the sizes of the transform kernel on `dev` with the sessions `cfgs` (as many of them
/// as stay within four 1080-line pictures per tick) and says which to run with. Nothing is
/// timed on a device whose clock is not the wall clock (the reference adapter) or without
/// sessions; whatever goes wrong, the answer is the device's usual size. The device comes back
/// with nothing in flight; its arenas hold what the timing left there.
pub fn xf_blocks<D: TickDevice>(
    mut dev: D,
    opt: &EncoderOptions,
    cfgs: &[SessionConfig],
    device_timeout: f64,
) -> (D, Calibration) {
    let started = Instant::now();
    let usual = dev.xf_blocks();
    let give_up = |why: String, took| Calibration {
        chosen: usual,
        usual,
        median_ms: Vec::new(),
        skipped: Some(why),
        took,
    };
    if !dev.real_time() {
        return (dev, give_up("not a card".into(), Duration::ZERO));
    }
    let mut mbs = 0u64;
    let some: Vec<SessionConfig> = cfgs
        .iter()
        .take_while(|c| {
            mbs += u64::from(c.w.div_ceil(16)) * u64::from(c.h.div_ceil(16));
            mbs <= MAX_MBS
        })
        .copied()
        .collect();
    let some = if some.is_empty() {
        cfgs.iter().take(1).copied().collect()
    } else {
        some
    };
    if some.is_empty() {
        return (dev, give_up("no sessions".into(), Duration::ZERO));
    }
    // No deadline in the timing: every picture is coded.
    let opt = EncoderOptions {
        force_shed: None,
        ..*opt
    };
    let tile = Tile::new();
    let mut ticks: Vec<Vec<f64>> = vec![Vec::new(); XF_BLOCKS.len()];
    for round in 0..ROUNDS {
        for (i, &blocks) in XF_BLOCKS.iter().enumerate() {
            let (d, r) = time_size(dev, &opt, &some, device_timeout, blocks, &tile);
            dev = d;
            match r {
                Ok(ms) if round > 0 => ticks[i].extend(ms),
                Ok(_) => {}
                Err(e) => return (dev, give_up(e, started.elapsed())),
            }
        }
        // (rounds take about as long as each other)
        let spent = started.elapsed();
        if round > 0 && spent + spent / (round as u32 + 1) > BUDGET {
            break;
        }
    }
    let median_ms: Vec<(u32, f64)> = XF_BLOCKS
        .iter()
        .zip(ticks.iter_mut())
        .map(|(&b, v)| (b, median(v)))
        .collect();
    let chosen = choose(usual, &median_ms);
    (
        dev,
        Calibration {
            chosen,
            usual,
            median_ms,
            skipped: None,
            took: started.elapsed(),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reference_adapter_is_not_timed() {
        let arenas = tb_device::ArenaSizes {
            device: 1 << 20,
            host: 1 << 20,
        };
        let dev = tb_device_ref::RefDevice::new(arenas).unwrap();
        let (_dev, c) = xf_blocks(dev, &EncoderOptions::default(), &[], 1.0);
        assert_eq!(c.chosen, c.usual);
        assert_eq!(c.usual, XF_BLOCKS[0]);
        assert!(c.median_ms.is_empty());
        assert_eq!(c.skipped.as_deref(), Some("not a card"));
    }

    #[test]
    fn a_size_must_gain_three_percent_to_replace_the_usual_one() {
        let m = |v: [f64; 4]| -> Vec<(u32, f64)> { XF_BLOCKS.iter().copied().zip(v).collect() };
        // 64 is fastest, but by less than 3 % against the usual 8: 8 stays.
        assert_eq!(choose(8, &m([1.00, 1.01, 0.99, 0.98])), 8);
        // By a fifth: 64.
        assert_eq!(choose(8, &m([1.00, 0.90, 0.82, 0.80])), 64);
        // The usual size is itself the fastest.
        assert_eq!(choose(64, &m([1.45, 1.17, 1.02, 1.00])), 64);
        // A usual size that was not timed: the fastest.
        assert_eq!(choose(4, &m([1.00, 0.90, 0.95, 0.97])), 16);
        // Nothing timed: the usual size.
        assert_eq!(choose(32, &[]), 32);
    }

    #[test]
    fn the_picture_moves_and_fills_its_planes() {
        let tile = Tile::new();
        let (w, h) = (704usize, 480usize);
        let mut a = vec![0u8; w * h * 3 / 2];
        let mut b = a.clone();
        tile.frame(&mut a, w, h, 0);
        tile.frame(&mut b, w, h, 1);
        // frame 1 is frame 0 moved three pixels left and one line up
        assert_eq!(a[w + 3..2 * w], b[..w - 3]);
        assert_ne!(a, b);
        // every plane has texture
        for plane in [&a[..w * h], &a[w * h..w * h * 5 / 4], &a[w * h * 5 / 4..]] {
            assert!(plane.iter().any(|&p| p != plane[0]));
        }
    }
}
