//! The encoder and its tick (the C++ `Encoder`, `Session`, `Batch::begin` .. `collect`).
//!
//! # Calling sequence
//!
//! The driver owns a device (`tb_device::Device`) and one [`Plan`]; the encoder owns neither
//! a clock nor memory in the arenas, it is lent the host arena for the length of a call.
//!
//! ```text
//! let mut enc = Encoder::new(options, dev.arenas());
//! let s = enc.open(&config)?;                       // any time between ticks
//!
//! // Optional, after opening sessions and at the end (the C++ `Encoder::finish`): wait for
//! // the streams, so that the clearing of new batches is not charged to the first tick.
//! run(enc.settle(StreamId::UPLOAD, host, &mut plan));   // only with `overlap`
//! run(enc.settle(StreamId::MAIN, host, &mut plan));
//!
//! loop {                                            // one iteration per frame period
//!     // input, between ticks:
//!     let buf = enc.frame_buffer(s)?;               // write w*h*3/2 bytes of 4:2:0 there ...
//!     enc.submit(s, FrameSource::Host, pts, cc)?;   // ... then queue the frame
//!     // or: submit(s, FrameSource::HostAt(region), ..): a frame in the caller's own region
//!     // or: frame_target(s) + own kernels on MAIN + submit(s, FrameSource::Device{..}, ..)
//!     // or: submit(s, FrameSource::Again, ..); also set_rate, force_keyframe, hold, close.
//!
//!     let mut step = enc.begin_tick(dev.now(), deadline, dev.host_mut(), &mut plan);
//!     let mut ticket = None;
//!     loop {
//!         match step {
//!             Step::Done => break,
//!             Step::Run => {
//!                 // enc.phase() names the plan (stable wait class for the adapter)
//!                 match dev.submit(&plan) {
//!                     Ok(t) => ticket = Some(t),
//!                     Err(e) => { step = enc.resume(&failed(e), ..); continue; }
//!                 }
//!             }
//!             Step::Wait => {}                      // the plan is still in flight
//!         }
//!         let done = dev.wait(ticket, timeout);
//!         step = enc.resume(&done, dev.now(), dev.host_mut(), &mut plan);
//!     }
//!     while let Some(au) = enc.take_output(s) { ... }   // coded order, PTS/DTS, buffer state
//! }
//! ```
//!
//! `run(step)` above is the inner loop without the outputs. A plan the adapter refuses at
//! `submit` is reported to [`Encoder::resume`] as a completion with that error. Commands
//! (`open`, `submit`, `close`, ...) are refused with [`EncoderError::State`] while a tick is
//! in progress. The host arena must keep its contents between calls: the encoder reads
//! results there after a completion and nothing else writes the regions of its batches
//! except the caller's frames in [`Encoder::frame_buffer`].
//!
//! # A tick
//!
//! | Phase | Plan (all batches in one plan) | Waits on | Read at completion |
//! |---|---|---|---|
//! | [`Phase::Sync`] (only with `overlap`) | nothing | `UPLOAD` | the frames uploaded during the last tick are on the device |
//! | [`Phase::Begin`] | set-up of new batches; `k_tick`; padding of device-written frames; (without `overlap`: frames up and their padding;) parameters and lists up; `Record(PARAMS)`; set mode, pyramid, motion search, smoothing, macroblock statistics, transform, row statistics down. With `overlap`, on `UPLOAD`: `WaitFor(PARAMS)`, the next frames, their padding | `MAIN` | row statistics |
//! | [`Phase::Quant`] (1 + correction passes) | parameters and row quantizers up; repeated transforms; quantize, resolve, pack, row sums, picture sums; sizes down | `MAIN` | picture sizes |
//! | [`Phase::Finish`] | output positions up; clear; scatter; reconstruction and pyramid of anchors; coded pictures down | `MAIN` | coded pictures |
//!
//! The three main plans are issued in every tick, empty when no session codes a picture: the
//! C++ encoder waits three times per tick, and the number of waits of a tick does not depend on
//! its work. The operations and their order are those of the C++ (`Encoder::step`); the
//! golden cases replay against them in normal form, operation for operation and wait for wait.
//!
//! **The upload fence.** The measured rule that the frame upload waits until the tick's
//! parameter copies have gone through is `Record(PARAMS)` on
//! `MAIN` after the parameter uploads of all batches and `WaitFor(PARAMS)` as the first step
//! on `UPLOAD`, as the golden records have it. That next tick's kernels find the frames on the
//! device is the host wait of [`Phase::Sync`], also as in the C++ encoder; the second event
//! (`FRAMES`) that docs/ARCHITECTURE.md proposed in its place is not used, so nothing in a plan
//! is outside what the golden operation records can confirm. Two deviations from the C++
//! queue order, neither visible in a golden case: `k_tick` and the padding of device-written
//! frames are queued at the head of `Begin` instead of before the wait for the upload stream
//! (same order on `MAIN`), so that a plan carries `MAIN` steps only when it waits on `MAIN`.
//!
//! **Host frames with `overlap`.** The upload of tick n runs while the caller already writes
//! the frame for tick n + 1, so a session has two host frame regions and
//! [`Encoder::frame_buffer`] alternates between them.
//!
//! # Display time (film mode)
//!
//! A submitted frame is shown for a number of *display units*: fields of an interlaced
//! session, frame periods of a progressive one. [`Encoder::submit`] means one frame period;
//! [`Encoder::submit_shown`] takes a [`Shown`]: three fields of a progressive frame (3:2
//! pulldown as flags: repeat_first_field), or a frame of a 59.94 Hz progressive session shown
//! two or three times (H.262 6.3.10, p.49-50; Table 8-7, p.110). **The caller decides**: the
//! timeline knows which source frame is behind each output field (`tb_timeline::film`); the
//! encoder never guesses a cadence, and it accepts any mix, picture by picture, with no
//! change of sequence parameters: film, video (two fields from two instants), stills,
//! repeats. What it guarantees, whatever is submitted:
//!
//! - **Every display unit is covered exactly once.** The session counts the units it has
//!   accepted ([`Encoder::display_units`]); a frame starts where the one before it ended.
//! - **Output fields alternate** (H.262 7.12, p.106). A frame whose first field has the parity
//!   of the field before it is refused ([`ShownError::Parity`]); so are durations and flags
//!   the sequence cannot carry ([`ShownError`], each with its clause). A refused frame leaves
//!   the session as it was.
//! - **One access unit per accepted frame**, whatever happens to it. A picture that is given
//!   up (deadline policy, hold, failed plan, lost device) becomes a repeat made on the host
//!   *with the display of its frame*: the same duration, field order and flags, so display
//!   time never has a gap or an overlap.
//!
//! **The tick.** A tick codes at most one picture per session: the oldest queued frame that is
//! on the device. A film session has no frame to submit in some ticks (four pictures in five
//! frame periods with 3:2 pulldown, two in five at 59.94 Hz); such a session has *nothing
//! due*. It is not late: it is not a candidate of the deadline policy, it costs no device
//! time, nothing of its state moves, and the tick's three plans are issued as always. Which
//! tick a picture is coded in has no meaning in the stream; its times come from the clocks
//! below.
//!
//! **Clocks**, all in display units, all exact integers:
//!
//! | Clock | Advances | Used for |
//! |---|---|---|
//! | display | by each accepted frame's duration | PTS; the GOP grid; the GOP time code (frame periods of display time, rounded down, H.262 6.3.8); parity |
//! | decoding | per coded picture by the intervals of H.262 Annex C.11 (C.9 for a progressive session): the duration of the picture output at that moment, which is the picture itself if it is a B picture and the previous I or P picture otherwise | DTS; the time of the buffer model and of rate changes |
//!
//! PTS and DTS are the 90 kHz stamps of those unit counts on the caller's time line
//! (`floor(unit * 90000 * fps_d / (fps_n * units per frame))`, integer arithmetic on the
//! exact fraction, so nothing accumulates; not reduced modulo 2^33, see
//! [`AccessUnit::pts33`]). The first picture is decoded one frame period before it is shown.
//! By construction a B picture has PTS = DTS and an I or P picture is shown when the next I
//! or P picture is decoded.
//!
//! **Key frames** are on a grid of display times, `gop` frame periods apart: the key frame is
//! the first frame that starts at or after a grid time ([`crate::gop`]). The promise to the
//! mux and the statmux controller is therefore in display time: key frames `gop` frame
//! periods apart on average, each at most one field late (two frame periods in a progressive
//! session that repeats frames), staggered sessions staying staggered. GOPs are closed;
//! temporal_reference counts pictures (H.262 6.3.9).
//!
//! **Buffer model and rate.** The buffer model runs on the decoding clock, so its removal
//! times are those a decoder uses (Annex C.9 to C.12); the delivery guarantee is stated per
//! unit of time as before (a picture has left the mux queue D seconds after its decoding
//! time on that clock, and the decoder never holds more than vbv_buffer_size). Pictures now
//! arrive at irregular intervals, never closer than one frame period, which is what the
//! lowest rate ([`tb_m2v_rate::control::min_rate`]) assumes. The GOP budget is the delivery
//! over the display time to the next grid key frame, and a picture's floors are in terms of
//! its own display time. For the statmux controller an access unit says how long it is shown
//! ([`AccessUnit::units`]): complexity and bits per second are sums over pictures divided
//! by the sum of their display times, not by a picture count.
//!
//! **Captions.** The `cc_data()` of a frame must have the cc_count of its display time: 10
//! constructs per field period of 1/59.94 s, so 20 and 30 with 3:2 pulldown (CTA-708-E 4.3.6,
//! Table 4), and one leading CEA-608 construct per field period shown.
//!
//! # Errors (docs/ARCHITECTURE.md §4.5)
//!
//! Nothing here panics on a device error; a completion's status is handled as follows.
//!
//! - `Timeout`: not a failure. [`Encoder::resume`] returns [`Step::Wait`] and leaves the plan
//!   alone; the driver waits again with the same ticket.
//! - `Failed`, `BadPlan`, `UnknownTicket`: the tick ends. Every picture the plan was coding
//!   becomes a repeat made on the host (valid, all macroblocks skipped) and its session starts
//!   again with a key frame at the next frame submitted. The first picture of a stream cannot
//!   be a repeat: it stays queued and is tried again in the next tick. Frames whose upload was
//!   part of a failed plan are remembered as missing (their pictures become repeats, by the
//!   rules of the deadline policy). A failed `Sync` plan only marks the frames it should have
//!   confirmed; the tick goes on. A failed set-up of a new batch is queued again with the next
//!   plan. The device stays in use.
//! - `Lost`: as `Failed` for the tick, and from then on every session behaves as on hold: each
//!   due picture is a repeat made on the host, the buffer model, the GOP grid, PTS/DTS and
//!   the coded index advance as always, and no plan is written (`begin_tick` returns
//!   [`Step::Done`]). Only a session that has not coded its first picture waits. No state is
//!   left half-updated: a picture is either an access unit in the session's output or still
//!   in its queue. Two ways on:
//!   - same process, new adapter with the same arena sizes: [`Encoder::device_restored`];
//!     every batch is cleared and gets its tables again with the next plan, every session
//!     codes nothing but repeats until its next key frame, which the next submitted frame is;
//!   - a fresh process (the CUDA driver may refuse a new context in a process that saw a
//!     fault): [`Encoder::snapshot`] gives what a session needs to continue (configuration,
//!     rate and pending rate change, counts, GOP grid, buffer model and rate-control state)
//!     as plain data, and [`Encoder::restore`] opens a session from it in the new encoder.
//!     Frames queued but not coded are not part of it.
//!
//! # Allocation
//!
//! After warm-up, `begin_tick` and `resume` allocate the bytes of each access unit (one
//! vector per picture, as the C++) and nothing else, with these exceptions: the reconstruction
//! of `keep_recon` (one vector per picture of slot 0); a tick in which the deadline policy
//! actually gives pictures up (`ShedState::plan` sorts its candidates in a vector); a rate
//! change (the buffer model's history may grow); a failed plan that puts a first picture back
//! into its queue. Queues and working vectors grow to their working size during the first
//! ticks and are reused.

