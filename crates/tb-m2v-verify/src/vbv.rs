//! The video buffering verifier of H.262 Annex C, for both ways of data input (C.3.1 with
//! vbv_delay, C.3.2 with vbv_delay 0xFFFF), and the project's operating model.

use crate::mb::{B, FRAME};
use crate::report::{Position, Sink};
use crate::rules::Rule;

/// One coded picture as the buffer model sees it.
#[derive(Debug, Clone)]
pub(crate) struct VbvPic {
    pub(crate) ptype: u8,
    pub(crate) structure: u8,
    pub(crate) second_field: bool,
    pub(crate) rff: bool,
    pub(crate) tff: bool,
    pub(crate) tr: u16,
    pub(crate) gop_before: bool,
    pub(crate) vbv_delay: u16,
    /// Byte offset where the picture data start (the first header before the picture).
    pub(crate) start: u64,
    /// Byte offset after the picture start code.
    pub(crate) psc_end: u64,
    /// Byte offset where the picture data end.
    pub(crate) end: u64,
    pub(crate) pos: Position,
}

/// Sequence parameters of the buffer model.
#[derive(Debug, Clone, Copy)]
pub(crate) struct VbvSeq {
    /// bit_rate in bits per second.
    pub(crate) rate: f64,
    /// vbv_buffer_size in bits.
    pub(crate) size: f64,
    pub(crate) frame_rate: f64,
    pub(crate) progressive: bool,
    pub(crate) low_delay: bool,
}

/// How the data are assumed to reach the decoder.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Delivery {
    /// H.262 Annex C as coded: C.3.1 if vbv_delay is coded, C.3.2 if it is 0xFFFF.
    Normative,
    /// The project's operating model in addition (not part of H.262): pictures
    /// become available at their coding time and leave the multiplexer queue at `rate` bits
    /// per second (`None`: the bit_rate of the sequence header) within
    /// D = vbv_buffer_size / bit_rate.
    Operating {
        /// The delivery rate in bits per second.
        rate: Option<f64>,
    },
}

/// The intervals t(n+1) - t(n) of C.9 to C.12, in seconds.
pub(crate) fn intervals(seq: &VbvSeq, pics: &[VbvPic]) -> Vec<f64> {
    let frame = 1.0 / seq.frame_rate;
    let field = frame / 2.0;
    let mut out = Vec::with_capacity(pics.len());
    // the previous coded I- or P-frame: (frame structure, repeat_first_field, top_field_first)
    let mut prev_anchor: Option<(bool, bool, bool)> = None;
    for (n, p) in pics.iter().enumerate() {
        let frame_pic = p.structure == FRAME;
        let own = (frame_pic, p.rff, p.tff);
        let prog_mult = |a: (bool, bool, bool)| -> f64 {
            if !a.1 {
                1.0
            } else if !a.2 {
                2.0
            } else {
                3.0
            }
        };
        let mut dt = if seq.progressive {
            if p.ptype == B || seq.low_delay {
                prog_mult(own) * frame
            } else {
                prev_anchor.map_or(1.0, prog_mult) * frame
            }
        } else if p.ptype == B || seq.low_delay {
            // C.11 for B-pictures, C.12 for every picture of a low-delay sequence
            if frame_pic {
                if p.rff { 3.0 * field } else { 2.0 * field }
            } else {
                field
            }
        } else {
            let prev_three = prev_anchor.is_some_and(|a| a.0 && a.1);
            if frame_pic {
                if prev_three { 3.0 * field } else { 2.0 * field }
            } else if !p.second_field {
                field
            } else if prev_three {
                2.0 * field
            } else {
                field
            }
        };
        // C.7: a big picture is re-examined every two field periods; the temporal_reference
        // of the next picture tells how often.
        if seq.low_delay
            && let Some(next) = pics.get(n + 1)
            && !next.gop_before
            && frame_pic
        {
            let k = (i32::from(next.tr) - i32::from(p.tr) - 1).rem_euclid(1024);
            if k > 0 && k < 512 {
                dt += f64::from(k) * 2.0 * field;
            }
        }
        if p.ptype != B && (frame_pic || p.second_field) {
            prev_anchor = Some(own);
        }
        out.push(dt);
    }
    out
}

