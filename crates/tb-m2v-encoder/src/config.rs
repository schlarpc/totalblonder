//! Configuration of sessions and of the encoder (the C++ `SessionConfig`, `EncoderOptions`).

use tb_m2v_syntax::profile::{Color, Profile, SeqConfig};

/// One program's coding parameters. Defaults are those of the C++ encoder.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SessionConfig {
    /// Picture width in samples.
    pub w: u32,
    /// Picture height in lines.
    pub h: u32,
    /// Frame rate numerator.
    pub fps_n: u32,
    /// Frame rate denominator.
    pub fps_d: u32,
    /// Interlaced sequence.
    pub interlaced: bool,
    /// top_field_first (interlaced content).
    pub tff: bool,
    /// Key frame distance.
    pub gop: u32,
    /// Anchor distance (`m - 1` B pictures).
    pub m: u32,
    /// Key frames at display index d with `(d + gop_phase) % gop == 0` (and at 0).
    pub gop_phase: u32,
    /// Ceiling written in the sequence header (bit_rate), bit/s.
    pub max_bitrate: u64,
    /// vbv_buffer_size in bits; the buffer delay is `vbv_bits / max_bitrate`.
    pub vbv_bits: u64,
    /// Initial target (at most `max_bitrate`); 0 = fixed quantizer.
    pub bitrate: u64,
    /// Fixed quantiser_scale_code per picture type (I, P, B) when `bitrate == 0`.
    pub fixed_q: [u8; 3],
    /// Activity-based adaptive quantization.
    pub aq: bool,
    /// Its strength: factor range `1/s ..= s`.
    pub aq_strength: f32,
    /// Field-based prediction (interlaced formats only).
    pub field_pred: bool,
    /// Field DCT (interlaced formats only).
    pub field_dct: bool,
    /// Search range at quarter resolution.
    pub range2: i32,
    /// Motion vector cost weight.
    pub lambda: i32,
    /// Bits charged to a field-based candidate.
    pub field_bits: i32,
    /// Motion vector smoothing pass.
    pub smooth: bool,
    /// Temporal candidate in the motion search.
    pub tpred: bool,
    /// intra_vlc_format.
    pub intra_vlc: bool,
    /// alternate_scan; `None`: 1 for interlaced formats, else 0.
    pub alt_scan: Option<bool>,
    /// Session slots of the batch created for this format, if a new one is needed.
    pub capacity: u32,
    /// Deadline-miss policy: sessions with a lower value give up their pictures first.
    pub priority: i32,
    /// aspect_ratio_information: 0 the format's default, 1 = square samples, 2 = 4:3,
    /// 3 = 16:9.
    pub aspect: u8,
    /// The rules the stream must satisfy; a configuration outside them is refused when the
    /// session opens ([`tb_m2v_syntax::profile::check`]).
    pub profile: Profile,
    /// The color description of the pictures the caller submits (what its converter
    /// produced); `None`: BT.709 above 576 lines, SMPTE 170M otherwise.
    pub color: Option<Color>,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            w: 0,
            h: 0,
            fps_n: 30000,
            fps_d: 1001,
            interlaced: false,
            tff: true,
            gop: 15,
            m: 3,
            gop_phase: 0,
            max_bitrate: 0,
            vbv_bits: 0,
            bitrate: 0,
            fixed_q: [6, 6, 8],
            aq: true,
            aq_strength: 1.5,
            field_pred: true,
            field_dct: true,
            range2: 8,
            lambda: 4,
            field_bits: 12,
            smooth: true,
            tpred: true,
            intra_vlc: true,
            alt_scan: None,
            capacity: 1,
            priority: 0,
            aspect: 0,
            profile: Profile::default(),
            color: None,
        }
    }
}

impl SessionConfig {
    /// The part of the configuration that the sequence headers carry.
    pub fn seq_config(&self) -> SeqConfig {
        SeqConfig {
            width: self.w,
            height: self.h,
            fps_n: self.fps_n,
            fps_d: self.fps_d,
            interlaced: self.interlaced,
            aspect: self.aspect,
            max_bitrate: self.max_bitrate,
            vbv_bits: self.vbv_bits,
            color: self.color,
        }
    }

    /// Whether two configurations can share a batch (the C++ `Batch::same_format`).
    pub fn same_format(&self, o: &Self) -> bool {
        self.w == o.w
            && self.h == o.h
            && self.fps_n == o.fps_n
            && self.fps_d == o.fps_d
            && self.interlaced == o.interlaced
            && (self.interlaced && self.field_pred) == (o.interlaced && o.field_pred)
            && self.range2 == o.range2
            && self.tpred == o.tpred
    }
}

/// Options of the encoder as a whole.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EncoderOptions {
    /// Upload frames on the upload stream while the previous ones are coded (one frame more
    /// delay). The product's path; `false` reproduces the C++ encoder without `--overlap`.
    pub overlap: bool,
    /// Keep session 0's reconstruction of every picture (drift check of the gate).
    pub keep_recon: bool,
    /// Launch `k_tick` at the start of every tick.
    pub tickmark: bool,
    /// The deadline policy may replace pictures by repeats.
    pub shed: bool,
    /// The tick is planned to use `1 / shed_safety` of the time left.
    pub shed_safety: f64,
    /// Nanoseconds kept free before the deadline.
    pub shed_margin_ns: u64,
    /// Test hook: every candidate gives up its picture with this probability (`--force-shed`),
    /// with the seed of the C++ driver's generator.
    pub force_shed: Option<(f64, u64)>,
    /// Hand the first quantization plan and the finish plan to the driver one batch at a time
    /// ([`crate::Step::Queue`]), so that the device works on a batch while the host prepares
    /// the next one's, as the C++ does. The operations and their order on each stream are
    /// the same; only the plan boundaries differ, which is why it is off for replays of
    /// recorded plans. It matters with several batches (formats) only.
    pub split_plans: bool,
    /// With `overlap`: no wait of the host for the upload stream at the start of a tick (the
    /// `Sync` plan); the upload records `EventId::FRAMES` behind the frames and the next
    /// tick's kernel stream waits for the event (docs/ARCHITECTURE.md 4.2). One host wait
    /// fewer per tick; off by default, as the golden operation records have the host wait.
    pub frames_event: bool,
}

impl Default for EncoderOptions {
    fn default() -> Self {
        Self {
            overlap: false,
            keep_recon: false,
            tickmark: false,
            shed: true,
            shed_safety: 1.10,
            shed_margin_ns: 1_000_000,
            force_shed: None,
            split_plans: false,
            frames_event: false,
        }
    }
}