mod batch;
mod session;

use std::collections::VecDeque;

use tb_device::kernel::Tick as KTick;
use tb_device::{
    ArenaAlloc, ArenaSizes, Completion, DevBuf, DeviceError, EventId, HostBuf, Kernel, Launch,
    Nanos, Op, Plan, StreamId,
};
use tb_kernel_abi::Tables;
use tb_m2v_rate::RateControl;
use tb_m2v_rate::control::min_rate;
use tb_m2v_syntax::PicType;
use tb_m2v_syntax::headers::{MAX_CC_TRIPLETS, SeqInfo};

use self::batch::{Batch, PhaseCount, RING_SLOTS};
use self::session::{Pending, Session, Source, find};
use crate::au::AccessUnit;
use crate::config::{EncoderOptions, SessionConfig};
use crate::golden::Gold;
use crate::gop::{Frame, GopState};
use crate::layout::{BatchLayout, geometry};
use crate::shed::{Candidate, ShedState};

/// Names a session of an [`Encoder`]. Ids count up from 0 in the order of opening and are not
/// used again. Stays valid until [`Encoder::release`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SessionId(pub u32);

/// What the driver does next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Submit the plan, wait for it, call [`Encoder::resume`] with the completion.
    Run,
    /// The plan submitted last is still in flight (its wait timed out): wait for it again with
    /// the same ticket, do not submit, and call [`Encoder::resume`] with the completion.
    Wait,
    /// The tick is over; outputs are ready.
    Done,
    /// Submit the plan and do not wait for it: more of this phase follows. Call
    /// [`Encoder::queued`] for the next plan. Only with [`EncoderOptions::split_plans`]: the
    /// work of one batch is on the device while the host prepares the next batch's (as the
    /// C++ queues each batch's operations before it turns to the next). The driver waits for
    /// such a plan after the phase's last plan ([`Step::Run`]) has completed, and reports the
    /// first failure among them with that completion.
    Queue,
}

/// Which plan of a tick the encoder wrote last.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Phase {
    /// Parameters, motion search, transform, row statistics (the C++ `wait(st, 0)`).
    Begin,
    /// Quantization and entropy coding (the C++ `wait(st, 1)` and the waits of the correction
    /// passes).
    Quant,
    /// Scatter, reconstruction, download (the C++ `wait(st, 2)`).
    Finish,
    /// The host's wait for the upload stream at the start of a tick with `overlap`.
    Sync,
    /// A wait outside a tick ([`Encoder::settle`]).
    Settle,
}

impl Phase {
    /// A small stable number per phase, for adapters that keep statistics per kind of wait
    /// (the C++ `wait_ema[which]`): 0, 1, 2 as the C++ numbers its three waits, 3 for
    /// [`Phase::Sync`], 4 for [`Phase::Settle`].
    pub fn class(self) -> u8 {
        match self {
            Phase::Begin => 0,
            Phase::Quant => 1,
            Phase::Finish => 2,
            Phase::Sync => 3,
            Phase::Settle => 4,
        }
    }
}

/// Wait class of a correction pass of [`Phase::Quant`] (the C++ waits for those without a
/// moving average; their duration differs from the first pass).
pub const CLASS_QUANT_RETRY: u8 = 5;

/// Why a command was refused. The session and the encoder are unchanged.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum EncoderError {
    /// The configuration cannot be coded.
    #[error("configuration: {0}")]
    Layout(#[from] crate::layout::LayoutError),
    /// The rate configuration cannot hold the buffer guarantee.
    #[error("rate: {0}")]
    Rate(#[from] tb_m2v_rate::control::RateConfigError),
    /// The stream the configuration describes would break a rule of its profile.
    #[error("configuration: {0}")]
    Profile(#[from] tb_m2v_syntax::profile::ProfileError),
    /// The caption bytes of a frame are not a legal `cc_data()`.
    #[error("captions: {0}")]
    Captions(#[from] tb_m2v_syntax::captions::CaptionError),
    /// An active format the encoder does not write (A/53 Part 4 Table 6.14; see
    /// `tb_m2v_syntax::afd::ActiveFormat::is_written`).
    #[error("active format {0:?} is not one the encoder writes")]
    ActiveFormat(tb_m2v_syntax::afd::ActiveFormat),
    /// A value of the configuration is out of range.
    #[error("configuration: {0}")]
    Config(&'static str),
    /// No room in an arena for a new batch.
    #[error("arena full: {need} bytes of {which} memory needed, {free} free")]
    ArenaFull {
        /// "device" or "host".
        which: &'static str,
        /// Bytes the batch needs.
        need: u64,
        /// Bytes free (possibly fragmented).
        free: u64,
    },
    /// The way a frame is to be shown would break a rule of the stream.
    #[error("display: {0}")]
    Shown(#[from] ShownError),
    /// The session id is not open.
    #[error("no such session")]
    NoSession,
    /// The command is not valid in the session's state (for example `submit_again` before any
    /// frame, or any command while a tick is in progress).
    #[error("invalid in this state: {0}")]
    State(&'static str),
}

/// How long a submitted frame is displayed and which field comes first: the timeline's
/// decision ([`Encoder::submit_shown`]). The encoder turns it into top_field_first,
/// repeat_first_field and progressive_frame (H.262 6.3.10, p.49-50).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Shown {
    /// Display units the frame is shown for. Interlaced session: fields, 2 or 3 (3:
    /// repeat_first_field, the first field shown again after the second). Progressive
    /// session: frame periods, 1, 2 or 3 (repeat_first_field with top_field_first 0 or 1).
    pub units: u8,
    /// Interlaced session: the first field shown is the top field. Must continue the parity
    /// of the frame before ([`Encoder::next_top_first`]). `false` in a progressive session.
    pub top_first: bool,
    /// Both fields are from one instant (progressive_frame = 1 in an interlaced session;
    /// the picture is then coded without field tools). Required for three fields.
    pub progressive: bool,
}

/// Why [`Encoder::submit_shown`] refused a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ShownError {
    /// Not a duration the picture coding extension can signal.
    #[error(
        "a frame shown for {units} {unit}: a picture of an interlaced sequence is output for \
         two or three fields, one of a progressive sequence for one, two or three frame periods \
         (H.262 §6.3.10, p.49-50)"
    )]
    Units {
        /// The duration asked for.
        units: u8,
        /// "fields" or "frame periods".
        unit: &'static str,
    },
    /// repeat_first_field is not allowed in this sequence.
    #[error(
        "a frame shown for {units} {unit} needs repeat_first_field = 1, which a {scan} sequence \
         at frame_rate_code {frame_rate_code} shall not use (H.262 §8.2.1, Table 8-7, p.110-111)"
    )]
    NotAllowed {
        /// The duration asked for.
        units: u8,
        /// "fields" or "frame periods".
        unit: &'static str,
        /// "progressive" or "interlaced".
        scan: &'static str,
        /// frame_rate_code of the session.
        frame_rate_code: u8,
    },
    /// repeat_first_field in a session whose B pictures may not carry it.
    #[error(
        "a frame shown for three fields in a Main level session of {height} lines at \
         frame_rate_code {frame_rate_code} with B pictures: there a B picture shall have \
         repeat_first_field = 0 (H.262 §8.2.1, p.111), and the picture type is not known yet"
    )]
    MainLevelB {
        /// Picture height.
        height: u32,
        /// frame_rate_code of the session.
        frame_rate_code: u8,
    },
    /// Three fields of a frame whose fields are two instants.
    #[error(
        "a frame of two instants shown for three fields: with progressive_frame = 0, \
         repeat_first_field shall be 0 (H.262 §6.3.10, p.50)"
    )]
    NotProgressive,
    /// The first field has the parity of the field before it.
    #[error(
        "the frame's first field is the {got} field, right after a {got} field: the fields at \
         the output of the decoding process shall always be alternately top and bottom (H.262 \
         §7.12, p.106)"
    )]
    Parity {
        /// "top" or "bottom".
        got: &'static str,
    },
}

impl Shown {
    /// The rules a frame shown this way must satisfy in session `se`; `progressive`: the frame
    /// is flagged as one instant.
    fn check(self, se: &Session, progressive: bool) -> Result<(), ShownError> {
        let interlaced = se.cfg.interlaced;
        let frc = se.seq.par.frame_rate_code;
        let unit = if interlaced {
            "fields"
        } else {
            "frame periods"
        };
        let units = self.units;
        let legal = if interlaced {
            matches!(units, 2 | 3)
        } else {
            matches!(units, 1..=3)
        };
        if !legal {
            return Err(ShownError::Units { units, unit });
        }
        if units != se.upf {
            // repeat_first_field = 1
            if !tb_m2v_syntax::profile::repeat_first_field_allowed(frc, !interlaced) {
                return Err(ShownError::NotAllowed {
                    units,
                    unit,
                    scan: if interlaced {
                        "interlaced"
                    } else {
                        "progressive"
                    },
                    frame_rate_code: frc,
                });
            }
            if se.cfg.m > 1
                && !tb_m2v_syntax::profile::repeat_first_field_in_b_allowed(
                    se.seq.par.level,
                    se.cfg.h,
                    frc,
                )
            {
                return Err(ShownError::MainLevelB {
                    height: se.cfg.h,
                    frame_rate_code: frc,
                });
            }
            if interlaced && !progressive {
                return Err(ShownError::NotProgressive);
            }
        }
        if interlaced && se.gop.n_in > 0 && self.top_first != se.next_top {
            return Err(ShownError::Parity {
                got: if self.top_first { "top" } else { "bottom" },
            });
        }
        Ok(())
    }
}

