//! The kernels of the device image and their argument records.
//!
//! One record per kernel, with the fields of the C signature in order (`crates/tb-kernel-abi/kernels/kernels.h`; the
//! comment above each kernel there says what it does and how its threads map to work). Pointers are
//! [`DevBuf`] handles; an adapter turns them into addresses when it launches. The host build of the
//! kernels reports the size of every parameter, and the tests of `tb-device-ref` compare them with
//! what [`Args::resolve`] produces, so a signature that changes on one side only fails `nix flake
//! check`.

use core::ffi::c_void;

use bytemuck::{Pod, Zeroable};
use tb_kernel_abi::{Bufs, ConvClass, CscPar, Geo, RectJob, Taps};

use crate::mem::DevBuf;

/// Most parameters any kernel takes (`k_recon`: 15).
pub const MAX_ARGS: usize = 16;

/// Handles of the picture stores of one batch: [`tb_kernel_abi::Bufs`] before address
/// resolution.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(missing_docs, reason = "fields are those of tb_kernel_abi::Bufs")]
pub struct BufsRef {
    pub ring_y: DevBuf,
    pub ring_u: DevBuf,
    pub ring_v: DevBuf,
    pub ring_stride: u64,
    pub cur1: DevBuf,
    pub cur2: DevBuf,
    pub ref_y: DevBuf,
    pub ref_u: DevBuf,
    pub ref_v: DevBuf,
    pub ref_y1: DevBuf,
    pub ref_y2: DevBuf,
}

impl BufsRef {
    fn all(&self) -> [DevBuf; 10] {
        [
            self.ring_y,
            self.ring_u,
            self.ring_v,
            self.cur1,
            self.cur2,
            self.ref_y,
            self.ref_u,
            self.ref_v,
            self.ref_y1,
            self.ref_y2,
        ]
    }
}

/// A place in device memory as plain data: a byte offset into the device arena, or into a
/// *foreign* region (memory on the device that the adapter does not own: a decoder surface)
/// that the driver has bound to the adapter under a slot number.
///
/// Bits 63..48 hold the space (0: the arena; `s + 1`: foreign slot `s`), bits 47..0 the byte
/// offset. The form exists for records that are uploaded as bytes and hold addresses
/// ([`ConvJobRef`], [`PreJobRef`]): the core writes handles, the adapter resolves them
/// ([`Op::UploadRefs`](crate::plan::Op::UploadRefs)).
///
/// Lifetime of a foreign region: the driver binds it before it submits a plan that names it and
/// keeps the memory alive and bound until that plan has completed. Only the adapter knows what
/// is bound; it refuses a plan that names an unbound slot or an offset outside the bound length.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Pod, Zeroable)]
pub struct DevRef(pub u64);

impl DevRef {
    const OFF_BITS: u32 = 48;
    const OFF_MASK: u64 = (1 << Self::OFF_BITS) - 1;
    /// Most foreign slots an adapter can be asked for.
    pub const MAX_FOREIGN: u32 = 0xfffe;

    /// `off` bytes into the device arena.
    pub const fn arena(off: u64) -> Self {
        Self(off & Self::OFF_MASK)
    }
    /// `off` bytes into foreign slot `slot`.
    pub const fn foreign(slot: u16, off: u64) -> Self {
        Self(((slot as u64 + 1) << Self::OFF_BITS) | (off & Self::OFF_MASK))
    }
    /// The foreign slot, or `None` for the arena.
    pub const fn slot(self) -> Option<u16> {
        match self.0 >> Self::OFF_BITS {
            0 => None,
            s => Some((s - 1) as u16),
        }
    }
    /// The byte offset inside its space.
    pub const fn off(self) -> u64 {
        self.0 & Self::OFF_MASK
    }
    /// The same space, `by` bytes further.
    #[must_use]
    pub const fn add(self, by: u64) -> Self {
        Self((self.0 & !Self::OFF_MASK) | ((self.off() + by) & Self::OFF_MASK))
    }
}

impl From<DevBuf> for DevRef {
    fn from(b: DevBuf) -> Self {
        Self::arena(b.off)
    }
}

/// [`tb_kernel_abi::ConvJob`] with handles for addresses: the same layout, uploaded through
/// [`Op::UploadRefs`](crate::plan::Op::UploadRefs).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Pod, Zeroable)]
#[allow(missing_docs, reason = "fields are those of tb_kernel_abi::ConvJob")]
pub struct ConvJobRef {
    pub sy: DevRef,
    pub suv: DevRef,
    pub dy: DevRef,
    pub du: DevRef,
    pub dv: DevRef,
    pub sp_y: i32,
    pub sp_uv: i32,
    pub dp: i32,
    pub pad: i32,
}

/// [`tb_kernel_abi::PreJob`] with handles for addresses: the same layout.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Pod, Zeroable)]
#[allow(missing_docs, reason = "fields are those of tb_kernel_abi::PreJob")]
pub struct PreJobRef {
    pub sy: DevRef,
    pub suv: DevRef,
    pub sp_y: i32,
    pub sp_uv: i32,
}

