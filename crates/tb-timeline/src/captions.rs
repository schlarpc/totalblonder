//! Closed captions through the frame clock: CEA-608/708 `cc_data` triplets carried from decoded
//! source frames to output frames, so that nothing is lost or sent twice when source frames are
//! dropped or repeated, or when two source frames go into one output frame.
//!
//! The C++ harness has nothing of this; the rules are new and are these.
//!
//! **Data.** A triplet is three bytes as in ATSC A/53 `cc_data()`: five marker bits, `cc_valid`
//! (0x04) and `cc_type` (0x03) in the first byte, then two data bytes. It is the form libavcodec
//! gives as `AV_FRAME_DATA_A53_CC` and the encoder takes as user data (at most
//! [`MAX_TRIPLETS`] per coded frame). `cc_type` 0 and 1 are CEA-608 byte pairs of field 1 and
//! field 2; 3 starts a CEA-708 (DTVCC) caption channel packet and 2 continues it.
//!
//! **In.** [`CaptionCarrier::push`] is called for every decoded source frame in decode output
//! order with the frame's index, also for frames the frame clock later drops. Valid triplets are
//! queued with that index; triplets with `cc_valid` = 0 (padding) are not queued. There are
//! three queues, one per service: 608 field 1, 608 field 2, DTVCC. Order is kept within each;
//! nothing is promised between them (they are independent services).
//!
//! **Out.** [`CaptionCarrier::take`] is called once per output frame with the index of the
//! newest source frame that frame shows (or has shown: the same index again on a repeat). Only
//! triplets of source frames up to that index go out, so captions never run ahead of their
//! pictures. It writes exactly the `cc_data` of one frame at the output rate ([`CcRate`]):
//! - 29.97 Hz: `cc_count` 20: one 608 pair of field 1, one of field 2, then 18 DTVCC triplets;
//! - 59.94 Hz: `cc_count` 10: one 608 pair, field 1 on even output frames and field 2 on odd
//!   ones, then 9 DTVCC triplets.
//!
//! Film mode: [`CaptionCarrier::take_shown`] writes the `cc_data` of a picture that is shown
//! for one, two or three field periods (10 triplets and one 608 pair per field period), with
//! the 608 pairs in the display order of its fields.
//!
//! A 608 slot without data is a triplet with `cc_valid` = 0 of the slot's type and the null
//! pair ([`PAD_608`]); unused DTVCC slots are `FA 00 00` ([`PAD_DTVCC`]). Both rates carry the
//! full 9600 bit/s of CEA-708, so a source at either rate fits an output at either rate.
//!
//! **Drops and repeats.** Whatever is queued and due goes out at the rate above, oldest first.
//! When the clock drops a source frame, its triplets are due together with the next frame's;
//! the 608 slots take one pair per field, so the field's later pairs follow one output frame
//! behind until a repeat (or a frame without a pair) absorbs the backlog. When an output frame
//! repeats its input, nothing new is due: it carries backlog if there is any, else padding;
//! no triplet is ever sent twice.
//!
//! While a 608 field has a backlog, null pairs (both bytes zero without their parity bits:
//! filler a decoder ignores) are skipped instead of sent, which is what lets the backlog
//! drain in streams that fill every frame. One exception keeps the meaning: a null pair between
//! two equal control codes stays, since without it a decoder would take the second code for
//! the redundant copy of the first.
//!
//! **DTVCC packets stay whole.** A packet's length is in its header (`packet_size_code`); it
//! also ends where the next packet starts or at a start triplet with `cc_valid` = 0. A packet
//! begins to go out only when all of it is queued and due, and then goes out without a gap:
//! in consecutive DTVCC slots, over the end of one output frame into the start of the next when
//! it is longer than the slots left (a packet has up to 64 triplets). Padding is written only
//! between packets, never inside one, because some decoders end a packet at padding.
//! Continuation triplets without a packet (before the first start, after a packet's declared
//! length, or of a packet that was dropped) are counted and dropped.
//!
//! **Bound.** Each queue is bounded (by default two seconds of its service's full rate, see
//! [`CaptionCfg::new`]). On overflow the oldest 608 pair or the oldest whole DTVCC packet that
//! has not begun to go out is dropped and counted. With matched average rates the bound is
//! never reached; it is there for a source that sends faster than the output can carry, and for
//! an output that stops calling `take`.
//!
//! **Input switches.** [`CaptionCarrier::clear`] empties the queues (a packet that has begun to
//! go out is cut; decoders drop a short packet).

use std::collections::VecDeque;

/// Most triplets one coded frame's `cc_data()` can hold (`cc_count` is five bits).
pub const MAX_TRIPLETS: usize = 31;

/// An unused DTVCC slot: `cc_valid` = 0, `cc_type` = 2.
pub const PAD_DTVCC: [u8; 3] = [0xFA, 0x00, 0x00];

/// An unused 608 slot of field 1 and of field 2: `cc_valid` = 0, the null pair with parity.
pub const PAD_608: [[u8; 3]; 2] = [[0xF8, 0x80, 0x80], [0xF9, 0x80, 0x80]];

/// Marker bits and `cc_valid`.
const VALID: u8 = 0xFC;

/// The output frame rate, which fixes the layout of a frame's `cc_data` (CEA-708).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CcRate {
    /// 29.97 frames per second (also interlaced): 20 triplets, 2 for 608 and 18 for DTVCC.
    F2997,
    /// 59.94 frames per second: 10 triplets, 1 for 608 (fields alternate) and 9 for DTVCC.
    F5994,
}

impl CcRate {
    /// `cc_count` of every output frame.
    pub const fn cc_count(self) -> usize {
        match self {
            Self::F2997 => 20,
            Self::F5994 => 10,
        }
    }
}

