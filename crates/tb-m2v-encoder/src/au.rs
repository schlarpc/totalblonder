//! Coded pictures as the encoder hands them to the mux and the statmux controller.

/// One coded picture and what the mux and the statmux controller need to know about it
/// (the C++ `AccessUnit`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AccessUnit {
    /// Headers and slices.
    pub data: Vec<u8>,
    /// Presentation time, 90 kHz.
    pub pts: i64,
    /// Decoding time, 90 kHz.
    pub dts: i64,
    /// Display index.
    pub disp: u64,
    /// Coded index.
    pub coded: u64,
    /// `PIC_*`.
    pub pic_type: u8,
    /// I picture with sequence and GOP header (closed GOP).
    pub keyframe: bool,
    /// Mean quantiser_scale of the picture.
    pub avg_q: f32,
    /// Bits x mean quantizer (the statmux input, SPEC 8.4).
    pub complexity: f64,
    /// Size in bits.
    pub bits: u64,
    /// Delivery rate assumed for this picture, bit/s.
    pub rate: u64,
    /// Ticket of the `set_rate` call that first applies to this picture, else 0.
    pub rate_ticket: u32,
    /// Mux queue of the buffer model after this picture.
    pub queue_bits: f64,
    /// How much larger the picture could have been.
    pub room_bits: f64,
    /// Bits the mux is assumed to deliver in the next D seconds.
    pub cap_bits: f64,
    /// 0 normal; 32.. ladder rung.
    pub rung: u8,
    /// Quantization passes beyond the first.
    pub tries: u32,
    /// Intra macroblocks.
    pub intra_mbs: u32,
    /// Macroblocks with field-based prediction.
    pub field_mbs: u32,
    /// A repeated picture made on the host (deadline-miss policy, a session on hold, a device
    /// failure, or a B picture between two identical anchors).
    pub repeat: bool,
    /// This position was due a key frame and carries a repeat instead.
    pub key_missed: bool,
    /// Key frame that makes up for a missed one.
    pub off_grid: bool,
    /// Last picture of a closed session (the caller ends the stream with sequence_end_code).
    pub last: bool,
    /// Display duration in display units: fields of an interlaced session (2, or 3 with
    /// repeat_first_field), frame periods of a progressive one (1; 2 or 3 with
    /// repeat_first_field). The next picture in display order is presented this much later;
    /// rates per unit time (bits, complexity) are per this duration.
    pub units: u8,
    /// Interlaced session: the first field shown is the top field (top_field_first).
    pub top_first: bool,
    /// Reconstructed picture, cropped 4:2:0 (`EncoderOptions::keep_recon`, session 0 only).
    pub recon: Vec<u8>,
}

impl AccessUnit {
    /// `pts` as the 33-bit field of a PES header carries it (H.222.0 2.4.3.7: the value is
    /// taken modulo 2^33). `pts` and `dts` themselves are not reduced, so that the caller can
    /// order and subtract them.
    pub fn pts33(&self) -> u64 {
        self.pts.rem_euclid(1 << 33) as u64
    }

    /// `dts` modulo 2^33.
    pub fn dts33(&self) -> u64 {
        self.dts.rem_euclid(1 << 33) as u64
    }
}
