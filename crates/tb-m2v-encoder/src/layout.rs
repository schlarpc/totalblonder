//! Geometry and memory layout of a batch (the C++ `Batch::init`).
//!
//! A batch is every buffer the kernels need for `n` session slots of one format. The layout is
//! arithmetic: offsets into the two arenas, computed once when the batch is made.
//!
//! Ported from the C++ `Batch::init`, arena path only: every device buffer takes
//! `bytes + 2 * GUARD` rounded up to 256 and its usable region starts `GUARD` bytes in; every
//! host buffer takes `max(bytes, 16)` rounded up to 64. Buffers are placed in the order the C++
//! allocates them; `h_frames` (new in the port) is the last host buffer.

use core::mem::size_of;

use tb_device::kernel::BufsRef;
use tb_device::{DevBuf, HostBuf};
use tb_kernel_abi::{
    Geo, MB_SCRATCH_WORDS, ME_MAX_RANGE2, MbInfo, MbRes, PicStat, RowStat, SessPar, Tables,
};

use crate::config::SessionConfig;

/// Source frames kept per session on the device (`Batch::RING`).
pub const RING: u32 = 8;
/// Bytes left free on both sides of every device buffer: packed loads may touch a few bytes
/// outside a plane (the C++ `GUARD`).
pub const GUARD: u64 = 64;
/// Threads per block of most kernels (`BS`).
pub const BS: u32 = 256;
/// Threads per block of `k_me` (`ME_BS`).
pub const ME_BS: u32 = 64;
/// Threads per block of `k_recon` (`RECON_BS`).
pub const RECON_BS: u32 = 64;

/// The session lists of a batch (`Batch::L_*`): each is an `int` array on both sides.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub enum List {
    /// Sessions whose frame was uploaded in this tick.
    Up = 0,
    /// Sessions that code a picture in this tick.
    Code = 1,
    /// Of those, the P and B pictures.
    Me = 2,
    /// Of those, the I pictures.
    Intra = 3,
    /// Sessions that quantize again.
    Retry = 4,
    /// Sessions that reconstruct (anchors), or that became all-skipped.
    Rec = 5,
    /// Sessions that repeat the transform below the quantizer floor.
    Xf = 6,
    /// Sessions whose frame was written on the device in this tick.
    Dev = 7,
}

/// Why a format cannot be coded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum LayoutError {
    /// Rows longer than 256 macroblocks (4096 samples): the row kernels use one block per row.
    #[error("pictures wider than 4096 samples are not supported")]
    TooWide,
    /// Zero or odd dimensions, or a size whose buffers do not fit the integer types of the
    /// layout (`Geo` is `i32`, offsets `u64`). A capacity of 0 is not an error: as in the C++,
    /// the batch then has one slot.
    #[error("invalid picture size or capacity")]
    BadSize,
    /// The search range needs vectors that no f_code of the level can code.
    #[error(
        "search range {range2}: its vectors need f_code {f_code}, and the vertical f_code of a \
         frame picture is at most 5 at every level (vertical vector range [-128, 127.5] \
         samples; H.262 Table 8-8, p.112); the range is at most {max}"
    )]
    SearchRange {
        /// The configured range.
        range2: i32,
        /// The f_code it needs.
        f_code: i32,
        /// The largest range this format can code.
        max: i32,
    },
}

// The quarter-resolution search loads whole words around a row: up to `4 * ceil(R / 4)` bytes
// before it and that plus 7 past it (kernels.h, me_search). At the largest range the encoder
// accepts both stay inside the guard bytes of a device buffer, so no load leaves the buffer.
const _: () = assert!(4 * (ME_MAX_RANGE2 as u64).div_ceil(4) + 7 <= GUARD);

