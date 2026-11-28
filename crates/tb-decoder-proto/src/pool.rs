//! Output pools: decoded frames come back in fixed slots of shared memory.
//!
//! A pool serves one stream. The parent makes it (a sealed memfd of `slots * slot_size`
//! bytes), maps it read-only, and grants it to the worker, which maps it writable. Every slot
//! is the worker's to write until it sends a frame in it; from then on it is the parent's until
//! the parent releases it. That is the whole of the flow control on this side: the worker can
//! have at most `slots` frames outstanding, and when none is free it waits a bounded time and
//! then drops the frame (counted), so neither side waits on the other for long.
//!
//! [`PoolTable`] is the parent's record of who owns which slot, and the validation of every
//! frame descriptor against it: a descriptor that names an unknown or retired pool, a slot the
//! parent already holds, a plane outside its slot, planes that overlap, a stride shorter than a
//! row, a size beyond the limits, captions that are not whole triplets or a timestamp far from
//! anything the parent sent is a [`Violation`]. The only way to obtain a [`ValidSlot`] is that
//! validation, and the only way to give a slot back is to consume the `ValidSlot`.

use std::collections::BTreeMap;

use tb_media::{
    AudioFrameDesc, MediaKind, PixelFormat, Plane, Rational, StreamIndex, VideoFrameDesc,
};

use crate::wire::{AudioOut, OutputSpec, PoolGrant, RESAMPLED_RATE, StreamOpen, VideoOut};

/// What the parent accepts from a worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Limits {
    /// Widest picture.
    pub max_width: u32,
    /// Tallest picture.
    pub max_height: u32,
    /// Largest slot.
    pub max_frame_bytes: u64,
    /// Most slots in one pool.
    pub max_slots: u16,
    /// Most bytes in all pools of one worker (retired ones included until emptied).
    pub max_total_bytes: u64,
    /// Most audio channels.
    pub max_channels: u8,
    /// Most samples per channel in one audio frame.
    pub max_samples: u32,
    /// Seconds a frame's timestamp may lie outside the range of the timestamps of the packets
    /// sent on its stream.
    pub pts_slack_secs: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_width: 4096,
            max_height: 2304,
            // 4096 x 2304 P010 with 64-byte row alignment
            max_frame_bytes: 4096 * 2304 * 3,
            max_slots: 32,
            max_total_bytes: 1 << 30,
            max_channels: 16,
            max_samples: 16_384,
            pts_slack_secs: 10,
        }
    }
}

