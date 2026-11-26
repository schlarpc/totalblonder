//! Rate control of one session (the C++ `RateControl` and the rate-control halves of
//! `Batch::begin`, `quantize`, `choose`, `retry`, `collect`, `host_repeat`).
//!
//! Per picture the caller runs:
//! 1. [`RateControl::begin_picture`] before the picture's parameters go to the device, with
//!    `t` the picture's decoding time on the session's clock (`n_coded / fps` while every
//!    picture is shown for one frame period; with repeat_first_field the intervals of H.262
//!    Annex C.9 and C.11) and `new_rate = Some(rate)` when a pending rate change takes effect
//!    with this picture (the C++: a pending ticket, and the picture is a key frame or the
//!    change is not held for one). Its [`ParInputs`] go into the picture's `SessPar`;
//!    `SessPar::keep` starts at 64.
//! 2. [`RateControl::plan_picture`] when the row statistics are back, with a [`PicCtx`] whose
//!    `rate` is the session's rate after step 1: target, limits, row quantizers in `rowq`.
//!    The returned value is `SessPar::keep` for the first pass. Then the caller's own check
//!    (`launch_qent`): when a row code is below `SessPar::qfloor`, set `qfloor` to 1, repeat
//!    the transform and count `stats.rexf`.
//! 3. [`RateControl::after_pass`] when the sizes of a quantize-and-pack pass are back: accept,
//!    or quantize again (once toward the target, always when the picture does not fit, down
//!    the ladder to the all-skipped picture). On [`Pass::Again`] the caller sets
//!    `SessPar::keep`; on `all_skipped` it also sets `want_pred = 1`, every macroblock to
//!    forward with zero vectors, and repeats the transform; on `i_becomes_p` (`ctx.pic_type`
//!    and `ctx.key` are already changed) it sets `pic_type = P`, `fwd = newer`, drops the GOP
//!    header and restores the old `tref_base`; then it uploads `rowq`, makes the `qfloor`
//!    check of step 2 again and runs the pass. Repeat until [`Pass::Accept`]. A session with a
//!    fixed quantizer is judged too: its pictures must fit the buffer.
//! 4. [`RateControl::finish_picture`] with the same `ctx`, the last pass's [`PicStat`], the
//!    size of the access unit as written (`8 * (header bytes + slice bytes)`, headers re-made
//!    for the new type when an I picture became P; no accepted pass is larger than the output
//!    buffer, so nothing is cut) and the row
//!    quantizers as they stand: buffer and model updates.
//!
//! A picture made on the host (a repeat, shed or held picture) takes only
//! [`RateControl::repeat_picture`], at its decoding time, with the type the GOP structure
//! gave the position; it does not apply a pending rate change (the C++ applies those only in
//! `Batch::begin`).

use tb_kernel_abi::{NZ_BINS, PIC_B, PIC_I, PIC_P, PIC_ROW_WORDS, PicStat, RowStat};

use crate::model::{QSCALE, RateModel, nz_at};
use crate::vbv::Vbv;

/// Ladder rung of the all-skipped picture. Rungs 32..=36 keep the first 12, 6, 3, 1, 0
/// coefficients of each block at quantizer code 31.
pub const RUNG_SKIP: u8 = 37;
/// Coefficients kept per block on rungs 32.. (`k_rung_keep`).
pub const RUNG_KEEP: [u8; 6] = [12, 6, 3, 1, 0, 0];
/// Quantizer codes below the last picture's lowest for which the transform keeps coefficients
/// (`QFLOOR_MARGIN`).
pub const QFLOOR_MARGIN: u8 = 3;

/// What rate control decided, as it decides it: the hooks of the golden recorder
/// (`crates/tb-golden/vectors/README.md`, records `vbv`, `choose` and `pass`). Every method has an empty default;
/// `()` observes nothing. The `*_obs` methods of [`RateControl`] call it at the points where the
/// C++ wrote those records; the plain methods pass `()`.
pub trait Observer {
    /// The buffer model after an operation.
    fn vbv(&mut self, _event: VbvEvent, _vbv: &Vbv) {}
    /// [`RateControl::choose`] has chosen.
    fn choose(&mut self, _event: &ChooseEvent<'_>) {}
    /// A pass of [`RateControl::after_pass`] is judged (sessions with rate control only).
    fn pass(&mut self, _event: &PassEvent) {}
}

impl Observer for () {}

/// An operation on the buffer model, with its operands.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum VbvEvent {
    /// [`Vbv::advance`] to this time.
    Advance {
        /// The time.
        to: f64,
    },
    /// [`Vbv::set_rate`].
    SetRate {
        /// From this time.
        at: f64,
        /// The rate.
        rate: f64,
    },
    /// [`Vbv::add`] of a picture.
    Add {
        /// Its bits.
        bits: f64,
    },
}

/// What [`RateControl::choose`] decided.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChooseEvent<'a> {
    /// `PIC_*`.
    pub pic_type: u8,
    /// The slice budget asked for.
    pub target: f64,
    /// The model used.
    pub theta: f64,
    /// The model's header bits as stored (negative: not measured yet).
    pub hdr: f64,
    /// Some code was predicted to fit.
    pub ok: bool,
    /// The result: `cur.code`, `cur.frac`, `cur.nzpred` and the row codes.
    pub code: u8,
    #[allow(missing_docs, reason = "as code")]
    pub frac: f64,
    #[allow(missing_docs, reason = "as code")]
    pub nzpred: f64,
    #[allow(missing_docs, reason = "as code")]
    pub rowq: &'a [u8],
}

