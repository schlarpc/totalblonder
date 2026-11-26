//! Conversion planning: classes, job lists per class and tick, and the device operations that
//! carry them out (the C++ `ConvKey`, `ConvClassHost`, `PreSet`, `Converter`).
//!
//! The C++ `Converter` allocates device memory, uploads tap tables and launches kernels itself.
//! Here the same decisions come out as data: [`ConvOp`]s, in the order the C++ issues the
//! corresponding calls. Buffers are named by [`ConvBuf`] (the caller places them; an
//! [`ConvOp::Alloc`] says when one is needed and how large), launches are [`ConvLaunch`]es with
//! the kernels' arguments by name. [`crate::Lowerer`] maps them to `tb_device` kernel records.
//!
//! Usage per tick, as in the C++ pipeline harness: [`Converter::fill`] for background fills,
//! [`Converter::add`] for every (source surface, destination rectangle), [`Converter::run`],
//! then [`Converter::csc`] and [`Converter::overlay`]. `add` can emit operations (a new class's
//! buffers and taps, a larger job list): the C++ performs those synchronously inside `add`, so
//! they precede everything queued afterwards.

use std::collections::BTreeMap;

use tb_device::mem::DevBuf;

use crate::{ConvertError, Csc, Filter, Rows, TapSpec, Taps, make_taps};

/// Threads per block of every conversion kernel.
pub const BLOCK: u32 = 256;
/// Bytes of one `ConvJob` record on the device.
pub const CONV_JOB_BYTES: u64 = size_of::<tb_kernel_abi::ConvJob>() as u64;
/// Bytes of one `PreJob` record on the device.
pub const PRE_JOB_BYTES: u64 = size_of::<tb_kernel_abi::PreJob>() as u64;
/// Jobs per block of the half-size pre-stage.
pub const PRE_BLOCK_JOBS: u32 = 8;

/// A device address the caller gives the converter (a plane of a decoder surface, a plane of an
/// encoder input slot at a rectangle's origin).
pub trait DevAddr: Copy + core::fmt::Debug + PartialEq {
    /// Whether the address is a multiple of four bytes (the kernels load 32-bit words).
    fn aligned4(&self) -> bool;
}

impl DevAddr for u64 {
    fn aligned4(&self) -> bool {
        self.is_multiple_of(4)
    }
}

impl DevAddr for DevBuf {
    fn aligned4(&self) -> bool {
        self.off.is_multiple_of(4)
    }
}

/// What a conversion class is keyed by (the C++ `ConvKey`). Classes are ordered as the C++
/// orders them (the fields in this order), which is the order of their launches.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConvKey {
    /// Source frame width (luma).
    pub sw: u32,
    /// Source frame height (luma).
    pub sh: u32,
    /// Bytes per source sample: 1 (NV12), 2 (P010 / P016).
    pub bps: u32,
    /// Source crop origin x (luma, even).
    pub cx: u32,
    /// Source crop origin y (luma, even).
    pub cy: u32,
    /// Source crop width (even).
    pub cw: u32,
    /// Source crop height (even).
    pub ch: u32,
    /// Destination rectangle width (a multiple of 8).
    pub dw: u32,
    /// Destination rectangle height (even).
    pub dh: u32,
    /// Source rows used.
    pub srows: Rows,
    /// Destination rows written.
    pub drows: Rows,
    /// The filter.
    pub filter: Filter,
    /// The [1 2 1]/4 low-pass across destination lines (frame to field).
    pub weave_lp: bool,
}

/// One of the four tap tables of a class.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Axis {
    /// Luma, horizontal.
    Hy,
    /// Chroma, horizontal.
    Hc,
    /// Luma, vertical.
    Vy,
    /// Chroma, vertical.
    Vc,
}

impl Axis {
    /// The four, in the order the C++ allocates and uploads them.
    pub const ALL: [Axis; 4] = [Axis::Hy, Axis::Hc, Axis::Vy, Axis::Vc];
}