/// Where the caller puts a frame for [`Encoder::submit`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameSource {
    /// Raw 4:2:0 at the picture size (Y, then U, then V, no padding), written by the caller
    /// into the session's frame region of the host arena ([`Encoder::frame_buffer`], asked for
    /// before this submit); the tick uploads it.
    Host,
    /// Raw 4:2:0 at the picture size in a region of the host arena that the caller owns
    /// (outside the encoder's batches; `w * h * 3 / 2` bytes), uploaded from where it is: the
    /// C++ `Session::submit(const uint8_t *frame, ...)`. The caller leaves the region unchanged
    /// until the upload has completed: the end of the tick that follows this submit, or, with
    /// the overlapped upload, of the tick after that one.
    HostAt(HostBuf),
    /// The caller queues kernels on `MAIN` that write the frame into the session's input planes
    /// on the device before the tick's first plan ([`Encoder::frame_target`], asked for before
    /// this submit); the tick fills the padding.
    Device {
        /// Both fields are from one instant (progressive_frame = 1 in an interlaced sequence).
        progressive: bool,
    },
    /// The previous frame again (frame clock repeat, SPEC 5.4): no upload, no conversion.
    Again,
}

/// What the last tick did (the C++ `tk_*` fields).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TickStats {
    /// Pictures the deadline policy could have given up.
    pub candidates: u32,
    /// Pictures it gave up.
    pub shed: u32,
    /// Correction rounds of the quantization.
    pub rounds: u32,
    /// Pictures quantized again, summed over the rounds.
    pub retry: u32,
    /// Pictures transformed again because their quantizer went below the floor.
    pub rexf: u32,
    /// Macroblocks coded on the device.
    pub mbs: f64,
    /// The tick ended with a failed plan.
    pub failed: bool,
}

/// Counters of one session.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SessionStats {
    /// Pictures coded on the device.
    pub pictures: u64,
    /// Repeats made on the host, whatever the reason.
    pub repeats: u64,
    /// Key frame positions that carried a repeat.
    pub keys_missed: u64,
    /// Pictures given up by the deadline policy.
    pub shed: u64,
    /// Pictures that became repeats because their plan failed.
    pub failed: u64,
    /// Bits of all access units.
    pub total_bits: f64,
    /// Counters of the rate control.
    pub rate: tb_m2v_rate::control::Stats,
    /// Access units by display duration in display units (index 1, 2, 3; index 0 unused): in
    /// an interlaced session `shown[3]` counts the pictures with repeat_first_field, in a
    /// progressive one `shown[2]` and `shown[3]` the frames output twice and three times.
    pub shown: [u64; 4],
    /// Frames whose PTS as submitted differed from the display time the session counts.
    pub pts_mismatch: u64,
}

/// The state of a session as plain data: what [`Encoder::restore`] needs to continue the
/// stream in another encoder (another process), and what tests compare with the golden records.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionSnapshot {
    /// The configuration the session was opened with.
    pub config: SessionConfig,
    /// The delivery rate in force, bit/s.
    pub rate: u64,
    /// A rate change not yet applied: rate, at the next key frame only, ticket.
    pub pending_rate: Option<(u64, bool, u32)>,
    /// The ticket the next `set_rate` returns.
    pub next_ticket: u32,
    /// Frames accepted (the next display index).
    pub n_in: u64,
    /// Access units produced (the next coded index).
    pub n_coded: u64,
    /// Frames accepted and not yet coded.
    pub queued: u64,
    /// Display index of the first frame of the current GOP.
    pub gop_first: u64,
    /// Display time of the next key frame on the grid, in display units (fields of an
    /// interlaced session, frame periods of a progressive one) from the first frame.
    pub next_key: u64,
    /// Display units accepted: the display time at which the next frame starts.
    pub start: u64,
    /// Interlaced session: the next frame's first field is the top field.
    pub next_top: bool,
    /// The decoding clock in display units from the first picture's decoding time.
    pub dec_units: u64,
    /// Display units of the last coded I or P picture.
    pub prev_anchor: u8,
    /// Pictures and display units coded since the last key frame.
    pub gop_acc: (u32, u32),
    /// A key frame was asked for.
    pub want_key: bool,
    /// The next frame is a key frame off the grid.
    pub rekey: bool,
    /// Display index temporal_reference counts from.
    pub tref_base: u64,
    /// PTS of display index 0.
    pub pts0: i64,
    /// Reference slot of the most recent anchor.
    pub newer: u8,
    /// Display index of the picture in each reference slot.
    pub ref_disp: [i64; 2],
    /// A picture has been coded (the decoder has a reference).
    pub have_ref: bool,
    /// The newest anchor is a repeat of the one before.
    pub dup: bool,
    /// The session is on hold.
    pub hold: bool,
    /// The Active Format Description given to submitted frames.
    pub afd: Option<tb_m2v_syntax::afd::ActiveFormat>,
    /// Only a key frame is coded on the device (after `Lost` or a restore).
    pub need_key: bool,
    /// `close` was called.
    pub closing: bool,
    /// The last picture has been coded; the slot is free.
    pub closed: bool,
    /// Rate control and buffer model.
    pub rc: RateControl,
    /// Counters.
    pub stats: SessionStats,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Idle,
    Settle,
    Sync,
    Begin,
    Quant {
        pass: u32,
    },
    Finish,
    /// A part of the first quantization plan is queued; batches from `part` on follow.
    QuantParts,
    /// A part of the finish plan is queued.
    FinishParts,
}

/// The encoder: every batch and session of one device.
#[derive(Debug)]
pub struct Encoder {
    opt: EncoderOptions,
    arenas: ArenaSizes,
    dev_alloc: ArenaAlloc,
    host_alloc: ArenaAlloc,
    tables: Box<Tables>,
    h_tables: Option<HostBuf>,
    batches: Vec<Batch>,
    /// Ascending by id.
    sessions: Vec<Session>,
    next_session: u32,
    next_batch: u32,
    shed: ShedState,
    cands: Vec<Candidate>,
    cand_sess: Vec<usize>,
    shed_flags: Vec<bool>,
    scratch: Vec<u8>,
    /// With the frames event: an upload has been recorded on the upload stream.
    frames_recorded: bool,
    /// With split plans: the next batch of the phase being queued in parts.
    part: usize,
    /// Byte vectors given back by the caller ([`Encoder::recycle`]) for coming access units.
    spare: Vec<Vec<u8>>,
    state: State,
    t0: Nanos,
    lost: bool,
    stats: TickStats,
    /// The golden recorder (off unless [`Encoder::record_golden`]).
    gold: Gold,
}

fn round_up(x: u64, to: u64) -> u64 {
    x.div_ceil(to) * to
}

impl Encoder {
    /// An encoder for a device with these arenas.
    pub fn new(mut opt: EncoderOptions, arenas: ArenaSizes) -> Self {
        // A size the transform kernel does not exist in: the first one it does.
        if !tb_device::kernel::XF_BLOCKS.contains(&opt.xf_blocks) {
            opt.xf_blocks = tb_device::kernel::XF_BLOCKS[0];
        }
        let shed = match opt.force_shed {
            Some((_, seed)) => ShedState::seeded(seed),
            None => ShedState::seeded(0),
        };
        Self {
            opt,
            arenas,
            dev_alloc: ArenaAlloc::new(arenas.device),
            host_alloc: ArenaAlloc::new(arenas.host),
            tables: Box::new(tb_m2v_syntax::tables::tables()),
            h_tables: None,
            batches: Vec::new(),
            sessions: Vec::new(),
            next_session: 0,
            next_batch: 0,
            shed,
            cands: Vec::new(),
            cand_sess: Vec::new(),
            shed_flags: Vec::new(),
            scratch: Vec::with_capacity(1024),
            frames_recorded: false,
            part: 0,
            spare: Vec::new(),
            state: State::Idle,
            t0: Nanos(0),
            lost: false,
            stats: TickStats::default(),
            gold: Gold::default(),
        }
    }

    /// Record what the encoder decides and what crosses the host/device boundary as the
    /// records of `crates/tb-golden/vectors/README.md` (from now on; `false` stops and drops what was not taken).
    /// Turn it on before opening sessions. For the records to show one plan per phase as the
    /// format expects, leave [`EncoderOptions::split_plans`] off.
    pub fn record_golden(&mut self, on: bool) {
        self.gold.set(on);
    }

    /// The golden records written since the last call, in order, one JSON object per line
    /// (empty when recording is off).
    pub fn take_golden(&mut self) -> String {
        self.gold.take()
    }

    /// Hand a step to the caller: the recorder writes the plan and its wait, or the records
    /// written without a plan.
    fn handed(&mut self, step: Step, plan: &Plan) -> Step {
        if self.gold.on() {
            match step {
                Step::Run => {
                    let which = match self.state {
                        State::Begin => Some(0),
                        State::Quant { pass: 0 } => Some(1),
                        State::Finish => Some(2),
                        _ => None,
                    };
                    self.gold.commit(plan, true, which);
                }
                Step::Queue => self.gold.commit(plan, false, None),
                Step::Done => self.gold.flush(),
                Step::Wait => {}
            }
        }
        step
    }

    fn between_ticks(&self) -> Result<(), EncoderError> {
        if self.state == State::Idle {
            Ok(())
        } else {
            Err(EncoderError::State("a tick is in progress"))
        }
    }

    fn si(&self, s: SessionId) -> Result<usize, EncoderError> {
        find(&self.sessions, s.0).ok_or(EncoderError::NoSession)
    }

    fn bi(&self, id: u32) -> Option<usize> {
        self.batches.iter().position(|b| b.id == id)
    }

    /// The session and its batch.
    fn pair(&self, s: SessionId) -> Result<(usize, usize), EncoderError> {
        let si = self.si(s)?;
        let bi = self
            .bi(self.sessions[si].batch)
            .ok_or(EncoderError::State("the session's batch is gone"))?;
        Ok((si, bi))
    }