/// A worker said something that cannot be true. The client kills it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Violation {
    /// A message did not decode.
    #[error("malformed message: {0}")]
    Wire(#[from] crate::wire::WireError),
    /// A message that is not expected in the current state.
    #[error("unexpected {0}")]
    Unexpected(&'static str),
    /// A pool that does not exist (or no longer exists).
    #[error("unknown pool {0}")]
    UnknownPool(u32),
    /// A frame in a pool the worker retired.
    #[error("frame in retired pool {0}")]
    RetiredPool(u32),
    /// A pool asked for a stream that already has an active one.
    #[error("second active pool for stream {0}")]
    SecondPool(u16),
    /// A slot index beyond the pool.
    #[error("slot {slot} of pool {pool} with {slots} slots")]
    SlotRange {
        /// Pool.
        pool: u32,
        /// Slot.
        slot: u16,
        /// Slots in the pool.
        slots: u16,
    },
    /// A slot the parent holds (sent twice without a release).
    #[error("slot {slot} of pool {pool} is not the worker's")]
    SlotNotOwned {
        /// Pool.
        pool: u32,
        /// Slot.
        slot: u16,
    },
    /// A stream that was not announced, or of the other kind, or not the pool's.
    #[error("frame for stream {0} that is not announced, of another kind, or not the pool's")]
    Stream(u16),
    /// Size outside the limits.
    #[error("picture {width}x{height} outside the limits")]
    Dimensions {
        /// Width.
        width: u32,
        /// Height.
        height: u32,
    },
    /// Wrong number of planes for the format.
    #[error("{got} planes for {format:?}")]
    PlaneCount {
        /// Format.
        format: PixelFormat,
        /// Planes sent.
        got: usize,
    },
    /// A plane's stride is shorter than its rows.
    #[error("plane {plane}: stride {stride} below row of {row} bytes")]
    Stride {
        /// Plane.
        plane: usize,
        /// Stride.
        stride: u32,
        /// Bytes of one row.
        row: u64,
    },
    /// A plane reaches beyond its slot.
    #[error("plane {plane} ends at {end}, slot has {slot_size} bytes")]
    PlaneBounds {
        /// Plane.
        plane: usize,
        /// End offset.
        end: u64,
        /// Slot size.
        slot_size: u64,
    },
    /// Two planes share bytes.
    #[error("planes overlap")]
    PlanesOverlap,
    /// Caption bytes that are not whole triplets.
    #[error("{0} caption bytes")]
    Captions(usize),
    /// An active rectangle that is empty or larger than the frame.
    #[error("active rectangle {width}x{height}")]
    Active {
        /// Width.
        width: u32,
        /// Height.
        height: u32,
    },
    /// Audio parameters outside the limits.
    #[error("audio {rate} Hz, {channels} channels, {samples} samples")]
    Audio {
        /// Rate.
        rate: u32,
        /// Channels.
        channels: u8,
        /// Samples per channel.
        samples: u32,
    },
    /// Audio that does not fit its slot.
    #[error("audio of {bytes} bytes in a slot of {slot_size}")]
    AudioBounds {
        /// Bytes.
        bytes: u64,
        /// Slot size.
        slot_size: u64,
    },
    /// A timestamp nowhere near those of the packets sent.
    #[error("stream {stream}: timestamp {pts:?} outside the packets' range")]
    Timestamp {
        /// Stream.
        stream: u16,
        /// Timestamp.
        pts: Option<i64>,
    },
    /// Converted output that is not what the stream's spec asked for.
    #[error("stream {0}: frame does not match the requested output")]
    Spec(u16),
    /// Ring credit for bytes never sent, or going backwards.
    #[error("input ring: {0}")]
    Ring(#[from] crate::ring::RingError),
    /// A pong for a ping never sent.
    #[error("pong {0} for no ping")]
    Pong(u64),
}

/// Why a pool request is refused (not a violation: the stream's frames are dropped).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Deny {
    /// Zero or more than the limit of slots or bytes per slot.
    #[error("slot size or count outside the limits")]
    Size,
    /// The worker's pools would exceed the total.
    #[error("total pool memory exceeded")]
    Total,
}

/// Bytes of one row and number of rows of each plane of a format.
pub fn plane_shapes(format: PixelFormat, width: u32, height: u32) -> Vec<(u64, u64)> {
    let (w, h) = (u64::from(width), u64::from(height));
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    match format {
        PixelFormat::Yuv420p => vec![(w, h), (cw, ch), (cw, ch)],
        PixelFormat::Nv12 => vec![(w, h), (cw * 2, ch)],
        PixelFormat::P010 => vec![(w * 2, h), (cw * 4, ch)],
    }
}

/// The layout the worker uses: planes one after the other, every row starting on an `align`
/// byte boundary (a power of two). Returns the planes and the bytes they take.
pub fn pack_planes(format: PixelFormat, width: u32, height: u32, align: u64) -> (Vec<Plane>, u64) {
    let mut at = 0u64;
    let mut planes = Vec::new();
    for (row, rows) in plane_shapes(format, width, height) {
        let stride = row.div_ceil(align) * align;
        planes.push(Plane {
            offset: u32::try_from(at).unwrap_or(u32::MAX),
            stride: u32::try_from(stride).unwrap_or(u32::MAX),
        });
        at += stride * rows;
    }
    (planes, at)
}

/// Bytes of an audio frame: f32 planar.
pub fn audio_bytes(channels: u8, samples: u32) -> u64 {
    u64::from(channels) * u64::from(samples) * 4
}

/// One stream the parent announced, and the range of timestamps it has sent on it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Announced {
    index: StreamIndex,
    kind: MediaKind,
    time_base: Rational,
    output: OutputSpec,
    lo: Option<i64>,
    hi: Option<i64>,
}

/// The streams the parent has announced (the current set, and the previous one until the
/// worker shows it has taken the new one: frames of the old session may still be in flight).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Streams {
    current: Vec<Announced>,
    previous: Vec<Announced>,
}

impl Streams {
    /// Announces a new set; the old one stays valid until [`Streams::acknowledged`].
    pub fn replace(&mut self, opens: &[StreamOpen]) {
        let new = opens
            .iter()
            .map(|o| Announced {
                index: o.info.index,
                kind: o.info.codec.kind(),
                time_base: o.info.time_base,
                output: o.output,
                lo: None,
                hi: None,
            })
            .collect();
        self.previous = std::mem::replace(&mut self.current, new);
    }

    /// The worker has answered for the new set: frames of the old one are no longer valid.
    pub fn acknowledged(&mut self) {
        self.previous.clear();
    }

    /// Forgets everything (a new worker).
    pub fn clear(&mut self) {
        self.current.clear();
        self.previous.clear();
    }

    /// The kind of an announced stream (of the current set, else of the previous one).
    pub fn kind(&self, stream: StreamIndex) -> Option<MediaKind> {
        self.current
            .iter()
            .chain(&self.previous)
            .find(|a| a.index == stream)
            .map(|a| a.kind)
    }

