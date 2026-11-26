//! A small, complete use of the protocol that every adapter must execute with the same bytes:
//! the pyramid kernel (`k_pyr`) on two sessions. It is the proof that the plumbing works end to
//! end (kernel source -> image or host library -> adapter -> plan -> bytes back), and the
//! template for adapter tests.

use tb_kernel_abi::{SL_CUR, SL_ZERO, SessPar};

use crate::kernel::{Kernel, Launch, Pyr};
use crate::mem::{DevBuf, HostBuf};
use crate::plan::{EventId, Op, Plan, StreamId};
use crate::{ArenaSizes, Completion, Device, DeviceError};

/// Luma width of the case (a multiple of 16).
pub const W: usize = 64;
/// Luma height of the case (a multiple of 4).
pub const H: usize = 32;
/// Session slots of the case.
pub const N: usize = 2;
/// Ring slots of the case.
pub const SLOTS: usize = 2;
const YSZ: usize = W * H;
/// Threads per block, as the C++ `LAUNCH` macro uses.
const BS: u32 = 256;

/// Arena sizes the case needs.
pub const ARENAS: ArenaSizes = ArenaSizes {
    device: 16 * 1024,
    host: 16 * 1024,
};

// Device layout: 256-byte alignment with room on both sides of every buffer, as the batches of
// the encoder have (packed loads may touch a few bytes outside a plane).
const D_SRC: DevBuf = DevBuf::new(256, (SLOTS * N * YSZ) as u64);
const D_D1: DevBuf = DevBuf::new(8704, (N * YSZ / 4) as u64);
const D_D2: DevBuf = DevBuf::new(9984, (N * YSZ / 16) as u64);
const D_LIST: DevBuf = DevBuf::new(10496, (N * 4) as u64);
const D_PAR: DevBuf = DevBuf::new(10752, (N * size_of::<SessPar>()) as u64);
// Host layout.
const H_SRC: HostBuf = HostBuf::new(0, D_SRC.len);
const H_LIST: HostBuf = HostBuf::new(8192, D_LIST.len);
const H_PAR: HostBuf = HostBuf::new(8256, D_PAR.len);
const H_D1: HostBuf = HostBuf::new(8320, D_D1.len);
const H_D2: HostBuf = HostBuf::new(9344, D_D2.len);

/// The source slot of each session (the kernel reads frame `(slot[s], s)` of the ring).
const SLOT_OF: [u8; N] = [1, 0];
/// The sessions listed for the launch, in list order.
const LIST: [i32; N] = [1, 0];

/// The source planes: `[SLOTS][N]` planes of `W x H`, a fixed pseudo-random pattern.
pub fn source() -> Vec<u8> {
    let mut x = 0x2545_F491_4F6C_DD1Du64;
    (0..SLOTS * N * YSZ)
        .map(|_| {
            x = x
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (x >> 56) as u8
        })
        .collect()
}

/// What the kernel must produce, computed here from its definition (2:1 box decimation with
/// rounding, twice): the half-resolution planes `[N]` and the quarter-resolution planes `[N]`.
pub fn expected(src: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let down = |p: &[u8], w: usize, h: usize| -> Vec<u8> {
        let mut o = Vec::with_capacity(w * h / 4);
        for y in 0..h / 2 {
            for x in 0..w / 2 {
                let s = |dx: usize, dy: usize| u32::from(p[(2 * y + dy) * w + 2 * x + dx]);
                o.push(((s(0, 0) + s(1, 0) + s(0, 1) + s(1, 1) + 2) >> 2) as u8);
            }
        }
        o
    };
    let (mut d1, mut d2) = (Vec::new(), Vec::new());
    for (s, slot) in SLOT_OF.iter().enumerate() {
        let at = (usize::from(*slot) * N + s) * YSZ;
        let half = down(&src[at..at + YSZ], W, H);
        d2.extend(down(&half, W / 2, H / 2));
        d1.extend(half);
    }
    (d1, d2)
}