/// Which records an [`Op::UploadRefs`](crate::plan::Op::UploadRefs) carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RefKind {
    /// [`ConvJobRef`] records.
    ConvJobs,
    /// [`PreJobRef`] records.
    PreJobs,
}

impl RefKind {
    /// Bytes of one record.
    pub const fn record(self) -> usize {
        match self {
            RefKind::ConvJobs => size_of::<ConvJobRef>(),
            RefKind::PreJobs => size_of::<PreJobRef>(),
        }
    }
    /// How many leading 8-byte fields of a record are handles.
    const fn handles(self) -> usize {
        match self {
            RefKind::ConvJobs => 5,
            RefKind::PreJobs => 2,
        }
    }

    /// Turn the handle-form records in `bytes` into the kernels' records, in place: every
    /// handle becomes the address `space(slot) + offset`. `arena` is the device arena's base
    /// address and length; `foreign` gives base address and length of a bound foreign slot.
    /// Fails, leaving `bytes` in an unspecified state, when the length is not a whole number of
    /// records, an offset lies outside its space or a slot is not bound.
    pub fn resolve(
        self,
        bytes: &mut [u8],
        arena: (u64, u64),
        foreign: &dyn Fn(u16) -> Option<(u64, u64)>,
    ) -> Result<(), &'static str> {
        let rec = self.record();
        if !bytes.len().is_multiple_of(rec) {
            return Err("handle records: not a whole number of records");
        }
        for r in bytes.chunks_exact_mut(rec) {
            for h in r.as_chunks_mut::<8>().0.iter_mut().take(self.handles()) {
                let d = DevRef(u64::from_ne_bytes(*h));
                let (base, len) = match d.slot() {
                    None => arena,
                    Some(s) => foreign(s).ok_or("handle records: foreign slot not bound")?,
                };
                if d.off() >= len {
                    return Err("handle records: offset outside its space");
                }
                *h = (base + d.off()).to_ne_bytes();
            }
        }
        Ok(())
    }
}

/// [`tb_kernel_abi::Taps`] before address resolution: offsets into the device arena.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(missing_docs, reason = "fields are those of tb_kernel_abi::Taps")]
pub struct TapsRef {
    /// Arena offset of the `n` start indices (`int32`).
    pub start: u64,
    /// Arena offset of the `n * nt` coefficients (`int16`).
    pub coef: u64,
    pub n: i32,
    pub nt: i32,
}

impl TapsRef {
    fn fits(&self, arena: u64) -> bool {
        let (n, nt) = (
            u64::from(self.n.max(0) as u32),
            u64::from(self.nt.max(0) as u32),
        );
        if n == 0 {
            return true;
        }
        DevBuf::new(self.start, n * 4).fits(arena) && DevBuf::new(self.coef, n * nt * 2).fits(arena)
    }
    fn resolve(&self, base: u64) -> Taps {
        Taps {
            start: (base + self.start) as *const i32,
            coef: (base + self.coef) as *const i16,
            n: self.n,
            nt: self.nt,
        }
    }
}

/// [`tb_kernel_abi::ConvClass`] before address resolution.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(missing_docs, reason = "fields are those of tb_kernel_abi::ConvClass")]
pub struct ConvClassRef {
    pub bps: i32,
    pub dw: i32,
    pub th_y: i32,
    pub th_c: i32,
    pub s_step: i32,
    pub s_off: i32,
    pub d_step: i32,
    pub d_off: i32,
    pub tstride: i32,
    pub copy_w: i32,
    pub copy_h: i32,
    pub hy: TapsRef,
    pub hc: TapsRef,
    pub vy: TapsRef,
    pub vc: TapsRef,
}

/// [`tb_kernel_abi::RectJob`] before address resolution. The overlay planes are read by
/// `k_overlay` only; elsewhere they stay empty.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(missing_docs, reason = "fields are those of tb_kernel_abi::RectJob")]
pub struct RectRef {
    pub dy: DevBuf,
    pub du: DevBuf,
    pub dv: DevBuf,
    pub dp: i32,
    pub w: i32,
    pub h: i32,
    pub yuv: u32,
    pub oy: DevBuf,
    pub ou: DevBuf,
    pub ov: DevBuf,
    pub oa: DevBuf,
    pub oa2: DevBuf,
}

impl RectRef {
    fn all(&self) -> [DevBuf; 8] {
        [
            self.dy, self.du, self.dv, self.oy, self.ou, self.ov, self.oa, self.oa2,
        ]
    }
}