    /// Records the timestamps of a packet about to be sent; false if the stream is not in the
    /// current set.
    pub fn note_packet(&mut self, stream: StreamIndex, pts: Option<i64>, dts: Option<i64>) -> bool {
        let Some(a) = self.current.iter_mut().find(|a| a.index == stream) else {
            return false;
        };
        for t in [pts, dts].into_iter().flatten() {
            a.lo = Some(a.lo.map_or(t, |lo| lo.min(t)));
            a.hi = Some(a.hi.map_or(t, |hi| hi.max(t)));
        }
        true
    }

    fn find(&self, stream: StreamIndex, kind: MediaKind) -> Option<&Announced> {
        self.current
            .iter()
            .chain(&self.previous)
            .find(|a| a.index == stream && a.kind == kind)
    }

    /// A converted picture is what its stream's spec asked for: planar 4:2:0 of the target
    /// size, rows without padding, limited range, the target's matrix.
    fn check_video_spec(&self, d: &VideoFrameDesc) -> Result<(), Violation> {
        let a = self
            .find(d.stream, MediaKind::Video)
            .ok_or(Violation::Stream(d.stream.0))?;
        let OutputSpec::Video(v) = a.output else {
            return Ok(());
        };
        let (planes, _) = pack_planes(PixelFormat::Yuv420p, v.width, v.height, 1);
        if d.format != PixelFormat::Yuv420p
            || d.width != v.width
            || d.height != v.height
            || d.planes != planes
            || d.range != tb_media::Range::Limited
            || d.matrix != v.matrix()
        {
            return Err(Violation::Spec(d.stream.0));
        }
        Ok(())
    }

    /// Resampled audio is 48 kHz with the spec's channel count (`Source`: 1, 2 or 6).
    fn check_audio_spec(&self, d: &AudioFrameDesc) -> Result<(), Violation> {
        let a = self
            .find(d.stream, MediaKind::Audio)
            .ok_or(Violation::Stream(d.stream.0))?;
        let OutputSpec::Audio(s) = a.output else {
            return Ok(());
        };
        let channels_ok = match s.layout {
            crate::wire::AudioLayout::Source => matches!(d.channels, 1 | 2 | 6),
            l => d.channels == l.channels(),
        };
        if d.sample_rate != RESAMPLED_RATE || !channels_ok {
            return Err(Violation::Spec(d.stream.0));
        }
        Ok(())
    }

    /// A frame's timestamp is plausible: none if no packet carried one, else within the
    /// packets' range widened by `slack` seconds.
    fn check_pts(
        &self,
        stream: StreamIndex,
        kind: MediaKind,
        pts: Option<i64>,
        slack_secs: u32,
    ) -> Result<(), Violation> {
        let bad = Violation::Timestamp {
            stream: stream.0,
            pts,
        };
        self.find(stream, kind).ok_or(Violation::Stream(stream.0))?;
        let Some(t) = pts else { return Ok(()) };
        // In the range of the current set or of the previous one: until the worker takes the
        // new set, a frame may belong to either session, and both may use the same index (an
        // input that starts over, a looping file, with timestamps from the start again).
        let fits = self
            .current
            .iter()
            .chain(&self.previous)
            .filter(|a| a.index == stream && a.kind == kind)
            .any(|a| {
                let (Some(lo), Some(hi)) = (a.lo, a.hi) else {
                    return false;
                };
                let slack = Rational::new(1, 1)
                    .map(|one| one.rescale(i64::from(slack_secs), a.time_base))
                    .unwrap_or(0);
                t >= lo.saturating_sub(slack) && t <= hi.saturating_add(slack)
            });
        if fits { Ok(()) } else { Err(bad) }
    }
}

/// A slot the parent holds, obtained only by validating a frame in it. Consumed by
/// [`PoolTable::release`]; not `Clone`, so a slot is given back once.
#[derive(Debug, PartialEq, Eq)]
#[must_use = "a ValidSlot that is dropped keeps its slot from the worker until the pool goes"]
pub struct ValidSlot {
    pool: u32,
    slot: u16,
    offset: u64,
    len: u64,
}

