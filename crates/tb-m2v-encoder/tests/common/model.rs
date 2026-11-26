//! A model device for the tests the golden vectors do not reach: it executes plans on two
//! byte arrays with toy kernels whose results depend only on what the encoder uploaded
//! (parameters, lists, row quantizers, frames), so that runs are deterministic and a session's
//! output does not depend on its slot or on its neighbors. It takes the simulator's faults
//! ([`tb_device::sim::Fault`]) by submission index.
//!
//! The toy kernels (everything else is a no-op):
//! - `k_set_mode` writes the mode of every macroblock and clears its cost; `k_me` writes
//!   forward mode with cost 1; `k_mvsmooth` copies;
//! - `k_mbstat` reads the first luma sample `c` of the session's current frame (through
//!   `SessPar::cur`) into the activity buffer: the "content" of the picture;
//! - `k_rowstat` writes row statistics from `c` and the mode;
//! - `k_quant` computes the picture's size from `c`, the mean row quantizer, `SessPar::keep`
//!   and whether the picture is all-skipped (forward, cost 0), and the entropy kernels pass it
//!   on to `PicStat`; `c == 255` is a picture that never shrinks unless all-skipped;
//! - `k_ent_scatter` writes a pattern of the reserved length.
//!
//! Time: a plan takes 20 µs plus 4 ns per launched thread.

use tb_device::kernel::Kernel;
use tb_device::sim::{Fault, HostGuard};
use tb_device::{
    ArenaSizes, Completion, DevBuf, Device, DeviceError, Nanos, Op, Plan, StreamId, Ticket,
};
use tb_kernel_abi::{MODE_FWD, MODE_INTRA, MbInfo, PIC_I, PicStat, RowStat, SessPar};
use tb_m2v_syntax::tables::QSCALE;

/// Size of the picture the model never shrinks (content 255), bytes.
pub const HUGE: u32 = 30_000;

/// A plan in flight: ticket, time left, result, the bytes its downloads will write.
type Flying = (
    u64,
    u64,
    Result<(), DeviceError>,
    Vec<(tb_device::HostBuf, Vec<u8>)>,
);

#[derive(Debug)]
pub struct Model {
    arenas: ArenaSizes,
    pub mem: Vec<u8>,
    host: Vec<u8>,
    clock: Nanos,
    pub faults: Vec<Fault>,
    /// Plans submitted so far.
    pub submits: usize,
    /// After a `Lost` fault every plan is lost.
    pub lost: bool,
    /// (ticket, time left, result, downloads to write)
    flying: Vec<Flying>,
    /// The host-arena rule (a violation is a bug of the core: the test fails).
    guard: HostGuard,
}

const INFO: usize = size_of::<MbInfo>();
const MODE_AT: usize = 16;
const COST_AT: usize = 20;

impl Model {
    pub fn new(arenas: ArenaSizes) -> Self {
        Self {
            arenas,
            // not zero: a batch must clear its block itself
            mem: vec![0xA5; arenas.device as usize],
            host: vec![0; arenas.host as usize],
            clock: Nanos(0),
            faults: Vec::new(),
            submits: 0,
            lost: false,
            flying: Vec::new(),
            guard: HostGuard::new(),
        }
    }

    /// Let time pass up to `t` (the driver sleeping until the next tick is due).
    pub fn advance_to(&mut self, t: Nanos) {
        self.clock = self.clock.max(t);
    }

    fn i32s(&self, list: DevBuf, n: i32) -> Vec<usize> {
        (0..n.max(0) as usize)
            .map(|k| {
                let o = list.off as usize + 4 * k;
                i32::from_le_bytes(self.mem[o..o + 4].try_into().unwrap()) as usize
            })
            .collect()
    }

    fn par(&self, par: DevBuf, sidx: usize) -> SessPar {
        let o = par.off as usize + sidx * size_of::<SessPar>();
        bytemuck::pod_read_unaligned(&self.mem[o..o + size_of::<SessPar>()])
    }

    fn u32_at(&self, o: usize) -> u32 {
        u32::from_le_bytes(self.mem[o..o + 4].try_into().unwrap())
    }

    fn put_u32(&mut self, o: usize, v: u32) {
        self.mem[o..o + 4].copy_from_slice(&v.to_le_bytes());
    }