/// Configuration of a [`CaptionCarrier`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CaptionCfg {
    /// The output rate.
    pub rate: CcRate,
    /// Most 608 pairs queued per field.
    pub max_608: usize,
    /// Most DTVCC triplets queued. Below 64 a single long packet may exceed it while it goes
    /// out.
    pub max_dtvcc: usize,
    /// The output is interlaced and its frames begin with the top field. The CEA-608 pairs of
    /// a frame are then written field 2 first: "the CEA-608 data field order (field 1 vs.
    /// field 2) depends on the field display order" (CTA-708-E §4.3.1 rule 2, p.11), and the
    /// pair of cc_type 01 goes with the top field (Annex C.1, p.99: top_field_first 1 begins
    /// 01, 00; C.2: top_field_first 0 begins 00, 01).
    pub top_first: bool,
}

impl CaptionCfg {
    /// The default bounds: two seconds of each service at its full rate (60 pairs per 608
    /// field, 1080 DTVCC triplets).
    pub const fn new(rate: CcRate) -> Self {
        Self {
            rate,
            max_608: 60,
            max_dtvcc: 1080,
            top_first: false,
        }
    }

    /// The same for an interlaced output whose frames begin with the top field.
    pub const fn top_first(rate: CcRate) -> Self {
        Self {
            top_first: true,
            ..Self::new(rate)
        }
    }
}

/// Counters of a [`CaptionCarrier`]. For each service, `in = out + dropped + queued`
/// (`+ nulls_skipped` for 608).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CaptionStats {
    /// Valid 608 pairs queued, per field.
    pub in_608: [u64; 2],
    /// 608 pairs sent, per field.
    pub out_608: [u64; 2],
    /// Null 608 pairs skipped to drain a backlog, per field.
    pub nulls_skipped: [u64; 2],
    /// 608 pairs dropped by the bound or by [`CaptionCarrier::clear`], per field.
    pub dropped_608: [u64; 2],
    /// DTVCC triplets queued.
    pub in_dtvcc: u64,
    /// DTVCC triplets sent.
    pub out_dtvcc: u64,
    /// DTVCC triplets dropped by the bound or by [`CaptionCarrier::clear`].
    pub dropped_dtvcc: u64,
    /// Whole DTVCC packets dropped by the bound.
    pub dropped_packets: u64,
    /// DTVCC continuation triplets that belonged to no packet (not queued).
    pub orphans: u64,
}

/// A queued DTVCC packet; its triplets are in `CaptionCarrier::dtvcc`, in order.
#[derive(Clone, Copy, Debug)]
struct Packet {
    /// Source frame of its last triplet.
    end_idx: i64,
    /// Triplets queued for it so far (sent ones included).
    len: usize,
    /// Triplets its header declares.
    want: usize,
    /// Triplets already sent.
    sent: usize,
    /// No more triplets will be added.
    closed: bool,
}

/// Carries one channel's captions from source frames to output frames (see the module text).
#[derive(Clone, Debug)]
pub struct CaptionCarrier {
    cfg: CaptionCfg,
    f608: [VecDeque<(i64, [u8; 2])>; 2],
    last_608: [Option<[u8; 2]>; 2],
    packets: VecDeque<Packet>,
    dtvcc: VecDeque<[u8; 2]>,
    /// Field periods written on a progressive output (the 608 fields alternate).
    out_fields: u64,
    /// The counters.
    pub stats: CaptionStats,
}

/// A 608 pair that carries nothing (both bytes zero under the parity bit).
fn is_null(d: [u8; 2]) -> bool {
    (d[0] | d[1]) & 0x7F == 0
}

/// A 608 control code (first byte 0x10 to 0x1F under the parity bit): sent twice by
/// convention, the second copy ignored by decoders.
fn is_control(d: [u8; 2]) -> bool {
    (0x10..=0x1F).contains(&(d[0] & 0x7F))
}

impl CaptionCarrier {
    /// An empty carrier.
    pub fn new(cfg: CaptionCfg) -> Self {
        Self {
            cfg,
            f608: [VecDeque::new(), VecDeque::new()],
            last_608: [None, None],
            packets: VecDeque::new(),
            dtvcc: VecDeque::new(),
            out_fields: 0,
            stats: CaptionStats::default(),
        }
    }

    /// The configuration.
    pub fn cfg(&self) -> &CaptionCfg {
        &self.cfg
    }

    /// Queued 608 pairs of field 1 and field 2, and queued DTVCC triplets.
    pub fn queued(&self) -> (usize, usize, usize) {
        (self.f608[0].len(), self.f608[1].len(), self.dtvcc.len())
    }

    /// The `cc_data` triplets of decoded source frame `frame_idx` (`cc_data.len() / 3` of them;
    /// a trailing partial triplet is ignored). Call it for every decoded frame, in order, with
    /// indices that never decrease; an empty slice is fine.
    pub fn push(&mut self, frame_idx: i64, cc_data: &[u8]) {
        for tr in cc_data.as_chunks::<3>().0 {
            let valid = tr[0] & 0x04 != 0;
            let data = [tr[1], tr[2]];
            match (valid, tr[0] & 0x03) {
                (true, f @ (0 | 1)) => self.push_608(usize::from(f), frame_idx, data),
                (true, 3) => self.start_packet(frame_idx, data),
                (true, _) => self.continue_packet(frame_idx, data),
                (false, 3) => self.close_packet(),
                (false, _) => {}
            }
        }
    }