/// The f_code a search range needs: the frame search reaches `4 * range2 + 3` full samples plus
/// half a sample; field vectors derived from it stay within half that range vertically, with
/// two half-samples of margin. With field prediction the second seed scales the frame vector by
/// up to 3/2 and searches +-1 around it. One value serves all four f_codes, so the limit of the
/// vertical component of a frame picture binds (H.262 Table 8-8, p.112: at most 5): a range
/// that needs more is refused, never clamped (a clamped code could not code the vectors the
/// search finds).
pub fn f_code_for(range2: i32, field_pred: bool) -> Result<i32, LayoutError> {
    let need = |range2: i32| {
        let mut reach = 4 * i64::from(range2) + 3;
        if field_pred {
            reach = reach * 3 / 2 + 2;
        }
        let mut f_code: i32 = 1;
        while (16i64 << (f_code - 1)) < 2 * reach + 6 && f_code < 16 {
            f_code += 1;
        }
        f_code
    };
    let limit = i32::from(tb_m2v_syntax::profile::MAX_F_CODE_VERTICAL);
    let f_code = need(range2);
    if !(0..=ME_MAX_RANGE2).contains(&range2) || f_code > limit {
        let max = (0..=ME_MAX_RANGE2)
            .rev()
            .find(|r| need(*r) <= limit)
            .unwrap_or(0);
        return Err(LayoutError::SearchRange {
            range2,
            f_code,
            max,
        });
    }
    Ok(f_code)
}

/// Geometry of the batch for a configuration with `capacity` slots (`Geo`, including the
/// f_code derived from the search range), and the block size of the row kernels (`row_bs`).
///
/// As in the C++, a `capacity` of 0 makes a batch of one slot.
pub fn geometry(c: &SessionConfig, capacity: u32) -> Result<(Geo, u32), LayoutError> {
    if c.w == 0 || c.h == 0 || !c.w.is_multiple_of(2) || !c.h.is_multiple_of(2) {
        return Err(LayoutError::BadSize);
    }
    let n = i32::try_from(capacity.max(1)).map_err(|_| LayoutError::BadSize)?;
    let w = i32::try_from(c.w).map_err(|_| LayoutError::BadSize)?;
    let h = i32::try_from(c.h).map_err(|_| LayoutError::BadSize)?;
    let mbw = w.checked_add(15).ok_or(LayoutError::BadSize)? / 16;
    let mbh = if c.interlaced {
        2 * (h.checked_add(31).ok_or(LayoutError::BadSize)? / 32)
    } else {
        h.checked_add(15).ok_or(LayoutError::BadSize)? / 16
    };
    let row_bs = 32 * ((mbw + 31) / 32);
    if row_bs > 256 {
        return Err(LayoutError::TooWide);
    }
    let gw = mbw * 16;
    let gh = mbh.checked_mul(16).ok_or(LayoutError::BadSize)?;
    let nmb = mbw.checked_mul(mbh).ok_or(LayoutError::BadSize)?;
    let f_code = f_code_for(c.range2, c.interlaced && c.field_pred)?;
    let alt_scan = c.alt_scan.unwrap_or(c.interlaced);
    let g = Geo {
        n,
        w: gw,
        h: gh,
        mbw,
        mbh,
        nmb,
        f_code,
        intra_vlc_format: i32::from(c.intra_vlc),
        alt_scan: i32::from(alt_scan),
        interlaced: i32::from(c.interlaced && (c.field_dct || c.field_pred)),
        field_pred: i32::from(c.interlaced && c.field_pred),
        range2: c.range2,
        lambda: c.lambda,
        field_bits: c.field_bits,
        aq_s: c.aq_strength,
        smooth: i32::from(c.smooth),
        tpred: i32::from(c.tpred),
    };
    // row_bs is at most 256 here.
    Ok((g, row_bs.unsigned_abs()))
}

/// The two cursors of the arena path: the device block and the host block.
struct Cursor {
    dpos: u64,
    hpos: u64,
}