/// The tap tables of a resampling class.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClassTaps {
    /// Luma, horizontal.
    pub hy: Taps,
    /// Chroma, horizontal.
    pub hc: Taps,
    /// Luma, vertical.
    pub vy: Taps,
    /// Chroma, vertical.
    pub vc: Taps,
}

impl ClassTaps {
    /// One table.
    pub fn get(&self, axis: Axis) -> &Taps {
        match axis {
            Axis::Hy => &self.hy,
            Axis::Hc => &self.hc,
            Axis::Vy => &self.vy,
            Axis::Vc => &self.vc,
        }
    }
}

/// A conversion class: the kernels' `ConvClass` record without device addresses, and its taps
/// (the C++ `ConvClass`, as `Converter::get` fills it).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConvClass {
    /// The key.
    pub key: ConvKey,
    /// Bytes per source sample.
    pub bps: u32,
    /// Luma width of the destination rectangle.
    pub dw: u32,
    /// Rows of the intermediate luma plane: the source lattice rows (0 for a copy).
    pub th_y: u32,
    /// Rows of the intermediate chroma plane (0 for a copy).
    pub th_c: u32,
    /// Source rows used: `s_off + s_step * i`.
    pub s_step: u32,
    /// Source lattice offset.
    pub s_off: u32,
    /// Destination rows written: `d_off + d_step * k`.
    pub d_step: u32,
    /// Destination lattice offset.
    pub d_off: u32,
    /// Bytes of intermediate per job (0 for a copy).
    pub tstride: u32,
    /// Copy path: luma width (0 otherwise).
    pub copy_w: u32,
    /// Copy path: luma height (0 otherwise).
    pub copy_h: u32,
    /// Same size, all rows, no crop, no filter: `k_conv_copy`.
    pub copy: bool,
    /// The taps (`None` for a copy).
    pub taps: Option<ClassTaps>,
}

impl ConvClass {
    /// The class of a key (the body of the C++ `Converter::get`).
    pub fn new(k: ConvKey) -> Result<Self, ConvertError> {
        if !k.dw.is_multiple_of(8)
            || !k.dh.is_multiple_of(2)
            || !k.cw.is_multiple_of(2)
            || !k.ch.is_multiple_of(2)
            || !k.cx.is_multiple_of(2)
            || !k.cy.is_multiple_of(2)
        {
            return Err(ConvertError::BadRect { dw: k.dw, dh: k.dh });
        }
        let mut c = ConvClass {
            key: k,
            bps: k.bps,
            dw: k.dw,
            th_y: 0,
            th_c: 0,
            s_step: k.srows.step(),
            s_off: k.srows.off(),
            d_step: k.drows.step(),
            d_off: k.drows.off(),
            tstride: 0,
            copy_w: 0,
            copy_h: 0,
            copy: false,
            taps: None,
        };
        c.copy = k.cw == k.dw
            && k.ch == k.dh
            && k.srows == Rows::All
            && k.drows == Rows::All
            && k.cx == 0
            && k.cy == 0
            && !k.weave_lp;
        if c.copy {
            c.copy_w = k.dw;
            c.copy_h = k.dh;
            return Ok(c);
        }
        let [hy, hc, vy, vc] = Self::specs(&k);
        let taps = ClassTaps {
            hy: make_taps(&hy, true)?,
            hc: make_taps(&hc, k.bps == 1)?,
            vy: make_taps(&vy, false)?,
            vc: make_taps(&vc, false)?,
        };
        c.th_y = taps.vy.n_lat;
        c.th_c = taps.vc.n_lat;
        let ts = u64::from(c.dw) * u64::from(c.th_y) + u64::from(c.dw) * u64::from(c.th_c) + 15;
        c.tstride = u32::try_from(ts & !15).map_err(|_| ConvertError::BadSpec)?;
        c.taps = Some(taps);
        Ok(c)
    }

