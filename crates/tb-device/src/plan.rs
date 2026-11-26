//! Plans: what the core asks a device to do in one phase of a tick.

use crate::kernel::{Launch, RefKind};
use crate::mem::{DevBuf, HostBuf};

/// A queue of operations that the device runs in order. Operations on different streams may
/// overlap unless an event orders them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StreamId(pub u8);

impl StreamId {
    /// The kernel stream: parameters, kernels, results. Every phase waits on it.
    pub const MAIN: StreamId = StreamId(0);
    /// The upload stream: next tick's frames go to the device while this tick is coded
    /// (the C++ `st_up`, `--overlap`).
    pub const UPLOAD: StreamId = StreamId(1);
    /// Clearing of new batches, so that it does not hold up the tick in flight.
    pub const SIDE: StreamId = StreamId(2);
    /// Number of streams an adapter provides.
    pub const COUNT: usize = 3;
}

/// A marker in one stream that another stream can wait for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct EventId(pub u8);

impl EventId {
    /// The tick's parameter copies have gone through on [`StreamId::MAIN`]. The frame upload
    /// waits for it: next to a busy context of another process the copy engine was seen to run
    /// the whole upload before parameter copies queued ahead of it, which held the first kernel
    /// back by the upload time (notes/cuda-mpeg2-optlog.md, experiment 41).
    pub const PARAMS: EventId = EventId(0);
    /// The frames queued on [`StreamId::UPLOAD`] are on the device. The first kernel that reads
    /// them waits for it on [`StreamId::MAIN`] (the C++ synchronizes the upload stream from the
    /// host at the start of the next tick instead).
    pub const FRAMES: EventId = EventId(1);
    /// Number of events an adapter provides.
    pub const COUNT: usize = 2;
}

/// One operation.
#[derive(Clone, Copy, Debug, PartialEq)]
#[allow(
    clippy::large_enum_variant,
    reason = "a plan is one flat vector rewritten in place every phase; boxing the launch would \
              allocate on the hot path"
)]
pub enum Op {
    /// Set every byte of a device region.
    Fill {
        /// Region to fill.
        dst: DevBuf,
        /// Value.
        byte: u8,
    },
    /// Copy host to device. Lengths must be equal.
    Upload {
        /// Destination on the device.
        dst: DevBuf,
        /// Source in the host arena.
        src: HostBuf,
    },
    /// Copy `rows` rows of `width` bytes host to device with different pitches (a picture into
    /// its padded plane; `cudaMemcpy2DAsync` in the C++).
    Upload2d {
        /// Destination on the device (covers `(rows - 1) * dst_pitch + width` bytes).
        dst: DevBuf,
        /// Bytes from one destination row to the next.
        dst_pitch: u32,
        /// Source in the host arena (covers `(rows - 1) * src_pitch + width` bytes).
        src: HostBuf,
        /// Bytes from one source row to the next.
        src_pitch: u32,
        /// Bytes per row.
        width: u32,
        /// Rows.
        rows: u32,
    },
    /// Copy device to host. Lengths must be equal.
    Download {
        /// Destination in the host arena.
        dst: HostBuf,
        /// Source on the device.
        src: DevBuf,
    },
    /// Copy device to device inside the arena. Lengths must be equal; the regions must not
    /// overlap.
    Copy {
        /// Destination on the device.
        dst: DevBuf,
        /// Source on the device.
        src: DevBuf,
    },
    /// Copy records that hold device addresses host to device. The host region holds them in
    /// handle form ([`crate::kernel::ConvJobRef`], [`crate::kernel::PreJobRef`]); the adapter
    /// turns every handle into an address ([`RefKind::resolve`]) and uploads the result. It may
    /// do so in place: after the submit the contents of `src` are unspecified, and the core
    /// writes the records again before it names the region in another plan. A handle that names
    /// an unbound foreign slot or lies outside its space makes the whole plan a `BadPlan`.
    UploadRefs {
        /// Destination on the device.
        dst: DevBuf,
        /// Source in the host arena: a whole number of records.
        src: HostBuf,
        /// Which records.
        kind: RefKind,
    },
    /// Launch a kernel.
    Launch(Launch),
    /// Mark this point of the stream.
    Record(EventId),
    /// Do not run what follows on this stream before the event's marked point has been reached
    /// on the stream that recorded it.
    WaitFor(EventId),
}

/// An operation and the stream it is queued on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Step {
    /// The stream.
    pub stream: StreamId,
    /// The operation.
    pub op: Op,
}