/// What a quantize-and-pack pass measured and how [`RateControl::after_pass`] judges it, before
/// it changes anything.
#[derive(Clone, Copy, Debug, PartialEq)]
#[allow(
    missing_docs,
    reason = "the fields of the golden record `pass` (crates/tb-golden/vectors/README.md)"
)]
pub struct PassEvent {
    pub pic_type: u8,
    pub have_ref: bool,
    pub tries: u32,
    pub rung: u8,
    pub bytes: u32,
    pub tex_bits: u32,
    pub over_cap: bool,
    /// Rows with more than two macroblocks above the limit of H.262 Table 8-6
    /// ([`PicStat::big_rows`]).
    pub big_rows: [u32; PIC_ROW_WORDS],
    pub tgt: f64,
    pub limit: f64,
    pub must: bool,
    pub want: bool,
    pub code: u8,
    pub frac: f64,
    pub nzpred: f64,
    pub theta: f64,
    pub hdr: f64,
}

/// Picture type index used by the per-type state: 1 = I, 2 = P, 3 = B (index 0 unused, as in
/// the C++).
pub type ByType<T> = [T; 4];

/// What rate control needs to know about the picture being coded.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PicCtx {
    /// `PIC_*` as the picture is being coded (an I picture that the ladder replaces becomes P).
    pub pic_type: u8,
    /// A key frame: starts a GOP budget.
    pub key: bool,
    /// The session has a reference picture (false only for the first picture of a stream,
    /// which can be neither skipped nor replaced).
    pub have_ref: bool,
    /// Macroblocks per picture. Must equal the `nmb` given to [`RateControl::new`], which is
    /// the value rate control uses.
    pub nmb: u32,
    /// Frames per second.
    pub fps: f64,
    /// The session's delivery rate in bit/s that applies to this picture.
    pub rate: f64,
    /// Key frame distance of the session.
    pub gop: u32,
    /// Anchor distance of the session.
    pub m: u32,
    /// Pictures from this one to the next key frame (the length of the GOP when `key`). The
    /// C++ difference `next_key - disp` can be 0 or negative only when something upstream went
    /// wrong; both are treated as 1 there and 0 is treated as 1 here. When pictures are shown
    /// for different times (film: two and three fields) this is an estimate, the time to the
    /// next key frame over the mean display time of a picture; it only divides the budget.
    pub to_next_key: u32,
    /// Display time from this picture to the next key frame in frame periods: what the GOP
    /// budget is delivered in. `to_next_key` as a number while every picture is shown for one
    /// frame period.
    pub gop_frames: f64,
    /// Display time of this picture in frame periods (1; 1.5 for three fields; 2 or 3 for a
    /// repeated frame of a progressive sequence): its share of the delivery.
    pub frames: f64,
    /// Bits of the access unit's headers.
    pub hdr_bits: f64,
    /// Bytes the output buffer holds per picture (`cap_words * 4`).
    pub cap_bytes: u32,
    /// Fixed quantizer codes (I, P, B) for a session without rate control.
    pub fixed_q: [u8; 3],
}

/// The values of `SessPar` that rate control decides.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParInputs {
    /// `SessPar::avg_act`.
    pub avg_act: u16,
    /// `SessPar::lam`.
    pub lam: u16,
    /// `SessPar::qfloor`.
    pub qfloor: u8,
}

/// What to do after a quantize-and-pack pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pass {
    /// The picture stands.
    Accept,
    /// Quantize and pack again with the row quantizers now in `rowq`.
    Again {
        /// `SessPar::keep` for the pass (64: all coefficients).
        keep: u8,
        /// The picture becomes all-skipped: the caller sets every macroblock to forward with a
        /// zero vector and repeats the transform (`want_pred = 1`) before quantizing.
        all_skipped: bool,
        /// An I picture is replaced by a P picture (no GOP header; temporal_reference keeps
        /// counting from the old GOP).
        i_becomes_p: bool,
    },
}

/// What the access unit reports about rate control.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PicResult {
    /// Mean quantiser_scale of the picture (`112 * (1 + rung - 31)` on the ladder).
    pub avg_q: f64,
    /// Mux queue after this picture, bits.
    pub queue_bits: f64,
    /// Bits deliverable in the next D seconds.
    pub cap_bits: f64,
    /// How much larger the picture could have been.
    pub room_bits: f64,
    /// 0, or the ladder rung (32..=37).
    pub rung: u8,
    /// Quantization passes beyond the first.
    pub tries: u32,
}

/// Rate-control state of one session. Field names follow the C++ `RateControl` so that golden
/// records map one to one.
#[derive(Clone, Debug, PartialEq)]
pub struct RateControl {
    /// Rate-controlled: the quantizer follows a target. `false`: fixed quantizer, which only
    /// the buffer (and the macroblock size rule) can make coarser. The buffer model runs in
    /// both cases.
    pub cbr: bool,
    /// Macroblocks per picture (the C++ reads `g.nmb` of the batch; kept here so that
    /// [`RateControl::choose`] can be called without a [`PicCtx`]).
    pub nmb: u32,
    /// Complexity per picture type (bits x quantiser_scale).
    pub x: ByType<f64>,
    /// Bits left in the GOP budget.
    pub r: f64,
    /// Pictures of each type left in the GOP.
    pub left: ByType<u32>,
    /// Model per picture type.
    pub mdl: ByType<RateModel>,
    /// Rung the last picture of each type ended on (0: none).
    pub last_rung: ByType<u8>,
    /// Average macroblock activity of the last picture.
    pub avg_act: f64,
    /// quantiser_scale of the previous picture.
    pub last_qs: f64,
    /// Lowest row quantizer code of the last picture of each type (0: none yet).
    pub floor_hist: ByType<u8>,
    /// The buffer model.
    pub vbv: Vbv,
    /// Working state of the picture being coded.
    pub cur: Working,
    /// Per macroblock row of the picture being coded: the lowest quantizer code the row may
    /// take, raised when a pass showed more than two of its macroblocks above 4608 bits
    /// (H.262 Table 8-6); 0: no floor.
    pub row_floor: Vec<u8>,
    /// The row quantizers of the pass being judged, kept while a correction is tried (one
    /// entry per macroblock row; working memory of [`RateControl::after_pass`]).
    pub rowq_pass: Vec<u8>,
    /// Counters.
    pub stats: Stats,
}