    /// The four axes of a resampling class (`hy`, `hc`, `vy`, `vc`), as `Converter::get` builds
    /// them.
    pub fn specs(k: &ConvKey) -> [TapSpec; 4] {
        let (s_step, s_off) = (k.srows.step(), k.srows.off());
        let (d_step, d_off) = (k.drows.step(), k.drows.off());
        let spec =
            |n_src, s_step, s_off, n_dst, d_step, d_off, crop0, crop_len, phase, lp| TapSpec {
                n_src,
                s_step,
                s_off,
                n_dst,
                d_step,
                d_off,
                crop0,
                crop_len,
                phase,
                filter: k.filter,
                weave_lp: lp,
            };
        [
            spec(
                k.sw,
                1,
                0,
                k.dw,
                1,
                0,
                f64::from(k.cx),
                f64::from(k.cw),
                0.5,
                false,
            ),
            spec(
                k.sw / 2,
                1,
                0,
                k.dw / 2,
                1,
                0,
                f64::from(k.cx) / 2.0,
                f64::from(k.cw) / 2.0,
                0.25,
                false,
            ),
            spec(
                k.sh,
                s_step,
                s_off,
                k.dh,
                d_step,
                d_off,
                f64::from(k.cy),
                f64::from(k.ch),
                0.5,
                k.weave_lp,
            ),
            spec(
                k.sh / 2,
                s_step,
                s_off,
                k.dh / 2,
                d_step,
                d_off,
                f64::from(k.cy) / 2.0,
                f64::from(k.ch) / 2.0,
                0.5,
                k.weave_lp,
            ),
        ]
    }
}

/// A conversion class, by order of creation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ClassId(pub u32);

/// A set of half-size frames of the pre-stage (one per source size and sample size), by order of
/// creation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PreId(pub u32);

/// A device buffer the converter needs; the caller places it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ConvBuf {
    /// First source sample per output of a tap table (`int32`).
    TapStart {
        /// The class.
        class: ClassId,
        /// The table.
        axis: Axis,
    },
    /// Coefficients of a tap table (`int16`).
    TapCoef {
        /// The class.
        class: ClassId,
        /// The table.
        axis: Axis,
    },
    /// A class's job records.
    Jobs(ClassId),
    /// A class's intermediate planes (`tstride` bytes per job).
    Tmp(ClassId),
    /// A block of pre-stage job records.
    PreJobs {
        /// The set.
        set: PreId,
        /// The block.
        block: u32,
    },
    /// A block of half-size frames (eight frames of `stride` bytes).
    Pre {
        /// The set.
        set: PreId,
        /// The block.
        block: u32,
    },
}

/// A plane a job reads: one the caller gave, or a half-size frame of the pre-stage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Plane<A> {
    /// A caller's address.
    Ext(A),
    /// `off` bytes into a converter buffer.
    Conv {
        /// The buffer.
        buf: ConvBuf,
        /// Byte offset.
        off: u64,
    },
}

/// A source surface: luma plane and interleaved chroma plane at the frame's origin, with their
/// pitches in bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Source<A> {
    /// Luma plane.
    pub y: A,
    /// Luma pitch.
    pub sp_y: u32,
    /// Interleaved chroma plane.
    pub uv: A,
    /// Chroma pitch.
    pub sp_uv: u32,
}

/// A destination: the three planes at the rectangle's origin and the luma pitch (the chroma
/// pitch is half of it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dest<A> {
    /// Luma.
    pub y: A,
    /// Cb.
    pub u: A,
    /// Cr.
    pub v: A,
    /// Luma pitch.
    pub pitch: u32,
}

/// One job of a class (the C++ `ConvJob`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConvJob<A> {
    /// Source luma plane.
    pub sy: Plane<A>,
    /// Source chroma plane.
    pub suv: Plane<A>,
    /// Destination luma at the rectangle's origin.
    pub dy: A,
    /// Destination Cb.
    pub du: A,
    /// Destination Cr.
    pub dv: A,
    /// Source luma pitch.
    pub sp_y: u32,
    /// Source chroma pitch.
    pub sp_uv: u32,
    /// Destination luma pitch.
    pub dp: u32,
}

/// One job of the pre-stage (the C++ `PreJob`): a source frame to halve.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PreJob<A> {
    /// Source luma.
    pub sy: A,
    /// Source chroma.
    pub suv: A,
    /// Luma pitch.
    pub sp_y: u32,
    /// Chroma pitch.
    pub sp_uv: u32,
}