    /// Open a session: a free slot of a batch of its format, or a new batch of `c.capacity`
    /// slots laid out in the arenas (cleared, and given its tables, by the next plan that waits
    /// on `MAIN`). The first picture is a key frame with a sequence header. Sessions that are
    /// running are not touched.
    pub fn open(&mut self, c: &SessionConfig) -> Result<SessionId, EncoderError> {
        self.open_with(c, None)
    }

    fn open_with(
        &mut self,
        c: &SessionConfig,
        rc: Option<RateControl>,
    ) -> Result<SessionId, EncoderError> {
        self.between_ticks()?;
        // The one check of what the sequence may say (H.262 section 8, A/53 Part 4 6.1,
        // SCTE 43 7.1): nothing that passes can be written as an illegal header.
        let seq_par = tb_m2v_syntax::profile::check(&c.seq_config(), c.profile)?;
        let fps = f64::from(c.fps_n) / f64::from(c.fps_d);
        let found = self
            .batches
            .iter()
            .position(|b| b.matches(c) && (b.used as usize) < b.n());
        let (nmb, mbh) = match found {
            Some(bi) => {
                let g = &self.batches[bi].lay.geo;
                (g.nmb.unsigned_abs(), g.mbh.unsigned_abs())
            }
            None => {
                let (g, _) = geometry(c, c.capacity)?;
                (g.nmb.unsigned_abs(), g.mbh.unsigned_abs())
            }
        };
        let rc = match rc {
            Some(rc) => rc,
            None => RateControl::new(c.bitrate, c.max_bitrate, c.vbv_bits, nmb, mbh, fps)?,
        };
        let bi = match found {
            Some(bi) => bi,
            None => {
                let bi = self.new_batch(c)?;
                self.gold_batch(bi);
                bi
            }
        };
        let b = &mut self.batches[bi];
        let slot = b.sess.iter().position(Option::is_none).unwrap_or(0);
        // display units per frame period: fields, or frames of a progressive sequence
        let upf: u8 = if c.interlaced { 2 } else { 1 };
        let id = self.next_session;
        self.next_session += 1;
        if self.gold.on() {
            let first_min = i64::try_from(tb_m2v_rate::control::first_min(
                nmb,
                mbh,
                c.max_bitrate,
                c.vbv_bits,
            ))
            .unwrap_or(i64::MAX);
            let (bid, min) = (b.id, min_rate(mbh, fps));
            let (x, d) = (rc.x, rc.vbv.d);
            self.gold.put("open", |r| {
                r.i("s", i64::from(id))
                    .i("b", i64::from(bid))
                    .u("slot", slot as u64)
                    .s("error", "")
                    .u("w", u64::from(c.w))
                    .u("h", u64::from(c.h))
                    .u("fps_n", u64::from(c.fps_n))
                    .u("fps_d", u64::from(c.fps_d))
                    .b("interlaced", c.interlaced)
                    .b("tff", c.tff)
                    .u("gop", u64::from(c.gop))
                    .u("m", u64::from(c.m))
                    .u("gop_phase", u64::from(c.gop_phase))
                    .u("max_bitrate", c.max_bitrate)
                    .u("vbv_bits", c.vbv_bits)
                    .u("bitrate", c.bitrate)
                    .u("qi", u64::from(c.fixed_q[0]))
                    .u("qp", u64::from(c.fixed_q[1]))
                    .u("qb", u64::from(c.fixed_q[2]))
                    .b("aq", c.aq)
                    .d("aq_strength", f64::from(c.aq_strength))
                    .b("field_pred", c.field_pred)
                    .b("field_dct", c.field_dct)
                    .i("range2", i64::from(c.range2))
                    .i("lambda", i64::from(c.lambda))
                    .i("field_bits", i64::from(c.field_bits))
                    .b("smooth", c.smooth)
                    .b("tpred", c.tpred)
                    .b("intra_vlc", c.intra_vlc)
                    .i("alt_scan", c.alt_scan.map_or(-1, i64::from))
                    .u("capacity", u64::from(c.capacity))
                    .i("priority", i64::from(c.priority))
                    .u("aspect", u64::from(c.aspect))
                    .s("profile", c.profile.name())
                    .ia(
                        "color",
                        c.color.map_or(Vec::new(), |k| {
                            vec![
                                i64::from(k.primaries),
                                i64::from(k.transfer),
                                i64::from(k.matrix),
                            ]
                        }),
                    )
                    .u("min_rate", min)
                    .i("first_min", first_min)
                    .u("nmb", u64::from(nmb))
                    .u("mbh", u64::from(mbh))
                    .d("fps", fps)
                    .da("X", x)
                    .d("D", d);
            });
            self.gold.flush();
        }
        b.sess[slot] = Some(id);
        b.used += 1;
        b.idle_steps = 0;
        self.sessions.push(Session {
            id,
            cfg: *c,
            batch: b.id,
            slot,
            fps,
            seq: SeqInfo {
                width: c.w,
                height: c.h,
                interlaced: c.interlaced,
                par: seq_par,
            },
            gop: GopState::default(),
            n_coded: 0,
            tref_base: 0,
            pts0: 0,
            upf,
            unit0: 0,
            pts_off: 0,
            next_top: c.tff,
            dec_units: 0,
            prev_anchor: upf,
            gop_acc: (0, 0),
            shown: [0; 4],
            pts_mismatch: 0,
            newer: 0,
            ref_disp: [0, 0],
            have_ref: false,
            last_dist_f: 0,
            last_dist_b: 0,
            up: None,
            n_real: 0,
            n_host: 0,
            last_slot: 0,
            last_avail: 0,
            fly_main: None,
            fly_up: None,
            fly_up_prev: None,
            bad_slots: 0,
            need_key: false,
            closing: false,
            closed: false,
            hold: false,
            hold_prev: false,
            afd: None,
            shed_now: false,
            dup: false,
            repeats: 0,
            keys_missed: 0,
            shed_count: 0,
            failed: 0,
            pictures: 0,
            rc,
            // a fixed-quantizer session is delivered at its ceiling
            rate: if c.bitrate > 0 {
                c.bitrate
            } else {
                c.max_bitrate
            },
            pending_rate: 0,
            pending_ticket: 0,
            next_ticket: 1,
            pending_at_gop: true,
            out: VecDeque::new(),
            total_bits: 0.0,
        });
        Ok(SessionId(id))
    }

    /// Lay a batch out in the arenas (`Batch::init`, arena path).
    fn new_batch(&mut self, c: &SessionConfig) -> Result<usize, EncoderError> {
        let lay = BatchLayout::new(c, c.capacity, self.opt.keep_recon)?;
        let full = |which, need, a: &ArenaAlloc| EncoderError::ArenaFull {
            which,
            need,
            free: a.free_bytes(),
        };
        let tables_len = round_up(size_of::<Tables>() as u64, 64);
        let took_tables = self.h_tables.is_none();
        if took_tables {
            let off = self
                .host_alloc
                .take(tables_len)
                .ok_or_else(|| full("host", tables_len, &self.host_alloc))?;
            self.h_tables = Some(HostBuf::new(off, size_of::<Tables>() as u64));
        }
        let undo_tables = |e: &mut Self| {
            if took_tables && let Some(t) = e.h_tables.take() {
                e.host_alloc.give(t.off, tables_len);
            }
        };
        let alt_len = if self.opt.overlap {
            round_up(lay.h_frames.len.max(16), 64)
        } else {
            0
        };
        let host_len = lay.host_bytes + alt_len;
        let Some(dev_off) = self.dev_alloc.take(lay.dev_bytes) else {
            undo_tables(self);
            return Err(full("device", lay.dev_bytes, &self.dev_alloc));
        };
        let Some(host_off) = self.host_alloc.take(host_len) else {
            self.dev_alloc.give(dev_off, lay.dev_bytes);
            undo_tables(self);
            return Err(full("host", host_len, &self.host_alloc));
        };
        let placed = lay.at(dev_off, host_off);
        let alt = self
            .opt
            .overlap
            .then(|| HostBuf::new(host_off + lay.host_bytes, lay.h_frames.len));
        let id = self.next_batch;
        self.next_batch += 1;
        self.batches.push(Batch::new(
            id,
            c,
            placed,
            (dev_off, lay.dev_bytes),
            (host_off, host_len),
            alt,
            self.opt.xf_blocks,
        ));
        Ok(self.batches.len() - 1)
    }

    /// The `batch` record of a batch just laid out, and the names of its buffers.
    fn gold_batch(&mut self, bi: usize) {
        if !self.gold.on() {
            return;
        }
        let b = &self.batches[bi];
        let l = &b.lay;
        let keep_recon = self.opt.keep_recon;
        self.gold.batch(l, b.dev_block, self.h_tables);
        self.gold.put("batch", |r| {
            r.i("b", i64::from(b.id))
                .u("w", u64::from(b.fmt.w))
                .u("h", u64::from(b.fmt.h))
                .u("fps_n", u64::from(b.fmt.fps_n))
                .u("fps_d", u64::from(b.fmt.fps_d))
                .b("interlaced", b.fmt.interlaced)
                .i("capacity", i64::from(l.geo.n))
                .hex("geo", bytemuck::bytes_of(&l.geo))
                .u("row_bs", u64::from(l.row_bs))
                .u("dev_bytes", l.dev_bytes)
                .u("cap_words", u64::from(l.cap_words))
                .u("fbytes", l.fbytes())
                .b("arena", true)
                .b("keep_recon", keep_recon);
        });
    }

    /// No more input for the session: its waiting pictures are coded in the next ticks, the
    /// last one carries `AccessUnit::last`, then the slot is free ([`Encoder::is_closed`]).
    /// The id stays valid, for [`Encoder::take_output`], until [`Encoder::release`].
    pub fn close(&mut self, s: SessionId) -> Result<(), EncoderError> {
        self.between_ticks()?;
        let si = self.si(s)?;
        self.gold_flush(si);
        let se = &mut self.sessions[si];
        se.gop.flush();
        se.closing = true;
        if se.idle() && !se.closed {
            self.finish_close(si);
        }
        Ok(())
    }

    fn finish_close(&mut self, si: usize) {
        let s = &mut self.sessions[si];
        if let Some(au) = s.out.back_mut() {
            au.last = true;
        }
        s.closed = true;
        let (batch, slot) = (s.batch, s.slot);
        if let Some(bi) = self.bi(batch) {
            let b = &mut self.batches[bi];
            if let Some(e) = b.sess.get_mut(slot) {
                *e = None;
            }
            b.used = b.used.saturating_sub(1);
        }
    }

    /// Forget a closed session (its remaining output is dropped).
    pub fn release(&mut self, s: SessionId) -> Result<(), EncoderError> {
        self.between_ticks()?;
        let si = self.si(s)?;
        if !self.sessions[si].closed {
            return Err(EncoderError::State("the session is not closed"));
        }
        self.sessions.remove(si);
        Ok(())
    }