/// One kernel argument as the core states it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Arg {
    /// A pointer to the start of a device region.
    Ptr(DevBuf),
    /// A `size_t`.
    Size(u64),
    /// An `int`.
    Int(i32),
    /// A `Geo` by value.
    Geo(Geo),
    /// A `Bufs` by value.
    Bufs(BufsRef),
    /// A `ConvClass` by value.
    Class(ConvClassRef),
    /// A `RectJob` by value.
    Rect(RectRef),
    /// A `CscPar` by value.
    Csc(CscPar),
}

impl Arg {
    /// Whether every buffer the argument names lies inside a device arena of `arena` bytes.
    pub fn fits(&self, arena: u64) -> bool {
        match self {
            Arg::Ptr(b) => b.fits(arena),
            Arg::Bufs(b) => b.all().iter().all(|b| b.fits(arena)),
            Arg::Class(c) => [c.hy, c.hc, c.vy, c.vc].iter().all(|t| t.fits(arena)),
            Arg::Rect(r) => r.all().iter().all(|b| b.fits(arena)),
            Arg::Size(_) | Arg::Int(_) | Arg::Geo(_) | Arg::Csc(_) => true,
        }
    }
}

/// The arguments of one launch, in order.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Args {
    len: usize,
    args: [Arg; MAX_ARGS],
}

impl Args {
    fn new() -> Self {
        Self {
            len: 0,
            args: [Arg::Int(0); MAX_ARGS],
        }
    }
    fn push(&mut self, a: Arg) {
        // The records below are the only callers and none has more than MAX_ARGS fields
        // (checked by the test `no_kernel_exceeds_max_args`).
        if let Some(slot) = self.args.get_mut(self.len) {
            *slot = a;
            self.len += 1;
        }
    }
    /// The arguments.
    pub fn as_slice(&self) -> &[Arg] {
        &self.args[..self.len]
    }
    /// Whether every buffer argument lies inside a device arena of `arena` bytes.
    pub fn bufs_fit(&self, arena: u64) -> bool {
        self.as_slice().iter().all(|a| a.fits(arena))
    }
    /// The argument values for a device arena that starts at address `base`.
    pub fn resolve(&self, base: u64) -> Resolved {
        let mut r = Resolved::empty();
        for a in self.as_slice() {
            r.push(a, base);
        }
        r
    }
}

/// What one resolved argument is: a scalar held in place, or one of the launch's by-value
/// records (a kernel takes at most one of each kind).
#[derive(Clone, Copy, Debug)]
enum Slot {
    Addr(u64),
    Size(u64),
    Int(i32),
    Geo,
    Bufs,
    Class,
    Rect,
    Csc,
}

/// The argument values of one launch, in the form both launch interfaces take
/// (`cuLaunchKernel` and the host library's `tbk_launch`): an array of pointers, one to each
/// value. Small by design (scalars in place, each by-value record once): a launch copies a
/// few hundred bytes, not a full-size value per parameter.
#[derive(Clone, Copy, Debug)]
pub struct Resolved {
    len: usize,
    slots: [Slot; MAX_ARGS],
    geo: Option<Geo>,
    bufs: Option<Bufs>,
    class: Option<ConvClass>,
    rect: Option<RectJob>,
    csc: Option<CscPar>,
}

impl Resolved {
    fn empty() -> Self {
        Self {
            len: 0,
            slots: [Slot::Int(0); MAX_ARGS],
            geo: None,
            bufs: None,
            class: None,
            rect: None,
            csc: None,
        }
    }

    /// Append the value of `a` for a device arena that starts at address `base`.
    fn push(&mut self, a: &Arg, base: u64) {
        let p = |b: DevBuf| (base + b.off) as *mut u8;
        let slot = match *a {
            Arg::Ptr(b) => Slot::Addr(base + b.off),
            Arg::Size(s) => Slot::Size(s),
            Arg::Int(i) => Slot::Int(i),
            Arg::Geo(g) => {
                self.geo = Some(g);
                Slot::Geo
            }
            Arg::Bufs(b) => {
                self.bufs = Some(Bufs {
                    ring_y: p(b.ring_y),
                    ring_u: p(b.ring_u),
                    ring_v: p(b.ring_v),
                    ring_stride: b.ring_stride as usize,
                    cur1: p(b.cur1),
                    cur2: p(b.cur2),
                    ref_y: p(b.ref_y),
                    ref_u: p(b.ref_u),
                    ref_v: p(b.ref_v),
                    ref_y1: p(b.ref_y1),
                    ref_y2: p(b.ref_y2),
                });
                Slot::Bufs
            }
            Arg::Class(c) => {
                self.class = Some(ConvClass {
                    bps: c.bps,
                    dw: c.dw,
                    th_y: c.th_y,
                    th_c: c.th_c,
                    s_step: c.s_step,
                    s_off: c.s_off,
                    d_step: c.d_step,
                    d_off: c.d_off,
                    tstride: c.tstride,
                    copy_w: c.copy_w,
                    copy_h: c.copy_h,
                    hy: c.hy.resolve(base),
                    hc: c.hc.resolve(base),
                    vy: c.vy.resolve(base),
                    vc: c.vc.resolve(base),
                });
                Slot::Class
            }
            Arg::Rect(r) => {
                self.rect = Some(RectJob {
                    dy: p(r.dy),
                    du: p(r.du),
                    dv: p(r.dv),
                    dp: r.dp,
                    w: r.w,
                    h: r.h,
                    yuv: r.yuv,
                    oy: p(r.oy),
                    ou: p(r.ou),
                    ov: p(r.ov),
                    oa: p(r.oa),
                    oa2: p(r.oa2),
                });
                Slot::Rect
            }
            Arg::Csc(m) => {
                self.csc = Some(m);
                Slot::Csc
            }
        };
        // The records of this module are the only callers and none has more than MAX_ARGS
        // fields (test `no_kernel_exceeds_max_args`).
        if let Some(s) = self.slots.get_mut(self.len) {
            *s = slot;
            self.len += 1;
        }
    }