/// The operations of one phase, in queue order, and the stream whose completion ends the phase.
///
/// The core owns one `Plan` per driver loop and clears it before each phase, so the step vector
/// keeps its capacity.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    /// Operations in the order they are queued.
    pub steps: Vec<Step>,
    /// The phase is complete when everything queued on this stream has run.
    pub wait: StreamId,
}

impl Default for Plan {
    fn default() -> Self {
        Self {
            steps: Vec::new(),
            wait: StreamId::MAIN,
        }
    }
}

impl Plan {
    /// An empty plan that waits on [`StreamId::MAIN`].
    pub fn new() -> Self {
        Self::default()
    }
    /// Remove all steps, keep the capacity.
    pub fn clear(&mut self) {
        self.steps.clear();
        self.wait = StreamId::MAIN;
    }
    /// Queue an operation on a stream.
    pub fn push(&mut self, stream: StreamId, op: Op) {
        self.steps.push(Step { stream, op });
    }
    /// Queue an operation on [`StreamId::MAIN`].
    pub fn main(&mut self, op: Op) {
        self.push(StreamId::MAIN, op);
    }
    /// Check everything an adapter must be able to rely on: regions inside the arenas, equal
    /// lengths of copies, stream and event numbers in range, launch dimensions in range.
    pub fn validate(&self, arenas: crate::ArenaSizes) -> Result<(), crate::DeviceError> {
        let bad = |step, what| Err(crate::DeviceError::BadPlan { step, what });
        if usize::from(self.wait.0) >= StreamId::COUNT {
            return bad(0, "wait stream out of range");
        }
        for (i, s) in self.steps.iter().enumerate() {
            if usize::from(s.stream.0) >= StreamId::COUNT {
                return bad(i, "stream out of range");
            }
            match &s.op {
                Op::Fill { dst, .. } => {
                    if !dst.fits(arenas.device) {
                        return bad(i, "fill outside the device arena");
                    }
                }
                Op::Upload { dst, src } => {
                    if dst.len != src.len {
                        return bad(i, "upload lengths differ");
                    }
                    if !dst.fits(arenas.device) || !src.fits(arenas.host) {
                        return bad(i, "upload outside an arena");
                    }
                }
                Op::Copy { dst, src } => {
                    if dst.len != src.len {
                        return bad(i, "copy lengths differ");
                    }
                    if !dst.fits(arenas.device) || !src.fits(arenas.device) {
                        return bad(i, "copy outside the device arena");
                    }
                    if dst.off < src.end() && src.off < dst.end() && dst.len > 0 {
                        return bad(i, "copy regions overlap");
                    }
                }
                Op::UploadRefs { dst, src, kind } => {
                    if dst.len != src.len || src.len % kind.record() as u64 != 0 {
                        return bad(i, "handle upload lengths");
                    }
                    if !dst.fits(arenas.device) || !src.fits(arenas.host) {
                        return bad(i, "handle upload outside an arena");
                    }
                }
                Op::Upload2d {
                    dst,
                    dst_pitch,
                    src,
                    src_pitch,
                    width,
                    rows,
                } => {
                    if *rows == 0 || *width == 0 || width > dst_pitch || width > src_pitch {
                        return bad(i, "2-D upload geometry");
                    }
                    let need =
                        |pitch: u32| u64::from(*rows - 1) * u64::from(pitch) + u64::from(*width);
                    if dst.len < need(*dst_pitch) || src.len < need(*src_pitch) {
                        return bad(i, "2-D upload region too small");
                    }
                    if !dst.fits(arenas.device) || !src.fits(arenas.host) {
                        return bad(i, "2-D upload outside an arena");
                    }
                }
                Op::Download { dst, src } => {
                    if dst.len != src.len {
                        return bad(i, "download lengths differ");
                    }
                    if !src.fits(arenas.device) || !dst.fits(arenas.host) {
                        return bad(i, "download outside an arena");
                    }
                }
                Op::Launch(l) => {
                    if l.grid == 0 || l.block == 0 || l.block > 1024 {
                        return bad(i, "launch dimensions");
                    }
                    if !l.kernel.bufs_fit(arenas.device) {
                        return bad(i, "kernel argument outside the device arena");
                    }
                }
                Op::Record(e) | Op::WaitFor(e) => {
                    if usize::from(e.0) >= EventId::COUNT {
                        return bad(i, "event out of range");
                    }
                }
            }
        }
        Ok(())
    }
}