impl Cursor {
    /// `dalloc`: the usable region of the next device buffer.
    fn dev(&mut self, bytes: u64) -> Result<DevBuf, LayoutError> {
        let padded = bytes
            .checked_add(2 * GUARD + 255)
            .ok_or(LayoutError::BadSize)?;
        let n = padded & !255;
        let b = DevBuf::new(self.dpos + GUARD, bytes);
        self.dpos = self.dpos.checked_add(n).ok_or(LayoutError::BadSize)?;
        Ok(b)
    }

    /// `halloc`: the next host buffer.
    fn host(&mut self, bytes: u64) -> Result<HostBuf, LayoutError> {
        let n = bytes.max(16).checked_add(63).ok_or(LayoutError::BadSize)? & !63;
        let b = HostBuf::new(self.hpos, bytes);
        self.hpos = self.hpos.checked_add(n).ok_or(LayoutError::BadSize)?;
        Ok(b)
    }
}

/// `a * b` or [`LayoutError::BadSize`].
fn mul(a: u64, b: u64) -> Result<u64, LayoutError> {
    a.checked_mul(b).ok_or(LayoutError::BadSize)
}

/// Size of a record in bytes.
const fn sz<T>() -> u64 {
    size_of::<T>() as u64
}

/// Every buffer of a batch, relative to the start of the batch's block in each arena.
#[derive(Clone, Debug, PartialEq)]
#[allow(
    missing_docs,
    reason = "names are those of the C++ Batch (d_* device, h_* host)"
)]
pub struct BatchLayout {
    pub geo: Geo,
    pub row_bs: u32,
    /// Bytes of the device block.
    pub dev_bytes: u64,
    /// Bytes of the host block.
    pub host_bytes: u64,
    /// 32-bit words reserved per coded picture (`cap_words`).
    pub cap_words: u32,
    pub d_tables: DevBuf,
    pub bufs: BufsRef,
    pub d_brec: Option<[DevBuf; 3]>,
    pub d_info: DevBuf,
    pub d_info_me: DevBuf,
    pub d_act: DevBuf,
    pub d_coef: DevBuf,
    pub d_lev: DevBuf,
    pub d_pred: DevBuf,
    pub d_nz: DevBuf,
    pub d_mbq: DevBuf,
    pub d_hist: DevBuf,
    pub d_nzmask: DevBuf,
    pub d_cmask: DevBuf,
    pub d_rowstat: DevBuf,
    pub d_rowq: DevBuf,
    pub d_upslot: DevBuf,
    pub d_par: DevBuf,
    pub d_list: [DevBuf; 8],
    pub d_devslot: DevBuf,
    pub d_code: DevBuf,
    pub d_scratch: DevBuf,
    pub d_mbbits: DevBuf,
    pub d_mboff: DevBuf,
    pub d_mbtex: DevBuf,
    pub d_slice_bytes: DevBuf,
    pub d_slice_tex: DevBuf,
    pub d_slice_off: DevBuf,
    pub d_pic: DevBuf,
    pub d_out: DevBuf,
    pub d_outpos: DevBuf,
    pub h_list: [HostBuf; 8],
    pub h_devslot: HostBuf,
    pub h_recon: Option<HostBuf>,
    pub h_out: HostBuf,
    pub h_outpos: HostBuf,
    pub h_rowq: HostBuf,
    pub h_upslot: HostBuf,
    pub h_rowstat: HostBuf,
    pub h_par: HostBuf,
    pub h_pic: HostBuf,
    /// One frame of input per slot for the host upload path (`w * h * 3 / 2` bytes each).
    pub h_frames: HostBuf,
}