/// Overlay planes of a [`RectJob`] (alpha per luma sample and per chroma sample), pitch `w` and
/// `w / 2`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OverlayPlanes<A> {
    /// Luma.
    pub oy: A,
    /// Cb.
    pub ou: A,
    /// Cr.
    pub ov: A,
    /// Alpha per luma sample.
    pub oa: A,
    /// Alpha per chroma sample.
    pub oa2: A,
}

/// A rectangle of a destination (the C++ `RectJob`): fill, overlay, color matrix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RectJob<A> {
    /// Destination planes at the rectangle's origin.
    pub dst: Dest<A>,
    /// Width (a multiple of 8).
    pub w: u32,
    /// Height (even).
    pub h: u32,
    /// Fill color: Y | U << 8 | V << 16.
    pub yuv: u32,
    /// Overlay planes (overlay only).
    pub overlay: Option<OverlayPlanes<A>>,
}

/// A kernel launch, with the kernel's arguments by name. All run with [`BLOCK`] threads per
/// block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConvLaunch<A> {
    /// `k_half(jobs, nj, w2, h2, bps, pre, stride)`.
    Half {
        /// Job records.
        jobs: ConvBuf,
        /// Jobs.
        nj: u32,
        /// Half width.
        w2: u32,
        /// Half height.
        h2: u32,
        /// Bytes per source sample.
        bps: u32,
        /// Output frames.
        pre: ConvBuf,
        /// Bytes per output frame.
        stride: u64,
        /// Blocks.
        grid: u32,
    },
    /// `k_conv_copy(c, jobs, nj)`.
    ConvCopy {
        /// The class (`c`).
        class: ClassId,
        /// Job records.
        jobs: ConvBuf,
        /// Jobs.
        nj: u32,
        /// Blocks.
        grid: u32,
    },
    /// `k_conv_h(c, jobs, nj, tmp)`.
    ConvH {
        /// The class (`c`).
        class: ClassId,
        /// Job records.
        jobs: ConvBuf,
        /// Jobs.
        nj: u32,
        /// Intermediate planes.
        tmp: ConvBuf,
        /// Blocks.
        grid: u32,
    },
    /// `k_conv_v(c, jobs, nj, tmp)`.
    ConvV {
        /// The class (`c`).
        class: ClassId,
        /// Job records.
        jobs: ConvBuf,
        /// Jobs.
        nj: u32,
        /// Intermediate planes.
        tmp: ConvBuf,
        /// Blocks.
        grid: u32,
    },
    /// `k_fill(j)`.
    Fill {
        /// The rectangle and color.
        j: RectJob<A>,
        /// Blocks.
        grid: u32,
    },
    /// `k_overlay(j)`.
    Overlay {
        /// The rectangle and overlay planes.
        j: RectJob<A>,
        /// Blocks.
        grid: u32,
    },
    /// `k_csc(j, m)`; `interlaced` is the `CscPar` record's flag.
    Csc {
        /// The rectangle.
        j: RectJob<A>,
        /// The matrix.
        m: Csc,
        /// The frame is interlaced: the luma correction uses the chroma of the same field.
        interlaced: bool,
        /// Blocks.
        grid: u32,
    },
}

impl<A> ConvLaunch<A> {
    /// The kernel's name in the CUDA sources.
    pub fn kernel(&self) -> &'static str {
        match self {
            ConvLaunch::Half { .. } => "k_half",
            ConvLaunch::ConvCopy { .. } => "k_conv_copy",
            ConvLaunch::ConvH { .. } => "k_conv_h",
            ConvLaunch::ConvV { .. } => "k_conv_v",
            ConvLaunch::Fill { .. } => "k_fill",
            ConvLaunch::Overlay { .. } => "k_overlay",
            ConvLaunch::Csc { .. } => "k_csc",
        }
    }

    /// Blocks of the launch.
    pub fn grid(&self) -> u32 {
        match self {
            ConvLaunch::Half { grid, .. }
            | ConvLaunch::ConvCopy { grid, .. }
            | ConvLaunch::ConvH { grid, .. }
            | ConvLaunch::ConvV { grid, .. }
            | ConvLaunch::Fill { grid, .. }
            | ConvLaunch::Overlay { grid, .. }
            | ConvLaunch::Csc { grid, .. } => *grid,
        }
    }

    /// Threads per block: always [`BLOCK`].
    pub fn block(&self) -> u32 {
        BLOCK
    }
}