    fn push_608(&mut self, f: usize, idx: i64, data: [u8; 2]) {
        let q = &mut self.f608[f];
        q.push_back((idx, data));
        self.stats.in_608[f] += 1;
        while q.len() > self.cfg.max_608 {
            q.pop_front();
            self.stats.dropped_608[f] += 1;
        }
    }

    fn close_packet(&mut self) {
        if let Some(p) = self.packets.back_mut() {
            p.closed = true;
        }
    }

    fn start_packet(&mut self, idx: i64, data: [u8; 2]) {
        self.close_packet();
        // packet_size_code: the packet has that many byte pairs, 64 for code 0
        let want = match usize::from(data[0] & 0x3F) {
            0 => 64,
            n => n,
        };
        self.packets.push_back(Packet {
            end_idx: idx,
            len: 1,
            want,
            sent: 0,
            closed: want == 1,
        });
        self.dtvcc.push_back(data);
        self.stats.in_dtvcc += 1;
        self.trim();
    }

    fn continue_packet(&mut self, idx: i64, data: [u8; 2]) {
        match self.packets.back_mut() {
            Some(p) if !p.closed => {
                p.len += 1;
                p.end_idx = idx;
                p.closed = p.len >= p.want;
                self.dtvcc.push_back(data);
                self.stats.in_dtvcc += 1;
                self.trim();
            }
            _ => self.stats.orphans += 1,
        }
    }

    /// Keep the DTVCC queue within its bound: drop the oldest packets that have not begun to
    /// go out, whole.
    fn trim(&mut self) {
        while self.dtvcc.len() > self.cfg.max_dtvcc {
            let (i, at) = match self.packets.front() {
                Some(p) if p.sent > 0 => (1, p.len - p.sent),
                _ => (0, 0),
            };
            let Some(p) = self.packets.remove(i) else {
                return;
            };
            self.dtvcc.drain(at..(at + p.len).min(self.dtvcc.len()));
            self.stats.dropped_packets += 1;
            self.stats.dropped_dtvcc += p.len as u64;
        }
    }

    /// Forget everything queued (the channel's input changed). Counted as dropped.
    pub fn clear(&mut self) {
        for f in 0..2 {
            self.stats.dropped_608[f] += self.f608[f].len() as u64;
            self.f608[f].clear();
        }
        self.stats.dropped_dtvcc += self.dtvcc.len() as u64;
        self.dtvcc.clear();
        self.packets.clear();
        self.last_608 = [None, None];
    }

    /// The `cc_data` of the next output frame: `out` is cleared and filled with exactly
    /// `cfg.rate.cc_count()` triplets. `up_to`: index of the newest source frame the output
    /// frame shows or has shown (`i64::MAX` to send whatever is queued, e.g. on the slate).
    pub fn take(&mut self, up_to: i64, out: &mut Vec<u8>) {
        match self.cfg.rate {
            CcRate::F2997 => self.take_shown(up_to, 2, Some(self.cfg.top_first), out),
            CcRate::F5994 => self.take_shown(up_to, 1, None, out),
        }
    }

    /// The `cc_data` of a picture that is shown for `fields` field periods of 1/59.94 s (1 to
    /// 3): film mode, where a picture of an interlaced output is shown for two or three fields
    /// and a frame of a 59.94 Hz output once, twice or three times. `out` is cleared and
    /// filled with `10 * fields` triplets (CTA-708-E §4.3.6: cc_count follows the time the
    /// picture is presented; 20 and 30 with 3:2 pulldown, Table 4): one 608 pair per field
    /// period, in display order, then DTVCC. Nothing is lost or sent twice whatever the mix
    /// of durations: each service keeps its queue and every field period shown takes one pair
    /// of its field.
    ///
    /// `top_first`: `Some(parity of the picture's first field)` on an interlaced output: the
    /// pair of field 2 (cc_type 01) goes with a top field, the pair of field 1 with a bottom
    /// field (the rule of [`CaptionCfg::top_first`]), so a three-field picture that begins
    /// with the top field carries 01, 00, 01 and the next one, which begins with the bottom
    /// field, 00, 01 (CTA-708-E Annex C.1 to C.3). `None` on a progressive output: the two
    /// fields in turn, one per frame period, across pictures.
    pub fn take_shown(
        &mut self,
        up_to: i64,
        fields: u8,
        top_first: Option<bool>,
        out: &mut Vec<u8>,
    ) {
        out.clear();
        let fields = usize::from(fields.clamp(1, 3));
        for k in 0..fields {
            let f = match top_first {
                // field 2 with the top field
                Some(top) => usize::from(top == (k % 2 == 0)),
                None => usize::from((self.out_fields + k as u64) & 1 == 1),
            };
            self.slot_608(f, up_to, out);
        }
        self.out_fields += fields as u64;
        let mut room = 10 * fields - fields;
        while room > 0 {
            let Some(p) = self.packets.front_mut() else {
                break;
            };
            if p.sent == 0 && !(p.closed && p.end_idx <= up_to) {
                break;
            }
            let n = room.min(p.len - p.sent);
            for d in self.dtvcc.drain(..n.min(self.dtvcc.len())) {
                // cc_type 3 for the packet's first triplet, 2 for the others
                let ty = if p.sent == 0 { 3 } else { 2 };
                out.extend_from_slice(&[VALID | ty, d[0], d[1]]);
                p.sent += 1;
            }
            room -= n;
            self.stats.out_dtvcc += n as u64;
            if p.sent >= p.len {
                self.packets.pop_front();
            }
        }
        for _ in 0..room {
            out.extend_from_slice(&PAD_DTVCC);
        }
    }