impl ValidSlot {
    /// The pool.
    pub fn pool(&self) -> u32 {
        self.pool
    }
    /// The slot.
    pub fn slot(&self) -> u16 {
        self.slot
    }
    /// Byte offset of the slot in the pool's mapping.
    pub fn offset(&self) -> u64 {
        self.offset
    }
    /// Bytes of the slot.
    pub fn len(&self) -> u64 {
        self.len
    }
    /// Whether the slot has no bytes (never: pools have non-empty slots).
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// What to do after a slot was given back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Released {
    /// Tell the worker (send `Release`).
    ToWorker {
        /// Pool.
        pool: u32,
        /// Slot.
        slot: u16,
    },
    /// The pool was retired and is now empty: unmap it.
    PoolGone(u32),
    /// Nothing (a retired pool that still has slots out).
    Nothing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Pool {
    stream: StreamIndex,
    kind: MediaKind,
    slot_size: u64,
    held: Vec<bool>,
    retired: bool,
}

impl Pool {
    fn bytes(&self) -> u64 {
        self.slot_size * self.held.len() as u64
    }
}

/// The parent's record of a worker's pools.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PoolTable {
    pools: BTreeMap<u32, Pool>,
    next: u32,
    total: u64,
}

impl PoolTable {
    /// Bytes in all pools.
    pub fn total_bytes(&self) -> u64 {
        self.total
    }

    /// Slots the parent holds, in all pools.
    pub fn held(&self) -> usize {
        self.pools
            .values()
            .map(|p| p.held.iter().filter(|h| **h).count())
            .sum()
    }

    /// Validates a request for a pool. `Ok` is the grant to send (with a memfd of
    /// `slots * slot_size` bytes); `Err(Deny)` is answered with `PoolDenied`; a request for a
    /// stream that is not announced or already has an active pool is a violation.
    pub fn request(
        &mut self,
        streams: &Streams,
        stream: StreamIndex,
        slot_size: u64,
        slots: u16,
        limits: &Limits,
    ) -> Result<Result<PoolGrant, Deny>, Violation> {
        let kind = streams.kind(stream).ok_or(Violation::Stream(stream.0))?;
        if kind == MediaKind::Data {
            return Err(Violation::Stream(stream.0));
        }
        if self
            .pools
            .values()
            .any(|p| p.stream == stream && !p.retired)
        {
            return Err(Violation::SecondPool(stream.0));
        }
        if slot_size == 0
            || slot_size > limits.max_frame_bytes
            || slots == 0
            || slots > limits.max_slots
        {
            return Ok(Err(Deny::Size));
        }
        let bytes = slot_size * u64::from(slots);
        if self.total + bytes > limits.max_total_bytes {
            return Ok(Err(Deny::Total));
        }
        let pool = self.next;
        self.next = self.next.wrapping_add(1);
        self.total += bytes;
        self.pools.insert(
            pool,
            Pool {
                stream,
                kind,
                slot_size,
                held: vec![false; usize::from(slots)],
                retired: false,
            },
        );
        Ok(Ok(PoolGrant {
            pool,
            stream,
            slot_size,
            slots,
        }))
    }

    /// The worker retires a pool. Returns true if it is empty now (unmap it).
    pub fn retire(&mut self, pool: u32) -> Result<bool, Violation> {
        let p = self
            .pools
            .get_mut(&pool)
            .ok_or(Violation::UnknownPool(pool))?;
        if p.retired {
            return Err(Violation::RetiredPool(pool));
        }
        p.retired = true;
        Ok(self.remove_if_done(pool))
    }

    fn remove_if_done(&mut self, pool: u32) -> bool {
        let done = self
            .pools
            .get(&pool)
            .is_some_and(|p| p.retired && !p.held.iter().any(|h| *h));
        if done && let Some(p) = self.pools.remove(&pool) {
            self.total -= p.bytes();
        }
        done
    }

    /// Takes the slot a frame names, checking that pool, slot and stream agree.
    fn take(
        &mut self,
        pool: u32,
        slot: u16,
        stream: StreamIndex,
        kind: MediaKind,
    ) -> Result<(u64, ValidSlot), Violation> {
        let p = self
            .pools
            .get_mut(&pool)
            .ok_or(Violation::UnknownPool(pool))?;
        if p.retired {
            return Err(Violation::RetiredPool(pool));
        }
        if p.stream != stream || p.kind != kind {
            return Err(Violation::Stream(stream.0));
        }
        let slots = p.held.len() as u16;
        let h = p
            .held
            .get_mut(usize::from(slot))
            .ok_or(Violation::SlotRange { pool, slot, slots })?;
        if *h {
            return Err(Violation::SlotNotOwned { pool, slot });
        }
        *h = true;
        Ok((
            p.slot_size,
            ValidSlot {
                pool,
                slot,
                offset: u64::from(slot) * p.slot_size,
                len: p.slot_size,
            },
        ))
    }