    fn launch(&mut self, k: &Kernel) {
        match *k {
            Kernel::SetMode(k) => {
                for s in self.i32s(k.list, k.na) {
                    for mb in 0..k.nmb as usize {
                        let o = k.info.off as usize + (s * k.nmb as usize + mb) * INFO;
                        self.mem[o + MODE_AT] = k.mode as u8;
                        self.put_u32(o + COST_AT, 0);
                    }
                }
            }
            Kernel::Me(k) => {
                let nmb = k.g.nmb as usize;
                for s in self.i32s(k.list, k.na) {
                    for mb in 0..nmb {
                        let o = k.out.off as usize + (s * nmb + mb) * INFO;
                        self.mem[o + MODE_AT] = MODE_FWD;
                        self.put_u32(o + COST_AT, 1);
                    }
                }
            }
            Kernel::MvSmooth(k) => {
                let len = k.g.nmb as usize * INFO;
                for s in self.i32s(k.list, k.na) {
                    let (i, o) = (k.input.off as usize + s * len, k.out.off as usize + s * len);
                    self.mem.copy_within(i..i + len, o);
                }
            }
            Kernel::MbStat(k) => {
                for s in self.i32s(k.list, k.na) {
                    let p = self.par(k.par, s);
                    let frame = (p.cur as usize * k.g.n as usize + s) * k.ring_stride as usize;
                    let c = self.mem[k.ring_y.off as usize + frame];
                    let o = k.act.off as usize + s * k.g.nmb as usize * 2;
                    self.mem[o..o + 2].copy_from_slice(&u16::from(c).to_le_bytes());
                }
            }
            Kernel::RowStat(k) => {
                let (nmb, mbh, mbw) = (k.g.nmb as usize, k.g.mbh as usize, k.g.mbw as u32);
                for s in self.i32s(k.list, k.na) {
                    let c = u32::from(self.mem[k.act.off as usize + s * nmb * 2]);
                    let intra =
                        self.mem[k.info.off as usize + s * nmb * INFO + MODE_AT] == MODE_INTRA;
                    for y in 0..mbh {
                        let mut r = RowStat {
                            act_sum: c * mbw,
                            n_intra: if intra { mbw } else { 0 },
                            ..RowStat::default()
                        };
                        for (i, nz) in r.nz.iter_mut().enumerate() {
                            *nz = c * mbw * (16 - i as u32) / 8 * if intra { 3 } else { 1 };
                        }
                        let o = k.out.off as usize + (s * mbh + y) * size_of::<RowStat>();
                        self.mem[o..o + size_of::<RowStat>()]
                            .copy_from_slice(bytemuck::bytes_of(&r));
                    }
                }
            }
            Kernel::Quant(k) => {
                let (nmb, mbh) = (k.g.nmb as usize, k.g.mbh as usize);
                for s in self.i32s(k.list, k.na) {
                    let p = self.par(k.par, s);
                    let c = u64::from(self.mem[k.act.off as usize + s * nmb * 2]);
                    let io = k.info.off as usize + s * nmb * INFO;
                    let skipped =
                        self.mem[io + MODE_AT] == MODE_FWD && self.u32_at(io + COST_AT) == 0;
                    let q: u64 = (0..mbh)
                        .map(|y| {
                            u64::from(
                                QSCALE
                                    [usize::from(self.mem[k.rowq.off as usize + s * mbh + y] & 31)],
                            )
                        })
                        .sum::<u64>()
                        .max(1);
                    let bytes = if skipped {
                        mbh as u64 * 6
                    } else if c == 255 {
                        u64::from(HUGE)
                    } else {
                        let full = c * nmb as u64 * 64 * mbh as u64 / q;
                        let full = if p.pic_type == PIC_I { 3 * full } else { full };
                        full * (u64::from(p.keep) + 1) / 65 + mbh as u64 * 8
                    };
                    let o = k.mbq.off as usize + s * nmb;
                    self.put_u32(o, bytes as u32);
                    self.put_u32(o + 4, (bytes * 6) as u32);
                    self.mem[o + 8] = c as u8;
                }
            }
            Kernel::EntPack(k) => {
                let nmb = k.g.nmb as usize;
                for s in self.i32s(k.list, k.na) {
                    let (i, o) = (
                        k.mbq.off as usize + s * nmb,
                        k.mbbits.off as usize + s * nmb * 4,
                    );
                    self.mem.copy_within(i..i + 9, o);
                }
            }
            Kernel::EntRowSum(k) => {
                let (nmb, mbh) = (k.g.nmb as usize, k.g.mbh as usize);
                for s in self.i32s(k.list, k.na) {
                    let i = k.mbbits.off as usize + s * nmb * 4;
                    let (b, t) = (self.u32_at(i), self.u32_at(i + 4));
                    self.put_u32(k.slice_bytes.off as usize + s * mbh * 4, b);
                    self.put_u32(k.slice_tex.off as usize + s * mbh * 4, t);
                }
            }
            Kernel::EntPicSum(k) => {
                let mbh = k.g.mbh as usize;
                for s in self.i32s(k.list, k.na) {
                    let pic = PicStat {
                        bytes: self.u32_at(k.slice_bytes.off as usize + s * mbh * 4),
                        tex_bits: self.u32_at(k.slice_tex.off as usize + s * mbh * 4),
                        ..PicStat::default()
                    };
                    let o = k.pic.off as usize + s * size_of::<PicStat>();
                    self.mem[o..o + size_of::<PicStat>()].copy_from_slice(bytemuck::bytes_of(&pic));
                }
            }
            Kernel::EntScatter(k) => {
                let nmb = k.g.nmb as usize;
                for s in self.i32s(k.list, k.na) {
                    let i = k.mbbits.off as usize + s * nmb * 4;
                    let (bytes, c) = (self.u32_at(i), self.mem[i + 8]);
                    let first = self.u32_at(k.outpos.off as usize + 8 * s) as usize;
                    let words = self.u32_at(k.outpos.off as usize + 8 * s + 4) as usize;
                    let o = k.out.off as usize + 4 * first;
                    for (j, b) in self.mem[o..o + 4 * (words - 2)].iter_mut().enumerate() {
                        // never a start code: no zero bytes
                        *b = ((bytes as usize * 31 + usize::from(c) * 7 + j) % 251) as u8 + 2;
                    }
                }
            }
            _ => {}
        }
    }