    /// No more input for now: pending B pictures are closed with a P picture
    /// (`Session::flush`). The session stays open; the call has an effect once.
    pub fn flush(&mut self, s: SessionId) -> Result<(), EncoderError> {
        self.between_ticks()?;
        let si = self.si(s)?;
        self.gold_flush(si);
        self.sessions[si].gop.flush();
        self.gold.flush();
        Ok(())
    }

    /// The `flush` record and the `emit` records of what the flush closes (the first flush of
    /// a session only, as the C++ `Session::flush`).
    fn gold_flush(&mut self, si: usize) {
        let se = &mut self.sessions[si];
        if !self.gold.on() || se.gop.flushed {
            return;
        }
        let (id, pending, coded) = (se.id, se.gop.pending.len(), se.gop.coded.len());
        self.gold.put("flush", |r| {
            r.i("s", i64::from(id)).u("pending", pending as u64);
        });
        // the frames the flush closes, as Session::emit writes them
        let mut probe = se.gop.clone();
        probe.flush();
        for f in probe.coded.iter().skip(coded) {
            crate::golden::emit(&mut self.gold, id, f, se.film(f));
        }
    }

    /// Queue one frame, shown for one frame period in the session's field order, with its
    /// presentation time (90 kHz) and `cc_data` triplets (3 bytes each): none, or a legal
    /// `cc_data()` of one frame at the session's frame rate
    /// ([`tb_m2v_syntax::captions::validate`]: the cc_count of the rate, the marker bits, a
    /// legal order of constructs; anything else is refused with [`EncoderError::Captions`]).
    /// One frame per session and tick can come from [`FrameSource::Host`],
    /// [`FrameSource::HostAt`] or [`FrameSource::Device`]; [`FrameSource::Again`] needs one
    /// earlier frame. This is [`Encoder::submit_shown`] with [`Encoder::ordinary`].
    pub fn submit(
        &mut self,
        s: SessionId,
        source: FrameSource,
        pts: i64,
        cc: &[u8],
    ) -> Result<(), EncoderError> {
        let shown = self.ordinary(s)?;
        self.submit_shown(s, source, pts, cc, shown)
    }

    /// How a frame of one frame period is shown in this session: two fields in the session's
    /// field order, or one frame of a progressive session. Not progressive (a
    /// [`FrameSource::Device`] frame says so itself).
    pub fn ordinary(&self, s: SessionId) -> Result<Shown, EncoderError> {
        let se = &self.sessions[self.si(s)?];
        Ok(Shown {
            units: se.upf,
            top_first: se.cfg.interlaced && se.cfg.tff,
            progressive: false,
        })
    }

    /// The parity the next frame's first field must have (interlaced sessions): what
    /// [`Shown::top_first`] of the next [`Encoder::submit_shown`] has to be.
    pub fn next_top_first(&self, s: SessionId) -> Result<bool, EncoderError> {
        Ok(self.sessions[self.si(s)?].next_top)
    }

    /// Queue one frame that is shown as `shown` says: film mode. The caller (the timeline)
    /// decides how long each picture is displayed; the encoder writes the flags and keeps
    /// every clock in display time. See the module documentation, "Display time".
    ///
    /// Refused with [`EncoderError::Shown`], the session unchanged, when the stream would
    /// break a rule: a duration the sequence cannot signal, repeat_first_field where H.262
    /// Table 8-7 has none, three fields of a frame that is not progressive, or a first field
    /// of the wrong parity. `cc` is checked against the cc_count of the picture's display
    /// time ([`tb_m2v_syntax::captions::validate_shown`]). `pts`: of the session's first frame
    /// it fixes the time base; of later frames it should be the display time the session
    /// counts (the access units carry that; a different value is counted in
    /// [`SessionStats::pts_mismatch`] and otherwise ignored).
    pub fn submit_shown(
        &mut self,
        s: SessionId,
        source: FrameSource,
        pts: i64,
        cc: &[u8],
        shown: Shown,
    ) -> Result<(), EncoderError> {
        self.between_ticks()?;
        let (si, bi) = self.pair(s)?;
        let nstep = self.batches[bi].nstep;
        let fbytes = self.batches[bi].lay.fbytes();
        let host_size = self.arenas.host;
        let overlap = self.opt.overlap;
        let lost = self.lost;
        let se = &mut self.sessions[si];
        if se.closing {
            return Err(EncoderError::State("the session is closing"));
        }
        let from_device = matches!(source, FrameSource::Device { progressive: true });
        shown.check(se, shown.progressive || from_device)?;
        // The caption bytes go into the stream as they are: whole constructs with their marker
        // bits (which also keeps the user data free of start code emulation, H.262 6.3.4.1),
        // the cc_count of the picture's display time, a legal sequence of constructs
        // (CTA-708-E 4.3).
        tb_m2v_syntax::captions::validate_shown(
            cc,
            se.seq.par.frame_rate_code,
            shown.units,
            se.upf,
        )?;
        let fresh = |se: &mut Session, src: Source, avail: u64| {
            let slot = (se.n_real % RING_SLOTS) as u8;
            se.n_real += 1;
            se.last_slot = slot;
            se.last_avail = avail;
            if lost {
                // nothing reaches a lost device
                se.bad_slots |= 1 << slot;
            } else {
                se.bad_slots &= !(1 << slot);
                se.up = Some(Pending { slot, src });
            }
            (slot, avail)
        };
        let busy = EncoderError::State("a frame of this tick is already waiting for the device");
        let (slot, avail_step, progressive) = match source {
            FrameSource::Host => {
                if se.up.is_some() {
                    return Err(busy);
                }
                let alt = overlap && se.n_host % 2 == 1;
                se.n_host += 1;
                let (slot, avail) = fresh(se, Source::Host { alt }, nstep + u64::from(overlap));
                (slot, avail, false)
            }
            FrameSource::HostAt(buf) => {
                if se.up.is_some() {
                    return Err(busy);
                }
                if buf.len != fbytes || !buf.fits(host_size) {
                    return Err(EncoderError::State(
                        "the frame region is not w * h * 3 / 2 bytes inside the host arena",
                    ));
                }
                let (slot, avail) = fresh(
                    se,
                    Source::HostAt { off: buf.off },
                    nstep + u64::from(overlap),
                );
                (slot, avail, false)
            }
            FrameSource::Device { .. } => {
                if se.up.is_some() {
                    return Err(busy);
                }
                // the caller's kernels run ahead of the tick's on the same stream
                let (slot, avail) = fresh(se, Source::Dev, nstep);
                (slot, avail, false)
            }
            FrameSource::Again => {
                if se.n_real == 0 {
                    return Err(EncoderError::State("no earlier frame to submit again"));
                }
                (se.last_slot, nstep.max(se.last_avail), false)
            }
        };
        let progressive = progressive || shown.progressive || from_device;
        if se.gop.n_in == 0 {
            se.set_origin(pts);
        }
        // the display time the session counts is what the access unit says
        let shown_at = se.pts_of(se.gop.start);
        if shown_at != pts {
            se.pts_mismatch += 1;
        }
        let ordinary =
            shown.units == se.upf && shown.top_first == (se.cfg.interlaced && se.cfg.tff);
        let triplets = (cc.len() / 3).min(MAX_CC_TRIPLETS);
        let frame = Frame {
            disp: 0,
            slot,
            pts: shown_at,
            avail_step,
            pic_type: PicType::B,
            gop_start: false,
            progressive,
            off_grid: false,
            start: 0,
            dur: shown.units,
            tff: shown.top_first,
            cc: cc[..3 * triplets].to_vec(),
            afd: se.afd,
        };
        // two fields end on the other parity, so the next frame begins as this one did; three
        // fields end on the parity they began with, so the next begins on the other
        se.next_top = shown.top_first != (se.cfg.interlaced && shown.units == 3);
        if self.gold.on() {
            let (g, c) = (&se.gop, &se.cfg);
            let (id, hold, hold_prev) = (se.id, se.hold, se.hold_prev);
            self.gold.put("submit", |r| {
                r.i("s", i64::from(id))
                    .u("disp", g.n_in)
                    .i("pts", pts)
                    .u("slot", u64::from(slot))
                    .u("avail_step", avail_step)
                    .b("prog", progressive)
                    .b("want_key", g.want_key)
                    .b("rekey", g.rekey)
                    .b("hold", hold)
                    .b("hold_prev", hold_prev)
                    .u(
                        "next_key",
                        if g.n_in == 0 {
                            0
                        } else {
                            g.next_key / u64::from(se.upf)
                        },
                    )
                    .u("gop_first", g.gop_first)
                    .u("pending", g.pending.len() as u64)
                    .u("gop", u64::from(c.gop))
                    .u("m", u64::from(c.m))
                    .u("gop_phase", u64::from(c.gop_phase))
                    .hex("cc", &frame.cc);
                // a frame shown for other than one frame period in the session's field order
                if !ordinary {
                    r.u("start", g.start)
                        .u("units", u64::from(shown.units))
                        .b("top_first", shown.top_first);
                }
            });
        }
        // a session released from hold starts again with a key frame
        let released = se.hold_prev && !se.hold;
        se.hold_prev = se.hold;
        let before = se.gop.coded.len();
        se.gop.enqueue(
            frame,
            se.cfg.gop,
            se.cfg.m,
            se.cfg.gop_phase,
            released,
            se.upf,
        );
        if self.gold.on() {
            for f in se.gop.coded.iter().skip(before) {
                crate::golden::emit(&mut self.gold, se.id, f, se.film(f));
            }
            self.gold.flush();
        }
        Ok(())
    }

    /// The region of the host arena for the session's next [`FrameSource::Host`] frame:
    /// `w * h * 3 / 2` bytes. Ask before every submit: with `overlap` it alternates between two
    /// regions. The region belongs to the caller from the end of a tick until the submit, and to
    /// the encoder afterwards.
    pub fn frame_buffer(&self, s: SessionId) -> Result<HostBuf, EncoderError> {
        let (si, bi) = self.pair(s)?;
        let se = &self.sessions[si];
        let alt = self.opt.overlap && se.n_host % 2 == 1;
        Ok(self.batches[bi].host_frame(se.slot, alt))
    }

    /// Take `len` bytes of the host arena out of the encoder's hands, for the caller's own use
    /// (frames submitted with [`FrameSource::HostAt`]): no batch is laid out there. `None` when
    /// the arena has no free range of that size. The region is not given back.
    pub fn reserve_host(&mut self, len: u64) -> Option<HostBuf> {
        let off = self.host_alloc.take(round_up(len, 64))?;
        Some(HostBuf::new(off, len))
    }