/// One device operation of the conversion path, in issue order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConvOp<A> {
    /// Device memory for a buffer, `bytes` long. `replaces`: the buffer exists and is replaced
    /// by a larger one; the C++ waits for the stream before freeing the old one, so the old
    /// region must stay untouched until the work queued before this point has completed.
    Alloc {
        /// The buffer.
        buf: ConvBuf,
        /// Its size, slack included as the C++ allocates it.
        bytes: u64,
        /// It replaces an existing buffer of the same name.
        replaces: bool,
    },
    /// Upload a class's tap table ([`ConvClass::taps`]) into its `TapStart` (`n` `int32`) and
    /// `TapCoef` (`n * nt` `int16`) buffers. Synchronous in the C++, once per class.
    UploadTaps {
        /// The class.
        class: ClassId,
        /// The table.
        axis: Axis,
    },
    /// Upload the tick's jobs of a class into its `Jobs` buffer (`CONV_JOB_BYTES` each).
    UploadJobs {
        /// The class.
        class: ClassId,
        /// The jobs, in the order they were added.
        jobs: Vec<ConvJob<A>>,
    },
    /// Upload a block of pre-stage jobs into its `PreJobs` buffer (`PRE_JOB_BYTES` each).
    UploadPreJobs {
        /// The set.
        set: PreId,
        /// The block.
        block: u32,
        /// The jobs (at most eight).
        jobs: Vec<PreJob<A>>,
    },
    /// A kernel launch.
    Launch(ConvLaunch<A>),
}

/// A class and its per-tick state (`ConvClassHost`).
#[derive(Clone, Debug)]
struct ClassHost<A> {
    class: ConvClass,
    jobs: Vec<ConvJob<A>>,
    cap_jobs: u32,
    tmp_jobs: u32,
}

/// Half-size frames of the pre-stage for one source size (`PreSet`).
#[derive(Clone, Debug)]
struct PreSet<A> {
    w2: u32,
    h2: u32,
    bps: u32,
    stride: u64,
    blocks: u32,
    jobs: Vec<PreJob<A>>,
}

/// The converter: classes (tap tables, made when first used) and the per-tick job lists
/// (the C++ `Converter`).
#[derive(Clone, Debug)]
pub struct Converter<A> {
    /// Ratios of 3 and more in both directions: halve the source first (`k_half`).
    pub fast_down: bool,
    by_key: BTreeMap<ConvKey, ClassId>,
    classes: Vec<ClassHost<A>>,
    pres: BTreeMap<(u32, u32, u32), PreId>,
    pre_sets: Vec<PreSet<A>>,
    pre_jobs: u64,
    dev_bytes: u64,
    launches: u64,
}

impl<A: DevAddr> Default for Converter<A> {
    fn default() -> Self {
        Self::new()
    }
}

/// `(unsigned)((per * n + 255) / 256)`.
fn grid(per: u64, n: u64) -> u32 {
    (per * n).div_ceil(256) as u32
}

impl<A: DevAddr> Converter<A> {
    /// An empty converter with the pre-stage on.
    pub fn new() -> Self {
        Self {
            fast_down: true,
            by_key: BTreeMap::new(),
            classes: Vec::new(),
            pres: BTreeMap::new(),
            pre_sets: Vec::new(),
            pre_jobs: 0,
            dev_bytes: 0,
            launches: 0,
        }
    }

    /// The class of a key, if made.
    pub fn class_id(&self, key: &ConvKey) -> Option<ClassId> {
        self.by_key.get(key).copied()
    }

    /// A class.
    pub fn class(&self, id: ClassId) -> Option<&ConvClass> {
        self.classes.get(id.0 as usize).map(|h| &h.class)
    }

