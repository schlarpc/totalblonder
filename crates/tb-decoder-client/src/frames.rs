//! Decoded frames as the caller holds them: a validated descriptor and the slot of shared
//! memory it lives in, given back to the worker when the frame is dropped.

use std::sync::{Arc, Mutex, PoisonError};

use rustix::fd::{AsFd, OwnedFd};
use tb_decoder_proto::pool::{ValidSlot, plane_shapes};
use tb_media::{AudioFrameDesc, VideoFrameDesc};
use tb_sandbox::shm::Mapping;

/// Slots given back by dropped frames (from any thread), collected by the decoder's next
/// call. The eventfd wakes a thread waiting in [`crate::Decoder::poll`] or on its descriptor.
#[derive(Debug)]
pub(crate) struct ReleaseQueue {
    slots: Mutex<Vec<(u64, ValidSlot)>>,
    wake: OwnedFd,
}

impl ReleaseQueue {
    pub(crate) fn new(wake: OwnedFd) -> Arc<Self> {
        Arc::new(Self {
            slots: Mutex::new(Vec::new()),
            wake,
        })
    }

    pub(crate) fn wake_fd(&self) -> rustix::fd::BorrowedFd<'_> {
        self.wake.as_fd()
    }

    fn push(&self, generation: u64, slot: ValidSlot) {
        self.slots
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((generation, slot));
        let _ = rustix::io::write(&self.wake, &1u64.to_ne_bytes());
    }

    pub(crate) fn drain(&self) -> Vec<(u64, ValidSlot)> {
        let mut b = [0u8; 8];
        let _ = rustix::io::read(&self.wake, &mut b);
        std::mem::take(&mut *self.slots.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

/// A slot the caller holds. Dropping it gives the slot back.
#[derive(Debug)]
struct SlotGuard {
    map: Arc<Mapping>,
    slot: Option<ValidSlot>,
    generation: u64,
    queue: Arc<ReleaseQueue>,
}

impl SlotGuard {
    fn bytes(&self, off: u64, n: u64) -> Option<&[u8]> {
        let s = self.slot.as_ref()?;
        if off.checked_add(n)? > s.len() {
            return None;
        }
        self.map.slice(
            usize::try_from(s.offset() + off).ok()?,
            usize::try_from(n).ok()?,
        )
    }

    fn whole(&self) -> &[u8] {
        self.slot
            .as_ref()
            .and_then(|s| self.bytes(0, s.len()))
            .unwrap_or_default()
    }
}

impl Drop for SlotGuard {
    fn drop(&mut self) {
        if let Some(s) = self.slot.take() {
            self.queue.push(self.generation, s);
        }
    }
}

/// A decoded picture. The pixels are in shared memory and stay valid (and unchanged by a
/// well-behaved worker) until the frame is dropped, even if the worker dies meanwhile.
#[derive(Debug)]
pub struct VideoFrame {
    /// The validated descriptor (a copy in this process).
    pub desc: VideoFrameDesc,
    slot: SlotGuard,
}

impl VideoFrame {
    /// The bytes of plane `i`: from its first row to the end of its last row (`stride` per
    /// row, the last row only as long as the plane's rows).
    pub fn plane(&self, i: usize) -> Option<&[u8]> {
        let p = self.desc.planes.get(i)?;
        let (row, rows) =
            *plane_shapes(self.desc.format, self.desc.width, self.desc.height).get(i)?;
        let n = u64::from(p.stride) * (rows - 1) + row;
        self.slot.bytes(u64::from(p.offset), n)
    }

    /// Row `r` of plane `i`.
    pub fn row(&self, i: usize, r: usize) -> Option<&[u8]> {
        let p = self.desc.planes.get(i)?;
        let (row, rows) =
            *plane_shapes(self.desc.format, self.desc.width, self.desc.height).get(i)?;
        if r as u64 >= rows {
            return None;
        }
        self.slot
            .bytes(u64::from(p.offset) + r as u64 * u64::from(p.stride), row)
    }

    /// The whole slot (planes at their offsets).
    pub fn slot_bytes(&self) -> &[u8] {
        self.slot.whole()
    }
}

/// Decoded audio: `channels` planes of `samples` f32 values, contiguous.
#[derive(Debug)]
pub struct AudioFrame {
    /// The validated descriptor.
    pub desc: AudioFrameDesc,
    slot: SlotGuard,
}

impl AudioFrame {
    /// Channel `c` as little-endian f32 bytes.
    pub fn channel_bytes(&self, c: usize) -> Option<&[u8]> {
        if c >= usize::from(self.desc.channels) {
            return None;
        }
        let n = u64::from(self.desc.samples) * 4;
        self.slot.bytes(c as u64 * n, n)
    }

    /// Channel `c` as samples (copied).
    pub fn channel(&self, c: usize) -> Option<Vec<f32>> {
        Some(
            self.channel_bytes(c)?
                .as_chunks::<4>()
                .0
                .iter()
                .map(|b| f32::from_le_bytes(*b))
                .collect(),
        )
    }
}

/// A decoded frame.
#[derive(Debug)]
pub enum Decoded {
    /// A picture.
    Video(VideoFrame),
    /// A block of audio.
    Audio(AudioFrame),
}

impl Decoded {
    /// The stream it belongs to.
    pub fn stream(&self) -> tb_media::StreamIndex {
        match self {
            Self::Video(v) => v.desc.stream,
            Self::Audio(a) => a.desc.stream,
        }
    }
}

pub(crate) fn video(
    desc: VideoFrameDesc,
    map: Arc<Mapping>,
    slot: ValidSlot,
    generation: u64,
    queue: Arc<ReleaseQueue>,
) -> Decoded {
    Decoded::Video(VideoFrame {
        desc,
        slot: SlotGuard {
            map,
            slot: Some(slot),
            generation,
            queue,
        },
    })
}

pub(crate) fn audio(
    desc: AudioFrameDesc,
    map: Arc<Mapping>,
    slot: ValidSlot,
    generation: u64,
    queue: Arc<ReleaseQueue>,
) -> Decoded {
    Decoded::Audio(AudioFrame {
        desc,
        slot: SlotGuard {
            map,
            slot: Some(slot),
            generation,
            queue,
        },
    })
}