    /// Validates a decoded picture and takes its slot.
    pub fn accept_video(
        &mut self,
        out: &VideoOut,
        streams: &Streams,
        limits: &Limits,
    ) -> Result<ValidSlot, Violation> {
        let d = &out.desc;
        check_video(d, limits)?;
        streams.check_pts(d.stream, MediaKind::Video, d.pts, limits.pts_slack_secs)?;
        streams.check_video_spec(d)?;
        // the slot size is known only from the pool; check geometry before taking the slot
        let slot_size = self
            .pools
            .get(&out.pool)
            .map(|p| p.slot_size)
            .ok_or(Violation::UnknownPool(out.pool))?;
        check_planes(d, slot_size)?;
        let (_, v) = self.take(out.pool, out.slot, d.stream, MediaKind::Video)?;
        Ok(v)
    }

    /// Validates decoded audio and takes its slot.
    pub fn accept_audio(
        &mut self,
        out: &AudioOut,
        streams: &Streams,
        limits: &Limits,
    ) -> Result<ValidSlot, Violation> {
        let d = &out.desc;
        check_audio(d, limits)?;
        streams.check_pts(d.stream, MediaKind::Audio, d.pts, limits.pts_slack_secs)?;
        streams.check_audio_spec(d)?;
        let slot_size = self
            .pools
            .get(&out.pool)
            .map(|p| p.slot_size)
            .ok_or(Violation::UnknownPool(out.pool))?;
        let bytes = audio_bytes(d.channels, d.samples);
        if bytes > slot_size {
            return Err(Violation::AudioBounds { bytes, slot_size });
        }
        let (_, v) = self.take(out.pool, out.slot, d.stream, MediaKind::Audio)?;
        Ok(v)
    }

    /// Gives a slot back.
    pub fn release(&mut self, slot: ValidSlot) -> Released {
        let Some(p) = self.pools.get_mut(&slot.pool) else {
            return Released::Nothing;
        };
        if let Some(h) = p.held.get_mut(usize::from(slot.slot)) {
            *h = false;
        }
        if !p.retired {
            return Released::ToWorker {
                pool: slot.pool,
                slot: slot.slot,
            };
        }
        if self.remove_if_done(slot.pool) {
            Released::PoolGone(slot.pool)
        } else {
            Released::Nothing
        }
    }
}

/// Size, plane count and caption checks of a picture (independent of the slot).
fn check_video(d: &VideoFrameDesc, limits: &Limits) -> Result<(), Violation> {
    if d.width == 0 || d.height == 0 || d.width > limits.max_width || d.height > limits.max_height {
        return Err(Violation::Dimensions {
            width: d.width,
            height: d.height,
        });
    }
    let shapes = plane_shapes(d.format, d.width, d.height);
    if d.planes.len() != shapes.len() {
        return Err(Violation::PlaneCount {
            format: d.format,
            got: d.planes.len(),
        });
    }
    if !d.cc_data.len().is_multiple_of(3) {
        return Err(Violation::Captions(d.cc_data.len()));
    }
    if let Some((aw, ah)) = d.active
        && (aw == 0 || ah == 0 || aw > d.width || ah > d.height)
    {
        return Err(Violation::Active {
            width: aw,
            height: ah,
        });
    }
    Ok(())
}

/// Every plane lies in the slot, rows fit their stride, planes do not overlap.
fn check_planes(d: &VideoFrameDesc, slot_size: u64) -> Result<(), Violation> {
    let shapes = plane_shapes(d.format, d.width, d.height);
    let mut spans = Vec::with_capacity(shapes.len());
    for (i, (p, (row, rows))) in d.planes.iter().zip(shapes).enumerate() {
        let stride = u64::from(p.stride);
        if stride < row {
            return Err(Violation::Stride {
                plane: i,
                stride: p.stride,
                row,
            });
        }
        // offset + stride * (rows - 1) + row: no overflow, all terms below 2^32 * 2^32
        let end = u64::from(p.offset) + stride * (rows - 1) + row;
        if end > slot_size {
            return Err(Violation::PlaneBounds {
                plane: i,
                end,
                slot_size,
            });
        }
        spans.push((u64::from(p.offset), end));
    }
    spans.sort_unstable();
    if spans.windows(2).any(|w| w[0].1 > w[1].0) {
        return Err(Violation::PlanesOverlap);
    }
    Ok(())
}

