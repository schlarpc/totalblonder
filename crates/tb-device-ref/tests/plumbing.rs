//! The reference adapter executes the protocol's self-test with the kernel's real code, and the
//! same run replays through the recording device.
#![allow(clippy::print_stdout, reason = "the digest is the test's report")]

use tb_device::sim::{Fault, Recorder, Replay};
use tb_device::{Device, DeviceError, selftest};
use tb_device_ref::{RefDevice, library_kernels, library_param_sizes};

#[test]
fn pyramid_matches_the_definition() {
    let mut dev = RefDevice::new(selftest::ARENAS).unwrap();
    let out = selftest::run(&mut dev).unwrap();
    let (d1, d2) = selftest::expected(&selftest::source());
    assert_eq!(out.d1, d1);
    assert_eq!(out.d2, d2);
    assert_eq!(selftest::digest(&out), selftest::EXPECTED_DIGEST);
    println!(
        "k_pyr on the reference adapter: digest {:016x}",
        selftest::digest(&out)
    );
}

#[test]
fn recorded_run_replays_and_faults_inject() {
    let mut rec = Recorder::new(RefDevice::new(selftest::ARENAS).unwrap());
    let live = selftest::run(&mut rec).unwrap();
    let (trace, _) = rec.finish();
    assert_eq!(trace.records.len(), 1);

    let mut replay = Replay::new(trace.clone());
    let again = selftest::run(&mut replay).unwrap();
    assert_eq!((again.d1, again.d2), (live.d1, live.d2));

    let fault = Fault::Fail {
        submit: 0,
        error: DeviceError::Lost { code: 999 },
    };
    let mut broken = Replay::with_faults(trace, vec![fault]);
    assert_eq!(
        selftest::run(&mut broken),
        Err(DeviceError::Lost { code: 999 })
    );
}

#[test]
fn a_bad_plan_is_rejected_whole() {
    let mut dev = RefDevice::new(selftest::ARENAS).unwrap();
    let mut p = selftest::plan();
    if let Some(tb_device::Step {
        op: tb_device::Op::Fill { dst, .. },
        ..
    }) = p.steps.first_mut()
    {
        dst.len += 1;
    }
    assert!(matches!(
        dev.submit(&p),
        Err(DeviceError::BadPlan { step: 0, .. })
    ));
    // Nothing ran: the arena is still zero and the device takes the next plan.
    assert!(selftest::run(&mut dev).is_ok());
}

/// The library has every kernel of the protocol, and every kernel takes parameters of the sizes
/// the protocol passes (the launch signature check of docs/ARCHITECTURE.md §4.3).
#[test]
fn kernel_signatures_agree() {
    let lib = library_kernels();
    assert_eq!(lib.len(), Kernel::NAMES.len(), "{lib:?}");
    use tb_device::kernel::*;
    let d = tb_device::DevBuf::default();
    let g = tb_kernel_abi::Geo::default();
    let b = BufsRef::default();
    #[rustfmt::skip]
    let all = [
        Kernel::Tick(Tick {}),
        Kernel::PadCols(PadCols { plane: d, stride: 0, pw: 0, sw: 0, sh: 0, n: 0, list: d, slot: d, na: 0 }),
        Kernel::PadRows(PadRows { plane: d, stride: 0, pw: 0, sh: 0, ph: 0, n: 0, list: d, slot: d, na: 0 }),
        Kernel::Pyr(Pyr { src: d, sstride: 0, sw: 0, sh: 0, d1: d, d1stride: 0, d2: d, d2stride: 0, n: 0, list: d, na: 0, par: d, ssel: 0, dsel: 0 }),
        Kernel::SetMode(SetMode { info: d, nmb: 0, list: d, na: 0, mode: 0 }),
        Kernel::MbStat(MbStat { g, ring_y: d, ring_stride: 0, par: d, list: d, na: 0, info: d, act: d }),
        Kernel::Me(Me { g, b, par: d, list: d, na: 0, prev: d, out: d }),
        Kernel::MvSmooth(MvSmooth { g, b, par: d, list: d, na: 0, input: d, out: d }),
        Kernel::Xf(Xf { g, tables: d, b, par: d, list: d, na: 0, info: d, act: d, coef: d, predbuf: d, hist: d, cmask: d }),
        Kernel::Xf16(Xf16 { g, tables: d, b, par: d, list: d, na: 0, info: d, act: d, coef: d, predbuf: d, hist: d, cmask: d }),
        Kernel::Xf32(Xf32 { g, tables: d, b, par: d, list: d, na: 0, info: d, act: d, coef: d, predbuf: d, hist: d, cmask: d }),
        Kernel::Xf64(Xf64 { g, tables: d, b, par: d, list: d, na: 0, info: d, act: d, coef: d, predbuf: d, hist: d, cmask: d }),
        Kernel::RowStat(RowStat { g, list: d, na: 0, hist: d, act: d, info: d, out: d }),
        Kernel::Quant(Quant { g, tables: d, par: d, list: d, na: 0, rowq: d, info: d, act: d, coef: d, cmask: d, lev: d, nzmask: d, nz: d, mbq: d }),
        Kernel::Recon(Recon { g, tables: d, par: d, list: d, na: 0, lev: d, nzmask: d, nz: d, predbuf: d, info: d, mbq: d, out_y: d, out_u: d, out_v: d, osel: 0 }),
        Kernel::EntResolve(EntResolve { g, par: d, list: d, na: 0, info: d, nz: d, lev: d, code: d }),
        Kernel::EntPack(EntPack { g, tables: d, par: d, list: d, na: 0, code: d, info: d, lev: d, nzmask: d, mbq: d, scratch: d, mbbits: d, mbtex: d }),
        Kernel::EntRowSum(EntRowSum { g, list: d, na: 0, mbbits: d, mbtex: d, mboff: d, slice_bytes: d, slice_tex: d }),
        Kernel::EntPicSum(EntPicSum { g, list: d, na: 0, slice_bytes: d, slice_tex: d, slice_off: d, pic: d }),
        Kernel::EntScatter(EntScatter { g, list: d, na: 0, code: d, scratch: d, mbbits: d, mboff: d, slice_off: d, out: d, outpos: d }),
        Kernel::ConvH(ConvH { c: ConvClassRef::default(), jobs: d, nj: 0, tmp: d }),
        Kernel::ConvV(ConvV { c: ConvClassRef::default(), jobs: d, nj: 0, tmp: d }),
        Kernel::ConvCopy(ConvCopy { c: ConvClassRef::default(), jobs: d, nj: 0 }),
        Kernel::Half(Half { jobs: d, nj: 0, w2: 0, h2: 0, bps: 0, pre: d, stride: 0 }),
        Kernel::FillRect(FillRect { j: RectRef::default() }),
        Kernel::Overlay(Overlay { j: RectRef::default() }),
        Kernel::Csc(Csc { j: RectRef::default(), m: tb_kernel_abi::CscPar::default() }),
    ];
    assert_eq!(all.len(), Kernel::NAMES.len());
    for k in all {
        let i = lib.iter().position(|n| n == k.name()).unwrap();
        let ours: Vec<usize> = k.args().resolve(0).sizes().collect();
        assert_eq!(library_param_sizes(i), ours, "{}", k.name());
    }
}