    /// Number of arguments.
    pub fn len(&self) -> usize {
        self.len
    }
    /// Whether there are no arguments.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    /// Size in bytes of each argument value.
    pub fn sizes(&self) -> impl Iterator<Item = usize> + '_ {
        self.slots[..self.len].iter().map(|v| match v {
            Slot::Addr(_) | Slot::Size(_) => 8,
            Slot::Int(_) => 4,
            Slot::Geo => size_of::<Geo>(),
            Slot::Bufs => size_of::<Bufs>(),
            Slot::Class => size_of::<ConvClass>(),
            Slot::Rect => size_of::<RectJob>(),
            Slot::Csc => size_of::<CscPar>(),
        })
    }
    /// Pointers to the argument values. They point into `self` and are valid while it is
    /// neither moved nor dropped; entries past [`Resolved::len`] are null.
    pub fn pointers(&mut self) -> [*mut c_void; MAX_ARGS] {
        fn of<T>(v: &mut Option<T>) -> *mut c_void {
            v.as_mut()
                .map_or(core::ptr::null_mut(), |x| core::ptr::from_mut(x).cast())
        }
        let mut out = [core::ptr::null_mut(); MAX_ARGS];
        let (geo, bufs, class) = (of(&mut self.geo), of(&mut self.bufs), of(&mut self.class));
        let (rect, csc) = (of(&mut self.rect), of(&mut self.csc));
        for (o, v) in out.iter_mut().zip(self.slots[..self.len].iter_mut()) {
            *o = match v {
                Slot::Addr(x) | Slot::Size(x) => core::ptr::from_mut(x).cast(),
                Slot::Int(x) => core::ptr::from_mut(x).cast(),
                Slot::Geo => geo,
                Slot::Bufs => bufs,
                Slot::Class => class,
                Slot::Rect => rect,
                Slot::Csc => csc,
            };
        }
        out
    }
}

macro_rules! arg_ty {
    (ptr) => {
        DevBuf
    };
    (size) => {
        u64
    };
    (int) => {
        i32
    };
    (geo) => {
        Geo
    };
    (bufs) => {
        BufsRef
    };
    (class) => {
        ConvClassRef
    };
    (rect) => {
        RectRef
    };
    (csc) => {
        CscPar
    };
}
macro_rules! arg_of {
    (ptr, $e:expr) => { Arg::Ptr($e) };
    (size, $e:expr) => { Arg::Size($e) };
    (int, $e:expr) => { Arg::Int($e) };
    (geo, $e:expr) => { Arg::Geo($e) };
    (bufs, $e:expr) => { Arg::Bufs($e) };
    (class, $e:expr) => { Arg::Class($e) };
    (rect, $e:expr) => { Arg::Rect($e) };
    (csc, $e:expr) => { Arg::Csc($e) };
}