    /// One 608 slot of field `f`.
    fn slot_608(&mut self, f: usize, up_to: i64, out: &mut Vec<u8>) {
        let q = &mut self.f608[f];
        loop {
            let data = match q.front() {
                Some(&(idx, data)) if idx <= up_to => data,
                _ => {
                    out.extend_from_slice(&PAD_608[f]);
                    return;
                }
            };
            q.pop_front();
            // filler is skipped while more is due behind it, unless that would put two equal
            // control codes next to each other
            let next = q.front().filter(|n| n.0 <= up_to).map(|n| n.1);
            let joins = |n: [u8; 2]| is_control(n) && self.last_608[f] == Some(n);
            if is_null(data) && next.is_some_and(|n| !joins(n)) {
                self.stats.nulls_skipped[f] += 1;
                continue;
            }
            out.extend_from_slice(&[VALID | f as u8, data[0], data[1]]);
            self.last_608[f] = Some(data);
            self.stats.out_608[f] += 1;
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SplitMix64, as in the crate's property tests.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
    }

    /// Makes numbered payloads: 608 text pairs and DTVCC packets.
    #[derive(Default)]
    struct Gen {
        n: u32,
        seq: u8,
    }
    impl Gen {
        /// Two bytes that are neither null nor a control code.
        fn bytes(&mut self) -> [u8; 2] {
            self.n += 1;
            [
                0x20 + (self.n % 0x50) as u8,
                0x20 + (self.n / 0x50 % 0x50) as u8,
            ]
        }
        fn pair(&mut self, field: u8) -> [u8; 3] {
            let b = self.bytes();
            [0xFC | field, b[0], b[1]]
        }
        /// A caption channel packet of `n` triplets (1 to 64).
        fn packet(&mut self, n: usize) -> Vec<[u8; 3]> {
            self.seq = (self.seq + 1) & 3;
            let b = self.bytes();
            let mut v = vec![[0xFF, (self.seq << 6) | (n & 0x3F) as u8, b[1]]];
            for _ in 1..n {
                let b = self.bytes();
                v.push([0xFE, b[0], b[1]]);
            }
            v
        }
    }

    fn flat(t: &[[u8; 3]]) -> Vec<u8> {
        t.concat()
    }
    fn trips(b: &[u8]) -> Vec<[u8; 3]> {
        b.as_chunks::<3>().0.to_vec()
    }

    /// The valid triplets of a service in a run of output frames; checks every frame's layout
    /// for `rate` and that no packet has padding inside (or anything but its own triplets).
    #[derive(Default)]
    struct Sink {
        f1: Vec<[u8; 3]>,
        f2: Vec<[u8; 3]>,
        dtvcc: Vec<[u8; 3]>,
        /// Triplets the packet being received still needs.
        open: usize,
        frames: u64,
    }
    impl Sink {
        fn frame(&mut self, rate: CcRate, out: &[u8]) {
            let t = trips(out);
            assert_eq!(t.len(), rate.cc_count());
            assert!(t.len() <= MAX_TRIPLETS);
            let n608 = match rate {
                CcRate::F2997 => 2,
                CcRate::F5994 => 1,
            };
            for (k, tr) in t.iter().enumerate() {
                assert_eq!(tr[0] & 0xF8, 0xF8, "marker bits");
                let (valid, ty) = (tr[0] & 4 != 0, tr[0] & 3);
                if k < n608 {
                    let field = match rate {
                        CcRate::F2997 => k as u8,
                        CcRate::F5994 => (self.frames & 1) as u8,
                    };
                    assert_eq!(ty, field, "608 slot type");
                    if valid {
                        [&mut self.f1, &mut self.f2][usize::from(field)].push(*tr);
                    } else {
                        assert_eq!(*tr, PAD_608[usize::from(field)]);
                    }
                    continue;
                }
                assert!(ty >= 2, "DTVCC slot");
                if !valid {
                    assert_eq!(*tr, PAD_DTVCC);
                    assert_eq!(self.open, 0, "padding inside a packet");
                    continue;
                }
                if ty == 3 {
                    assert_eq!(self.open, 0, "a packet starts inside another");
                    self.open = match usize::from(tr[1] & 0x3F) {
                        0 => 64,
                        n => n,
                    };
                } else {
                    assert!(self.open > 0, "continuation without a packet");
                }
                self.open -= 1;
                self.dtvcc.push(*tr);
            }
            self.frames += 1;
        }
    }

    fn of_type(t: &[[u8; 3]], f: impl Fn(u8) -> bool) -> Vec<[u8; 3]> {
        t.iter()
            .copied()
            .filter(|x| x[0] & 4 != 0 && f(x[0] & 3))
            .collect()
    }

    #[test]
    fn equal_rate_pass_through_is_identity() {
        let mut g = Gen::default();
        let mut c = CaptionCarrier::new(CaptionCfg::new(CcRate::F2997));
        let mut out = Vec::new();
        for k in 0..200i64 {
            // a full frame as an encoder writes it: both 608 slots, packets, padding to 20
            let mut src = vec![g.pair(0)];
            src.push(if k % 3 == 0 { PAD_608[1] } else { g.pair(1) });
            for n in [[5usize, 7], [18, 0], [1, 1], [0, 0]][(k % 4) as usize] {
                if n > 0 {
                    src.extend(g.packet(n));
                }
            }
            src.resize(20, PAD_DTVCC);
            c.push(k, &flat(&src));
            c.take(k, &mut out);
            assert_eq!(trips(&out), src, "frame {k}");
        }
        assert_eq!(c.queued(), (0, 0, 0));
        assert_eq!(c.stats.in_dtvcc, c.stats.out_dtvcc);

        // 59.94 Hz: one 608 pair a frame, the fields in turn
        let mut c = CaptionCarrier::new(CaptionCfg::new(CcRate::F5994));
        for k in 0..200i64 {
            let mut src = vec![g.pair((k & 1) as u8)];
            src.extend(g.packet(1 + (k % 9) as usize));
            src.resize(10, PAD_DTVCC);
            c.push(k, &flat(&src));
            c.take(k, &mut out);
            assert_eq!(trips(&out), src, "frame {k}");
        }
    }

    #[test]
    fn two_60p_frames_merge_into_one_30i_frame_without_loss() {
        let mut g = Gen::default();
        let mut c = CaptionCarrier::new(CaptionCfg::new(CcRate::F2997));
        let (mut out, mut sink) = (Vec::new(), Sink::default());
        let mut all = Vec::new();
        for j in 0..100i64 {
            // even source frames carry field 1, odd ones field 2; nine DTVCC slots each: a
            // packet of 12 runs over the frame boundary inside the pair, one of 6 fills the rest
            let (big, small) = (g.packet(12), g.packet(6));
            let mut a = vec![g.pair(0)];
            a.extend_from_slice(&big[..9]);
            let mut b = vec![g.pair(1)];
            b.extend_from_slice(&big[9..]);
            b.extend(small);
            c.push(2 * j, &flat(&a));
            c.push(2 * j + 1, &flat(&b));
            c.take(2 * j + 1, &mut out);
            // everything of both frames in this one frame
            let mut want = vec![a[0], b[0]];
            want.extend_from_slice(&a[1..]);
            want.extend_from_slice(&b[1..]);
            assert_eq!(trips(&out), want, "output frame {j}");
            sink.frame(CcRate::F2997, &out);
            all.extend(a);
            all.extend(b);
        }
        assert_eq!(sink.f1, of_type(&all, |t| t == 0));
        assert_eq!(sink.f2, of_type(&all, |t| t == 1));
        assert_eq!(sink.dtvcc, of_type(&all, |t| t >= 2));
    }

    #[test]
    fn a_packet_waits_until_all_of_it_is_due() {
        let mut g = Gen::default();
        let mut c = CaptionCarrier::new(CaptionCfg::new(CcRate::F2997));
        let (mut out, mut sink) = (Vec::new(), Sink::default());
        // a packet that starts in frame 0 and ends in frame 1; a later one whole in frame 1
        let (p, q) = (g.packet(10), g.packet(3));
        c.push(0, &flat(&p[..4]));
        c.push(1, &flat(&p[4..]));
        c.push(1, &flat(&q));
        c.take(0, &mut out);
        sink.frame(CcRate::F2997, &out);
        assert!(sink.dtvcc.is_empty(), "half a packet is not sent");
        c.take(1, &mut out);
        sink.frame(CcRate::F2997, &out);
        assert_eq!(sink.dtvcc, [p, q].concat());
    }

    #[test]
    fn captions_do_not_run_ahead_of_their_frames() {
        let mut g = Gen::default();
        let mut c = CaptionCarrier::new(CaptionCfg::new(CcRate::F2997));
        let mut out = Vec::new();
        let frames: Vec<Vec<[u8; 3]>> = (0..4)
            .map(|_| {
                let mut v = vec![g.pair(0), g.pair(1)];
                v.extend(g.packet(4));
                v
            })
            .collect();
        // the decoder is three frames ahead of the display
        for (k, f) in frames.iter().enumerate() {
            c.push(k as i64, &flat(f));
        }
        c.take(-1, &mut out);
        assert!(trips(&out).iter().all(|t| t[0] & 4 == 0));
        for (k, f) in frames.iter().enumerate() {
            c.take(k as i64, &mut out);
            assert_eq!(&trips(&out)[..6], &f[..], "frame {k}");
        }
    }

    #[test]
    fn a_dropped_frame_delays_and_a_repeat_absorbs() {
        let mut g = Gen::default();
        let mut c = CaptionCarrier::new(CaptionCfg::new(CcRate::F2997));
        let mut out = Vec::new();
        let frames: Vec<Vec<[u8; 3]>> = (0..7)
            .map(|_| {
                let mut v = vec![g.pair(0), g.pair(1)];
                v.extend(g.packet(5));
                v
            })
            .collect();
        for (k, f) in frames.iter().enumerate() {
            c.push(k as i64, &flat(f));
        }
        let mut show = |c: &mut CaptionCarrier, idx: i64| {
            c.take(idx, &mut out);
            trips(&out)
        };
        let f = |k: usize| frames[k].clone();
        assert_eq!(show(&mut c, 0)[..7], f(0)[..]);
        assert_eq!(show(&mut c, 1)[..7], f(1)[..]);
        // frame 2 is dropped by the clock: the output frame that shows 3 carries the 608 pairs
        // of 2 and the packets of 2 and 3
        let o = show(&mut c, 3);
        assert_eq!(o[..2], f(2)[..2]);
        assert_eq!(o[2..12], [&f(2)[2..], &f(3)[2..]].concat()[..]);
        assert_eq!(o[12], PAD_DTVCC);
        // from here the 608 pairs are one output frame behind; the DTVCC service is not
        let o = show(&mut c, 4);
        assert_eq!(o[..2], f(3)[..2]);
        assert_eq!(o[2..7], f(4)[2..]);
        let o = show(&mut c, 5);
        assert_eq!(o[..2], f(4)[..2]);
        // a repeat of frame 5: the backlog, and no packet again
        let o = show(&mut c, 5);
        assert_eq!(o[..2], f(5)[..2]);
        assert_eq!(o[2], PAD_DTVCC);
        // in step again
        assert_eq!(show(&mut c, 6)[..7], f(6)[..]);
        // another repeat: only padding
        let o = show(&mut c, 6);
        assert_eq!(o[..2], PAD_608);
        assert!(o[2..].iter().all(|t| *t == PAD_DTVCC));
        assert_eq!(o.len(), 20);
        let s = c.stats;
        assert_eq!((s.out_608, s.out_dtvcc), ([7, 7], 35));
        assert_eq!((s.dropped_608, s.dropped_dtvcc, s.orphans), ([0, 0], 0, 0));
    }

    #[test]
    fn null_pairs_are_skipped_only_to_drain_a_backlog() {
        let mut c = CaptionCarrier::new(CaptionCfg::new(CcRate::F2997));
        let mut out = Vec::new();
        let null = [0xFC, 0x80, 0x80];
        let (a, b) = ([0xFC, b'a', b'b'], [0xFC, b'c', b'd']);
        // in step: nulls pass
        c.push(0, &flat(&[null]));
        c.take(0, &mut out);
        assert_eq!(out[..3], null);
        // two frames due at once: the null in front is skipped
        c.push(1, &flat(&[null]));
        c.push(2, &flat(&[a]));
        c.push(3, &flat(&[null]));
        c.push(4, &flat(&[b]));
        c.take(2, &mut out);
        assert_eq!(out[..3], a);
        c.take(4, &mut out);
        assert_eq!(out[..3], b);
        assert_eq!(c.stats.nulls_skipped, [2, 0]);
        assert_eq!(c.queued().0, 0);
        // a trailing null with nothing behind it is sent
        c.push(5, &flat(&[a]));
        c.push(6, &flat(&[null]));
        c.take(6, &mut out);
        assert_eq!(out[..3], a);
        c.take(6, &mut out);
        assert_eq!(out[..3], null);

        // a null between two equal control codes stays: RCL, RCL (the redundant copy), null,
        // RCL (a new command)
        let rcl = [0xFC, 0x94, 0x20];
        c.push(7, &flat(&[rcl]));
        c.push(8, &flat(&[rcl, null, rcl, null, a]));
        let mut got = Vec::new();
        for _ in 0..5 {
            c.take(8, &mut out);
            got.push([out[0], out[1], out[2]]);
        }
        // the null before the text is skipped, the one between the codes is not
        assert_eq!(got, [rcl, rcl, null, rcl, a]);
        assert_eq!(c.stats.nulls_skipped, [3, 0]);
    }

    #[test]
    fn sixty_hz_output_alternates_fields_from_a_30_hz_source() {
        let mut g = Gen::default();
        let mut c = CaptionCarrier::new(CaptionCfg::new(CcRate::F5994));
        let (mut out, mut sink) = (Vec::new(), Sink::default());
        let mut all = Vec::new();
        for k in 0..50i64 {
            let mut src = vec![g.pair(0), g.pair(1)];
            src.extend(g.packet(18));
            c.push(k, &flat(&src));
            // new frame, then the repeat
            c.take(k, &mut out);
            assert_eq!(out[..3], src[0]);
            assert_eq!(trips(&out)[1..], src[2..11]);
            sink.frame(CcRate::F5994, &out);
            c.take(k, &mut out);
            assert_eq!(out[..3], src[1]);
            assert_eq!(trips(&out)[1..], src[11..]);
            sink.frame(CcRate::F5994, &out);
            all.extend(src);
        }
        assert_eq!(sink.dtvcc, of_type(&all, |t| t >= 2));
        assert_eq!(c.queued(), (0, 0, 0));
    }

    #[test]
    fn long_packets_span_output_frames_without_a_gap() {
        let mut g = Gen::default();
        let mut c = CaptionCarrier::new(CaptionCfg::new(CcRate::F2997));
        let (mut out, mut sink) = (Vec::new(), Sink::default());
        // size code 0: 64 triplets; then a short one that must follow at once
        let (big, small) = (g.packet(64), g.packet(2));
        assert_eq!(big[0][1] & 0x3F, 0);
        c.push(0, &flat(&big));
        c.push(0, &flat(&small));
        let mut pads = Vec::new();
        for _ in 0..5 {
            c.take(0, &mut out);
            sink.frame(CcRate::F2997, &out);
            pads.push(trips(&out).iter().filter(|t| **t == PAD_DTVCC).count());
        }
        assert_eq!(pads, [0, 0, 0, 6, 18]);
        assert_eq!(sink.dtvcc, [big, small].concat());
    }

    #[test]
    fn malformed_dtvcc_is_bounded_and_counted() {
        let mut g = Gen::default();
        let mut c = CaptionCarrier::new(CaptionCfg::new(CcRate::F2997));
        let (mut out, mut sink) = (Vec::new(), Sink::default());
        // continuation before any start
        c.push(0, &[0xFE, 1, 2, 0xFE, 3, 4]);
        assert_eq!(c.stats.orphans, 2);
        // more triplets than the header declares
        let p = g.packet(3);
        c.push(1, &flat(&p));
        c.push(1, &[0xFE, 9, 9]);
        assert_eq!(c.stats.orphans, 3);
        // a packet cut short by the next start goes out as it is
        let (cut, next) = (g.packet(8), g.packet(2));
        c.push(2, &flat(&cut[..5]));
        c.take(2, &mut out);
        assert_eq!(
            of_type(&trips(&out), |t| t >= 2),
            p,
            "the open packet waits"
        );
        c.push(3, &flat(&next));
        c.take(3, &mut out);
        assert_eq!(
            of_type(&trips(&out), |t| t >= 2),
            [&cut[..5], &next[..]].concat()
        );
        // a start triplet with cc_valid = 0 ends the open packet too
        let cut = g.packet(8);
        c.push(4, &flat(&cut[..2]));
        c.push(4, &[0xFB, 0, 0]);
        c.push(4, &[0xFE, 7, 7]);
        assert_eq!(c.stats.orphans, 4);
        c.take(4, &mut out);
        assert_eq!(of_type(&trips(&out), |t| t >= 2), cut[..2]);
        // padding of type 2 inside a packet in the source does not end it
        let p = g.packet(4);
        c.push(5, &flat(&[p[0], p[1], PAD_DTVCC, p[2], p[3]]));
        c.take(5, &mut out);
        sink.frame(CcRate::F2997, &out);
        assert_eq!(sink.dtvcc, p);
        // a trailing partial triplet and invalid 608 slots are ignored
        c.push(6, &[0xF8, 0x80, 0x80, 0xF9, 0x80, 0x80, 0xFC, 0x41]);
        assert_eq!(c.queued(), (0, 0, 0));
        assert_eq!(c.stats.in_608, [0, 0]);
    }

    #[test]
    fn overflow_drops_the_oldest_pairs_and_whole_packets() {
        let mut g = Gen::default();
        let cfg = CaptionCfg {
            max_dtvcc: 95,
            ..CaptionCfg::new(CcRate::F2997)
        };
        let mut c = CaptionCarrier::new(cfg);
        let (mut out, mut sink) = (Vec::new(), Sink::default());
        let mut pairs = Vec::new();
        let mut packets = Vec::new();
        // a 30-triplet packet begins to go out, then the output stops
        let first = g.packet(30);
        c.push(0, &flat(&first));
        c.take(0, &mut out);
        sink.frame(CcRate::F2997, &out);
        for k in 1..=100i64 {
            pairs.push(g.pair(0));
            packets.push(g.packet(10));
            c.push(k, &flat(&[pairs[pairs.len() - 1]]));
            c.push(k, &flat(&packets[packets.len() - 1]));
            let (q1, q2, qd) = c.queued();
            assert!(q1 <= 60 && q2 == 0 && qd <= 95);
        }
        // 12 triplets of the first packet and eight whole packets are left
        assert_eq!(c.queued(), (60, 0, 92));
        assert_eq!(c.stats.dropped_608, [40, 0]);
        assert_eq!((c.stats.dropped_packets, c.stats.dropped_dtvcc), (92, 920));
        for _ in 0..70 {
            c.take(i64::MAX, &mut out);
            sink.frame(CcRate::F2997, &out);
        }
        assert_eq!(sink.f1, pairs[40..]);
        assert_eq!(sink.dtvcc, [first, packets[92..].concat()].concat());
        let s = c.stats;
        assert_eq!(s.in_608[0], s.out_608[0] + s.dropped_608[0]);
        assert_eq!(s.in_dtvcc, s.out_dtvcc + s.dropped_dtvcc);

        // the default bound is two seconds
        let mut c = CaptionCarrier::new(CaptionCfg::new(CcRate::F5994));
        for k in 0..1000i64 {
            c.push(k, &flat(&[g.pair(0), g.pair(1)]));
            c.push(k, &flat(&g.packet(18)));
        }
        assert_eq!(c.queued(), (60, 60, 1080));
        // clear counts what it forgets
        c.clear();
        assert_eq!(c.queued(), (0, 0, 0));
        assert_eq!(c.stats.in_dtvcc, c.stats.dropped_dtvcc);
        assert_eq!(c.stats.in_608, c.stats.dropped_608);
    }

    /// Every valid input triplet appears exactly once in the output, in order per service,
    /// under random drops and repeats with matched average rates.
    fn conservation(rate: CcRate, seed: u64) {
        let mut rng = Rng(seed);
        let mut g = Gen::default();
        let mut c = CaptionCarrier::new(CaptionCfg::new(rate));
        let (mut out, mut sink) = (Vec::new(), Sink::default());
        let mut all: Vec<[u8; 3]> = Vec::new();
        let mut carry: Vec<[u8; 3]> = Vec::new();
        let (per_frame, max_packet) = match rate {
            CcRate::F2997 => (18, 8),
            CcRate::F5994 => (9, 4),
        };
        let mut pushed = 0i64;
        let mut shown = -1i64;
        for j in 0..5000i64 {
            // the clock: mostly the next frame; now and then a repeat or a drop, held within
            // two frames of the nominal position
            let step = match rng.below(10) {
                0 if shown > j - 2 => 0,
                1 if shown < j + 2 => 2,
                _ if shown < j - 2 => 2,
                _ if shown > j + 2 => 0,
                _ => 1,
            };
            shown += step;
            // the decoder is up to three frames ahead of the display
            while pushed <= shown + rng.below(4) as i64 {
                let mut src = Vec::new();
                match rate {
                    CcRate::F2997 => {
                        if rng.below(10) < 9 {
                            src.push(g.pair(0));
                        }
                        if rng.below(10) < 9 {
                            src.push(g.pair(1));
                        }
                    }
                    CcRate::F5994 => src.push(g.pair((pushed & 1) as u8)),
                }
                // the rest of a packet that began in the frame before, then up to two packets,
                // the last of which may run into the next frame
                let mut room = per_frame - carry.len();
                src.append(&mut carry);
                for _ in 0..rng.below(3) {
                    let n = 1 + rng.below(max_packet) as usize;
                    if n > room {
                        break;
                    }
                    room -= n;
                    let mut p = g.packet(n);
                    if n > 1 && rng.below(4) == 0 {
                        carry = p.split_off(1 + rng.below(n as u64 - 1) as usize);
                        src.extend(p);
                        break;
                    }
                    src.extend(p);
                }
                // some source padding in between
                if rng.below(2) == 0 {
                    src.push(PAD_DTVCC);
                }
                c.push(pushed, &flat(&src));
                all.extend(src);
                pushed += 1;
            }
            c.take(shown, &mut out);
            sink.frame(rate, &out);
            let (q1, q2, _) = c.queued();
            assert!(q1 <= 12 && q2 <= 12, "608 backlog stays small: {q1} {q2}");
        }
        all.extend(carry);
        for _ in 0..40 {
            c.take(i64::MAX, &mut out);
            sink.frame(rate, &out);
        }
        // the last packet may be waiting for its end
        let open = c.queued().2;
        assert!(open < max_packet as usize);
        let want = of_type(&all, |t| t >= 2);
        assert_eq!(sink.dtvcc[..], want[..sink.dtvcc.len()]);
        assert!(want.len() - sink.dtvcc.len() <= max_packet as usize);
        assert_eq!(sink.f1, of_type(&all, |t| t == 0), "seed {seed}");
        assert_eq!(sink.f2, of_type(&all, |t| t == 1), "seed {seed}");
        let s = c.stats;
        assert_eq!((s.dropped_608, s.dropped_dtvcc, s.orphans), ([0, 0], 0, 0));
        assert_eq!(s.nulls_skipped, [0, 0]);
    }

    /// Film mode: pictures of two and three fields (or one to three frame periods). Every
    /// pair and every packet goes out once, in order, the 608 pairs in the display order of
    /// the fields.
    #[test]
    fn pictures_of_two_and_three_fields_lose_and_repeat_nothing() {
        for (interlaced, seed) in [(true, 1u64), (true, 2), (false, 3), (false, 4)] {
            let rate = if interlaced {
                CcRate::F2997
            } else {
                CcRate::F5994
            };
            let mut c = CaptionCarrier::new(CaptionCfg::new(rate));
            let mut r = Rng(seed);
            let mut g = Gen::default();
            let (mut sent, mut got): (Vec<[u8; 3]>, Vec<[u8; 3]>) = (Vec::new(), Vec::new());
            let mut out = Vec::new();
            let mut top = true;
            let mut fields_out = 0u64;
            // a film source: one frame per picture, with a 608 pair per field at 24 frames
            // per second (five pairs of each field in two frames is the full rate; here four)
            for n in 0..400i64 {
                let mut cc = vec![g.pair(0), g.pair(1)];
                if n % 2 == 0 {
                    cc.push(g.pair((n / 2 % 2) as u8));
                }
                if r.below(3) == 0 {
                    cc.extend(g.packet(1 + r.below(20) as usize));
                }
                sent.extend(&cc);
                c.push(n, &flat(&cc));
                let units = if interlaced {
                    if n % 2 == 0 { 3 } else { 2 }
                } else {
                    [3, 2, 2, 3][n as usize % 4]
                };
                c.take_shown(n, units, interlaced.then_some(top), &mut out);
                let t = trips(&out);
                assert_eq!(t.len(), 10 * usize::from(units));
                // the leading 608 constructs: one per field period, in display order
                for (k, tr) in t.iter().take(usize::from(units)).enumerate() {
                    let want = if interlaced {
                        u8::from(top == (k % 2 == 0))
                    } else {
                        ((fields_out + k as u64) % 2) as u8
                    };
                    assert_eq!(tr[0] & 3, want, "picture {n}, field {k}");
                    assert_eq!(tr[0] & 0xF8, 0xF8);
                }
                assert!(t[usize::from(units)..].iter().all(|tr| tr[0] & 2 != 0));
                fields_out += u64::from(units);
                if interlaced && units == 3 {
                    top = !top;
                }
                got.extend(t.iter().filter(|tr| tr[0] & 4 != 0));
            }
            // drain
            for _ in 0..200 {
                c.take_shown(i64::MAX, 2, interlaced.then_some(top), &mut out);
                got.extend(trips(&out).iter().filter(|tr| tr[0] & 4 != 0));
            }
            for ty in [0u8, 1] {
                assert_eq!(
                    of_type(&got, |t| t == ty),
                    of_type(&sent, |t| t == ty),
                    "608 field {ty}"
                );
            }
            assert_eq!(
                of_type(&got, |t| t >= 2),
                of_type(&sent, |t| t >= 2),
                "DTVCC"
            );
            assert_eq!(c.stats.dropped_608, [0, 0]);
            assert_eq!(c.stats.dropped_dtvcc, 0);
        }
    }

    /// The fix of the field order: an interlaced output that begins with the top field
    /// writes the pair of field 2 first.
    #[test]
    fn top_field_first_output_writes_field_two_first() {
        let mut g = Gen::default();
        for top in [false, true] {
            let cfg = if top {
                CaptionCfg::top_first(CcRate::F2997)
            } else {
                CaptionCfg::new(CcRate::F2997)
            };
            let mut c = CaptionCarrier::new(cfg);
            let mut out = Vec::new();
            c.push(0, &flat(&[g.pair(0), g.pair(1)]));
            c.take(0, &mut out);
            let t = trips(&out);
            assert_eq!(t.len(), 20);
            let want = if top { [1, 0] } else { [0, 1] };
            assert_eq!([t[0][0] & 3, t[1][0] & 3], want);
            assert!(t[0][0] & 4 != 0 && t[1][0] & 4 != 0);
        }
    }

    #[test]
    fn long_run_conservation_under_random_drops_and_repeats() {
        for seed in 1..=8 {
            conservation(CcRate::F2997, seed);
            conservation(CcRate::F5994, seed);
        }
    }
}
