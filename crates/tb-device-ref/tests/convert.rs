//! The conversion, composition and color kernels (`kernels/pipe_kernels.h`) on
//! the reference adapter, each against a Rust model written from its definition, sample by
//! sample. The kernels are integer arithmetic, so every comparison is exact.
//!
//! The plans are made as the pipeline makes them: `tb_convert::Converter` decides the
//! launches, `tb_convert::Lowerer` turns them into steps with the records of
//! `tb_device::kernel`. Sources lie in a foreign region (the stand-in for a decoder surface)
//! or in the arena; job records go up in handle form (`Op::UploadRefs`).
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

use tb_convert::{
    Axis, ConvBuf, ConvClass, ConvKey, ConvOp, Converter, Dest, Filter, Lowerer, OverlayPlanes,
    PreId, RectJob, Rows, Source, make_csc,
};
use tb_device::kernel::DevRef;
use tb_device::{ArenaSizes, DevBuf, Device, HostBuf, Op, Plan};
use tb_device_ref::RefDevice;

/// A source surface: NV12 (`bps` 1) or P010 (`bps` 2, value in the high bits), pitches in bytes.
#[derive(Clone)]
struct Surface {
    w: u32,
    h: u32,
    bps: u32,
    pitch: u32,
    y: Vec<u8>,
    uv: Vec<u8>,
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 33) as u32
    }
}

impl Surface {
    /// Smooth content plus noise, so that filters have something to do and clamps are reached.
    fn new(w: u32, h: u32, bps: u32, seed: u64) -> Self {
        let pitch = (w * bps + 15) & !15;
        let mut r = Rng(seed);
        let mut s = Self {
            w,
            h,
            bps,
            pitch,
            y: vec![0; (pitch * h) as usize],
            uv: vec![0; (pitch * h / 2) as usize],
        };
        for row in 0..h {
            for x in 0..w {
                let v = match r.next() % 8 {
                    0 => 255,
                    1 => 0,
                    _ => ((x * 3 + row * 5) % 256 + r.next() % 32).min(255),
                };
                s.put(false, row, x, v);
            }
        }
        for row in 0..h / 2 {
            for x in 0..w {
                let v = ((x * 7 + row * 11) % 256 + r.next() % 16).min(255);
                s.put(true, row, x, v);
            }
        }
        s
    }
    /// Store an 8-bit value `v` as a sample (for P010: as a 10-bit value with a random low
    /// part, in the high bits of the word).
    fn put(&mut self, chroma: bool, row: u32, x: u32, v: u32) {
        let at = (row * self.pitch + x * self.bps) as usize;
        let p = if chroma { &mut self.uv } else { &mut self.y };
        if self.bps == 1 {
            p[at] = v as u8;
        } else {
            let ten = (v << 2) | (v & 3);
            p[at..at + 2].copy_from_slice(&((ten << 6) as u16).to_le_bytes());
        }
    }
    /// Luma sample as the 16-bit word (bps 2) or the byte (bps 1).
    fn luma(&self, row: u32, x: u32) -> u32 {
        self.raw(&self.y, row, x)
    }
    /// Chroma sample `c` (0: Cb, 1: Cr) of pair `x`.
    fn chroma(&self, row: u32, x: u32, c: u32) -> u32 {
        self.raw(&self.uv, row, 2 * x + c)
    }
    fn raw(&self, p: &[u8], row: u32, x: u32) -> u32 {
        let at = (row * self.pitch + x * self.bps) as usize;
        if self.bps == 1 {
            u32::from(p[at])
        } else {
            u32::from(u16::from_le_bytes([p[at], p[at + 1]]))
        }
    }
}

/// A destination frame: three planes with a pitch, as the encoder's input planes.
#[derive(Clone, PartialEq, Debug)]
struct Frame {
    w: u32,
    h: u32,
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
}

impl Frame {
    fn filled(w: u32, h: u32, byte: u8) -> Self {
        Self {
            w,
            h,
            y: vec![byte; (w * h) as usize],
            u: vec![byte; (w * h / 4) as usize],
            v: vec![byte; (w * h / 4) as usize],
        }
    }
    fn random(w: u32, h: u32, seed: u64) -> Self {
        let mut r = Rng(seed);
        let mut f = Self::filled(w, h, 0);
        for p in [&mut f.y, &mut f.u, &mut f.v] {
            for b in p.iter_mut() {
                *b = match r.next() % 16 {
                    0 => 0,
                    1 => 255,
                    _ => (r.next() % 256) as u8,
                };
            }
        }
        f
    }
}