macro_rules! kernels {
    ($( $(#[$m:meta])* $rec:ident = $name:literal { $( $f:ident : $k:ident ),* $(,)? } )*) => {
        $(
            $(#[$m])*
            #[derive(Clone, Copy, Debug, PartialEq)]
            #[allow(missing_docs, reason = "fields are the parameters of the C signature")]
            pub struct $rec { $( pub $f: arg_ty!($k) ),* }
        )*

        /// A kernel of the device image with its arguments.
        #[derive(Clone, Copy, Debug, PartialEq)]
        #[allow(missing_docs, reason = "one variant per record of this module")]
        pub enum Kernel { $( $rec($rec) ),* }

        /// The kernels in declaration order: the discriminant is the position in `NAMES`.
        #[repr(usize)]
        enum KernelIndex { $( $rec ),* }

        impl Kernel {
            /// Names of all kernels, in the order of [`Kernel::index`].
            pub const NAMES: &'static [&'static str] = &[ $( $name ),* ];

            /// The kernel's name in the image (the C function name).
            pub fn name(&self) -> &'static str {
                match self { $( Kernel::$rec(_) => $name ),* }
            }

            /// The kernel of this name with every argument zero (null handles, zero sizes and
            /// counts, a default `Geo`), or `None` for a name not in [`Kernel::NAMES`]. For plans
            /// that only stand for a recorded sequence of launches and are compared by kernel
            /// name and launch dimensions ([`crate::sim::normal`]), as the traces made from the
            /// golden records are.
            pub fn placeholder(name: &str) -> Option<Kernel> {
                match name {
                    $( $name => Some(Kernel::$rec($rec { $( $f: Default::default() ),* })), )*
                    _ => None,
                }
            }

            /// Position of the kernel in [`Kernel::NAMES`].
            pub fn index(&self) -> usize {
                match self { $( Kernel::$rec(_) => KernelIndex::$rec as usize ),* }
            }

            /// Whether every buffer argument lies inside a device arena of `arena` bytes
            /// (as [`Args::bufs_fit`], without building the list).
            pub fn bufs_fit(&self, arena: u64) -> bool {
                match self {
                    $( Kernel::$rec(r) => { let _ = r; true $( && arg_of!($k, r.$f).fits(arena) )* } ),*
                }
            }

            /// The argument values for a device arena that starts at address `base`, in the
            /// order of the C signature (as `args().resolve(base)`, without the list).
            pub fn resolve(&self, base: u64) -> Resolved {
                let mut out = Resolved::empty();
                match self {
                    $( Kernel::$rec(r) => { let _ = r; $( out.push(&arg_of!($k, r.$f), base); )* } ),*
                }
                out
            }

            /// The arguments in the order of the C signature.
            pub fn args(&self) -> Args {
                let mut a = Args::new();
                match self {
                    $( Kernel::$rec(r) => { let _ = r; $( a.push(arg_of!($k, r.$f)); )* } ),*
                }
                a
            }
        }
    };
}

kernels! {
    /// `k_tick()`: does nothing; marks the start of a tick in a timeline.
    Tick = "k_tick" { }
    /// `k_pad_cols`: columns `sw..pw` of rows `0..sh` repeat column `sw - 1`.
    PadCols = "k_pad_cols" {
        plane: ptr, stride: size, pw: int, sw: int, sh: int, n: int, list: ptr, slot: ptr, na: int,
    }
    /// `k_pad_rows`: rows `sh..ph` repeat row `sh - 1` (after `k_pad_cols`).
    PadRows = "k_pad_rows" {
        plane: ptr, stride: size, pw: int, sh: int, ph: int, n: int, list: ptr, slot: ptr, na: int,
    }
    /// `k_pyr`: half- and quarter-resolution luma of a plane, per listed session.
    Pyr = "k_pyr" {
        src: ptr, sstride: size, sw: int, sh: int, d1: ptr, d1stride: size, d2: ptr,
        d2stride: size, n: int, list: ptr, na: int, par: ptr, ssel: int, dsel: int,
    }
    /// `k_set_mode`: every macroblock of the listed sessions intra, or forward with a zero
    /// vector.
    SetMode = "k_set_mode" { info: ptr, nmb: int, list: ptr, na: int, mode: int }
    /// `k_mbstat`: frame or field DCT per macroblock, and its activity.
    MbStat = "k_mbstat" {
        g: geo, ring_y: ptr, ring_stride: size, par: ptr, list: ptr, na: int, info: ptr, act: ptr,
    }
    /// `k_me`: motion estimation and mode decision.
    Me = "k_me" { g: geo, b: bufs, par: ptr, list: ptr, na: int, prev: ptr, out: ptr }
    /// `k_mvsmooth`: a macroblock adopts its left neighbor's vectors when that is cheap.
    MvSmooth = "k_mvsmooth" {
        g: geo, b: bufs, par: ptr, list: ptr, na: int, input: ptr, out: ptr,
    }
    /// `k_xf`: prediction, residual, forward DCT, zero-threshold histogram, in thread blocks
    /// of `tb_kernel_abi::XB` 8x8 blocks (eight threads each).
    Xf = "k_xf" {
        g: geo, tables: ptr, b: bufs, par: ptr, list: ptr, na: int, info: ptr, act: ptr,
        coef: ptr, predbuf: ptr, hist: ptr, cmask: ptr,
    }
    /// `k_xf16`: `k_xf` in thread blocks of 16 8x8 blocks.
    Xf16 = "k_xf16" {
        g: geo, tables: ptr, b: bufs, par: ptr, list: ptr, na: int, info: ptr, act: ptr,
        coef: ptr, predbuf: ptr, hist: ptr, cmask: ptr,
    }
    /// `k_xf32`: `k_xf` in thread blocks of 32 8x8 blocks.
    Xf32 = "k_xf32" {
        g: geo, tables: ptr, b: bufs, par: ptr, list: ptr, na: int, info: ptr, act: ptr,
        coef: ptr, predbuf: ptr, hist: ptr, cmask: ptr,
    }
    /// `k_xf64`: `k_xf` in thread blocks of 64 8x8 blocks.
    Xf64 = "k_xf64" {
        g: geo, tables: ptr, b: bufs, par: ptr, list: ptr, na: int, info: ptr, act: ptr,
        coef: ptr, predbuf: ptr, hist: ptr, cmask: ptr,
    }
    /// `k_rowstat`: row statistics for the rate control. One block per (session, row).
    RowStat = "k_rowstat" {
        g: geo, list: ptr, na: int, hist: ptr, act: ptr, info: ptr, out: ptr,
    }
    /// `k_quant`: quantization with the row quantizers the rate control chose.
    Quant = "k_quant" {
        g: geo, tables: ptr, par: ptr, list: ptr, na: int, rowq: ptr, info: ptr, act: ptr,
        coef: ptr, cmask: ptr, lev: ptr, nzmask: ptr, nz: ptr, mbq: ptr,
    }
    /// `k_recon`: inverse quantization, IDCT, reconstruction into a reference slot.
    Recon = "k_recon" {
        g: geo, tables: ptr, par: ptr, list: ptr, na: int, lev: ptr, nzmask: ptr, nz: ptr,
        predbuf: ptr, info: ptr, mbq: ptr, out_y: ptr, out_u: ptr, out_v: ptr, osel: int,
    }
    /// `k_ent_resolve`: skipped macroblocks, DC predictors.
    EntResolve = "k_ent_resolve" {
        g: geo, par: ptr, list: ptr, na: int, info: ptr, nz: ptr, lev: ptr, code: ptr,
    }
    /// `k_ent_pack`: each macroblock's bits into its scratch area.
    EntPack = "k_ent_pack" {
        g: geo, tables: ptr, par: ptr, list: ptr, na: int, code: ptr, info: ptr, lev: ptr,
        nzmask: ptr, mbq: ptr, scratch: ptr, mbbits: ptr, mbtex: ptr,
    }
    /// `k_ent_rowsum`: bit offsets inside each slice, slice sizes. One block per slice.
    EntRowSum = "k_ent_rowsum" {
        g: geo, list: ptr, na: int, mbbits: ptr, mbtex: ptr, mboff: ptr, slice_bytes: ptr,
        slice_tex: ptr,
    }
    /// `k_ent_picsum`: slice offsets, picture size.
    EntPicSum = "k_ent_picsum" {
        g: geo, list: ptr, na: int, slice_bytes: ptr, slice_tex: ptr, slice_off: ptr, pic: ptr,
    }
    /// `k_ent_scatter`: scratch bits into the output buffer.
    EntScatter = "k_ent_scatter" {
        g: geo, list: ptr, na: int, code: ptr, scratch: ptr, mbbits: ptr, mboff: ptr,
        slice_off: ptr, out: ptr, outpos: ptr,
    }
    /// `k_conv_h`: horizontal pass of the resampler, source rows of the lattice into the
    /// intermediate planes (`crates/tb-kernel-abi/kernels/pipe_kernels.h`). `jobs` holds `nj`
    /// `ConvJob` records ([`Op::UploadRefs`](crate::plan::Op::UploadRefs)).
    ConvH = "k_conv_h" { c: class, jobs: ptr, nj: int, tmp: ptr }
    /// `k_conv_v`: vertical pass, intermediate planes into the destination rectangle.
    ConvV = "k_conv_v" { c: class, jobs: ptr, nj: int, tmp: ptr }
    /// `k_conv_copy`: same size, no filter: copy luma, split the interleaved chroma (16-bit
    /// samples rounded to 8).
    ConvCopy = "k_conv_copy" { c: class, jobs: ptr, nj: int }
    /// `k_half`: 2:1 reduction of a source frame into an 8-bit NV12 frame, in front of the
    /// resampler for large ratios. `jobs` holds `nj` `PreJob` records.
    Half = "k_half" { jobs: ptr, nj: int, w2: int, h2: int, bps: int, pre: ptr, stride: size }
    /// `k_fill`: solid color over a rectangle.
    FillRect = "k_fill" { j: rect }
    /// `k_overlay`: overlay with alpha over a rectangle.
    Overlay = "k_overlay" { j: rect }
    /// `k_csc`: color matrix and range in place over a rectangle.
    Csc = "k_csc" { j: rect, m: csc }
}

/// One kernel launch: `grid` blocks of `block` threads (one-dimensional, as in the C++).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Launch {
    /// The kernel and its arguments.
    pub kernel: Kernel,
    /// Number of blocks.
    pub grid: u32,
    /// Threads per block.
    pub block: u32,
}