    /// Classes made so far.
    pub fn classes(&self) -> usize {
        self.classes.len()
    }

    /// Device bytes the C++ counts (`dev_bytes`: tap tables, intermediate planes, half-size
    /// frames).
    pub fn dev_bytes(&self) -> u64 {
        self.dev_bytes
    }

    /// Launches issued so far.
    pub fn launches(&self) -> u64 {
        self.launches
    }

    /// Jobs that went through the pre-stage so far.
    pub fn pre_jobs(&self) -> u64 {
        self.pre_jobs
    }

    /// Make a class's ops: its tap buffers and uploads (`Converter::get` / `up_taps`).
    fn install(&mut self, class: ConvClass, out: &mut Vec<ConvOp<A>>) -> ClassId {
        let id = ClassId(self.classes.len() as u32);
        if let Some(t) = &class.taps {
            for axis in Axis::ALL {
                let h = t.get(axis);
                let starts = u64::from(h.n) * 4;
                let coefs = u64::from(h.n) * u64::from(h.nt) * 2;
                out.push(ConvOp::Alloc {
                    buf: ConvBuf::TapStart { class: id, axis },
                    bytes: starts + 64,
                    replaces: false,
                });
                out.push(ConvOp::Alloc {
                    buf: ConvBuf::TapCoef { class: id, axis },
                    bytes: coefs + 64,
                    replaces: false,
                });
                out.push(ConvOp::UploadTaps { class: id, axis });
                self.dev_bytes += starts + coefs + 128;
            }
        }
        self.by_key.insert(class.key, id);
        self.classes.push(ClassHost {
            class,
            jobs: Vec::new(),
            cap_jobs: 0,
            tmp_jobs: 0,
        });
        id
    }

    /// Take in a class that was made elsewhere ([`ConvClass::new`], which is the expensive
    /// part: the tap tables; it is pure and can run on any thread before a channel starts):
    /// only its buffers and uploads are issued here. Returns the existing class when the key
    /// is known already.
    pub fn adopt(&mut self, class: ConvClass, out: &mut Vec<ConvOp<A>>) -> ClassId {
        match self.class_id(&class.key) {
            Some(id) => id,
            None => self.install(class, out),
        }
    }

    /// The class of a key, made when first used (`Converter::get`).
    pub fn get(&mut self, key: ConvKey, out: &mut Vec<ConvOp<A>>) -> Result<ClassId, ConvertError> {
        if let Some(id) = self.class_id(&key) {
            return Ok(id);
        }
        let class = ConvClass::new(key)?;
        Ok(self.install(class, out))
    }

