//! What the pipeline needs of a device beyond the tick loop: foreign regions for decoder
//! surfaces, a decoder device in the adapter's context, the card's memory in use.
//!
//! The one place of this crate with `unsafe`: it hands raw device addresses of decoder
//! surfaces to the CUDA adapter and the adapter's context to libav.
#![allow(
    unsafe_code,
    reason = "the edge between the decoder's surfaces and the CUDA adapter"
)]

use std::sync::Arc;

use tb_decode::{DecDevice, DecodeError, Frame};
use tb_device_cuda::CudaDevice;
use tb_device_ref::RefDevice;
use tb_engine::device::TickDevice;

/// A device the pipeline can run on.
pub trait PipeDevice: TickDevice {
    /// Bind the planes of decoder surfaces to foreign slots 0, 1, ... in the order given
    /// (replacing every earlier binding). `frames` are the frames the planes belong to: the
    /// caller keeps them alive until every plan submitted before the next call has completed.
    fn bind_surfaces(&mut self, planes: &[(u64, u64)], frames: &[Arc<Frame>]);
    /// A decoder device in this device's context (`shared`) or in one of libav's own;
    /// `Ok(None)` on a device that has no decoder.
    fn decoder(&mut self, shared: bool, ordinal: u32) -> Result<Option<DecDevice>, DecodeError>;
    /// Megabytes of the card's memory in use (all processes), if the device can tell.
    fn mem_used_mb(&self) -> Option<f64>;
}

impl PipeDevice for CudaDevice {
    fn bind_surfaces(&mut self, planes: &[(u64, u64)], frames: &[Arc<Frame>]) {
        self.unbind_foreign();
        debug_assert!(frames.iter().all(|f| f.holds_device_frame()) || planes.is_empty());
        for (slot, &(base, len)) in planes.iter().enumerate() {
            let Ok(slot) = u16::try_from(slot) else {
                break;
            };
            // SAFETY: `base .. base + len` is a plane of a decoder surface of one of `frames`
            // (the caller derives the pairs from `Surface::Cuda` of exactly these frames),
            // device memory of this adapter's context because the decoders were opened on
            // `decoder(true, ..)`. The frames own their surfaces, and the caller holds them
            // until the plans that name the slots have completed, as the trait requires.
            unsafe { self.bind_foreign(slot, base, len) };
        }
    }

    fn decoder(&mut self, shared: bool, ordinal: u32) -> Result<Option<DecDevice>, DecodeError> {
        if !shared {
            return DecDevice::own(ordinal).map(Some);
        }
        let ctx = self.context();
        // A stream of its own for libav's copies, so that they neither wait for nor hold up
        // the adapter's streams; without one libav uses the context's default stream.
        let stream = self.aux_stream().unwrap_or(std::ptr::null_mut());
        // SAFETY: `ctx` is the adapter's context and `stream` a stream of it (or null); both
        // live as long as the adapter. The pipeline drops every input and frame before the
        // adapter (`run` owns them in that order).
        unsafe { DecDevice::wrap(ctx, stream) }.map(Some)
    }

    fn mem_used_mb(&self) -> Option<f64> {
        self.mem_info()
            .map(|(free, total)| (total - free) as f64 / 1_048_576.0)
    }
}

impl PipeDevice for RefDevice {
    fn bind_surfaces(&mut self, _planes: &[(u64, u64)], _frames: &[Arc<Frame>]) {}

    fn decoder(&mut self, _shared: bool, _ordinal: u32) -> Result<Option<DecDevice>, DecodeError> {
        Ok(None)
    }

    fn mem_used_mb(&self) -> Option<f64> {
        None
    }
}