/// Runs the buffer model over the pictures of one sequence.
pub(crate) fn check(seq: &VbvSeq, pics: &[VbvPic], delivery: Delivery, sink: &mut Sink) {
    if pics.is_empty() || seq.rate <= 0.0 || seq.frame_rate <= 0.0 {
        return;
    }
    let dts = intervals(seq, pics);
    let mut t = Vec::with_capacity(pics.len());
    let mut acc = 0.0;
    for dt in &dts {
        t.push(acc);
        acc += dt;
    }
    let base = pics.first().map_or(0, |p| p.start);
    let bits = |byte: u64| -> f64 { (byte.saturating_sub(base) * 8) as f64 };
    let b = seq.size;
    let eps = 1e-6;

    // a picture larger than the buffer can never be decoded
    let mut too_big = vec![false; pics.len()];
    for (i, p) in pics.iter().enumerate() {
        let size = bits(p.end) - bits(p.start);
        if size > b + eps {
            if let Some(f) = too_big.get_mut(i) {
                *f = true;
            }
            sink.emit(Rule::VbvPictureSize, p.pos, || {
                format!(
                    "the picture data are {size:.0} bits, more than the VBV buffer of {b:.0} bits"
                )
            });
        }
    }

    let all_ffff = pics.iter().all(|p| p.vbv_delay == 0xFFFF);
    let none_ffff = pics.iter().all(|p| p.vbv_delay != 0xFFFF);
    if none_ffff {
        check_c31(seq, pics, &t, &too_big, sink);
    } else {
        // C.3.2 (also used when the values are mixed, which PIC-VBV-DELAY-MIX reports)
        let total = bits(pics.last().map_or(base, |p| p.end));
        let mut delivered = total.min(b.max(bits(pics.first().map_or(base, |p| p.psc_end))));
        for (i, p) in pics.iter().enumerate() {
            let end = bits(p.end);
            if end > delivered + eps && !too_big.get(i).copied().unwrap_or(false) {
                let missing = end - delivered;
                sink.emit(Rule::VbvUnderflow, p.pos, || {
                    format!(
                        "VBV underflow (C.3.2 input at bit_rate {:.0}): {missing:.0} bits of the picture are not in the buffer at its decoding time t = {:.4} s",
                        seq.rate,
                        t.get(i).copied().unwrap_or(0.0)
                    )
                });
            }
            let removed = end;
            delivered = delivered.max(end);
            let dt = dts.get(i).copied().unwrap_or(0.0);
            delivered = total.min(delivered + seq.rate * dt).min(removed + b);
        }
        let _ = all_ffff;
    }

    if let Delivery::Operating { rate } = delivery {
        let rho = rate.unwrap_or(seq.rate);
        if rho > 0.0 {
            let d = b / seq.rate;
            let mut done = 0.0f64;
            for (i, p) in pics.iter().enumerate() {
                let e = t.get(i).copied().unwrap_or(0.0);
                let size = bits(p.end) - bits(p.start);
                done = done.max(e) + size / rho;
                if done > e + d + eps {
                    let late = (done - e - d) * 1000.0;
                    sink.emit(Rule::VbvOperating, p.pos, || {
                        format!(
                            "operating model: at {rho:.0} bit/s the picture ({size:.0} bits) leaves the queue {late:.2} ms after its deadline e(n) + D (D = {:.1} ms)",
                            d * 1000.0
                        )
                    });
                    done = e + d;
                }
            }
        }
    }
}