    /// Queue one job for this tick (`Converter::add`): `src` converted into the rectangle of
    /// `dst` that `key` describes. Nothing is changed when it fails.
    pub fn add(
        &mut self,
        key: ConvKey,
        src: Source<A>,
        dst: Dest<A>,
        out: &mut Vec<ConvOp<A>>,
    ) -> Result<(), ConvertError> {
        if src.sp_y & 3 != 0 || src.sp_uv & 3 != 0 || !src.y.aligned4() || !src.uv.aligned4() {
            return Err(ConvertError::Unaligned);
        }
        let mut k = key;
        let halve = self.fast_down
            && k.srows == Rows::All
            && k.cw >= 3 * k.dw
            && k.ch >= 3 * k.dh
            && k.sw.is_multiple_of(8)
            && k.sh.is_multiple_of(4)
            && k.cx.is_multiple_of(4)
            && k.cy.is_multiple_of(4)
            && k.cw.is_multiple_of(4)
            && k.ch.is_multiple_of(4);
        let pre_key = (k.sw, k.sh, k.bps);
        if halve {
            k.sw /= 2;
            k.sh /= 2;
            k.bps = 1;
            k.cx /= 2;
            k.cy /= 2;
            k.cw /= 2;
            k.ch /= 2;
        }
        // The class is made (and can fail) before anything changes; its operations follow the
        // pre-stage's, as in the C++.
        let new_class = match self.class_id(&k) {
            Some(_) => None,
            None => Some(ConvClass::new(k)?),
        };
        let (sy, suv, sp_y, sp_uv) = if halve {
            let set = match self.pres.get(&pre_key) {
                Some(&id) => id,
                None => {
                    let id = PreId(self.pre_sets.len() as u32);
                    let (w2, h2) = (pre_key.0 / 2, pre_key.1 / 2);
                    self.pre_sets.push(PreSet {
                        w2,
                        h2,
                        bps: pre_key.2,
                        stride: u64::from(w2) * u64::from(h2) * 3 / 2,
                        blocks: 0,
                        jobs: Vec::new(),
                    });
                    self.pres.insert(pre_key, id);
                    id
                }
            };
            let ps = &mut self.pre_sets[set.0 as usize];
            let nj = ps.jobs.len() as u32;
            if nj == PRE_BLOCK_JOBS * ps.blocks {
                let block = ps.blocks;
                out.push(ConvOp::Alloc {
                    buf: ConvBuf::Pre { set, block },
                    bytes: 8 * ps.stride + 64,
                    replaces: false,
                });
                out.push(ConvOp::Alloc {
                    buf: ConvBuf::PreJobs { set, block },
                    bytes: 8 * PRE_JOB_BYTES,
                    replaces: false,
                });
                ps.blocks += 1;
                self.dev_bytes += 8 * ps.stride;
            }
            ps.jobs.push(PreJob {
                sy: src.y,
                suv: src.uv,
                sp_y: src.sp_y,
                sp_uv: src.sp_uv,
            });
            let buf = ConvBuf::Pre {
                set,
                block: nj / PRE_BLOCK_JOBS,
            };
            let off = u64::from(nj % PRE_BLOCK_JOBS) * ps.stride;
            self.pre_jobs += 1;
            (
                Plane::Conv { buf, off },
                Plane::Conv {
                    buf,
                    off: off + u64::from(ps.w2) * u64::from(ps.h2),
                },
                ps.w2,
                ps.w2,
            )
        } else {
            (Plane::Ext(src.y), Plane::Ext(src.uv), src.sp_y, src.sp_uv)
        };
        let id = match new_class {
            Some(c) => self.install(c, out),
            None => self.class_id(&k).ok_or(ConvertError::BadSpec)?,
        };
        let h = &mut self.classes[id.0 as usize];
        if h.jobs.len() as u32 == h.cap_jobs {
            let ncap = 8.max(2 * h.cap_jobs);
            out.push(ConvOp::Alloc {
                buf: ConvBuf::Jobs(id),
                bytes: u64::from(ncap) * CONV_JOB_BYTES,
                replaces: h.cap_jobs > 0,
            });
            h.cap_jobs = ncap;
        }
        h.jobs.push(ConvJob {
            sy,
            suv,
            dy: dst.y,
            du: dst.u,
            dv: dst.v,
            sp_y,
            sp_uv,
            dp: dst.pitch,
        });
        Ok(())
    }