/// Write the case's inputs into the host arena.
pub fn fill_host(host: &mut [u8]) -> Result<(), DeviceError> {
    let short = DeviceError::BadPlan {
        step: 0,
        what: "host arena smaller than selftest::ARENAS",
    };
    H_SRC
        .of_mut(host)
        .ok_or(short.clone())?
        .copy_from_slice(&source());
    H_LIST
        .of_mut(host)
        .ok_or(short.clone())?
        .copy_from_slice(bytemuck::cast_slice(&LIST));
    let par: [SessPar; N] = SLOT_OF.map(|cur| SessPar {
        cur,
        ..SessPar::default()
    });
    H_PAR
        .of_mut(host)
        .ok_or(short)?
        .copy_from_slice(bytemuck::cast_slice(&par));
    Ok(())
}

/// The plan of the case: clear, upload (parameters on the main stream, the picture on the
/// upload stream behind the parameter event, as the encoder orders them), launch, download.
pub fn plan() -> Plan {
    let mut p = Plan::new();
    p.main(Op::Fill {
        dst: DevBuf::new(0, ARENAS.device),
        byte: 0,
    });
    p.main(Op::Upload {
        dst: D_LIST,
        src: H_LIST,
    });
    p.main(Op::Upload {
        dst: D_PAR,
        src: H_PAR,
    });
    p.main(Op::Record(EventId::PARAMS));
    p.push(StreamId::UPLOAD, Op::WaitFor(EventId::PARAMS));
    p.push(
        StreamId::UPLOAD,
        Op::Upload {
            dst: D_SRC,
            src: H_SRC,
        },
    );
    p.push(StreamId::UPLOAD, Op::Record(EventId::FRAMES));
    p.main(Op::WaitFor(EventId::FRAMES));
    let pyr = Pyr {
        src: D_SRC,
        sstride: YSZ as u64,
        sw: W as i32,
        sh: H as i32,
        d1: D_D1,
        d1stride: (YSZ / 4) as u64,
        d2: D_D2,
        d2stride: (YSZ / 16) as u64,
        n: N as i32,
        list: D_LIST,
        na: N as i32,
        par: D_PAR,
        ssel: SL_CUR,
        dsel: SL_ZERO,
    };
    // One thread per four pixels of the quarter plane: ysz / 64 per session.
    p.main(Op::Launch(Launch::covering(
        Kernel::Pyr(pyr),
        (N * YSZ / 64) as u64,
        BS,
    )));
    p.main(Op::Download {
        dst: H_D1,
        src: D_D1,
    });
    p.main(Op::Download {
        dst: H_D2,
        src: D_D2,
    });
    p
}

/// The bytes a run produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PyrOutput {
    /// Half-resolution planes.
    pub d1: Vec<u8>,
    /// Quarter-resolution planes.
    pub d2: Vec<u8>,
    /// The completion the adapter reported.
    pub completion: Completion,
}

/// Run the case on a device: write inputs, submit, wait, read outputs.
pub fn run(dev: &mut impl Device) -> Result<PyrOutput, DeviceError> {
    fill_host(dev.host_mut())?;
    let ticket = dev.submit(&plan())?;
    let completion = dev.wait(ticket, None);
    completion.status.clone()?;
    let short = DeviceError::BadPlan {
        step: 0,
        what: "host arena smaller than selftest::ARENAS",
    };
    Ok(PyrOutput {
        d1: H_D1.of(dev.host()).ok_or(short.clone())?.to_vec(),
        d2: H_D2.of(dev.host()).ok_or(short)?.to_vec(),
        completion,
    })
}

/// FNV-1a (64 bit) of the two outputs: one number to compare across machines.
pub fn digest(out: &PyrOutput) -> u64 {
    out.d1
        .iter()
        .chain(&out.d2)
        .fold(0xcbf2_9ce4_8422_2325, |h, b| {
            (h ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3)
        })
}

/// Digest of [`expected`] for [`source`]: what every adapter must report.
pub const EXPECTED_DIGEST: u64 = 0x5cf4_b0ba_c2eb_e6e9;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_is_valid() {
        plan().validate(ARENAS).unwrap();
    }

    #[test]
    fn expected_digest_is_pinned() {
        let (d1, d2) = expected(&source());
        let c = Completion {
            ticket: crate::Ticket(0),
            status: Ok(()),
            submitted: crate::Nanos(0),
            finished: crate::Nanos(0),
        };
        assert_eq!(
            digest(&PyrOutput {
                d1,
                d2,
                completion: c
            }),
            EXPECTED_DIGEST
        );
    }
}