/// Working state of the picture being coded.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[allow(
    missing_docs,
    reason = "names and meanings are those of the C++ RateControl"
)]
pub struct Working {
    pub target: f64,
    pub hard: f64,
    pub limit: f64,
    pub hdr_bits: f64,
    pub nzpred: f64,
    pub cap: f64,
    pub e_before: f64,
    pub tries: u32,
    pub rung: u8,
    pub code: u8,
    pub frac: f64,
}

/// Counters of one session.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[allow(
    missing_docs,
    reason = "names and meanings are those of the C++ RateControl"
)]
pub struct Stats {
    pub retries: u64,
    pub rung_pics: u64,
    pub skip_pics: u64,
    pub isub: u64,
    pub violations: u64,
    pub rexf: u64,
    /// Passes made because a row broke the macroblock size rule of H.262 Table 8-6.
    pub mbfix: u64,
    pub room_min: f64,
}

/// Why a session's rate configuration is refused (the C++ `Encoder::open`).
#[derive(Clone, Copy, Debug, PartialEq, thiserror::Error)]
pub enum RateConfigError {
    /// The initial rate is outside `[min, max]`.
    #[error("session rate {rate} outside [{min}, {max}]")]
    RateOutOfRange {
        /// Requested rate, bit/s.
        rate: u64,
        /// Lowest rate at which the buffer guarantee holds for this format.
        min: u64,
        /// The session's ceiling.
        max: u64,
    },
    /// `vbv_bits` is below 16384.
    #[error("no buffer")]
    NoBuffer,
}

/// `std::max(a, b)`: `b` only when `a < b`.
fn max(a: f64, b: f64) -> f64 {
    if a < b { b } else { a }
}

/// `std::min(a, b)`: `b` only when `b < a`.
fn min(a: f64, b: f64) -> f64 {
    if b < a { b } else { a }
}

/// `std::clamp(v, lo, hi)` (libstdc++: `v < lo ? lo : hi < v ? hi : v`).
fn clamp(v: f64, lo: f64, hi: f64) -> f64 {
    if v < lo {
        lo
    } else if hi < v {
        hi
    } else {
        v
    }
}

/// Sum of the histograms of all rows (the C++ `RowStat tot{}` loop; `uint32_t` adds wrap).
fn total(rows: &[RowStat]) -> RowStat {
    let mut tot = RowStat::default();
    for r in rows {
        for i in 0..NZ_BINS {
            tot.nz[i] = tot.nz[i].wrapping_add(r.nz[i]);
        }
    }
    tot
}

/// quantiser_scale of a code, as `double`.
fn qscale(code: u8) -> f64 {
    f64::from(QSCALE[usize::from(code)])
}

/// Header bits of the model, or the default per macroblock while not measured.
fn model_hdr(m: &RateModel, nmb: u32, pic_type: u8) -> f64 {
    if m.hdr >= 0.0 {
        m.hdr
    } else {
        f64::from(nmb)
            * if pic_type == PIC_I {
                30.0
            } else if pic_type == PIC_P {
                14.0
            } else {
                10.0
            }
    }
}

/// The next quantizer code that at least doubles quantiser_scale (31 at the end of the scale):
/// the step by which a row is made coarser when its macroblocks are too large.
pub fn coarser(code: u8) -> u8 {
    let want = 2 * u32::from(QSCALE[usize::from(code.min(31))]);
    (code.saturating_add(1)..=31)
        .find(|c| u32::from(QSCALE[usize::from(*c)]) >= want)
        .unwrap_or(31)
}

/// `SessPar::keep` that goes with a ladder rung: 64 below rung 32, `RUNG_KEEP` on rungs
/// 32..=36, 0 on the all-skipped rung.
pub fn keep_for_rung(rung: u8) -> u8 {
    match rung {
        0..=31 => 64,
        32..=36 => RUNG_KEEP[usize::from(rung - 32)],
        _ => 0,
    }
}

/// Lowest rate at which the buffer guarantee holds: one frame period of delivery covers twice
/// the all-skipped picture with its headers and captions (the C++ `Session::min_rate`).
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the C++ truncates to long"
)]
pub fn min_rate(mbh: u32, fps: f64) -> u64 {
    // per slice: start code, quantizer, two coded macroblocks, alignment: under 10 bytes;
    // sequence, GOP and picture headers under 80 bytes; cc_data with 31 triplets 105 bytes
    let bytes = f64::from(mbh) * 10.0 + 80.0 + 105.0;
    (16.0 * bytes * fps) as i64 as u64 + 1
}

/// Most bits of an I picture on ladder rung 36, where every block is its DC coefficient alone
/// (the only form the first picture of a stream can take when nothing larger fits: it can be
/// neither skipped nor replaced). Per macroblock: address increment 1, macroblock_type 2,
/// dct_type 1, quantiser_scale_code 5, four luminance blocks of at most 7 + 8 bits
/// (dct_dc_size 8, Table B.12, and the differential), two chrominance blocks of at most
/// 8 + 8 (Table B.13), six End of Block codes of at most 4 bits (Table B.15): 125 bits. Per
/// row: the slice header (38 bits) and up to 7 bits of alignment. Plus the sequence, GOP and
/// picture headers and the caption user data that [`min_rate`] counts (185 bytes).
pub fn first_picture_bits(nmb: u32, mbh: u32) -> u64 {
    125 * u64::from(nmb) + 45 * u64::from(mbh) + 8 * 185
}