    /// Take `len` bytes of the device arena out of the encoder's hands, for the caller's own use
    /// (the conversion path's tables and intermediate planes, frames held on the device): no
    /// batch is laid out there. `None` when the arena has no free range of that size. The
    /// region is not given back.
    pub fn reserve_device(&mut self, len: u64) -> Option<DevBuf> {
        let off = self.dev_alloc.take(round_up(len, 256))?;
        Some(DevBuf::new(off, len))
    }

    /// The planes (Y, U, V) and luma pitch (chroma: half) of the device region for the
    /// session's next [`FrameSource::Device`] frame. The caller writes `w` x `h` samples
    /// (chroma `w/2` x `h/2`); the tick fills the padding up to the coded size.
    ///
    /// Refused while the session's batch has not been set up on the device (a batch made by
    /// an `open` is cleared by the next plan that waits on `MAIN`: [`Encoder::settle`] or the
    /// next tick): a frame written before that would be wiped. Settle first.
    pub fn frame_target(&self, s: SessionId) -> Result<([DevBuf; 3], u32), EncoderError> {
        let (si, bi) = self.pair(s)?;
        if self.batches[bi].fresh {
            return Err(EncoderError::State(
                "the session's batch is not set up on the device yet: settle first",
            ));
        }
        let se = &self.sessions[si];
        let ring = (se.n_real % RING_SLOTS) as u8;
        Ok(self.batches[bi].ring_planes(ring, se.slot))
    }

    /// New delivery rate in bit/s (clamped to the session's range). Takes effect at the next
    /// key frame (`at_gop`) or at the next coded picture. Returns a ticket; the first access
    /// unit coded under the new rate carries it. A later call replaces a change not yet applied.
    /// A repeat made on the host never applies a change.
    pub fn set_rate(&mut self, s: SessionId, bps: u64, at_gop: bool) -> Result<u32, EncoderError> {
        self.between_ticks()?;
        let (si, bi) = self.pair(s)?;
        let mbh = self.batches[bi].lay.geo.mbh.unsigned_abs();
        let se = &mut self.sessions[si];
        let (lo, hi) = (min_rate(mbh, se.fps), se.cfg.max_bitrate);
        // std::clamp(bps, lo, hi)
        se.pending_rate = if bps < lo {
            lo
        } else if hi < bps {
            hi
        } else {
            bps
        };
        se.pending_at_gop = at_gop;
        se.pending_ticket = se.next_ticket;
        se.next_ticket = se.next_ticket.wrapping_add(1).max(1);
        let (id, clamped, ticket) = (se.id, se.pending_rate, se.pending_ticket);
        self.gold.put("set_rate", |r| {
            r.i("s", i64::from(id))
                .u("bps", bps)
                .u("clamped", clamped)
                .b("at_gop", at_gop)
                .i("ticket", i64::from(ticket));
        });
        self.gold.flush();
        Ok(ticket)
    }

    /// The next submitted frame starts a new closed GOP.
    pub fn force_keyframe(&mut self, s: SessionId) -> Result<(), EncoderError> {
        self.between_ticks()?;
        let si = self.si(s)?;
        self.sessions[si].gop.want_key = true;
        self.gold.put("force_key", |r| {
            r.i("s", i64::from(s.0));
        });
        self.gold.flush();
        Ok(())
    }

    /// While held, every picture of the session is a repeat made on the host (input failed;
    /// costs no device time). The first frame submitted after the release is a key frame.
    pub fn hold(&mut self, s: SessionId, held: bool) -> Result<(), EncoderError> {
        self.between_ticks()?;
        let si = self.si(s)?;
        self.sessions[si].hold = held;
        Ok(())
    }

    /// The Active Format Description written in the pictures of the frames submitted from now
    /// on, with the bar data of a boxed picture (`None`: none). Anything
    /// [`tb_m2v_syntax::afd::ActiveFormat::is_written`] rejects is refused.
    pub fn set_active_format(
        &mut self,
        s: SessionId,
        afd: Option<tb_m2v_syntax::afd::ActiveFormat>,
    ) -> Result<(), EncoderError> {
        let si = self.si(s)?;
        if let Some(af) = afd
            && !af.is_written()
        {
            return Err(EncoderError::ActiveFormat(af));
        }
        self.sessions[si].afd = afd;
        Ok(())
    }

    /// A plan that only waits for a stream, outside a tick (the C++ `Encoder::finish` is
    /// `settle(UPLOAD)` then `settle(MAIN)`, or `settle(MAIN)` alone without `overlap`). The
    /// plan for `MAIN` carries the set-up of new batches. Returns [`Step::Run`]: submit, wait,
    /// [`Encoder::resume`]. With the device lost, or during a tick, there is nothing to wait
    /// for: [`Step::Done`], the plan is empty.
    pub fn settle(&mut self, stream: StreamId, host: &mut [u8], plan: &mut Plan) -> Step {
        let step = self.settle_inner(stream, host, plan);
        self.handed(step, plan)
    }

    fn settle_inner(&mut self, stream: StreamId, host: &mut [u8], plan: &mut Plan) -> Step {
        plan.clear();
        if self.state != State::Idle || self.lost {
            return Step::Done;
        }
        plan.wait = stream;
        if stream == StreamId::MAIN {
            self.setup(host, plan);
        }
        self.state = State::Settle;
        Step::Run
    }

    /// Queue the clearing and the tables of new batches.
    fn setup(&mut self, host: &mut [u8], plan: &mut Plan) {
        let Some(h_tables) = self.h_tables else {
            return;
        };
        if !self.batches.iter().any(|b| b.fresh) {
            return;
        }
        if let Some(dst) = h_tables.of_mut(host) {
            dst.copy_from_slice(bytemuck::bytes_of(&*self.tables));
        }
        for b in &mut self.batches {
            b.setup(h_tables, plan);
        }
    }

    /// The set-up in the plan that completed: done, or to be queued again.
    fn setup_done(&mut self, ok: bool) {
        for b in &mut self.batches {
            if b.setup_flying {
                b.setup_flying = false;
                b.fresh = !ok;
            }
        }
    }

    /// Start a tick at time `now`. `deadline`: when the tick's output must exist (`None`: no
    /// deadline; the deadline policy then gives nothing up). Writes the tick's parameters into
    /// `host` and the first plan into `plan`. Calling it while a tick is in progress abandons
    /// that tick as if its plan had failed.
    pub fn begin_tick(
        &mut self,
        now: Nanos,
        deadline: Option<Nanos>,
        host: &mut [u8],
        plan: &mut Plan,
    ) -> Step {
        let step = self.begin_tick_inner(now, deadline, host, plan);
        self.handed(step, plan)
    }

    fn begin_tick_inner(
        &mut self,
        now: Nanos,
        deadline: Option<Nanos>,
        host: &mut [u8],
        plan: &mut Plan,
    ) -> Step {
        if self.state != State::Idle {
            self.fail_tick(false, false, now);
            self.gold.flush();
        }
        plan.clear();
        self.t0 = now;
        self.stats = TickStats::default();
        if self.gold.on() {
            let sh = self.shed;
            let force = self.opt.force_shed.map_or(0.0, |(p, _)| p);
            self.gold.set_tick(sh.steps);
            let tick = self.gold.tick();
            self.gold.put("tick", |r| {
                r.i("tick", tick)
                    .b("deadline", deadline.is_some())
                    .hex("rng", &sh.rng.to_le_bytes())
                    .d("force_shed", force);
            });
            self.gold.put("shed_hist", |r| {
                r.i("tick", tick).da("rate_hist", sh.rate_hist);
            });
        }
        self.plan_shed(now, deadline);
        if self.lost {
            for s in &mut self.sessions {
                if let Some(p) = s.up.take() {
                    s.bad_slots |= 1 << (p.slot & 7);
                }
            }
            for b in &mut self.batches {
                b.select(&mut self.sessions, true, &mut self.scratch, &mut self.gold);
            }
            self.end_tick(now, 0.0);
            return Step::Done;
        }
        if self.opt.overlap && !self.opt.frames_event {
            // the previous tick's frames are on the device
            plan.wait = StreamId::UPLOAD;
            self.state = State::Sync;
            return Step::Run;
        }
        self.build_begin(host, plan);
        Step::Run
    }

    /// The deadline-miss policy (the C++ `Encoder::plan`; [`crate::shed`]).
    fn plan_shed(&mut self, now: Nanos, deadline: Option<Nanos>) {
        self.cands.clear();
        self.cand_sess.clear();
        for s in &mut self.sessions {
            s.shed_now = false;
        }
        for b in &self.batches {
            for id in b.sess.iter().flatten() {
                let Some(si) = find(&self.sessions, *id) else {
                    continue;
                };
                let s = &self.sessions[si];
                let Some(f) = s.gop.coded.front() else {
                    continue;
                };
                if f.avail_step > b.nstep {
                    continue;
                }
                let t = f.pic_type;
                // repeats anyway
                if s.hold || (t == PicType::B && s.dup) {
                    continue;
                }
                if self.lost
                    || s.bad_slots & (1 << (f.slot & 7)) != 0
                    || (s.need_key && t != PicType::I)
                {
                    continue;
                }
                self.cands.push(Candidate {
                    session: s.id,
                    priority: s.cfg.priority,
                    class: match t {
                        PicType::B => 0,
                        PicType::P => 1,
                        PicType::I => 2,
                    },
                    mbs: f64::from(b.nmb()),
                    first: !s.have_ref,
                });
                if self.gold.on() {
                    let (c, k, steps) = (
                        self.cands[self.cands.len() - 1],
                        self.cands.len() - 1,
                        self.shed.steps,
                    );
                    self.gold.put("shed_cand", |r| {
                        r.u("tick", steps)
                            .i("s", i64::from(c.session))
                            .i("class", i64::from(c.class))
                            .d("mbs", c.mbs)
                            .u("rot", ShedState::rotation(steps, k as u64))
                            .i("priority", i64::from(c.priority))
                            .b("have_ref", !c.first);
                    });
                }
                self.cand_sess.push(si);
            }
        }
        self.shed_flags.clear();
        self.shed_flags.resize(self.cands.len(), false);
        let left_s = match deadline {
            Some(d) if self.opt.shed => {
                Some((d.0 as f64 - now.0 as f64) * 1e-9 - self.opt.shed_margin_ns as f64 * 1e-9)
            }
            _ => None,
        };
        let (n, budget) = self.shed.plan_with_budget(
            &self.cands,
            left_s,
            self.opt.shed_safety,
            self.opt.force_shed.map(|(p, _)| p),
            &mut self.shed_flags,
        );
        for (&si, &shed) in self.cand_sess.iter().zip(&self.shed_flags) {
            if shed {
                let s = &mut self.sessions[si];
                s.shed_now = true;
                s.shed_count += 1;
            }
        }
        self.stats.candidates = self.cands.len() as u32;
        self.stats.shed = n;
        if self.gold.on() {
            let steps = self.shed.steps;
            if let Some(b) = budget {
                self.gold.put("shed_budget", |r| {
                    r.u("tick", steps)
                        .d("budget", b.budget)
                        .d("rate", b.rate)
                        .d("allowed", b.allowed)
                        .d("mbs", b.mbs);
                });
            }
            // in the order of the batches and their slots
            let mut shed = Vec::new();
            for b in &self.batches {
                for id in b.sess.iter().flatten() {
                    if find(&self.sessions, *id).is_some_and(|si| self.sessions[si].shed_now) {
                        shed.push(i64::from(*id));
                    }
                }
            }
            let (ncand, rng) = (self.cands.len() as u64, self.shed.rng);
            self.gold.put("shed", |r| {
                r.u("tick", steps)
                    .u("candidates", ncand)
                    .u("shed_count", u64::from(n))
                    .ia("shed", shed)
                    .hex("rng", &rng.to_le_bytes());
            });
        }
    }