impl Launch {
    /// A launch of enough blocks of `block` threads for `count` threads of work
    /// (the C++ `LAUNCHB` macro).
    pub fn covering(kernel: Kernel, count: u64, block: u32) -> Self {
        let grid = count.div_ceil(u64::from(block.max(1)));
        Self {
            kernel,
            grid: u32::try_from(grid).unwrap_or(u32::MAX),
            block,
        }
    }
}

/// The sizes of thread block the transform kernel exists in, in 8x8 blocks (eight threads
/// each): `tb_kernel_abi::XB` (`k_xf`) first, then `k_xf16`, `k_xf32`, `k_xf64`.
pub const XF_BLOCKS: [u32; 4] = [tb_kernel_abi::XB as u32, 16, 32, 64];

impl Xf {
    /// The transform kernel for thread blocks of `blocks` 8x8 blocks with these arguments, or
    /// `None` when there is none of that size ([`XF_BLOCKS`]). Launch it with `blocks * 8`
    /// threads per block: each of the kernels has shared memory for its own size only.
    pub fn sized(self, blocks: u32) -> Option<Kernel> {
        let Xf {
            g,
            tables,
            b,
            par,
            list,
            na,
            info,
            act,
            coef,
            predbuf,
            hist,
            cmask,
        } = self;
        macro_rules! as_ {
            ($rec:ident) => {
                Kernel::$rec($rec {
                    g,
                    tables,
                    b,
                    par,
                    list,
                    na,
                    info,
                    act,
                    coef,
                    predbuf,
                    hist,
                    cmask,
                })
            };
        }
        match blocks {
            x if x == XF_BLOCKS[0] => Some(Kernel::Xf(self)),
            16 => Some(as_!(Xf16)),
            32 => Some(as_!(Xf32)),
            64 => Some(as_!(Xf64)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_kernel_exceeds_max_args() {
        // k_recon has the most parameters; `push` would drop any beyond MAX_ARGS silently.
        let d = DevBuf::default();
        let k = Kernel::Recon(Recon {
            g: Geo::default(),
            tables: d,
            par: d,
            list: d,
            na: 0,
            lev: d,
            nzmask: d,
            nz: d,
            predbuf: d,
            info: d,
            mbq: d,
            out_y: d,
            out_u: d,
            out_v: d,
            osel: 0,
        });
        assert_eq!(k.args().as_slice().len(), 15);
        assert_eq!(k.name(), "k_recon");
        assert_eq!(Kernel::NAMES[k.index()], "k_recon");
        assert_eq!(Kernel::NAMES.len(), 27);
    }

    #[test]
    fn the_direct_paths_agree_with_the_argument_list() {
        let d = |o| DevBuf::new(o, 64);
        let ks = [
            Kernel::Xf(Xf {
                g: Geo {
                    n: 3,
                    w: 32,
                    ..Geo::default()
                },
                tables: d(64),
                b: BufsRef {
                    ring_y: d(128),
                    ring_stride: 99,
                    ref_y2: d(900),
                    ..BufsRef::default()
                },
                par: d(256),
                list: d(320),
                na: 2,
                info: d(384),
                act: d(448),
                coef: d(512),
                predbuf: d(576),
                hist: d(640),
                cmask: d(704),
            }),
            Kernel::Csc(Csc {
                j: RectRef {
                    dy: d(64),
                    w: 16,
                    ..RectRef::default()
                },
                m: CscPar {
                    interlaced: 1,
                    ..CscPar::default()
                },
            }),
            Kernel::ConvH(ConvH {
                c: ConvClassRef {
                    dw: 8,
                    hy: TapsRef {
                        start: 64,
                        coef: 128,
                        n: 4,
                        nt: 4,
                    },
                    ..ConvClassRef::default()
                },
                jobs: d(256),
                nj: 1,
                tmp: d(512),
            }),
        ];
        for k in ks {
            let (mut a, mut b) = (k.args().resolve(0x1000), k.resolve(0x1000));
            assert_eq!(a.len(), k.args().as_slice().len());
            assert_eq!(a.sizes().collect::<Vec<_>>(), b.sizes().collect::<Vec<_>>());
            let (pa, pb) = (a.pointers(), b.pointers());
            // the same bytes behind every pointer
            for ((x, y), n) in pa.iter().zip(&pb).zip(a.sizes().collect::<Vec<_>>()) {
                assert!(!x.is_null() && !y.is_null());
                let (off_a, off_b) = (
                    *x as usize - core::ptr::from_ref(&a) as usize,
                    *y as usize - core::ptr::from_ref(&b) as usize,
                );
                assert_eq!(off_a, off_b);
                assert!(off_a + n <= size_of::<Resolved>());
            }
            assert!(format!("{a:?}") == format!("{b:?}"));
            for arena in [0, 600, 1000, 4096] {
                assert_eq!(
                    k.bufs_fit(arena),
                    k.args().bufs_fit(arena),
                    "{} {arena}",
                    k.name()
                );
            }
        }
        // a launch's values are small
        assert!(size_of::<Resolved>() <= 800, "{}", size_of::<Resolved>());
    }

    #[test]
    fn index_is_the_position_of_the_name() {
        for (i, name) in Kernel::NAMES.iter().enumerate() {
            let k = Kernel::placeholder(name).unwrap();
            assert_eq!(k.index(), i);
            assert_eq!(k.name(), *name);
        }
    }

    #[test]
    fn handle_records_have_the_abi_layout() {
        use core::mem::{align_of, offset_of};
        use tb_kernel_abi::{ConvJob, PreJob};
        assert_eq!(size_of::<ConvJobRef>(), size_of::<ConvJob>());
        assert_eq!(align_of::<ConvJobRef>(), align_of::<ConvJob>());
        assert_eq!(offset_of!(ConvJobRef, sy), offset_of!(ConvJob, sy));
        assert_eq!(offset_of!(ConvJobRef, suv), offset_of!(ConvJob, suv));
        assert_eq!(offset_of!(ConvJobRef, dy), offset_of!(ConvJob, dy));
        assert_eq!(offset_of!(ConvJobRef, du), offset_of!(ConvJob, du));
        assert_eq!(offset_of!(ConvJobRef, dv), offset_of!(ConvJob, dv));
        assert_eq!(offset_of!(ConvJobRef, sp_y), offset_of!(ConvJob, sp_y));
        assert_eq!(offset_of!(ConvJobRef, sp_uv), offset_of!(ConvJob, sp_uv));
        assert_eq!(offset_of!(ConvJobRef, dp), offset_of!(ConvJob, dp));
        assert_eq!(offset_of!(ConvJobRef, pad), offset_of!(ConvJob, pad));
        assert_eq!(size_of::<PreJobRef>(), size_of::<PreJob>());
        assert_eq!(align_of::<PreJobRef>(), align_of::<PreJob>());
        assert_eq!(offset_of!(PreJobRef, sy), offset_of!(PreJob, sy));
        assert_eq!(offset_of!(PreJobRef, suv), offset_of!(PreJob, suv));
        assert_eq!(offset_of!(PreJobRef, sp_y), offset_of!(PreJob, sp_y));
        assert_eq!(offset_of!(PreJobRef, sp_uv), offset_of!(PreJob, sp_uv));
        // The handles are the leading 8-byte fields RefKind::resolve rewrites.
        assert_eq!(offset_of!(ConvJobRef, sp_y), 5 * 8);
        assert_eq!(offset_of!(PreJobRef, sp_y), 2 * 8);
    }

    #[test]
    fn handles_resolve_in_place() {
        let jobs = [
            ConvJobRef {
                sy: DevRef::foreign(3, 64),
                suv: DevRef::foreign(3, 4096),
                dy: DevRef::arena(128),
                du: DevRef::arena(256),
                dv: DevRef::arena(512),
                sp_y: 1920,
                sp_uv: 1920,
                dp: 1920,
                pad: 0,
            },
            ConvJobRef::default(),
        ];
        let mut bytes = bytemuck::cast_slice::<_, u8>(&jobs).to_vec();
        let foreign = |s: u16| (s == 3).then_some((0x7000_0000_0000u64, 8192u64));
        RefKind::ConvJobs
            .resolve(&mut bytes, (0x1000, 1024), &foreign)
            .unwrap();
        let words: Vec<u64> = bytes
            .as_chunks::<8>()
            .0
            .iter()
            .map(|c| u64::from_ne_bytes(*c))
            .collect();
        assert_eq!(
            &words[..5],
            [0x7000_0000_0040, 0x7000_0000_1000, 0x1080, 0x1100, 0x1200]
        );
        assert_eq!(words[5] as u32, 1920);
        assert_eq!(&words[7..12], [0x1000; 5]);
        // An unbound slot, an offset past the bound length, and a ragged length are refused.
        let mut b = bytemuck::bytes_of(&jobs[0]).to_vec();
        assert!(
            RefKind::ConvJobs
                .resolve(&mut b, (0, 1024), &|_| None)
                .is_err()
        );
        let mut b = bytemuck::bytes_of(&jobs[0]).to_vec();
        assert!(
            RefKind::ConvJobs
                .resolve(&mut b, (0, 1024), &|_| Some((0, 4096)))
                .is_err()
        );
        let mut b = vec![0u8; 55];
        assert!(
            RefKind::ConvJobs
                .resolve(&mut b, (0, 1024), &foreign)
                .is_err()
        );
        assert_eq!(DevRef::foreign(7, 5).slot(), Some(7));
        assert_eq!(DevRef::foreign(7, 5).add(3).off(), 8);
        assert_eq!(DevRef::arena(9).slot(), None);
    }

    #[test]
    fn resolve_adds_the_base() {
        let k = Kernel::SetMode(SetMode {
            info: DevBuf::new(256, 24),
            nmb: 1,
            list: DevBuf::new(512, 4),
            na: 1,
            mode: 0,
        });
        let mut r = k.args().resolve(0x1000);
        assert_eq!(r.sizes().collect::<Vec<_>>(), [8, 4, 8, 4, 4]);
        let p = r.pointers();
        assert!(!p[4].is_null() && p[5].is_null());
    }
}