impl BatchLayout {
    /// The layout for a configuration, in the allocation order and with the rounding of the
    /// C++ arena path (device: guard on both sides, rounded up to 256; host: at least 16 bytes,
    /// rounded up to 64), so that `dev_bytes` equals the C++ `Batch::dev_bytes`.
    pub fn new(c: &SessionConfig, capacity: u32, keep_recon: bool) -> Result<Self, LayoutError> {
        let (geo, row_bs) = geometry(c, capacity)?;
        // geometry() checked that every one of these is positive.
        let n = u64::from(geo.n.unsigned_abs());
        let mbh = u64::from(geo.mbh.unsigned_abs());
        let fbytes = mul(mul(u64::from(c.w), u64::from(c.h))?, 3)? / 2;
        let ysz = mul(
            u64::from(geo.w.unsigned_abs()),
            u64::from(geo.h.unsigned_abs()),
        )?;
        let csz = ysz / 4;
        let nmbtot = mul(n, u64::from(geo.nmb.unsigned_abs()))?;
        let nblk = mul(nmbtot, 6)?;
        let cap_words = u32::try_from(fbytes / 4).map_err(|_| LayoutError::BadSize)?;
        let out_bytes = mul(mul(n, u64::from(cap_words) + 2)?, 4)?;
        let rows = mul(n, mbh)?;

        let mut a = Cursor { dpos: 0, hpos: 0 };
        let d_tables = a.dev(sz::<Tables>())?;
        let ring_stride = ysz + 2 * csz;
        let ring_bytes = mul(mul(u64::from(RING), n)?, ring_stride)?;
        let ring = a.dev(ring_bytes)?;
        // The ring planes are one allocation: `ring_u = ring_y + ysz`, `ring_v = ring_u + csz`.
        // Each handle reaches from its first byte to the last byte of that plane in the last
        // frame of the ring.
        let ring_y = DevBuf::new(ring.off, ring_bytes - 2 * csz);
        let ring_u = DevBuf::new(ring.off + ysz, ring_bytes - ysz - csz);
        let ring_v = DevBuf::new(ring.off + ysz + csz, ring_bytes - ysz - csz);
        let cur1 = a.dev(mul(n, ysz)? / 4)?;
        let cur2 = a.dev(mul(n, ysz)? / 16)?;
        let ref_y = a.dev(mul(mul(2, n)?, ysz)?)?;
        let ref_u = a.dev(mul(mul(2, n)?, csz)?)?;
        let ref_v = a.dev(mul(mul(2, n)?, csz)?)?;
        let ref_y1 = a.dev(mul(mul(2, n)?, ysz)? / 4)?;
        let ref_y2 = a.dev(mul(mul(2, n)?, ysz)? / 16)?;
        let bufs = BufsRef {
            ring_y,
            ring_u,
            ring_v,
            ring_stride,
            cur1,
            cur2,
            ref_y,
            ref_u,
            ref_v,
            ref_y1,
            ref_y2,
        };
        let d_info = a.dev(mul(nmbtot, sz::<MbInfo>())?)?;
        let d_info_me = a.dev(mul(nmbtot, sz::<MbInfo>())?)?;
        let d_act = a.dev(mul(nmbtot, 2)?)?;
        // Whole groups of blocks (`tb_kernel_abi::lev_at`, `lev_len`).
        let group = tb_kernel_abi::LEV_GROUP as u64;
        let lev_bytes = mul(mul(nblk.div_ceil(group), group)?, 64 * 2)?;
        let d_coef = a.dev(lev_bytes)?;
        let d_lev = a.dev(lev_bytes)?;
        let d_pred = a.dev(mul(nblk, 64)?)?;
        let d_nz = a.dev(nblk)?;
        let d_mbq = a.dev(nmbtot)?;
        let d_hist = a.dev(mul(nblk, 16)?)?;
        let d_nzmask = a.dev(mul(nblk, 8)?)?;
        let d_cmask = a.dev(mul(nblk, 8)?)?;
        let d_rowstat = a.dev(mul(rows, sz::<RowStat>())?)?;
        let d_rowq = a.dev(rows)?;
        let d_upslot = a.dev(n)?;
        let d_par = a.dev(mul(n, sz::<SessPar>())?)?;
        let int_list = mul(n, sz::<i32>())?;
        let mut d_list = [DevBuf::default(); 8];
        let mut h_list = [HostBuf::default(); 8];
        for (d, h) in d_list.iter_mut().zip(h_list.iter_mut()) {
            *d = a.dev(int_list)?;
            *h = a.host(int_list)?;
        }
        let d_devslot = a.dev(n)?;
        let h_devslot = a.host(n)?;
        let d_code = a.dev(mul(nmbtot, sz::<MbRes>())?)?;
        let d_scratch = a.dev(mul(nmbtot, MB_SCRATCH_WORDS as u64 * 4)?)?;
        let d_mbbits = a.dev(mul(nmbtot, 4)?)?;
        let d_mboff = a.dev(mul(nmbtot, 4)?)?;
        let d_mbtex = a.dev(mul(nmbtot, 2)?)?;
        let d_slice_bytes = a.dev(mul(rows, 4)?)?;
        let d_slice_tex = a.dev(mul(rows, 4)?)?;
        let d_slice_off = a.dev(mul(rows, 4)?)?;
        let d_pic = a.dev(mul(n, sz::<PicStat>())?)?;
        let d_out = a.dev(out_bytes)?;
        let d_outpos = a.dev(mul(n, 8)?)?;
        let (d_brec, h_recon) = if keep_recon {
            let y = a.dev(mul(n, ysz)?)?;
            let u = a.dev(mul(n, csz)?)?;
            let v = a.dev(mul(n, csz)?)?;
            let r = a.host(ysz + 2 * csz)?;
            (Some([y, u, v]), Some(r))
        } else {
            (None, None)
        };
        let h_out = a.host(out_bytes)?;
        let h_outpos = a.host(mul(n, 8)?)?;
        let h_rowq = a.host(rows)?;
        let h_upslot = a.host(n)?;
        let h_rowstat = a.host(mul(rows, sz::<RowStat>())?)?;
        let h_par = a.host(mul(n, sz::<SessPar>())?)?;
        let h_pic = a.host(mul(n, sz::<PicStat>())?)?;
        let h_frames = a.host(mul(n, fbytes)?)?;

        Ok(Self {
            geo,
            row_bs,
            dev_bytes: a.dpos,
            host_bytes: a.hpos,
            cap_words,
            d_tables,
            bufs,
            d_brec,
            d_info,
            d_info_me,
            d_act,
            d_coef,
            d_lev,
            d_pred,
            d_nz,
            d_mbq,
            d_hist,
            d_nzmask,
            d_cmask,
            d_rowstat,
            d_rowq,
            d_upslot,
            d_par,
            d_list,
            d_devslot,
            d_code,
            d_scratch,
            d_mbbits,
            d_mboff,
            d_mbtex,
            d_slice_bytes,
            d_slice_tex,
            d_slice_off,
            d_pic,
            d_out,
            d_outpos,
            h_list,
            h_devslot,
            h_recon,
            h_out,
            h_outpos,
            h_rowq,
            h_upslot,
            h_rowstat,
            h_par,
            h_pic,
            h_frames,
        })
    }