/// Lowest rate at which the first picture of a stream is certain to fit the buffer: delivered
/// at `rate`, the buffer has room for `rate * vbv_bits / max_bitrate` bits (less the 64 that
/// rate control keeps free), and [`first_picture_bits`] must fit.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "bits and rates are far below 2^53"
)]
pub fn first_min(nmb: u32, mbh: u32, max_bitrate: u64, vbv_bits: u64) -> u64 {
    let need = (first_picture_bits(nmb, mbh) + 64) as f64;
    (need * max_bitrate as f64 / vbv_bits.max(1) as f64).ceil() as u64
}

impl RateControl {
    /// State of a new session: fixed quantizer when `bitrate == 0`, else rate-controlled with
    /// the initial complexities of `Encoder::open`; in both cases with a buffer of
    /// `vbv_bits / max_bitrate` seconds. A fixed-quantizer session is delivered at
    /// `max_bitrate`; its quantizer is fixed as long as its pictures fit the buffer (see
    /// [`RateControl::after_pass`]).
    ///
    /// A session needs `vbv_bits >= 16384` ([`RateConfigError::NoBuffer`]) and
    /// `max(min_rate, first_min) <= rate <= max_bitrate`, `rate` being `bitrate` or, for a
    /// fixed quantizer, `max_bitrate`, where [`first_min`] is the rate at which the first
    /// picture of the stream fits the buffer whatever its content
    /// ([`RateConfigError::RateOutOfRange`]). With it the buffer guarantee has no exception:
    /// every later picture can end as an all-skipped picture, which [`min_rate`] covers.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss,
        reason = "the C++ converts long to double and truncates to long"
    )]
    pub fn new(
        bitrate: u64,
        max_bitrate: u64,
        vbv_bits: u64,
        nmb: u32,
        mbh: u32,
        fps: f64,
    ) -> Result<Self, RateConfigError> {
        let cbr = bitrate > 0;
        let mut x = [0.0; 4];
        let min = min_rate(mbh, fps).max(first_min(nmb, mbh, max_bitrate, vbv_bits));
        if vbv_bits < 16384 {
            return Err(RateConfigError::NoBuffer);
        }
        // A fixed-quantizer session is delivered at its ceiling.
        let rate = if cbr { bitrate } else { max_bitrate };
        if rate < min || rate > max_bitrate {
            return Err(RateConfigError::RateOutOfRange {
                rate,
                min,
                max: max_bitrate,
            });
        }
        if cbr {
            let b = bitrate as f64;
            x[usize::from(PIC_I)] = 160.0 * b / 115.0;
            x[usize::from(PIC_P)] = 60.0 * b / 115.0;
            x[usize::from(PIC_B)] = 42.0 * b / 115.0;
        }
        let vbv = Vbv::new(vbv_bits as f64 / max_bitrate as f64, rate as f64);
        Ok(Self {
            cbr,
            nmb,
            x,
            r: 0.0,
            left: [0; 4],
            mdl: [RateModel::default(); 4],
            last_rung: [0; 4],
            avg_act: 400.0,
            last_qs: 16.0,
            floor_hist: [0; 4],
            vbv,
            cur: Working::default(),
            row_floor: vec![0; mbh as usize],
            rowq_pass: vec![0; mbh as usize],
            stats: Stats {
                room_min: 1e18,
                ..Stats::default()
            },
        })
    }

    /// `SessPar::keep` for the current pass: follows from the rung (see [`keep_for_rung`]).
    pub fn keep(&self) -> u8 {
        keep_for_rung(self.cur.rung)
    }

    /// Start a picture at time `t` (coded index over frame rate): advance the buffer, apply a
    /// rate change that takes effect with this picture, note the deliverable bits. Returns what
    /// goes into the picture's `SessPar`. `fixed_q` is the picture's fixed quantizer code for a
    /// session without rate control. Resets the picture's `tries` and `rung`
    /// (the C++ `Batch::begin`). `pic_type` must be 1, 2 or 3.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the C++ converts the clamped doubles to uint16_t"
    )]
    pub fn begin_picture(
        &mut self,
        t: f64,
        new_rate: Option<f64>,
        pic_type: u8,
        fixed_q: u8,
    ) -> ParInputs {
        self.begin_picture_obs(t, new_rate, pic_type, fixed_q, &mut ())
    }

    /// [`RateControl::begin_picture`], observed.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the C++ converts the clamped doubles to uint16_t"
    )]
    pub fn begin_picture_obs<O: Observer + ?Sized>(
        &mut self,
        t: f64,
        new_rate: Option<f64>,
        pic_type: u8,
        fixed_q: u8,
        obs: &mut O,
    ) -> ParInputs {
        self.cur.e_before = self.vbv.advance(t);
        obs.vbv(VbvEvent::Advance { to: t }, &self.vbv);
        if let Some(rate) = new_rate {
            self.vbv.set_rate(t, rate);
            obs.vbv(VbvEvent::SetRate { at: t, rate }, &self.vbv);
        }
        self.cur.cap = self.vbv.cap();
        let avg_act = clamp(self.avg_act, 1.0, 65535.0) as u16;
        let lam = clamp(self.last_qs / 2.0, 1.0, 400.0) as u16;
        let qfloor = if self.cbr {
            match self.floor_hist[usize::from(pic_type)] {
                0 => 1,
                f => f.saturating_sub(QFLOOR_MARGIN).max(1),
            }
        } else {
            fixed_q
        };
        self.cur.tries = 0;
        self.cur.rung = 0;
        self.row_floor.fill(0);
        ParInputs {
            avg_act,
            lam,
            qfloor,
        }
    }

    /// Target, limits and row quantizers for the picture, from its row statistics. Writes one
    /// quantizer code per macroblock row to `rowq` and returns `SessPar::keep` for the first
    /// pass (below 64 when the last picture of this type ended on the ladder)
    /// (the C++ `Batch::quantize`). `rows` and `rowq` hold one entry per macroblock row.
    pub fn plan_picture(&mut self, ctx: &PicCtx, rows: &[RowStat], rowq: &mut [u8]) -> u8 {
        self.plan_picture_obs(ctx, rows, rowq, &mut ())
    }

    /// [`RateControl::plan_picture`], observed.
    pub fn plan_picture_obs<O: Observer + ?Sized>(
        &mut self,
        ctx: &PicCtx,
        rows: &[RowStat],
        rowq: &mut [u8],
        obs: &mut O,
    ) -> u8 {
        let ty = usize::from(ctx.pic_type);
        let nmb = f64::from(self.nmb);
        let mut asum = 0.0;
        let mut intra: u32 = 0;
        for r in rows {
            asum += f64::from(r.act_sum);
            intra = intra.wrapping_add(r.n_intra);
        }
        self.avg_act = asum / nmb;
        self.cur.hdr_bits = ctx.hdr_bits;
        let room = self.cur.cap - self.cur.e_before;
        if !self.cbr {
            // The fixed quantizer, and the one limit the picture has: it must fit the buffer.
            let q = ctx.fixed_q[ty - 1].clamp(1, 31);
            rowq.fill(q);
            self.cur.code = q;
            self.cur.frac = 0.0;
            self.cur.hard = room - 64.0;
            self.cur.limit = self.cur.hard;
            self.cur.target = 0.0;
            return self.keep();
        }
        let rate = ctx.rate;
        let t = 1.0 / ctx.fps;
        if ctx.key {
            // GOP budget (TM5 step 1), anchored to the buffer: every GOP is planned to end with
            // the queue at 15 % of what the mux delivers in D at the current rate.
            let len = ctx.to_next_key.max(1);
            let m = ctx.m;
            self.left = [0, 1, 0, 0];
            for gpos in 1..len {
                let k = if gpos % m == 0 || gpos == len - 1 {
                    PIC_P
                } else {
                    PIC_B
                };
                self.left[usize::from(k)] += 1;
            }
            // the budget is what the mux delivers until the next key frame: a matter of display
            // time, not of the number of pictures
            self.r = rate * ctx.gop_frames * t + (0.15 * rate * self.vbv.d - self.cur.e_before);
        }
        let kp = 1.0;
        let kb = 1.4;
        // Complexity of this picture from its own statistics (bits at the last quantizer x
        // that quantizer).
        {
            let tot = total(rows);
            let m = self.mdl[ty];
            let qref = clamp(self.last_qs, 2.0, 112.0);
            let hdr = model_hdr(&m, self.nmb, ctx.pic_type);
            self.x[ty] = qref * (m.theta * nz_at(&tot, qref) + hdr + self.cur.hdr_bits);
        }
        let xi = self.x[usize::from(PIC_I)];
        let xp = self.x[usize::from(PIC_P)];
        let xb = self.x[usize::from(PIC_B)];
        let np = f64::from(self.left[usize::from(PIC_P)]);
        let nb = f64::from(self.left[usize::from(PIC_B)]);
        let mut tt = if ctx.pic_type == PIC_I {
            self.r / (1.0 + np * xp / (xi * kp) + nb * xb / (xi * kb))
        } else if ctx.pic_type == PIC_P {
            self.r / (max(np, 1.0) + nb * kp * xb / (kb * xp))
        } else {
            self.r / (max(nb, 1.0) + np * kb * xp / (kp * xb))
        };
        // A P picture that is mostly intra (scene cut) needs an I picture's share, out of the
        // buffer.
        if ctx.pic_type == PIC_P && u64::from(intra) * 2 > u64::from(self.nmb) {
            let ti = min(
                0.4 * room,
                rate * f64::from(ctx.gop) * t / (1.0 + np * xp / (xi * kp) + nb * xb / (xi * kb)),
            );
            if ti > tt {
                self.r += ti - tt;
                tt = ti;
            }
        }
        // floors in terms of what is delivered while this picture is shown
        let shown = t * ctx.frames;
        tt = max(tt, rate * shown / 8.0);
        tt = max(tt, rate * shown - self.cur.e_before);
        tt = min(tt, 0.85 * room);
        self.cur.target = tt;
        self.cur.hard = room - 64.0;
        let mi = self.mdl[usize::from(PIC_I)];
        let ifloor = if mi.hdr >= 0.0 { mi.hdr } else { 90.0 * nmb };
        let cap = self.cur.cap;
        let reserve = if ctx.pic_type == PIC_I {
            0.05 * cap
        } else {
            min(0.6 * cap, max(0.3 * cap, 1.1 * ifloor + 0.05 * cap))
        };
        self.cur.limit = min(self.cur.hard, max(1.5 * tt, room - reserve));
        // ladder state from the last picture of this type: start where it ended
        let ok = self.choose_obs(ctx.pic_type, tt - self.cur.hdr_bits, rows, rowq, obs);
        if !ok && self.last_rung[ty] >= 32 {
            // the all-skipped picture is only ever reached by a pass that failed
            self.cur.rung = self.last_rung[ty].min(36);
        }
        self.keep()
    }

    /// Row quantizers for a slice budget of `target` bits (the C++ `Batch::choose`).
    /// Returns false when even code 31 is predicted not to fit. Sets `cur.code`, `cur.frac`,
    /// `cur.nzpred`, with the model `mdl[pic_type]` as it is now.
    pub fn choose(&mut self, pic_type: u8, target: f64, rows: &[RowStat], rowq: &mut [u8]) -> bool {
        self.choose_obs(pic_type, target, rows, rowq, &mut ())
    }

    /// [`RateControl::choose`], observed.
    pub fn choose_obs<O: Observer + ?Sized>(
        &mut self,
        pic_type: u8,
        target: f64,
        rows: &[RowStat],
        rowq: &mut [u8],
        obs: &mut O,
    ) -> bool {
        let ok = self.choose_inner(pic_type, target, rows, rowq);
        let m = self.mdl[usize::from(pic_type)];
        obs.choose(&ChooseEvent {
            pic_type,
            target,
            theta: m.theta,
            hdr: m.hdr,
            ok,
            code: self.cur.code,
            frac: self.cur.frac,
            nzpred: self.cur.nzpred,
            rowq,
        });
        ok
    }

    fn choose_inner(
        &mut self,
        pic_type: u8,
        target: f64,
        rows: &[RowStat],
        rowq: &mut [u8],
    ) -> bool {
        let m = self.mdl[usize::from(pic_type)];
        let hdr = model_hdr(&m, self.nmb, pic_type);
        let tot = total(rows);
        let mut c1: u8 = 0;
        let mut nz1 = 0.0;
        for c in 1..=31u8 {
            nz1 = nz_at(&tot, qscale(c));
            if m.theta * nz1 + hdr <= target {
                c1 = c;
                break;
            }
        }
        if c1 == 0 {
            rowq.fill(31);
            self.cur.code = 31;
            self.cur.frac = 0.0;
            self.cur.nzpred = nz1;
            return false;
        }
        let mut frac = 0.0;
        if c1 > 1 {
            let p1 = m.theta * nz1 + hdr;
            let p0 = m.theta * nz_at(&tot, qscale(c1 - 1)) + hdr;
            if p0 > p1 {
                frac = clamp((target - p1) / (p0 - p1), 0.0, 1.0);
            }
        }
        let mut nz = 0.0;
        for (y, (q, row)) in rowq.iter_mut().zip(rows).enumerate() {
            let y = y as f64;
            let fine = ((y + 1.0) * frac).floor() > (y * frac).floor();
            *q = if fine { c1 - 1 } else { c1 };
            nz += nz_at(row, qscale(*q));
        }
        self.cur.code = c1;
        self.cur.frac = frac;
        self.cur.nzpred = nz;
        true
    }

    /// Judge a pass by its measured size (the C++ `Batch::retry`, per session). May
    /// rewrite `rowq`, and `ctx.pic_type` / `ctx.key` when an I picture is replaced by a P
    /// picture.
    ///
    /// A session with a fixed quantizer keeps it unless the picture does not fit: the buffer
    /// (`bits > limit`, the limit being the room the buffer has), the output buffer, or the
    /// macroblock size rule. Then every row (for the rule: the rows named) takes the next code
    /// that doubles its quantiser_scale, and from code 31 on the picture goes up the ladder like
    /// any other. Nothing is ever cut: a picture that cannot fit ends as an all-skipped picture.
    ///
    /// The macroblock size rule (H.262 8.2, Table 8-6, p.110: at most 4608 bits per 4:2:0
    /// macroblock, at most two macroblocks of a row above that) is enforced here: a pass whose
    /// [`PicStat::big_rows`] names a row is never accepted. Each such row gets a floor of the
    /// next quantizer code that doubles its quantiser_scale ([`coarser`]), kept for the rest of
    /// the picture whatever the model chooses afterwards, and the picture is quantized again;
    /// a row already at code 31 sends the picture up the ladder, whose rungs keep at most 12
    /// coefficients per block (at most 72 x 24 bits per macroblock) and so cannot break the rule.
    ///
    /// A correction that ends with the same code and nearly the same fraction
    /// (`|frac - frac0| < 0.03`) is not worth a pass: the pass stands, and so do its row codes
    /// and `code` / `frac` / `nzpred`, which describe the bits the picture has
    /// ([`RateControl::finish_picture`] reads them). (The C++ accepted the pass but left the
    /// values of the correction it had just computed and not used.)
    pub fn after_pass(
        &mut self,
        ctx: &mut PicCtx,
        rows: &[RowStat],
        pic: PicStat,
        rowq: &mut [u8],
    ) -> Pass {
        self.after_pass_obs(ctx, rows, pic, rowq, &mut ())
    }

    /// [`RateControl::after_pass`], observed.
    pub fn after_pass_obs<O: Observer + ?Sized>(
        &mut self,
        ctx: &mut PicCtx,
        rows: &[RowStat],
        pic: PicStat,
        rowq: &mut [u8],
        obs: &mut O,
    ) -> Pass {
        let ty = usize::from(ctx.pic_type);
        let bits = 8.0 * f64::from(pic.bytes);
        let tex = f64::from(pic.tex_bits);
        // uint32_t + int stays uint32_t in the C++.
        let over_cap = u64::from(pic.bytes.wrapping_add(64)) > u64::from(ctx.cap_bytes);
        let tgt = self.cur.target - self.cur.hdr_bits;
        let limit = self.cur.limit - self.cur.hdr_bits;
        // The picture does not fit; or it has a row that H.262 Table 8-6 forbids.
        let must_size = bits > limit || over_cap;
        let big = pic.any_big_row();
        let must = must_size || big;
        let want = !must
            && self.cbr
            && self.cur.tries == 0
            && self.cur.rung == 0
            && (bits > 1.3 * tgt
                || (bits < 0.7 * tgt
                    && !(self.cur.code == 1 && self.cur.frac >= 1.0)
                    && self.cur.code > 1));
        let m = self.mdl[ty];
        obs.pass(&PassEvent {
            pic_type: ctx.pic_type,
            have_ref: ctx.have_ref,
            tries: self.cur.tries,
            rung: self.cur.rung,
            bytes: pic.bytes,
            tex_bits: pic.tex_bits,
            over_cap,
            big_rows: pic.big_rows,
            tgt,
            limit,
            must,
            want,
            code: self.cur.code,
            frac: self.cur.frac,
            nzpred: self.cur.nzpred,
            theta: m.theta,
            hdr: m.hdr,
        });
        if !must && !want {
            return Pass::Accept;
        }
        // Rows that broke the macroblock size rule: coarser from now on. `stuck`: one of them
        // is at the end of the quantizer scale already.
        let mut stuck = false;
        if big {
            self.stats.mbfix += 1;
            for (r, (q, floor)) in rowq.iter().zip(self.row_floor.iter_mut()).enumerate() {
                if pic.big_row(r) {
                    stuck |= *q >= 31;
                    *floor = coarser(*q);
                }
            }
        }
        // correct the model with what this pass measured, then choose again
        let saved = self.mdl[ty];
        if self.cur.rung == 0 && tex > 0.0 {
            self.mdl[ty].hdr = bits - tex;
            if self.cur.nzpred > f64::from(self.nmb) * 2.0 {
                self.mdl[ty].theta = clamp(tex / self.cur.nzpred, 2.0, 16.0);
            }
        }
        let mut redo = false;
        if self.cur.rung == 0 {
            let code0 = self.cur.code;
            let frac0 = self.cur.frac;
            if !self.cbr {
                // Fixed quantizer: the picture does not fit, so every row takes the next code
                // that doubles its quantiser_scale; from code 31 on, the ladder.
                if must_size {
                    if rowq.iter().all(|q| *q >= 31) {
                        self.cur.code = 31;
                        self.cur.rung = 32;
                    } else {
                        for q in rowq.iter_mut() {
                            *q = coarser(*q);
                        }
                        self.cur.code = rowq.iter().copied().fold(0, u8::max);
                    }
                }
            } else if must_size || want {
                let t2 = if must_size {
                    min(tgt, 0.9 * limit)
                } else {
                    tgt
                };
                // what the pass was coded with, in case the correction is not worth making
                let nzpred0 = self.cur.nzpred;
                let n = rowq.len().min(self.rowq_pass.len());
                self.rowq_pass[..n].copy_from_slice(&rowq[..n]);
                let ok = self.choose_obs(ctx.pic_type, t2, rows, rowq, obs);
                let coarser = self.cur.code > code0
                    || (self.cur.code == code0 && self.cur.frac < frac0 - 1e-9);
                if !must && self.cur.code == code0 && (self.cur.frac - frac0).abs() < 0.03 {
                    self.mdl[ty] = saved;
                    rowq[..n].copy_from_slice(&self.rowq_pass[..n]);
                    self.cur.frac = frac0;
                    self.cur.nzpred = nzpred0;
                    return Pass::Accept;
                }
                let c2 = 31.min(code0 + 2);
                if must_size && ok && coarser && self.cur.tries >= 2 && self.cur.code < c2 {
                    // the model keeps under-correcting: take a larger step
                    rowq.fill(c2);
                    self.cur.code = c2;
                    self.cur.frac = 0.0;
                }
                if must_size && (!ok || !coarser) {
                    // the quantizer cannot be raised (enough): enter the ladder
                    rowq.fill(31);
                    self.cur.code = 31;
                    self.cur.frac = 0.0;
                    // 31: plain code 31 everywhere first
                    self.cur.rung = if code0 == 31 && frac0 == 0.0 { 32 } else { 31 };
                }
            }
            if stuck && self.cur.rung == 0 {
                // a row at code 31 still has macroblocks above the limit: only fewer
                // coefficients help
                rowq.fill(31);
                self.cur.code = 31;
                self.cur.frac = 0.0;
                self.cur.rung = 32;
            }
            if self.cur.rung == 0 {
                // the floors of this picture hold whatever was chosen
                for (q, floor) in rowq.iter_mut().zip(&self.row_floor) {
                    *q = (*q).max(*floor);
                }
            }
            redo = true;
        } else if must {
            // larger steps when far over; the all-skipped picture only directly after rung 36
            let ratio = bits / max(limit, 1.0);
            let step = if ratio > 2.5 {
                3
            } else if ratio > 1.5 {
                2
            } else {
                1
            };
            let rung = self.cur.rung;
            self.cur.rung = if rung == 31 {
                32
            } else if rung >= 36 {
                rung + 1
            } else {
                36.min(rung + step)
            };
            redo = true;
        }
        self.mdl[ty] = saved;
        if !redo {
            return Pass::Accept;
        }
        let mut all_skipped = false;
        let mut i_becomes_p = false;
        if self.cur.rung >= RUNG_SKIP {
            // last resort: an all-skipped P or B picture. An I picture is replaced by a P
            // picture (not possible for the first picture of a stream, which keeps rung 36).
            if ctx.pic_type == PIC_I && !ctx.have_ref {
                self.cur.rung = 36;
                self.stats.violations += 1;
                return Pass::Accept;
            }
            if ctx.pic_type == PIC_I {
                ctx.pic_type = PIC_P;
                ctx.key = false;
                self.stats.isub += 1;
                i_becomes_p = true;
            }
            if self.cur.rung > RUNG_SKIP {
                self.cur.rung = RUNG_SKIP;
                self.stats.violations += 1;
                return Pass::Accept;
            }
            all_skipped = true;
        }
        self.cur.tries += 1;
        self.stats.retries += 1;
        Pass::Again {
            keep: self.keep(),
            all_skipped,
            i_becomes_p,
        }
    }

    /// The picture is final: `au_bits` is the size of the whole access unit (headers and the
    /// slices as written, `8 * len`), `pic` the slice size the kernels measured, `rowq` the row
    /// quantizers as they stand. Updates the buffer, the models, the GOP budget and the ladder
    /// memory (the C++ `Batch::collect`). `ctx.pic_type` is the type as coded.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "the access unit reports avg_q as float"
    )]
    pub fn finish_picture(
        &mut self,
        ctx: &PicCtx,
        pic: PicStat,
        au_bits: f64,
        rowq: &[u8],
    ) -> PicResult {
        self.finish_picture_obs(ctx, pic, au_bits, rowq, &mut ())
    }

    /// [`RateControl::finish_picture`], observed.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "the access unit reports avg_q as float"
    )]
    pub fn finish_picture_obs<O: Observer + ?Sized>(
        &mut self,
        ctx: &PicCtx,
        pic: PicStat,
        au_bits: f64,
        rowq: &[u8],
        obs: &mut O,
    ) -> PicResult {
        let ty = usize::from(ctx.pic_type);
        let bits = au_bits;
        let mut qs = 0.0;
        for &q in rowq {
            qs += qscale(q);
        }
        qs /= rowq.len() as f64;
        let rung = self.cur.rung;
        if rung >= 32 {
            qs = 112.0 * f64::from(1 + i32::from(rung) - 31);
        }
        self.last_qs = qs;
        let mut res = PicResult {
            avg_q: f64::from(qs as f32),
            queue_bits: 0.0,
            cap_bits: 0.0,
            room_bits: 0.0,
            rung: if rung >= 32 { rung } else { 0 },
            tries: self.cur.tries,
        };
        let cap = self.cur.cap;
        if bits > cap - self.cur.e_before + 0.5 {
            self.stats.violations += 1;
        }
        self.vbv.add(bits);
        obs.vbv(VbvEvent::Add { bits }, &self.vbv);
        res.queue_bits = self.vbv.queue;
        res.cap_bits = cap;
        res.room_bits = cap - self.vbv.queue;
        self.stats.room_min = min(self.stats.room_min, res.room_bits);
        if rung >= 32 {
            self.stats.rung_pics += 1;
        }
        if rung >= RUNG_SKIP {
            self.stats.skip_pics += 1;
        }
        if self.cbr {
            // model and complexity updates
            let sl = 8.0 * f64::from(pic.bytes);
            let tex = f64::from(pic.tex_bits);
            let m = &mut self.mdl[ty];
            if rung < 32 && self.cur.nzpred > f64::from(self.nmb) * 2.0 && tex > 0.0 {
                let th = clamp(tex / self.cur.nzpred, 2.0, 16.0);
                m.theta = if m.hdr < 0.0 {
                    th
                } else {
                    0.5 * m.theta + 0.5 * th
                };
                m.hdr = if m.hdr < 0.0 {
                    sl - tex
                } else {
                    0.5 * m.hdr + 0.5 * (sl - tex)
                };
            } else if rung < 32 && tex >= 0.0 {
                m.hdr = if m.hdr < 0.0 {
                    sl - tex
                } else {
                    0.5 * m.hdr + 0.5 * (sl - tex)
                };
            }
            self.x[ty] = bits * qs;
            self.r -= bits;
            if self.left[ty] > 0 {
                self.left[ty] -= 1;
            }
            self.floor_hist[ty] = rowq.iter().copied().fold(31, u8::min);
            // next picture of this type starts on the same rung, or one easier if this one had
            // room to spare
            self.last_rung[ty] = if rung >= 32 { rung } else { 0 };
            if rung >= 32 && sl < 0.6 * (self.cur.limit - self.cur.hdr_bits) {
                self.last_rung[ty] = if rung > 32 { rung - 1 } else { 0 };
            }
        }
        res
    }

    /// A repeated picture made on the host, of `bits` bits at time `t`, in the position of a
    /// picture of type `position_type` (the C++ `Batch::host_repeat`). Touches the
    /// buffer, `e_before`, `cap`, the GOP budget and the counters; not the models, `last_qs`,
    /// the working `rung` / `tries` or the ladder memory. The two halves are
    /// [`RateControl::repeat_advance`] and [`RateControl::repeat_add`].
    pub fn repeat_picture(&mut self, t: f64, bits: f64, position_type: u8) -> PicResult {
        self.repeat_advance(t, &mut ());
        self.repeat_add(bits, position_type, &mut ())
    }

    /// First half of [`RateControl::repeat_picture`]: the buffer advances to the picture's time
    /// (before the picture is made, as in the C++).
    pub fn repeat_advance<O: Observer + ?Sized>(&mut self, t: f64, obs: &mut O) {
        self.cur.e_before = self.vbv.advance(t);
        obs.vbv(VbvEvent::Advance { to: t }, &self.vbv);
        self.cur.cap = self.vbv.cap();
    }

    /// Second half of [`RateControl::repeat_picture`]: the picture of `bits` bits enters the
    /// buffer.
    pub fn repeat_add<O: Observer + ?Sized>(
        &mut self,
        bits: f64,
        position_type: u8,
        obs: &mut O,
    ) -> PicResult {
        let mut res = PicResult {
            avg_q: 0.0,
            queue_bits: 0.0,
            cap_bits: 0.0,
            room_bits: 0.0,
            rung: RUNG_SKIP,
            tries: 0,
        };
        let cap = self.cur.cap;
        if bits > cap - self.cur.e_before + 0.5 {
            self.stats.violations += 1;
        }
        self.vbv.add(bits);
        obs.vbv(VbvEvent::Add { bits }, &self.vbv);
        res.queue_bits = self.vbv.queue;
        res.cap_bits = cap;
        res.room_bits = cap - self.vbv.queue;
        self.stats.room_min = min(self.stats.room_min, res.room_bits);
        if self.cbr {
            self.r -= bits;
            let ty = usize::from(position_type);
            if self.left[ty] > 0 {
                self.left[ty] -= 1;
            }
        }
        res
    }
}