    /// The `Begin` plan (the C++ `Encoder::step`).
    fn build_begin(&mut self, host: &mut [u8], plan: &mut Plan) {
        plan.clear();
        let overlap = self.opt.overlap;
        if overlap && self.opt.frames_event && self.frames_recorded {
            // The kernels of this tick read the frames the last tick queued on the upload
            // stream: an event in place of the host's wait for that stream.
            plan.main(Op::WaitFor(EventId::FRAMES));
        }
        self.setup(host, plan);
        let Self {
            opt,
            batches,
            sessions,
            scratch,
            gold,
            ..
        } = self;
        if opt.tickmark {
            plan.main(Op::Launch(Launch {
                kernel: Kernel::Tick(KTick {}),
                grid: 1,
                block: 1,
            }));
        }
        for b in batches.iter_mut() {
            b.pad_dev(sessions, host, plan);
        }
        if !overlap {
            for b in batches.iter_mut() {
                b.upload(sessions, StreamId::MAIN, host, plan);
            }
        }
        // Parameters of all batches first, then the kernels. The upload of the next tick's
        // frames waits for the parameters (log, experiment 41).
        for b in batches.iter_mut() {
            gold.at(plan);
            b.select(sessions, false, scratch, gold);
            b.begin(sessions, opt, host, plan, gold);
        }
        if overlap {
            plan.main(Op::Record(EventId::PARAMS));
        }
        for b in batches.iter() {
            b.launch_begin(plan);
        }
        if overlap {
            // while the device works on this tick
            plan.push(StreamId::UPLOAD, Op::WaitFor(EventId::PARAMS));
            for b in batches.iter_mut() {
                b.upload(sessions, StreamId::UPLOAD, host, plan);
            }
            if opt.frames_event {
                plan.push(StreamId::UPLOAD, Op::Record(EventId::FRAMES));
                self.frames_recorded = true;
            }
        }
        self.state = State::Begin;
    }

    /// Continue with the completion of the plan last written. Reads results from `host`,
    /// writes the next parameters into it and the next plan into `plan`. `now`: the time of
    /// the call (the tick's duration feeds the deadline policy).
    pub fn resume(
        &mut self,
        done: &Completion,
        now: Nanos,
        host: &mut [u8],
        plan: &mut Plan,
    ) -> Step {
        let step = self.resume_inner(done, now, host, plan);
        self.handed(step, plan)
    }

    fn resume_inner(
        &mut self,
        done: &Completion,
        now: Nanos,
        host: &mut [u8],
        plan: &mut Plan,
    ) -> Step {
        if self.state == State::Idle {
            return Step::Done;
        }
        let (ok, lost) = match &done.status {
            Ok(()) => (true, false),
            Err(DeviceError::Timeout) => return Step::Wait,
            Err(DeviceError::Lost { .. }) => (false, true),
            Err(_) => (false, false),
        };
        match self.state {
            State::Idle => Step::Done,
            State::Settle => {
                self.setup_done(ok);
                self.lost |= lost;
                plan.clear();
                self.state = State::Idle;
                Step::Done
            }
            State::Sync => {
                for s in &mut self.sessions {
                    if let Some(slot) = s.fly_up.take()
                        && !ok
                    {
                        s.bad_slots |= 1 << (slot & 7);
                    }
                }
                if lost {
                    plan.clear();
                    self.fail_tick(true, false, now);
                    return Step::Done;
                }
                self.build_begin(host, plan);
                Step::Run
            }
            State::Begin => {
                if self.opt.overlap && self.opt.frames_event {
                    // what the wait for the upload stream confirms otherwise
                    for s in &mut self.sessions {
                        if let Some(slot) = s.fly_up_prev.take()
                            && !ok
                        {
                            s.bad_slots |= 1 << (slot & 7);
                        }
                        s.fly_up_prev = s.fly_up.take();
                    }
                }
                self.setup_done(ok);
                if !ok {
                    plan.clear();
                    self.fail_tick(lost, true, now);
                    return Step::Done;
                }
                for s in &mut self.sessions {
                    s.fly_main = None;
                }
                self.part = 0;
                self.build_quant(host, plan)
            }
            State::QuantParts | State::FinishParts => {
                // Only a failure brings the driver here while parts are being queued (a plan
                // the adapter refused); the parts already queued were this tick's too.
                plan.clear();
                self.fail_tick(lost, false, now);
                Step::Done
            }
            State::Quant { pass } => {
                plan.clear();
                if !ok {
                    self.fail_tick(lost, false, now);
                    return Step::Done;
                }
                let mut again = false;
                let mut count = PhaseCount::default();
                for b in &mut self.batches {
                    again |= b.retry(&mut self.sessions, host, plan, &mut count, &mut self.gold);
                }
                self.stats.retry += count.retry;
                self.stats.rexf += count.rexf;
                if again {
                    self.stats.rounds += 1;
                    self.state = State::Quant { pass: pass + 1 };
                    return Step::Run;
                }
                self.part = 0;
                self.build_finish(host, plan)
            }
            State::Finish => {
                plan.clear();
                if !ok {
                    self.fail_tick(lost, false, now);
                    return Step::Done;
                }
                let mut mbs = 0.0;
                for b in &mut self.batches {
                    mbs += b.na() as f64 * f64::from(b.nmb());
                    b.collect(&mut self.sessions, host, &mut self.spare, &mut self.gold);
                }
                self.end_tick(now, mbs);
                Step::Done
            }
        }
    }

    /// The first quantization plan, from batch `self.part` on: all of it, or with split plans
    /// one batch's part at a time (`for (auto &b : batches) b->quantize()` in the C++: each
    /// batch's rate control runs while the device works on the batches before it).
    fn build_quant(&mut self, host: &mut [u8], plan: &mut Plan) -> Step {
        plan.clear();
        let n = self.batches.len();
        let mut count = PhaseCount::default();
        let mut step = Step::Run;
        while self.part < n {
            let i = self.part;
            self.part += 1;
            self.batches[i].quantize(&mut self.sessions, host, plan, &mut count, &mut self.gold);
            if self.opt.split_plans && self.part < n && !plan.steps.is_empty() {
                step = Step::Queue;
                break;
            }
        }
        self.stats.rexf += count.rexf;
        self.state = if step == Step::Queue {
            State::QuantParts
        } else {
            State::Quant { pass: 0 }
        };
        step
    }

    /// The finish plan, from batch `self.part` on (as [`Encoder::build_quant`]).
    fn build_finish(&mut self, host: &mut [u8], plan: &mut Plan) -> Step {
        plan.clear();
        let n = self.batches.len();
        let mut step = Step::Run;
        while self.part < n {
            let i = self.part;
            self.part += 1;
            self.batches[i].finish_gpu(&self.opt, host, plan, &mut self.gold);
            if self.opt.split_plans && self.part < n && !plan.steps.is_empty() {
                step = Step::Queue;
                break;
            }
        }
        self.state = if step == Step::Queue {
            State::FinishParts
        } else {
            State::Finish
        };
        step
    }

    /// After [`Step::Queue`]: the plan was submitted (not waited for); write the next plan of
    /// the phase. In any other state the plan is left alone and the answer is [`Step::Wait`].
    pub fn queued(&mut self, host: &mut [u8], plan: &mut Plan) -> Step {
        let step = match self.state {
            State::QuantParts => self.build_quant(host, plan),
            State::FinishParts => self.build_finish(host, plan),
            _ => Step::Wait,
        };
        self.handed(step, plan)
    }

    /// A plan of the tick failed: its pictures become repeats, the tick ends.
    /// `uploads`: the plan carried this tick's frame uploads.
    fn fail_tick(&mut self, lost: bool, uploads: bool, now: Nanos) {
        if uploads {
            for s in &mut self.sessions {
                for slot in [s.fly_main.take(), s.fly_up.take(), s.fly_up_prev.take()]
                    .into_iter()
                    .flatten()
                {
                    s.bad_slots |= 1 << (slot & 7);
                }
            }
        }
        for b in &mut self.batches {
            b.setup_flying = false;
            b.fail_work(&mut self.sessions, &mut self.scratch, &mut self.gold);
        }
        self.lost |= lost;
        self.stats.failed = true;
        self.end_tick(now, 0.0);
    }

    /// The end of `Encoder::step`.
    fn end_tick(&mut self, now: Nanos, mbs: f64) {
        self.stats.mbs = mbs;
        // What the deadline policy plans with: macroblocks per second of this tick.
        let seconds = now.0.saturating_sub(self.t0.0) as f64 * 1e-9;
        self.shed.observe(mbs, seconds);
        if mbs > 0.0 && self.gold.on() {
            let (tick, hist) = (self.gold.tick(), self.shed.rate_hist);
            self.gold.put("tick_end", |r| {
                r.i("tick", tick)
                    .d("mbs", mbs)
                    .d("seconds", seconds.max(1e-6))
                    .da("rate_hist", hist);
            });
        }
        for si in 0..self.sessions.len() {
            let s = &self.sessions[si];
            if s.closing && !s.closed && s.idle() {
                self.finish_close(si);
            }
        }
        for b in &mut self.batches {
            b.idle_steps = if b.used > 0 { 0 } else { b.idle_steps + 1 };
        }
        self.shed.steps += 1;
        self.state = State::Idle;
    }