/// C.3.1: the input rate follows from vbv_delay.
fn check_c31(seq: &VbvSeq, pics: &[VbvPic], t: &[f64], too_big: &[bool], sink: &mut Sink) {
    let base = pics.first().map_or(0, |p| p.start);
    let bits = |byte: u64| -> f64 { (byte.saturating_sub(base) * 8) as f64 };
    let tick = 1.0 / 90000.0;
    let tau = |p: &VbvPic| f64::from(p.vbv_delay) * tick;
    let first_tau = pics.first().map_or(0.0, tau);
    // a(n): the time the last bit of the n-th picture start code enters; t(0) = a(0) + tau(0)
    let a: Vec<f64> = pics
        .iter()
        .zip(t)
        .map(|(p, tn)| first_tau + tn - tau(p))
        .collect();
    // rates
    let mut rates = Vec::with_capacity(pics.len());
    for (n, p) in pics.iter().enumerate() {
        let r = match (pics.get(n + 1), a.get(n), a.get(n + 1)) {
            (Some(next), Some(a0), Some(a1)) => {
                let d = bits(next.psc_end) - bits(p.psc_end);
                let dt = a1 - a0;
                // the most favorable reading of the two quantized vbv_delay values
                let slack = 2.0 * tick;
                if dt + slack <= 0.0 {
                    sink.emit(Rule::VbvDelayRate, p.pos, || {
                        format!(
                            "vbv_delay {} then {}: the next picture start code would enter the buffer {:.2} ms before this one",
                            p.vbv_delay,
                            next.vbv_delay,
                            -dt * 1000.0
                        )
                    });
                    seq.rate
                } else {
                    let r_low = d / (dt + slack);
                    if r_low > seq.rate * (1.0 + 1e-9) + 400.0 {
                        sink.emit(Rule::VbvDelayRate, p.pos, || {
                            format!(
                                "vbv_delay {} then {} imply an input rate of at least {:.0} bit/s for these {:.0} bits, above bit_rate {:.0}",
                                p.vbv_delay, next.vbv_delay, r_low, d, seq.rate
                            )
                        });
                    }
                    if dt > 0.0 { d / dt } else { seq.rate }
                }
            }
            _ => seq.rate,
        };
        rates.push(r.max(1.0));
    }
    // arrival time of a bit offset, and bits arrived by a time
    let arrival = |x: f64| -> f64 {
        let mut k = 0usize;
        for (i, p) in pics.iter().enumerate() {
            if bits(p.psc_end) <= x {
                k = i;
            } else {
                break;
            }
        }
        let p0 = pics.get(k).map_or(0.0, |p| bits(p.psc_end));
        if x <= p0 {
            return a
                .first()
                .copied()
                .unwrap_or(0.0)
                .min(a.get(k).copied().unwrap_or(0.0));
        }
        a.get(k).copied().unwrap_or(0.0) + (x - p0) / rates.get(k).copied().unwrap_or(seq.rate)
    };
    let total = bits(pics.last().map_or(base, |p| p.end));
    let arrived = |time: f64| -> f64 {
        let mut got = pics.first().map_or(0.0, |p| bits(p.psc_end));
        for (i, p) in pics.iter().enumerate() {
            let a0 = a.get(i).copied().unwrap_or(0.0);
            if time <= a0 {
                break;
            }
            let seg_end = pics.get(i + 1).map_or(total, |n| bits(n.psc_end));
            let p0 = bits(p.psc_end);
            let r = rates.get(i).copied().unwrap_or(seq.rate);
            got = (p0 + (time - a0) * r).min(seg_end);
        }
        got
    };
    let tol_bits = seq.rate * 2.0 * tick + 8.0;
    for (i, p) in pics.iter().enumerate() {
        if too_big.get(i).copied().unwrap_or(false) {
            continue;
        }
        let tn = first_tau + t.get(i).copied().unwrap_or(0.0);
        let end = bits(p.end);
        let late = arrival(end) - tn;
        if late > 2.0 * tick + 1e-9 {
            sink.emit(Rule::VbvUnderflow, p.pos, || {
                format!(
                    "VBV underflow (C.3.1 input from vbv_delay {}): the last bit of the picture enters {:.2} ms after its decoding time",
                    p.vbv_delay,
                    late * 1000.0
                )
            });
        }
        let occupancy = arrived(tn) - bits(p.start);
        if occupancy > seq.size + tol_bits {
            sink.emit(Rule::VbvOverflow, p.pos, || {
                format!(
                    "VBV overflow (C.3.1 input from vbv_delay {}): {occupancy:.0} bits in the buffer before removing the picture, the buffer is {:.0} bits",
                    p.vbv_delay, seq.size
                )
            });
        }
    }
}