fn check_audio(d: &AudioFrameDesc, limits: &Limits) -> Result<(), Violation> {
    if !(1000..=384_000).contains(&d.sample_rate)
        || d.channels == 0
        || d.channels > limits.max_channels
        || d.samples == 0
        || d.samples > limits.max_samples
    {
        return Err(Violation::Audio {
            rate: d.sample_rate,
            channels: d.channels,
            samples: d.samples,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tb_media::{AudioHint, CodecId, Matrix, Range, StreamInfo, VideoHint};

    fn open(i: u16, codec: CodecId) -> StreamOpen {
        StreamOpen {
            info: info(i, codec),
            output: OutputSpec::Native,
        }
    }

    fn info(i: u16, codec: CodecId) -> StreamInfo {
        StreamInfo {
            index: StreamIndex(i),
            codec,
            time_base: Rational::MPEG_90K,
            extradata: vec![],
            video: VideoHint::default(),
            audio: AudioHint::default(),
            language: None,
        }
    }

    fn setup() -> (PoolTable, Streams, Limits, PoolGrant, PoolGrant) {
        let mut streams = Streams::default();
        streams.replace(&[open(0, CodecId::H264), open(1, CodecId::Aac)]);
        streams.note_packet(StreamIndex(0), Some(90_000), Some(87_000));
        streams.note_packet(StreamIndex(1), Some(90_000), None);
        let limits = Limits::default();
        let mut t = PoolTable::default();
        let (_, size) = pack_planes(PixelFormat::Yuv420p, 1920, 1080, 64);
        let v = t
            .request(&streams, StreamIndex(0), size, 4, &limits)
            .unwrap()
            .unwrap();
        let a = t
            .request(&streams, StreamIndex(1), audio_bytes(2, 2048), 8, &limits)
            .unwrap()
            .unwrap();
        (t, streams, limits, v, a)
    }

    fn picture(pool: u32, slot: u16) -> VideoOut {
        let (planes, _) = pack_planes(PixelFormat::Yuv420p, 1920, 1080, 64);
        VideoOut {
            pool,
            slot,
            desc: VideoFrameDesc {
                stream: StreamIndex(0),
                pts: Some(90_000),
                width: 1920,
                height: 1080,
                format: PixelFormat::Yuv420p,
                planes,
                interlaced: false,
                top_field_first: false,
                matrix: Matrix::Bt709,
                range: Range::Limited,
                sample_aspect: None,
                cc_data: vec![],
                corrupt: false,
                active: None,
            },
        }
    }

    #[test]
    fn a_slot_is_held_once_and_returned_once() {
        let (mut t, s, l, v, _) = setup();
        let a = t.accept_video(&picture(v.pool, 1), &s, &l).unwrap();
        assert_eq!(a.offset(), v.slot_size);
        assert_eq!(
            t.accept_video(&picture(v.pool, 1), &s, &l),
            Err(Violation::SlotNotOwned {
                pool: v.pool,
                slot: 1
            })
        );
        assert_eq!(t.held(), 1);
        assert_eq!(
            t.release(a),
            Released::ToWorker {
                pool: v.pool,
                slot: 1
            }
        );
        assert_eq!(t.held(), 0);
        let again = t.accept_video(&picture(v.pool, 1), &s, &l).unwrap();
        let _ = t.release(again);
    }

    #[test]
    fn bad_descriptors_are_violations() {
        let (mut t, s, l, v, a) = setup();
        let mut p = picture(v.pool, 4);
        assert!(matches!(
            t.accept_video(&p, &s, &l),
            Err(Violation::SlotRange { .. })
        ));
        p = picture(v.pool + 100, 0);
        assert!(matches!(
            t.accept_video(&p, &s, &l),
            Err(Violation::UnknownPool(_))
        ));
        // a frame of the video stream in the audio pool
        p = picture(a.pool, 0);
        assert!(matches!(
            t.accept_video(&p, &s, &l),
            Err(Violation::Stream(0) | Violation::PlaneBounds { .. })
        ));
        p = picture(v.pool, 0);
        p.desc.planes[2].offset += 64;
        assert!(matches!(
            t.accept_video(&p, &s, &l),
            Err(Violation::PlaneBounds { plane: 2, .. })
        ));
        p = picture(v.pool, 0);
        p.desc.planes[1].offset = 100;
        assert_eq!(t.accept_video(&p, &s, &l), Err(Violation::PlanesOverlap));
        p = picture(v.pool, 0);
        p.desc.planes[0].stride = 1919;
        assert!(matches!(
            t.accept_video(&p, &s, &l),
            Err(Violation::Stride { plane: 0, .. })
        ));
        p = picture(v.pool, 0);
        p.desc.planes.pop();
        assert!(matches!(
            t.accept_video(&p, &s, &l),
            Err(Violation::PlaneCount { .. })
        ));
        p = picture(v.pool, 0);
        p.desc.width = 8192;
        assert!(matches!(
            t.accept_video(&p, &s, &l),
            Err(Violation::Dimensions { .. })
        ));
        p = picture(v.pool, 0);
        p.desc.cc_data = vec![0; 4];
        assert_eq!(t.accept_video(&p, &s, &l), Err(Violation::Captions(4)));
        p = picture(v.pool, 0);
        p.desc.active = Some((p.desc.width + 2, p.desc.height));
        assert!(matches!(
            t.accept_video(&p, &s, &l),
            Err(Violation::Active { .. })
        ));
        p = picture(v.pool, 0);
        p.desc.active = Some((0, p.desc.height));
        assert!(matches!(
            t.accept_video(&p, &s, &l),
            Err(Violation::Active { .. })
        ));
        p = picture(v.pool, 0);
        p.desc.planes[0].stride = u32::MAX;
        assert!(matches!(
            t.accept_video(&p, &s, &l),
            Err(Violation::PlaneBounds { .. })
        ));
        // none of these took a slot
        assert_eq!(t.held(), 0);
    }

    #[test]
    fn timestamps_near_the_packets_only() {
        let (mut t, s, l, v, _) = setup();
        let mut p = picture(v.pool, 0);
        p.desc.pts = Some(87_000 - 10 * 90_000);
        let ok = t.accept_video(&p, &s, &l).unwrap();
        let _ = t.release(ok);
        p.desc.pts = Some(87_000 - 10 * 90_000 - 1);
        assert!(matches!(
            t.accept_video(&p, &s, &l),
            Err(Violation::Timestamp { .. })
        ));
        p.desc.pts = Some(i64::MAX);
        assert!(t.accept_video(&p, &s, &l).is_err());
        p.desc.pts = None;
        let ok = t.accept_video(&p, &s, &l).unwrap();
        let _ = t.release(ok);
    }

    #[test]
    fn audio_checks() {
        let (mut t, s, l, _, a) = setup();
        let mut out = AudioOut {
            pool: a.pool,
            slot: 0,
            desc: AudioFrameDesc {
                stream: StreamIndex(1),
                pts: Some(90_000),
                sample_rate: 48_000,
                channels: 2,
                samples: 1024,
            },
        };
        let ok = t.accept_audio(&out, &s, &l).unwrap();
        let _ = t.release(ok);
        out.desc.samples = 4096;
        assert!(matches!(
            t.accept_audio(&out, &s, &l),
            Err(Violation::AudioBounds { .. })
        ));
        out.desc.samples = 1024;
        out.desc.channels = 0;
        assert!(matches!(
            t.accept_audio(&out, &s, &l),
            Err(Violation::Audio { .. })
        ));
    }

    #[test]
    fn pools_are_limited_and_retired() {
        let (mut t, s, l, v, _) = setup();
        // second active pool for the stream
        assert_eq!(
            t.request(&s, StreamIndex(0), 100, 2, &l),
            Err(Violation::SecondPool(0))
        );
        let held = t.accept_video(&picture(v.pool, 0), &s, &l).unwrap();
        assert_eq!(t.retire(v.pool), Ok(false));
        // frames in the retired pool are refused
        assert_eq!(
            t.accept_video(&picture(v.pool, 1), &s, &l),
            Err(Violation::RetiredPool(v.pool))
        );
        // a new pool may be asked for now; too big ones are denied
        assert_eq!(
            t.request(&s, StreamIndex(0), l.max_frame_bytes + 1, 2, &l),
            Ok(Err(Deny::Size))
        );
        let tight = Limits {
            max_total_bytes: t.total_bytes() + 1_000,
            ..l.clone()
        };
        assert_eq!(
            t.request(&s, StreamIndex(0), 1_001, 1, &tight),
            Ok(Err(Deny::Total))
        );
        let before = t.total_bytes();
        // the held slot comes back: the retired pool goes
        assert_eq!(t.release(held), Released::PoolGone(v.pool));
        assert_eq!(t.total_bytes(), before - v.slot_size * 4);
        assert!(t.request(&s, StreamIndex(9), 100, 2, &l).is_err());
    }

    #[test]
    fn previous_session_frames_are_valid_until_acknowledged() {
        let (mut t, mut s, l, v, _) = setup();
        s.replace(&[open(5, CodecId::Hevc)]);
        let ok = t.accept_video(&picture(v.pool, 0), &s, &l).unwrap();
        let _ = t.release(ok);
        s.acknowledged();
        assert!(t.accept_video(&picture(v.pool, 0), &s, &l).is_err());
    }

    /// An input that starts over with the same stream index and timestamps from the start (a
    /// looping file): frames of the old session, late in its range, stay valid until the
    /// worker takes the new set, although the new set's range does not hold them.
    #[test]
    fn previous_session_timestamps_with_the_same_index() {
        let (mut t, mut s, l, v, _) = setup();
        // the old session reached 60 s; the new one is announced and has no packet yet (what
        // a looping file does at its end)
        s.note_packet(StreamIndex(0), Some(5_400_000), None);
        s.replace(&[open(0, CodecId::H264), open(1, CodecId::Aac)]);
        let late = |slot| {
            let mut p = picture(v.pool, slot);
            p.desc.pts = Some(5_400_000);
            p
        };
        let ok = t.accept_video(&late(0), &s, &l).unwrap();
        let _ = t.release(ok);
        // the new session starts again at 1.4 s: the old frame is still valid
        s.note_packet(StreamIndex(0), Some(126_000), None);
        let ok = t.accept_video(&late(1), &s, &l).unwrap();
        let _ = t.release(ok);
        // a time in neither range (widened by the slack) is still refused
        let mut wild = picture(v.pool, 2);
        wild.desc.pts = Some(90_000 * 3600);
        assert!(matches!(
            t.accept_video(&wild, &s, &l),
            Err(Violation::Timestamp { .. })
        ));
        // once the worker has the new set, the old range is gone
        s.acknowledged();
        assert!(matches!(
            t.accept_video(&late(3), &s, &l),
            Err(Violation::Timestamp { .. })
        ));
    }

    #[test]
    fn converted_output_must_match_its_spec() {
        use crate::wire::{Aspect, AudioLayout, AudioSpec, Deinterlace, VideoSpec};
        let mut streams = Streams::default();
        let spec = VideoSpec {
            width: 1280,
            height: 720,
            display_aspect: Rational::new(16, 9).unwrap(),
            aspect: Aspect::Fit,
            deinterlace: Deinterlace::Bob,
        };
        streams.replace(&[
            StreamOpen {
                info: info(0, CodecId::H264),
                output: OutputSpec::Video(spec),
            },
            StreamOpen {
                info: info(1, CodecId::Aac),
                output: OutputSpec::Audio(AudioSpec {
                    layout: AudioLayout::Surround51,
                }),
            },
        ]);
        streams.note_packet(StreamIndex(0), Some(0), None);
        streams.note_packet(StreamIndex(1), Some(0), None);
        let l = Limits::default();
        let mut t = PoolTable::default();
        let v = t
            .request(&streams, StreamIndex(0), 1280 * 720 * 3 / 2, 2, &l)
            .unwrap()
            .unwrap();
        let (planes, _) = pack_planes(PixelFormat::Yuv420p, 1280, 720, 1);
        let mut out = picture(v.pool, 0);
        out.desc.width = 1280;
        out.desc.height = 720;
        out.desc.planes = planes.clone();
        out.desc.pts = Some(0);
        let ok = t.accept_video(&out, &streams, &l).unwrap();
        let _ = t.release(ok);
        // padded rows, another size, another matrix: not what was asked for
        let (padded, _) = pack_planes(PixelFormat::Yuv420p, 1280, 720, 256);
        out.desc.planes = padded;
        assert_eq!(t.accept_video(&out, &streams, &l), Err(Violation::Spec(0)));
        out.desc.planes = planes;
        out.desc.matrix = Matrix::Bt601;
        assert_eq!(t.accept_video(&out, &streams, &l), Err(Violation::Spec(0)));
        let a = t
            .request(&streams, StreamIndex(1), audio_bytes(6, 2048), 2, &l)
            .unwrap()
            .unwrap();
        let mut au = AudioOut {
            pool: a.pool,
            slot: 0,
            desc: AudioFrameDesc {
                stream: StreamIndex(1),
                pts: Some(0),
                sample_rate: 48_000,
                channels: 6,
                samples: 1024,
            },
        };
        let ok = t.accept_audio(&au, &streams, &l).unwrap();
        let _ = t.release(ok);
        au.desc.sample_rate = 44_100;
        assert_eq!(t.accept_audio(&au, &streams, &l), Err(Violation::Spec(1)));
        au.desc.sample_rate = 48_000;
        au.desc.channels = 2;
        assert_eq!(t.accept_audio(&au, &streams, &l), Err(Violation::Spec(1)));
        // `Source`: mono, stereo or 5.1, and nothing else.
        streams.replace(&[StreamOpen {
            info: info(1, CodecId::Aac),
            output: OutputSpec::Audio(AudioSpec {
                layout: AudioLayout::Source,
            }),
        }]);
        streams.note_packet(StreamIndex(1), Some(0), None);
        for (ch, ok) in [(1, true), (2, true), (6, true), (3, false), (8, false)] {
            au.desc.channels = ch;
            let r = t.accept_audio(&au, &streams, &l);
            assert_eq!(r.is_ok(), ok, "{ch} channels: {r:?}");
            if let Ok(s) = r {
                let _ = t.release(s);
            }
        }
    }

    #[test]
    fn packing() {
        let (planes, size) = pack_planes(PixelFormat::P010, 1281, 721, 64);
        assert_eq!(planes[0].stride, 2624);
        assert_eq!(planes[1].offset, 2624 * 721);
        assert_eq!(size, 2624 * 721 + 2624 * 361);
        let (planes, _) = pack_planes(PixelFormat::Nv12, 7, 3, 1);
        assert_eq!(planes[1].stride, 8);
    }
}
