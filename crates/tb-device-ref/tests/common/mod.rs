//! Test bench for single kernels on the reference adapter.
//!
//! A [`Bench`] lays out buffers as the C++ `Batch::init` did with arenas (each buffer in a block
//! of 256 bytes with 64 guard bytes on both sides), runs one plan (upload everything, the
//! launches in order, download everything) and returns the device arena afterwards.
//!
//! Every run is made twice, with two different bytes in the gaps between buffers, and checks
//! (a) that no gap byte changed (no kernel wrote outside its buffers), and (b) that the buffers
//! came out the same both times (no result depends on bytes outside the buffers, which a kernel
//! may read: packed loads run a few bytes past a row).
#![allow(dead_code, reason = "each test binary uses a part of the bench")]
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test support")]

use bytemuck::Pod;
use tb_device::kernel::Kernel;
use tb_device::{ArenaSizes, DevBuf, Device, DeviceError, HostBuf, Launch, Op, Plan};
use tb_device_ref::RefDevice;
use tb_kernel_abi::{Geo, Tables};

/// Guard bytes on each side of a buffer (the C++ `GUARD`).
pub const GUARD: u64 = 64;
/// Threads per block of the C++ `LAUNCH` macro (`BS`).
pub const BS: u32 = 256;
/// Threads per block of `k_me` (the C++ `ME_BS`).
pub const ME_BS: u32 = 64;
/// Threads per block of `k_recon` (the C++ `RECON_BS`).
pub const RECON_BS: u32 = 64;

/// Buffers and launches of one plan.
#[derive(Default)]
pub struct Bench {
    regions: Vec<(DevBuf, Vec<u8>)>,
    launches: Vec<Launch>,
    end: u64,
}

/// The device arena after a run.
pub struct Out {
    arena: Vec<u8>,
}

impl Out {
    /// The bytes of a buffer.
    pub fn bytes(&self, b: DevBuf) -> &[u8] {
        &self.arena[b.off as usize..b.end() as usize]
    }
    /// The records of a buffer.
    pub fn pods<T: Pod>(&self, b: DevBuf) -> Vec<T> {
        pods_of(self.bytes(b))
    }
}

impl Bench {
    pub fn new() -> Self {
        Self::default()
    }

    /// A buffer holding these bytes.
    pub fn bytes(&mut self, data: &[u8]) -> DevBuf {
        let len = data.len() as u64;
        let block = (len + 2 * GUARD + 255) & !255;
        let b = DevBuf::new(self.end + GUARD, len);
        self.end += block;
        self.regions.push((b, data.to_vec()));
        b
    }

    /// A buffer holding these records.
    pub fn pods<T: Pod>(&mut self, v: &[T]) -> DevBuf {
        self.bytes(bytemuck::cast_slice(v))
    }

    /// A buffer of `len` zero bytes.
    pub fn zeros(&mut self, len: usize) -> DevBuf {
        self.bytes(&vec![0; len])
    }

    /// A buffer of `len` bytes of one value (to see what a kernel leaves untouched).
    pub fn filled(&mut self, len: usize, byte: u8) -> DevBuf {
        self.bytes(&vec![byte; len])
    }

    /// Queue a launch of `grid` blocks of `block` threads.
    pub fn launch(&mut self, kernel: Kernel, grid: u32, block: u32) {
        self.launches.push(Launch {
            kernel,
            grid,
            block,
        });
    }

    /// Queue a launch of enough blocks of `block` threads for `count` threads (`LAUNCHB`).
    pub fn cover(&mut self, kernel: Kernel, count: usize, block: u32) {
        self.launches
            .push(Launch::covering(kernel, count as u64, block));
    }

    fn run_with(&self, gap: u8) -> Result<Vec<u8>, DeviceError> {
        let size = self.end.max(256);
        let mut dev = RefDevice::new(ArenaSizes {
            device: size,
            host: size,
        })
        .unwrap();
        let host = dev.host_mut();
        host.fill(gap);
        for (b, data) in &self.regions {
            host[b.off as usize..b.end() as usize].copy_from_slice(data);
        }
        let mut p = Plan::new();
        p.main(Op::Upload {
            dst: DevBuf::new(0, size),
            src: HostBuf::new(0, size),
        });
        for l in &self.launches {
            p.main(Op::Launch(*l));
        }
        p.main(Op::Download {
            dst: HostBuf::new(0, size),
            src: DevBuf::new(0, size),
        });
        let t = dev.submit(&p)?;
        dev.wait(t, None).status?;
        let arena = dev.host().to_vec();
        // Nothing written outside the buffers.
        let mut inside = vec![false; arena.len()];
        for (b, _) in &self.regions {
            inside[b.off as usize..b.end() as usize].fill(true);
        }
        if let Some(i) = (0..arena.len()).find(|&i| !inside[i] && arena[i] != gap) {
            panic!("a kernel wrote outside its buffers, at arena offset {i}");
        }
        Ok(arena)
    }

    /// Run the plan (twice; see the module comment) and return the arena.
    pub fn try_run(&self) -> Result<Out, DeviceError> {
        let a = self.run_with(0xa5)?;
        let b = self.run_with(0x5a)?;
        for (r, _) in &self.regions {
            let (x, y) = (r.off as usize, r.end() as usize);
            assert!(
                a[x..y] == b[x..y],
                "a result depends on bytes outside the buffers (buffer at {})",
                r.off
            );
        }
        Ok(Out { arena: a })
    }

    /// [`Bench::try_run`], which must succeed.
    pub fn run(&self) -> Out {
        self.try_run().unwrap()
    }
}

/// Records read from bytes (any alignment).
pub fn pods_of<T: Pod>(bytes: &[u8]) -> Vec<T> {
    bytes
        .chunks_exact(size_of::<T>())
        .map(bytemuck::pod_read_unaligned)
        .collect()
}

/// A deterministic byte stream (64-bit LCG, high byte).
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed ^ 0x2545_f491_4f6c_dd1d)
    }
    pub fn next_u32(&mut self) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 32) as u32
    }
    pub fn byte(&mut self) -> u8 {
        (self.next_u32() >> 24) as u8
    }
    /// A value in `0..n`.
    pub fn below(&mut self, n: u32) -> u32 {
        self.next_u32() % n
    }
    pub fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.byte()).collect()
    }
}

/// The geometry of a batch of `n` sessions of `mbw x mbh` macroblocks, the C++ defaults
/// otherwise (the C++ `Batch::init`; f_code 1 is enough for the vectors of these tests).
pub fn geo(n: i32, mbw: i32, mbh: i32) -> Geo {
    Geo {
        n,
        w: mbw * 16,
        h: mbh * 16,
        mbw,
        mbh,
        nmb: mbw * mbh,
        f_code: 2,
        intra_vlc_format: 0,
        alt_scan: 0,
        interlaced: 0,
        field_pred: 0,
        range2: 4,
        lambda: 4,
        field_bits: 4,
        aq_s: 2.0,
        smooth: 1,
        tpred: 0,
    }
}

/// The table record the encoder uploads (golden/tables.jsonl).
pub fn tables() -> Tables {
    let recs = tb_golden::case("tables").unwrap();
    let r = recs.iter().find(|r| r.kind() == "tables").unwrap();
    let bytes = r.bytes("bytes").unwrap();
    assert_eq!(bytes.len(), size_of::<Tables>());
    bytemuck::pod_read_unaligned(&bytes)
}

/// Threads per block of the row kernels (the C++ `row_bs`).
pub fn row_bs(mbw: i32) -> u32 {
    (32 * ((mbw + 31) / 32)) as u32
}