    /// Execute a plan; the bytes its downloads produce.
    fn execute(&mut self, plan: &Plan) -> Vec<(tb_device::HostBuf, Vec<u8>)> {
        let mut down = Vec::new();
        for s in &plan.steps {
            match &s.op {
                Op::Fill { dst, byte } => {
                    self.mem[dst.off as usize..dst.end() as usize].fill(*byte);
                }
                Op::Copy { dst, src } => {
                    self.mem
                        .copy_within(src.off as usize..src.end() as usize, dst.off as usize);
                }
                Op::Upload { dst, src } | Op::UploadRefs { dst, src, .. } => {
                    let src = src.of(&self.host).unwrap().to_vec();
                    self.mem[dst.off as usize..dst.end() as usize].copy_from_slice(&src);
                }
                Op::Upload2d {
                    dst,
                    dst_pitch,
                    src,
                    src_pitch,
                    width,
                    rows,
                } => {
                    for r in 0..*rows as usize {
                        let so = src.off as usize + r * *src_pitch as usize;
                        let d = dst.off as usize + r * *dst_pitch as usize;
                        let row = self.host[so..so + *width as usize].to_vec();
                        self.mem[d..d + *width as usize].copy_from_slice(&row);
                    }
                }
                Op::Download { dst, src } => {
                    down.push((
                        *dst,
                        self.mem[src.off as usize..src.end() as usize].to_vec(),
                    ));
                }
                Op::Launch(l) => self.launch(&l.kernel),
                Op::Record(_) | Op::WaitFor(_) => {}
            }
        }
        down
    }
}

/// Time a plan takes on the model.
pub fn cost(plan: &Plan) -> u64 {
    20_000
        + plan
            .steps
            .iter()
            .map(|s| match &s.op {
                Op::Launch(l) => 4 * u64::from(l.grid) * u64::from(l.block),
                _ => 0,
            })
            .sum::<u64>()
}

impl Device for Model {
    fn arenas(&self) -> ArenaSizes {
        self.arenas
    }
    fn host(&self) -> &[u8] {
        &self.host
    }
    fn host_mut(&mut self) -> &mut [u8] {
        &mut self.host
    }
    fn now(&self) -> Nanos {
        self.clock
    }
    fn submit(&mut self, plan: &Plan) -> Result<Ticket, DeviceError> {
        plan.validate(self.arenas)?;
        self.guard.admit(plan)?;
        assert!(
            plan.steps
                .iter()
                .all(|s| s.stream == plan.wait || s.stream == StreamId::UPLOAD),
            "a step on a stream the plan does not wait for"
        );
        let i = self.submits;
        self.submits += 1;
        let mut time = cost(plan);
        let mut result = Ok(());
        for f in &self.faults {
            match f {
                Fault::Fail { submit, error } if *submit == i => result = Err(error.clone()),
                Fault::Delay { submit, extra } if *submit == i => time += extra.0,
                _ => {}
            }
        }
        if self.lost {
            result = Err(DeviceError::Lost { code: -1 });
        }
        if matches!(result, Err(DeviceError::Lost { .. })) {
            self.lost = true;
        }
        let down = if result.is_ok() {
            self.execute(plan)
        } else {
            Vec::new()
        };
        self.flying.push((i as u64, time, result, down));
        self.guard.track(Ticket(i as u64), plan, &self.host, true);
        Ok(Ticket(i as u64))
    }
    fn wait(&mut self, ticket: Ticket, timeout: Option<Nanos>) -> Completion {
        let submitted = self.clock;
        let Some(k) = self.flying.iter().position(|f| f.0 == ticket.0) else {
            return Completion {
                ticket,
                status: Err(DeviceError::UnknownTicket),
                submitted,
                finished: self.clock,
            };
        };
        let left = self.flying[k].1;
        self.guard.check(ticket, &self.host);
        if let Some(t) = timeout
            && t.0 < left
        {
            self.clock.0 += t.0;
            self.flying[k].1 = left - t.0;
            return Completion {
                ticket,
                status: Err(DeviceError::Timeout),
                submitted,
                finished: self.clock,
            };
        }
        self.clock.0 += left;
        let (_, _, status, down) = self.flying.remove(k);
        assert_eq!(self.guard.release(ticket), None, "host-arena rule");
        for (b, bytes) in down {
            b.of_mut(&mut self.host).unwrap().copy_from_slice(&bytes);
        }
        Completion {
            ticket,
            status,
            submitted,
            finished: self.clock,
        }
    }
}