fn clamp8(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

// ---------------------------------------------------------------- models

/// `k_conv_h` + `k_conv_v`: the separable resampler into the rectangle at (`rx`, `ry`).
fn model_resample(c: &ConvClass, s: &Surface, dst: &mut Frame, rx: u32, ry: u32) {
    let t = c.taps.as_ref().unwrap();
    let dw = c.dw as usize;
    // horizontal pass: the source rows of the lattice into 8-bit intermediate planes
    let h_pass = |taps: &tb_convert::Taps, rows: u32, n: usize, get: &dyn Fn(u32, u32) -> u32| {
        let nt = taps.nt as usize;
        let mut out = vec![0u8; n * rows as usize];
        for row in 0..rows {
            let srow = c.s_off + c.s_step * row;
            for x in 0..n {
                let st = taps.start[x];
                let mut acc = 0i32;
                for k in 0..nt {
                    let v = get(srow, (st + k as i32) as u32) as i32;
                    let v = if c.bps == 1 { v } else { v >> 2 };
                    acc += v * i32::from(taps.coef[x * nt + k]);
                }
                let v = if c.bps == 1 {
                    (acc + 8192) >> 14
                } else {
                    (acc + (1 << 19)) >> 20
                };
                out[row as usize * n + x] = clamp8(v);
            }
        }
        out
    };
    let ty = h_pass(&t.hy, c.th_y, dw, &|r, x| s.luma(r, x));
    let tu = h_pass(&t.hc, c.th_c, dw / 2, &|r, x| s.chroma(r, x, 0));
    let tv = h_pass(&t.hc, c.th_c, dw / 2, &|r, x| s.chroma(r, x, 1));
    // vertical pass: into rows d_off + d_step * k of the rectangle
    let v_pass = |taps: &tb_convert::Taps,
                  tmp: &[u8],
                  n: usize,
                  out: &mut [u8],
                  pitch: usize,
                  ox: usize,
                  oy: usize| {
        let nt = taps.nt as usize;
        for k in 0..taps.n as usize {
            let st = taps.start[k] as usize;
            for x in 0..n {
                let mut acc = 8192i32;
                for i in 0..nt {
                    acc += i32::from(tmp[(st + i) * n + x]) * i32::from(taps.coef[k * nt + i]);
                }
                let row = oy + (c.d_off + c.d_step * k as u32) as usize;
                out[row * pitch + ox + x] = clamp8(acc >> 14);
            }
        }
    };
    let (p, ox, oy) = (dst.w as usize, rx as usize, ry as usize);
    v_pass(&t.vy, &ty, dw, &mut dst.y, p, ox, oy);
    v_pass(&t.vc, &tu, dw / 2, &mut dst.u, p / 2, ox / 2, oy / 2);
    v_pass(&t.vc, &tv, dw / 2, &mut dst.v, p / 2, ox / 2, oy / 2);
}

/// `k_conv_copy`: luma copied, interleaved chroma split, 16-bit samples rounded to 8.
fn model_copy(s: &Surface, dst: &mut Frame, rx: u32, ry: u32) {
    let r8 = |v: u32| {
        if s.bps == 1 {
            v as u8
        } else {
            clamp8(((v + 128) >> 8) as i32)
        }
    };
    let p = dst.w;
    for row in 0..s.h {
        for x in 0..s.w {
            dst.y[((ry + row) * p + rx + x) as usize] = r8(s.luma(row, x));
        }
    }
    for row in 0..s.h / 2 {
        for x in 0..s.w / 2 {
            let at = ((ry / 2 + row) * (p / 2) + rx / 2 + x) as usize;
            dst.u[at] = r8(s.chroma(row, x, 0));
            dst.v[at] = r8(s.chroma(row, x, 1));
        }
    }
}

/// `k_half`: luma 2 x 2 box; chroma [1 4 3]/8 across (left neighbor clamped at the edge) and
/// the mean of two lines down. The result is an 8-bit NV12 frame of half the size.
fn model_half(s: &Surface) -> Surface {
    let (w2, h2) = (s.w / 2, s.h / 2);
    let mut o = Surface {
        w: w2,
        h: h2,
        bps: 1,
        pitch: w2,
        y: vec![0; (w2 * h2) as usize],
        uv: vec![0; (w2 * h2 / 2) as usize],
    };
    for row in 0..h2 {
        for x in 0..w2 {
            let sum = s.luma(2 * row, 2 * x)
                + s.luma(2 * row, 2 * x + 1)
                + s.luma(2 * row + 1, 2 * x)
                + s.luma(2 * row + 1, 2 * x + 1);
            let v = if s.bps == 1 {
                (sum + 2) >> 2
            } else {
                (sum + 512) >> 10
            };
            o.y[(row * w2 + x) as usize] = clamp8(v as i32);
        }
    }
    for row in 0..h2 / 2 {
        for pair in 0..w2 / 2 {
            let (l, c, r) = ((2 * pair).saturating_sub(1), 2 * pair, 2 * pair + 1);
            for comp in 0..2 {
                let line = |rw: u32| {
                    s.chroma(rw, l, comp) + 4 * s.chroma(rw, c, comp) + 3 * s.chroma(rw, r, comp)
                };
                let sum = line(2 * row) + line(2 * row + 1);
                let v = if s.bps == 1 {
                    (sum + 8) >> 4
                } else {
                    (sum + 2048) >> 12
                };
                o.uv[(row * w2 + 2 * pair + comp) as usize] = clamp8(v as i32);
            }
        }
    }
    o
}

/// `k_fill`.
fn model_fill(dst: &mut Frame, rx: u32, ry: u32, w: u32, h: u32, yuv: u32) {
    let p = dst.w;
    for row in 0..h {
        for x in 0..w {
            dst.y[((ry + row) * p + rx + x) as usize] = yuv as u8;
        }
    }
    for row in 0..h / 2 {
        for x in 0..w / 2 {
            let at = ((ry / 2 + row) * (p / 2) + rx / 2 + x) as usize;
            dst.u[at] = (yuv >> 8) as u8;
            dst.v[at] = (yuv >> 16) as u8;
        }
    }
}

/// `k_overlay`: dst + (ov - dst) * a / 255, rounded as `(x * 257 + 32768) >> 16`.
fn model_overlay(dst: &mut Frame, rx: u32, ry: u32, o: &Frame, alpha: &[u8], alpha2: &[u8]) {
    let blend = |d: u8, o: u8, a: u8| {
        let (d, o, a) = (i32::from(d), i32::from(o), i32::from(a));
        (d + (((o - d) * a * 257 + 32768) >> 16)) as u8
    };
    let p = dst.w;
    for row in 0..o.h {
        for x in 0..o.w {
            let at = ((ry + row) * p + rx + x) as usize;
            let i = (row * o.w + x) as usize;
            dst.y[at] = blend(dst.y[at], o.y[i], alpha[i]);
        }
    }
    for row in 0..o.h / 2 {
        for x in 0..o.w / 2 {
            let at = ((ry / 2 + row) * (p / 2) + rx / 2 + x) as usize;
            let i = (row * (o.w / 2) + x) as usize;
            dst.u[at] = blend(dst.u[at], o.u[i], alpha2[i]);
            dst.v[at] = blend(dst.v[at], o.v[i], alpha2[i]);
        }
    }
}

/// `k_csc`: Q12 matrix on (Y, Cb - 128, Cr - 128); each luma sample takes its correction from
/// the chroma sample of its 2 x 2 group (interlaced: of the same field).
fn model_csc(
    f: &mut Frame,
    rx: u32,
    ry: u32,
    w: u32,
    h: u32,
    m: &tb_convert::Csc,
    interlaced: bool,
) {
    let p = f.w;
    for crow in 0..h / 2 {
        let (ly0, ly1) = if interlaced {
            let l = 4 * (crow >> 1) + (crow & 1);
            (l, l + 2)
        } else {
            (2 * crow, 2 * crow + 1)
        };
        for cx in 0..w / 2 {
            let cat = ((ry / 2 + crow) * (p / 2) + rx / 2 + cx) as usize;
            let (cb, cr) = (i32::from(f.u[cat]) - 128, i32::from(f.v[cat]) - 128);
            let ycorr = m.m[1] * cb + m.m[2] * cr + m.off[0] + 2048;
            for ly in [ly0, ly1] {
                for k in 0..2 {
                    let at = ((ry + ly) * p + rx + 2 * cx + k) as usize;
                    f.y[at] = clamp8((m.m[0] * i32::from(f.y[at]) + ycorr) >> 12);
                }
            }
            f.u[cat] = clamp8((m.m[4] * cb + m.m[5] * cr + m.off[1] + 2048) >> 12);
            f.v[cat] = clamp8((m.m[7] * cb + m.m[8] * cr + m.off[2] + 2048) >> 12);
        }
    }
}

// ---------------------------------------------------------------- the device side

/// Where things are in the test's device arena.
struct Rig {
    dev: RefDevice,
    conv: Converter<DevRef>,
    low: Lowerer,
    frame: (u32, u32),
    /// Planes of the destination frame, each with guard bytes around it.
    planes: [DevBuf; 3],
    /// Arena region for a source or overlay planes, when a test wants them in the arena.
    extra: DevBuf,
    host_frame: HostBuf,
    host_extra: HostBuf,
}

const GUARD: u64 = 256;
const GUARD_BYTE: u8 = 0x5a;

impl Rig {
    fn new(fw: u32, fh: u32, start: &Frame) -> Self {
        let luma = u64::from(fw * fh);
        let mut at = GUARD;
        let mut plane = |len: u64| {
            let b = DevBuf::new(at, len);
            at += len + GUARD;
            b
        };
        let planes = [plane(luma), plane(luma / 4), plane(luma / 4)];
        let extra = plane(1 << 20);
        let conv_dev = DevBuf::new(at, 8 << 20);
        let device = conv_dev.end();
        let frame_bytes = luma * 3 / 2;
        let host_frame = HostBuf::new(0, device.min(4 << 20));
        let host_extra = HostBuf::new(host_frame.end(), 1 << 20);
        let scratch = HostBuf::new(host_extra.end(), 2 << 20);
        let mut dev = RefDevice::new(ArenaSizes {
            device,
            host: scratch.end(),
        })
        .unwrap();
        // the start frame goes up, the guards are marked
        let mut p = Plan::new();
        p.main(Op::Fill {
            dst: DevBuf::new(0, conv_dev.off),
            byte: GUARD_BYTE,
        });
        let host = dev.host_mut();
        let mut off = 0u64;
        for (b, bytes) in planes.iter().zip([&start.y, &start.u, &start.v]) {
            host[off as usize..off as usize + bytes.len()].copy_from_slice(bytes);
            p.main(Op::Upload {
                dst: *b,
                src: HostBuf::new(off, b.len),
            });
            off += b.len;
        }
        assert!(off == frame_bytes);
        let t = dev.submit(&p).unwrap();
        dev.wait(t, None).status.unwrap();
        Self {
            dev,
            conv: Converter::new(),
            low: Lowerer::new(conv_dev, scratch),
            frame: (fw, fh),
            planes,
            extra,
            host_frame,
            host_extra,
        }
    }

    /// The destination at the rectangle origin (`rx`, `ry`).
    fn dest(&self, rx: u32, ry: u32) -> Dest<DevRef> {
        let p = u64::from(self.frame.0);
        let (rx, ry) = (u64::from(rx), u64::from(ry));
        Dest {
            y: DevRef::arena(self.planes[0].off + ry * p + rx),
            u: DevRef::arena(self.planes[1].off + (ry / 2) * (p / 2) + rx / 2),
            v: DevRef::arena(self.planes[2].off + (ry / 2) * (p / 2) + rx / 2),
            pitch: self.frame.0,
        }
    }

    /// A surface as a foreign region (`slot`): luma, then chroma.
    fn foreign(&mut self, slot: u16, s: &Surface) -> Source<DevRef> {
        let mem = self.dev.bind_foreign(slot, s.y.len() + s.uv.len());
        mem[..s.y.len()].copy_from_slice(&s.y);
        mem[s.y.len()..].copy_from_slice(&s.uv);
        Source {
            y: DevRef::foreign(slot, 0),
            sp_y: s.pitch,
            uv: DevRef::foreign(slot, s.y.len() as u64),
            sp_uv: s.pitch,
        }
    }

    /// Bytes into the arena's spare region at `off`; where they are.
    fn upload_extra(&mut self, off: u64, bytes: &[u8]) -> DevRef {
        let dst = self.extra.slice(off, bytes.len() as u64).unwrap();
        let src = self.host_extra.slice(0, bytes.len() as u64).unwrap();
        src.of_mut(self.dev.host_mut())
            .unwrap()
            .copy_from_slice(bytes);
        let mut p = Plan::new();
        p.main(Op::Upload { dst, src });
        let t = self.dev.submit(&p).unwrap();
        self.dev.wait(t, None).status.unwrap();
        DevRef::arena(dst.off)
    }

    /// Run conversion operations as one plan.
    fn run(&mut self, ops: &[ConvOp<DevRef>]) {
        let mut p = Plan::new();
        self.low.begin();
        self.low
            .lower(&self.conv, ops, self.dev.host_mut(), &mut p)
            .unwrap();
        let t = self.dev.submit(&p).unwrap();
        self.dev.wait(t, None).status.unwrap();
    }

    fn download(&mut self, b: DevBuf) -> Vec<u8> {
        let dst = HostBuf::new(self.host_frame.off, b.len);
        let mut p = Plan::new();
        p.main(Op::Download { dst, src: b });
        let t = self.dev.submit(&p).unwrap();
        self.dev.wait(t, None).status.unwrap();
        dst.of(self.dev.host()).unwrap().to_vec()
    }

    /// The destination frame; the guard bytes around its planes must be untouched.
    fn frame(&mut self) -> Frame {
        let all = self.download(DevBuf::new(0, self.planes[2].end() + GUARD));
        let mut covered = vec![false; all.len()];
        let mut planes = Vec::new();
        for b in self.planes {
            covered[b.off as usize..b.end() as usize].fill(true);
            planes.push(all[b.off as usize..b.end() as usize].to_vec());
        }
        for (i, (&byte, &c)) in all.iter().zip(&covered).enumerate() {
            assert!(c || byte == GUARD_BYTE, "guard byte {i} was written");
        }
        let v = planes.pop().unwrap();
        let u = planes.pop().unwrap();
        let y = planes.pop().unwrap();
        Frame {
            w: self.frame.0,
            h: self.frame.1,
            y,
            u,
            v,
        }
    }
}

fn key(s: &Surface, dw: u32, dh: u32, filter: Filter) -> ConvKey {
    ConvKey {
        sw: s.w,
        sh: s.h,
        bps: s.bps,
        cx: 0,
        cy: 0,
        cw: s.w,
        ch: s.h,
        dw,
        dh,
        srows: Rows::All,
        drows: Rows::All,
        filter,
        weave_lp: false,
    }
}

fn assert_frames(got: &Frame, want: &Frame) {
    for (name, g, w) in [
        ("Y", &got.y, &want.y),
        ("U", &got.u, &want.u),
        ("V", &got.v, &want.v),
    ] {
        let first = g.iter().zip(w.iter()).position(|(a, b)| a != b);
        assert!(
            first.is_none(),
            "plane {name} differs first at byte {first:?}"
        );
    }
}

/// One conversion job through the device and through the model; both start from `0xa5`
/// everywhere, so rows and columns outside the rectangle (and rows of the other field) must
/// come out untouched.
fn convert_case(s: &Surface, k: ConvKey, frame: (u32, u32), rect: (u32, u32), exact_down: bool) {
    let start = Frame::filled(frame.0, frame.1, 0xa5);
    let mut rig = Rig::new(frame.0, frame.1, &start);
    rig.conv.fast_down = !exact_down;
    let src = rig.foreign(2, s);
    let dst = rig.dest(rect.0, rect.1);
    let mut ops = Vec::new();
    rig.conv.add(k, src, dst, &mut ops).unwrap();
    rig.conv.run(&mut ops);
    rig.run(&ops);
    let got = rig.frame();

    let mut want = start;
    let class = ConvClass::new(k).unwrap();
    assert!(rig.conv.pre_jobs() == 0, "use half_case for the pre-stage");
    if class.copy {
        model_copy(s, &mut want, rect.0, rect.1);
    } else {
        model_resample(&class, s, &mut want, rect.0, rect.1);
    }
    assert_frames(&got, &want);
    assert_ne!(got, Frame::filled(frame.0, frame.1, 0xa5));
}

// ---------------------------------------------------------------- tests

#[test]
fn copy_splits_the_chroma() {
    let s = Surface::new(64, 32, 1, 1);
    convert_case(
        &s,
        key(&s, 64, 32, Filter::Lanczos3),
        (96, 48),
        (16, 8),
        false,
    );
    // the whole frame
    convert_case(
        &s,
        key(&s, 64, 32, Filter::Lanczos3),
        (64, 32),
        (0, 0),
        false,
    );
}

#[test]
fn copy_rounds_sixteen_bit_samples() {
    let s = Surface::new(64, 32, 2, 2);
    convert_case(
        &s,
        key(&s, 64, 32, Filter::Lanczos3),
        (96, 48),
        (8, 4),
        false,
    );
}

#[test]
fn bilinear_two_to_one_has_the_known_taps() {
    let s = Surface::new(128, 64, 1, 3);
    let k = key(&s, 64, 32, Filter::Bilinear);
    // Center-aligned 2:1 with the triangle stretched by 2: samples 2d-1 .. 2d+2 weigh
    // 1/8, 3/8, 3/8, 1/8 (Q14: 2048, 6144, 6144, 2048); the same down the columns.
    let c = ConvClass::new(k).unwrap();
    let t = c.taps.as_ref().unwrap();
    assert_eq!((t.hy.nt, t.vy.nt), (4, 4));
    assert_eq!(&t.hy.coef[4 * 10..4 * 11], [2048, 6144, 6144, 2048]);
    assert_eq!(t.hy.start[10], 19);
    assert_eq!(&t.vy.coef[4 * 5..4 * 6], [2048, 6144, 6144, 2048]);
    assert_eq!(t.vy.start[5], 9);
    convert_case(&s, k, (64, 32), (0, 0), false);
    // On a flat picture every output is the flat value (taps sum to one).
    let mut flat = s.clone();
    flat.y.fill(77);
    flat.uv.fill(131);
    let start = Frame::filled(64, 32, 0);
    let mut rig = Rig::new(64, 32, &start);
    let src = rig.foreign(0, &flat);
    let dst = rig.dest(0, 0);
    let mut ops = Vec::new();
    rig.conv.add(k, src, dst, &mut ops).unwrap();
    rig.conv.run(&mut ops);
    rig.run(&ops);
    let got = rig.frame();
    assert!(got.y.iter().all(|&b| b == 77));
    assert!(got.u.iter().all(|&b| b == 131) && got.v.iter().all(|&b| b == 131));
}

#[test]
fn resampler_matches_the_model_for_every_lattice() {
    let s = Surface::new(96, 64, 1, 4);
    for filter in [Filter::Lanczos3, Filter::Bicubic, Filter::Bilinear] {
        // down and up, frame to frame, into a rectangle of a larger frame
        convert_case(&s, key(&s, 64, 48, filter), (96, 64), (16, 8), false);
        convert_case(&s, key(&s, 160, 96, filter), (176, 112), (8, 8), false);
    }
    for (srows, drows, lp) in [
        (Rows::Top, Rows::Top, false), // field to field
        (Rows::Bottom, Rows::Bottom, false),
        (Rows::Top, Rows::All, false), // bob
        (Rows::Bottom, Rows::All, false),
        (Rows::All, Rows::Top, true), // frame to field with the low-pass
        (Rows::All, Rows::Bottom, true),
        (Rows::All, Rows::Bottom, false),
    ] {
        let mut k = key(&s, 64, 48, Filter::Lanczos3);
        (k.srows, k.drows, k.weave_lp) = (srows, drows, lp);
        convert_case(&s, k, (80, 48), (8, 0), false);
        // the same lattice at the source's size (weaving: not a copy)
        let mut k = key(&s, 96, 64, Filter::Lanczos3);
        (k.srows, k.drows, k.weave_lp) = (srows, drows, lp);
        convert_case(&s, k, (96, 64), (0, 0), false);
    }
    // a crop of the source
    let mut k = key(&s, 64, 32, Filter::Lanczos3);
    (k.cx, k.cy, k.cw, k.ch) = (8, 4, 72, 48);
    convert_case(&s, k, (64, 32), (0, 0), false);
}

#[test]
fn resampler_rounds_ten_bit_sources() {
    let s = Surface::new(96, 64, 2, 5);
    convert_case(
        &s,
        key(&s, 64, 48, Filter::Lanczos3),
        (64, 48),
        (0, 0),
        false,
    );
    convert_case(
        &s,
        key(&s, 128, 96, Filter::Bicubic),
        (128, 96),
        (0, 0),
        false,
    );
    let mut k = key(&s, 64, 32, Filter::Lanczos3);
    (k.srows, k.drows) = (Rows::Top, Rows::Top);
    convert_case(&s, k, (64, 32), (0, 0), false);
}

#[test]
fn large_ratios_without_the_half_stage() {
    let s = Surface::new(192, 96, 1, 6);
    convert_case(
        &s,
        key(&s, 32, 16, Filter::Lanczos3),
        (32, 16),
        (0, 0),
        true,
    );
}

/// The pre-stage: `k_half`, then the resampler on the half-size frame.
fn half_case(bps: u32, seed: u64) {
    let s = Surface::new(192, 96, bps, seed);
    let k = key(&s, 32, 16, Filter::Lanczos3);
    let start = Frame::filled(48, 32, 0xa5);
    let mut rig = Rig::new(48, 32, &start);
    // two jobs of the same source size into two rectangles: one launch of each kernel
    let src = rig.foreign(1, &s);
    let s2 = Surface::new(192, 96, bps, seed + 100);
    let src2 = rig.foreign(4, &s2);
    let mut ops = Vec::new();
    rig.conv.add(k, src, rig.dest(0, 0), &mut ops).unwrap();
    rig.conv.add(k, src2, rig.dest(16, 16), &mut ops).unwrap();
    rig.conv.run(&mut ops);
    assert_eq!(rig.conv.pre_jobs(), 2);
    let launches: Vec<&str> = ops
        .iter()
        .filter_map(|o| match o {
            ConvOp::Launch(l) => Some(l.kernel()),
            _ => None,
        })
        .collect();
    assert_eq!(launches, ["k_half", "k_conv_h", "k_conv_v"]);
    rig.run(&ops);

    let (h1, h2) = (model_half(&s), model_half(&s2));
    // the half-size frames on the device: luma then interleaved chroma, pitch w / 2
    let pre = rig
        .low
        .buf(ConvBuf::Pre {
            set: PreId(0),
            block: 0,
        })
        .unwrap();
    let stride = (96 * 48 * 3 / 2) as usize;
    let got = rig.download(pre);
    for (i, h) in [&h1, &h2].iter().enumerate() {
        let f = &got[i * stride..(i + 1) * stride];
        assert_eq!(&f[..96 * 48], &h.y[..], "half luma of job {i}");
        assert_eq!(&f[96 * 48..], &h.uv[..], "half chroma of job {i}");
    }
    let mut want = start;
    let mut hk = key(&h1, 32, 16, Filter::Lanczos3);
    hk.bps = 1;
    let class = ConvClass::new(hk).unwrap();
    model_resample(&class, &h1, &mut want, 0, 0);
    model_resample(&class, &h2, &mut want, 16, 16);
    assert_frames(&rig.frame(), &want);
}

#[test]
fn half_stage_eight_bit() {
    half_case(1, 7);
}

#[test]
fn half_stage_ten_bit() {
    half_case(2, 8);
}

#[test]
fn fill_writes_the_rectangle_only() {
    let start = Frame::random(96, 48, 9);
    let mut rig = Rig::new(96, 48, &start);
    let yuv = 16 | (128 << 8) | (240 << 16);
    let mut ops = Vec::new();
    let j = RectJob {
        dst: rig.dest(8, 4),
        w: 64,
        h: 32,
        yuv,
        overlay: None,
    };
    rig.conv.fill(j, &mut ops);
    rig.run(&ops);
    let mut want = start;
    model_fill(&mut want, 8, 4, 64, 32, yuv);
    assert_frames(&rig.frame(), &want);
}

#[test]
fn overlay_blends_with_alpha() {
    let start = Frame::random(96, 48, 10);
    let mut rig = Rig::new(96, 48, &start);
    let (w, h) = (64u32, 32u32);
    let ov = Frame::random(w, h, 11);
    // alpha: all values, with runs of zero (the kernel skips words whose alpha is zero)
    let mut r = Rng(12);
    let mut alpha = vec![0u8; (w * h) as usize];
    for (i, a) in alpha.iter_mut().enumerate() {
        *a = match (i / 16) % 4 {
            0 => 0,
            1 => 255,
            _ => (r.next() % 256) as u8,
        };
    }
    let alpha2: Vec<u8> = (0..(w * h / 4) as usize)
        .map(|i| {
            if (i / 8) % 3 == 0 {
                0
            } else {
                (r.next() % 256) as u8
            }
        })
        .collect();
    let mut at = 0u64;
    let mut up = |rig: &mut Rig, bytes: &[u8]| {
        let d = rig.upload_extra(at, bytes);
        at += (bytes.len() as u64 + 255) & !255;
        d
    };
    let overlay = OverlayPlanes {
        oy: up(&mut rig, &ov.y),
        ou: up(&mut rig, &ov.u),
        ov: up(&mut rig, &ov.v),
        oa: up(&mut rig, &alpha),
        oa2: up(&mut rig, &alpha2),
    };
    let mut ops = Vec::new();
    let j = RectJob {
        dst: rig.dest(16, 8),
        w,
        h,
        yuv: 0,
        overlay: Some(overlay),
    };
    rig.conv.overlay(j, &mut ops);
    rig.run(&ops);
    let mut want = start;
    model_overlay(&mut want, 16, 8, &ov, &alpha, &alpha2);
    assert_frames(&rig.frame(), &want);
}

#[test]
fn color_matrix_on_flat_and_random_pictures() {
    for (from709, to709, full) in [
        (true, false, false),
        (false, true, false),
        (true, true, true),
        (false, true, true),
    ] {
        let m = make_csc(from709, to709, full);
        for interlaced in [false, true] {
            // A flat picture: every sample of a plane gets the same value, the matrix applied
            // to (Y, Cb, Cr) once.
            let mut flat = Frame::filled(64, 32, 0);
            flat.y.fill(180);
            flat.u.fill(90);
            flat.v.fill(200);
            let mut rig = Rig::new(64, 32, &flat);
            let j = RectJob {
                dst: rig.dest(0, 0),
                w: 64,
                h: 32,
                yuv: 0,
                overlay: None,
            };
            let mut ops = Vec::new();
            rig.conv.csc(j, m, interlaced, &mut ops);
            rig.run(&ops);
            let got = rig.frame();
            let (cb, cr) = (90 - 128, 200 - 128);
            let y = clamp8((m.m[0] * 180 + m.m[1] * cb + m.m[2] * cr + m.off[0] + 2048) >> 12);
            let u = clamp8((m.m[4] * cb + m.m[5] * cr + m.off[1] + 2048) >> 12);
            let v = clamp8((m.m[7] * cb + m.m[8] * cr + m.off[2] + 2048) >> 12);
            assert!(got.y.iter().all(|&b| b == y), "flat luma");
            assert!(
                got.u.iter().all(|&b| b == u) && got.v.iter().all(|&b| b == v),
                "flat chroma"
            );

            // Random content in a rectangle of a larger frame.
            let start = Frame::random(96, 64, 13);
            let mut rig = Rig::new(96, 64, &start);
            let j = RectJob {
                dst: rig.dest(16, 8),
                w: 64,
                h: 48,
                yuv: 0,
                overlay: None,
            };
            let mut ops = Vec::new();
            rig.conv.csc(j, m, interlaced, &mut ops);
            rig.run(&ops);
            let mut want = start;
            model_csc(&mut want, 16, 8, 64, 48, &m, interlaced);
            assert_frames(&rig.frame(), &want);
        }
    }
}

/// A source in the arena instead of a foreign region gives the same picture, and a handle that
/// names an unbound slot is refused with the plan.
#[test]
fn sources_in_the_arena_and_unbound_slots() {
    let s = Surface::new(64, 32, 1, 14);
    let k = key(&s, 64, 32, Filter::Lanczos3);
    let start = Frame::filled(64, 32, 0xa5);
    let mut rig = Rig::new(64, 32, &start);
    let y = rig.upload_extra(0, &s.y);
    let uv = rig.upload_extra(s.y.len() as u64, &s.uv);
    let src = Source {
        y,
        sp_y: s.pitch,
        uv,
        sp_uv: s.pitch,
    };
    let mut ops = Vec::new();
    rig.conv.add(k, src, rig.dest(0, 0), &mut ops).unwrap();
    rig.conv.run(&mut ops);
    rig.run(&ops);
    let mut want = start;
    model_copy(&s, &mut want, 0, 0);
    assert_frames(&rig.frame(), &want);

    let bad = Source {
        y: DevRef::foreign(9, 0),
        sp_y: s.pitch,
        uv: DevRef::foreign(9, 2048),
        sp_uv: s.pitch,
    };
    let mut ops = Vec::new();
    rig.conv.add(k, bad, rig.dest(0, 0), &mut ops).unwrap();
    rig.conv.run(&mut ops);
    let mut p = Plan::new();
    rig.low.begin();
    rig.low
        .lower(&rig.conv, &ops, rig.dev.host_mut(), &mut p)
        .unwrap();
    let t = rig.dev.submit(&p).unwrap();
    let c = rig.dev.wait(t, None);
    assert!(matches!(
        c.status,
        Err(tb_device::DeviceError::BadPlan { .. })
    ));
    let _ = Axis::ALL;
}