    /// Launch everything queued this tick (`Converter::run`): the pre-stage of every set (in
    /// the order of their keys), then every class with jobs (in the order of their keys).
    pub fn run(&mut self, out: &mut Vec<ConvOp<A>>) {
        for &set in self.pres.values() {
            let ps = &mut self.pre_sets[set.0 as usize];
            let jobs = std::mem::take(&mut ps.jobs);
            for (b, chunk) in jobs.chunks(PRE_BLOCK_JOBS as usize).enumerate() {
                let block = b as u32;
                let n = chunk.len() as u32;
                out.push(ConvOp::UploadPreJobs {
                    set,
                    block,
                    jobs: chunk.to_vec(),
                });
                let per = u64::from(ps.w2 / 4) * u64::from(ps.h2) * 3 / 2;
                out.push(ConvOp::Launch(ConvLaunch::Half {
                    jobs: ConvBuf::PreJobs { set, block },
                    nj: n,
                    w2: ps.w2,
                    h2: ps.h2,
                    bps: ps.bps,
                    pre: ConvBuf::Pre { set, block },
                    stride: ps.stride,
                    grid: grid(per, u64::from(n)),
                }));
                self.launches += 1;
            }
        }
        for &id in self.by_key.values() {
            let h = &mut self.classes[id.0 as usize];
            if h.jobs.is_empty() {
                continue;
            }
            let jobs = std::mem::take(&mut h.jobs);
            let nj = jobs.len() as u32;
            out.push(ConvOp::UploadJobs { class: id, jobs });
            let c = &h.class;
            if c.copy {
                let per = u64::from(c.copy_w / 4) * u64::from(c.copy_h)
                    + u64::from(c.copy_w / 8) * u64::from(c.copy_h / 2);
                out.push(ConvOp::Launch(ConvLaunch::ConvCopy {
                    class: id,
                    jobs: ConvBuf::Jobs(id),
                    nj,
                    grid: grid(per, u64::from(nj)),
                }));
                self.launches += 1;
            } else {
                let tstride = u64::from(c.tstride);
                if nj > h.tmp_jobs {
                    let replaces = h.tmp_jobs > 0;
                    if replaces {
                        self.dev_bytes -= u64::from(h.tmp_jobs) * tstride;
                    }
                    h.tmp_jobs = nj.max(h.tmp_jobs + 4);
                    out.push(ConvOp::Alloc {
                        buf: ConvBuf::Tmp(id),
                        bytes: u64::from(h.tmp_jobs) * tstride + 64,
                        replaces,
                    });
                    self.dev_bytes += u64::from(h.tmp_jobs) * tstride;
                }
                let (vy_n, vc_n) = c
                    .taps
                    .as_ref()
                    .map_or((0, 0), |t| (u64::from(t.vy.n), u64::from(t.vc.n)));
                let ph = u64::from(c.dw / 4) * u64::from(c.th_y)
                    + u64::from(c.dw / 8) * u64::from(c.th_c);
                let pv = u64::from(c.dw / 4) * vy_n + 2 * u64::from(c.dw / 8) * vc_n;
                out.push(ConvOp::Launch(ConvLaunch::ConvH {
                    class: id,
                    jobs: ConvBuf::Jobs(id),
                    nj,
                    tmp: ConvBuf::Tmp(id),
                    grid: grid(ph, u64::from(nj)),
                }));
                out.push(ConvOp::Launch(ConvLaunch::ConvV {
                    class: id,
                    jobs: ConvBuf::Jobs(id),
                    nj,
                    tmp: ConvBuf::Tmp(id),
                    grid: grid(pv, u64::from(nj)),
                }));
                self.launches += 2;
            }
        }
    }

    /// Solid color over a rectangle (`Converter::fill`).
    pub fn fill(&mut self, j: RectJob<A>, out: &mut Vec<ConvOp<A>>) {
        let n = u64::from(j.w / 4) * u64::from(j.h) + 2 * u64::from(j.w / 8) * u64::from(j.h / 2);
        out.push(ConvOp::Launch(ConvLaunch::Fill {
            j,
            grid: grid(n, 1),
        }));
        self.launches += 1;
    }

    /// Overlay with alpha over a rectangle (`Converter::overlay`).
    pub fn overlay(&mut self, j: RectJob<A>, out: &mut Vec<ConvOp<A>>) {
        let n = u64::from(j.w / 4) * u64::from(j.h) + 2 * u64::from(j.w / 8) * u64::from(j.h / 2);
        out.push(ConvOp::Launch(ConvLaunch::Overlay {
            j,
            grid: grid(n, 1),
        }));
        self.launches += 1;
    }

    /// Color matrix and range in place over a rectangle (`Converter::csc`).
    pub fn csc(&mut self, j: RectJob<A>, m: Csc, interlaced: bool, out: &mut Vec<ConvOp<A>>) {
        let n = u64::from(j.w / 8) * u64::from(j.h / 2);
        out.push(ConvOp::Launch(ConvLaunch::Csc {
            j,
            m,
            interlaced,
            grid: grid(n, 1),
        }));
        self.launches += 1;
    }
}

#[cfg(test)]
mod tests;