    /// The same layout moved to its place in the arenas.
    pub fn at(&self, dev_off: u64, host_off: u64) -> Self {
        let mut l = self.clone();
        l.for_each_mut(
            &mut |d: &mut DevBuf| d.off += dev_off,
            &mut |h: &mut HostBuf| h.off += host_off,
        );
        l
    }

    /// Bytes of one input frame (`fbytes`: `w * h * 3 / 2` of the source size).
    pub fn fbytes(&self) -> u64 {
        self.h_frames.len / u64::from(self.geo.n.unsigned_abs().max(1))
    }

    /// The device allocations in the order the C++ makes them, each as its usable region
    /// (the three ring planes as the one ring allocation).
    pub fn dev_allocs(&self) -> Vec<DevBuf> {
        let b = &self.bufs;
        let ring = DevBuf::new(b.ring_y.off, b.ring_v.end() - b.ring_y.off);
        let mut v = vec![
            self.d_tables,
            ring,
            b.cur1,
            b.cur2,
            b.ref_y,
            b.ref_u,
            b.ref_v,
            b.ref_y1,
            b.ref_y2,
            self.d_info,
            self.d_info_me,
            self.d_act,
            self.d_coef,
            self.d_lev,
            self.d_pred,
            self.d_nz,
            self.d_mbq,
            self.d_hist,
            self.d_nzmask,
            self.d_cmask,
            self.d_rowstat,
            self.d_rowq,
            self.d_upslot,
            self.d_par,
        ];
        v.extend_from_slice(&self.d_list);
        v.extend_from_slice(&[
            self.d_devslot,
            self.d_code,
            self.d_scratch,
            self.d_mbbits,
            self.d_mboff,
            self.d_mbtex,
            self.d_slice_bytes,
            self.d_slice_tex,
            self.d_slice_off,
            self.d_pic,
            self.d_out,
            self.d_outpos,
        ]);
        if let Some(r) = self.d_brec {
            v.extend_from_slice(&r);
        }
        v
    }