    /// The next coded picture of a session, in coded order.
    pub fn take_output(&mut self, s: SessionId) -> Option<AccessUnit> {
        let si = self.si(s).ok()?;
        self.sessions[si].out.pop_front()
    }

    /// Give the bytes of an access unit back when they have been written out
    /// (`AccessUnit::data`): a coming access unit takes the vector instead of allocating one.
    /// With every access unit recycled, a tick allocates nothing once the vectors have grown
    /// to the size of the largest pictures. At most [`Encoder::SPARE_MAX`] vectors are kept.
    pub fn recycle(&mut self, mut data: Vec<u8>) {
        if data.capacity() > 0 && self.spare.len() < Self::SPARE_MAX {
            data.clear();
            self.spare.push(data);
        }
    }

    /// Most byte vectors kept by [`Encoder::recycle`].
    pub const SPARE_MAX: usize = 4096;

    /// Batches without sessions for at least `after` ticks give their memory back to the
    /// arenas.
    pub fn reap(&mut self, after: u64) {
        if self.state != State::Idle {
            return;
        }
        let mut i = 0;
        while i < self.batches.len() {
            let b = &self.batches[i];
            if b.used > 0 || b.idle_steps < after {
                i += 1;
                continue;
            }
            let b = self.batches.remove(i);
            self.gold.drop_batch(b.dev_block);
            self.dev_alloc.give(b.dev_block.0, b.dev_block.1);
            self.host_alloc.give(b.host_block.0, b.host_block.1);
        }
    }

    /// The device was lost and a new adapter with the same arena sizes is in place: plans are
    /// written again. Every batch is cleared and gets its tables with the next plan; frames
    /// submitted before the call are gone; every session that has coded a picture repeats
    /// until its next key frame, which the next frame submitted is.
    pub fn device_restored(&mut self) {
        if !self.lost {
            return;
        }
        self.lost = false;
        for b in &mut self.batches {
            b.fresh = true;
            b.setup_flying = false;
        }
        for s in &mut self.sessions {
            s.bad_slots = 0xff;
            s.up = None;
            s.fly_main = None;
            s.fly_up = None;
            s.fly_up_prev = None;
            if s.have_ref {
                s.need_key = true;
                s.gop.rekey = true;
            }
        }
    }

    /// A completion said the device is lost; see the module documentation.
    pub fn is_lost(&self) -> bool {
        self.lost
    }

    /// The phase of the plan written last, while a tick (or a `settle`) is in progress.
    pub fn phase(&self) -> Option<Phase> {
        match self.state {
            State::Idle => None,
            State::Settle => Some(Phase::Settle),
            State::Sync => Some(Phase::Sync),
            State::Begin => Some(Phase::Begin),
            State::Quant { .. } | State::QuantParts => Some(Phase::Quant),
            State::Finish | State::FinishParts => Some(Phase::Finish),
        }
    }

    /// [`Phase::class`] of the plan written last, with [`CLASS_QUANT_RETRY`] for a correction
    /// pass: a plan of one class takes about as long from tick to tick.
    pub fn wait_class(&self) -> Option<u8> {
        match self.state {
            State::Quant { pass } if pass > 0 => Some(CLASS_QUANT_RETRY),
            _ => self.phase().map(Phase::class),
        }
    }

    /// Whether no open session has a picture waiting (`Encoder::idle`).
    pub fn idle(&self) -> bool {
        self.sessions.iter().all(|s| s.closed || s.idle())
    }

    /// Whether a closing session has coded its last picture.
    pub fn is_closed(&self, s: SessionId) -> Result<bool, EncoderError> {
        Ok(self.sessions[self.si(s)?].closed)
    }

    /// Display time of the session's next key frame on its GOP grid, in frame periods from
    /// the session's first frame (rounded down): while every frame is shown for one frame
    /// period, the display index of the frame that starts a GOP (`Session::next_key`). Cheap,
    /// unlike [`Encoder::snapshot`].
    pub fn next_key(&self, s: SessionId) -> Result<u64, EncoderError> {
        let se = &self.sessions[self.si(s)?];
        Ok(se.gop.next_key / u64::from(se.upf))
    }

    /// Whether the next frame submitted starts at or after the next grid time of the GOP
    /// structure (or is the session's first): it will be a key frame on the grid.
    pub fn key_due(&self, s: SessionId) -> Result<bool, EncoderError> {
        let g = &self.sessions[self.si(s)?].gop;
        Ok(g.n_in == 0 || g.start >= g.next_key)
    }

    /// Display units the session has accepted: the display time, in fields of an interlaced
    /// session or frame periods of a progressive one, at which the next frame starts.
    pub fn display_units(&self, s: SessionId) -> Result<u64, EncoderError> {
        Ok(self.sessions[self.si(s)?].gop.start)
    }

    /// The state of a session as plain data.
    pub fn snapshot(&self, s: SessionId) -> Result<SessionSnapshot, EncoderError> {
        let se = &self.sessions[self.si(s)?];
        Ok(SessionSnapshot {
            config: se.cfg,
            rate: se.rate,
            pending_rate: (se.pending_ticket != 0).then_some((
                se.pending_rate,
                se.pending_at_gop,
                se.pending_ticket,
            )),
            next_ticket: se.next_ticket,
            n_in: se.gop.n_in,
            n_coded: se.n_coded,
            queued: (se.gop.pending.len() + se.gop.coded.len()) as u64,
            gop_first: se.gop.gop_first,
            next_key: se.gop.next_key,
            start: se.gop.start,
            next_top: se.next_top,
            dec_units: se.dec_units,
            prev_anchor: se.prev_anchor,
            gop_acc: se.gop_acc,
            want_key: se.gop.want_key,
            rekey: se.gop.rekey,
            tref_base: se.tref_base,
            pts0: se.pts0,
            newer: se.newer,
            ref_disp: se.ref_disp,
            have_ref: se.have_ref,
            dup: se.dup,
            hold: se.hold,
            afd: se.afd,
            need_key: se.need_key,
            closing: se.closing,
            closed: se.closed,
            rc: se.rc.clone(),
            stats: SessionStats {
                pictures: se.pictures,
                repeats: se.repeats,
                keys_missed: se.keys_missed,
                shed: se.shed_count,
                failed: se.failed,
                total_bits: se.total_bits,
                rate: se.rc.stats,
                shown: se.shown,
                pts_mismatch: se.pts_mismatch,
            },
        })
    }

    /// Open a session that continues the stream of a snapshot (taken from another encoder,
    /// typically in a process that lost its device): same configuration, rate, pending rate
    /// change and tickets, coded index, DTS origin, GOP grid, buffer model and rate-control
    /// state. Frames that were queued and not coded are not restored; the next frame submitted
    /// gets the display index `n_in` and is a key frame (off the grid unless it falls on it),
    /// and until it is coded every picture is a repeat. A snapshot of a session that never
    /// coded a picture opens as a new session with the snapshot's rate.
    pub fn restore(&mut self, snap: &SessionSnapshot) -> Result<SessionId, EncoderError> {
        if !snap.have_ref {
            let id = self.open_with(&snap.config, None)?;
            let si = self.si(id)?;
            self.apply_rate(si, snap);
            return Ok(id);
        }
        let id = self.open_with(&snap.config, Some(snap.rc.clone()))?;
        let si = self.si(id)?;
        self.apply_rate(si, snap);
        let se = &mut self.sessions[si];
        se.gop.n_in = snap.n_in;
        se.gop.gop_first = snap.gop_first;
        se.gop.next_key = snap.next_key;
        se.gop.start = snap.start;
        se.next_top = snap.next_top;
        se.dec_units = snap.dec_units;
        se.prev_anchor = snap.prev_anchor;
        se.gop_acc = snap.gop_acc;
        se.gop.want_key = snap.want_key;
        se.gop.rekey = true;
        se.n_coded = snap.n_coded;
        se.tref_base = snap.tref_base;
        se.set_origin(snap.pts0);
        se.have_ref = true;
        se.need_key = true;
        se.dup = true;
        se.hold = snap.hold;
        se.hold_prev = snap.hold;
        se.afd = snap.afd;
        se.ref_disp = snap.ref_disp;
        se.pictures = snap.stats.pictures;
        se.repeats = snap.stats.repeats;
        se.keys_missed = snap.stats.keys_missed;
        se.shed_count = snap.stats.shed;
        se.failed = snap.stats.failed;
        se.total_bits = snap.stats.total_bits;
        se.shown = snap.stats.shown;
        se.pts_mismatch = snap.stats.pts_mismatch;
        Ok(id)
    }

    fn apply_rate(&mut self, si: usize, snap: &SessionSnapshot) {
        let se = &mut self.sessions[si];
        se.rate = snap.rate;
        se.next_ticket = snap.next_ticket.max(1);
        if let Some((rate, at_gop, ticket)) = snap.pending_rate {
            se.pending_rate = rate;
            se.pending_at_gop = at_gop;
            se.pending_ticket = ticket;
        }
    }

    /// What the last tick did.
    pub fn tick_stats(&self) -> TickStats {
        self.stats
    }

    /// The state of the deadline policy (throughput history, tick count, the test hook's
    /// generator).
    pub fn shed_state(&self) -> &ShedState {
        &self.shed
    }

    /// The buffers of the session's batch at their places in the arenas, and the session's
    /// slot in it (for tests and tools that look at the device).
    pub fn batch_layout(&self, s: SessionId) -> Result<(&BatchLayout, usize), EncoderError> {
        let (si, bi) = self.pair(s)?;
        Ok((&self.batches[bi].lay, self.sessions[si].slot))
    }

    /// The block of the device arena that the session's batch occupies.
    pub fn batch_block(&self, s: SessionId) -> Result<DevBuf, EncoderError> {
        let (_, bi) = self.pair(s)?;
        let (off, len) = self.batches[bi].dev_block;
        Ok(DevBuf::new(off, len))
    }

    /// The region of the host arena the tables are uploaded from (once a batch exists).
    pub fn tables_buffer(&self) -> Option<HostBuf> {
        self.h_tables
    }

    /// Bytes of the arenas not taken by batches: device, host.
    pub fn free_bytes(&self) -> (u64, u64) {
        (self.dev_alloc.free_bytes(), self.host_alloc.free_bytes())
    }

    /// Number of batches.
    pub fn batch_count(&self) -> usize {
        self.batches.len()
    }

    /// The options the encoder was made with.
    pub fn options(&self) -> &EncoderOptions {
        &self.opt
    }

    /// The arena sizes the encoder lays batches out in.
    pub fn arenas(&self) -> ArenaSizes {
        self.arenas
    }
}