    /// The host allocations in the order the C++ makes them, then `h_frames`.
    pub fn host_allocs(&self) -> Vec<HostBuf> {
        let mut v = self.h_list.to_vec();
        v.push(self.h_devslot);
        v.extend(self.h_recon);
        v.extend_from_slice(&[
            self.h_out,
            self.h_outpos,
            self.h_rowq,
            self.h_upslot,
            self.h_rowstat,
            self.h_par,
            self.h_pic,
            self.h_frames,
        ]);
        v
    }

    /// What the C++ reports as `dev_bytes` when it allocates without the arena: the sum of
    /// `bytes + 2 * GUARD` over the device allocations.
    pub fn dev_bytes_unrounded(&self) -> u64 {
        self.dev_allocs().iter().map(|b| b.len + 2 * GUARD).sum()
    }

    fn for_each_mut(
        &mut self,
        dev: &mut impl FnMut(&mut DevBuf),
        host: &mut impl FnMut(&mut HostBuf),
    ) {
        let b = &mut self.bufs;
        for d in [
            &mut b.ring_y,
            &mut b.ring_u,
            &mut b.ring_v,
            &mut b.cur1,
            &mut b.cur2,
            &mut b.ref_y,
            &mut b.ref_u,
            &mut b.ref_v,
            &mut b.ref_y1,
            &mut b.ref_y2,
            &mut self.d_tables,
            &mut self.d_info,
            &mut self.d_info_me,
            &mut self.d_act,
            &mut self.d_coef,
            &mut self.d_lev,
            &mut self.d_pred,
            &mut self.d_nz,
            &mut self.d_mbq,
            &mut self.d_hist,
            &mut self.d_nzmask,
            &mut self.d_cmask,
            &mut self.d_rowstat,
            &mut self.d_rowq,
            &mut self.d_upslot,
            &mut self.d_par,
            &mut self.d_devslot,
            &mut self.d_code,
            &mut self.d_scratch,
            &mut self.d_mbbits,
            &mut self.d_mboff,
            &mut self.d_mbtex,
            &mut self.d_slice_bytes,
            &mut self.d_slice_tex,
            &mut self.d_slice_off,
            &mut self.d_pic,
            &mut self.d_out,
            &mut self.d_outpos,
        ] {
            dev(d);
        }
        self.d_list.iter_mut().for_each(&mut *dev);
        if let Some(r) = &mut self.d_brec {
            r.iter_mut().for_each(&mut *dev);
        }
        for h in [
            &mut self.h_devslot,
            &mut self.h_out,
            &mut self.h_outpos,
            &mut self.h_rowq,
            &mut self.h_upslot,
            &mut self.h_rowstat,
            &mut self.h_par,
            &mut self.h_pic,
            &mut self.h_frames,
        ] {
            host(h);
        }
        self.h_list.iter_mut().for_each(&mut *host);
        if let Some(r) = &mut self.h_recon {
            host(r);
        }
    }
}
