//! Helpers shared by the integration tests of the tick: the driver loop, configurations from
//! golden records, SHA-256.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    dead_code,
    reason = "test helpers outside #[test] functions; each test file uses a part"
)]

use tb_device::{Completion, Device, DeviceError, Nanos, Plan, Ticket};
use tb_golden::Record;
use tb_m2v_encoder::{Encoder, Phase, SessionConfig, Step};

/// The configuration of an `open` record, as the C++ `SessionConfig`.
pub fn config(o: &Record) -> SessionConfig {
    let int = |f: &str| o.int(f).unwrap();
    let alt = int("alt_scan");
    SessionConfig {
        w: int("w").try_into().unwrap(),
        h: int("h").try_into().unwrap(),
        fps_n: int("fps_n").try_into().unwrap(),
        fps_d: int("fps_d").try_into().unwrap(),
        interlaced: o.flag("interlaced").unwrap(),
        tff: o.flag("tff").unwrap(),
        gop: int("gop").try_into().unwrap(),
        m: int("m").try_into().unwrap(),
        gop_phase: int("gop_phase").try_into().unwrap(),
        max_bitrate: int("max_bitrate").try_into().unwrap(),
        vbv_bits: int("vbv_bits").try_into().unwrap(),
        bitrate: int("bitrate").try_into().unwrap(),
        fixed_q: [
            int("qi").try_into().unwrap(),
            int("qp").try_into().unwrap(),
            int("qb").try_into().unwrap(),
        ],
        aq: o.flag("aq").unwrap(),
        aq_strength: o.num("aq_strength").unwrap() as f32,
        field_pred: o.flag("field_pred").unwrap(),
        field_dct: o.flag("field_dct").unwrap(),
        range2: int("range2").try_into().unwrap(),
        lambda: int("lambda").try_into().unwrap(),
        field_bits: int("field_bits").try_into().unwrap(),
        smooth: o.flag("smooth").unwrap(),
        tpred: o.flag("tpred").unwrap(),
        intra_vlc: o.flag("intra_vlc").unwrap(),
        alt_scan: (alt >= 0).then_some(alt != 0),
        capacity: int("capacity").try_into().unwrap(),
        priority: int("priority").try_into().unwrap(),
        aspect: int("aspect").try_into().unwrap(),
        profile: tb_m2v_syntax::profile::Profile::from_name(o.text("profile").unwrap()).unwrap(),
        color: match o.ints("color").unwrap().as_slice() {
            [p, t, m] => Some(tb_m2v_syntax::profile::Color {
                primaries: *p as u8,
                transfer: *t as u8,
                matrix: *m as u8,
            }),
            _ => None,
        },
    }
}

/// A legal `cc_data()` of a 29.97 Hz frame: the CEA-608 pairs of both fields, then DTVCC
/// padding, 20 constructs (CTA-708-E Table 4).
pub fn captions() -> Vec<u8> {
    let mut cc = vec![0xFC, 0x80, 0x80, 0xFD, 0x80, 0x80];
    while cc.len() < 60 {
        cc.extend([0xFA, 0, 0]);
    }
    cc
}

/// What a driven tick (or settle) saw.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Driven {
    /// Phase of every plan submitted, in order.
    pub phases: Vec<Phase>,
    /// Wait class of every plan submitted.
    pub classes: Vec<u8>,
    /// Errors of completions and refused submits.
    pub errors: Vec<DeviceError>,
    /// Every plan submitted (only when asked for).
    pub plans: Vec<Plan>,
}

/// The driver loop of the module documentation: submit, wait, resume until the tick is done.
/// A refused submit is reported to the encoder as a failed completion.
pub fn drive<D: Device>(
    enc: &mut Encoder,
    dev: &mut D,
    plan: &mut Plan,
    mut step: Step,
    keep_plans: bool,
) -> Driven {
    let mut seen = Driven::default();
    let mut ticket = Ticket(u64::MAX);
    loop {
        match step {
            Step::Done => return seen,
            Step::Run => {
                seen.phases
                    .push(enc.phase().expect("a phase while a plan is out"));
                seen.classes.push(enc.wait_class().unwrap());
                if keep_plans {
                    seen.plans.push(plan.clone());
                }
                match dev.submit(plan) {
                    Ok(t) => ticket = t,
                    Err(e) => {
                        seen.errors.push(e.clone());
                        let now = dev.now();
                        let done = Completion {
                            ticket: Ticket(u64::MAX),
                            status: Err(e),
                            submitted: now,
                            finished: now,
                        };
                        step = enc.resume(&done, now, dev.host_mut(), plan);
                        continue;
                    }
                }
            }
            Step::Wait => {}
            Step::Queue => panic!("split plans are off in these tests"),
        }
        let done = dev.wait(ticket, Some(Nanos(2_000_000)));
        if let Err(e) = &done.status
            && *e != DeviceError::Timeout
        {
            seen.errors.push(e.clone());
        }
        let now = dev.now();
        step = enc.resume(&done, now, dev.host_mut(), plan);
    }
}

/// One tick with the driver loop.
pub fn tick<D: Device>(
    enc: &mut Encoder,
    dev: &mut D,
    plan: &mut Plan,
    deadline: Option<Nanos>,
) -> Driven {
    let now = dev.now();
    let step = enc.begin_tick(now, deadline, dev.host_mut(), plan);
    drive(enc, dev, plan, step, true)
}

/// The C++ `Encoder::finish`: wait for the upload stream, then for the kernel stream.
pub fn finish<D: Device>(enc: &mut Encoder, dev: &mut D, plan: &mut Plan) -> Driven {
    let first = if enc.options().overlap {
        tb_device::StreamId::UPLOAD
    } else {
        tb_device::StreamId::MAIN
    };
    let mut all = Driven::default();
    for st in [first, tb_device::StreamId::MAIN] {
        let step = enc.settle(st, dev.host_mut(), plan);
        let d = drive(enc, dev, plan, step, true);
        all.phases.extend(d.phases);
        all.classes.extend(d.classes);
        all.errors.extend(d.errors);
        all.plans.extend(d.plans);
    }
    all
}

/// SHA-256 (FIPS 180-4), for the stream digests of `crates/tb-golden/vectors/*.sha256`.
pub fn sha256(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&(data.len() as u64 * 8).to_be_bytes());
    for block in msg.as_chunks::<64>().0 {
        let mut w = [0u32; 64];
        for (i, c) in block.as_chunks::<4>().0.iter().enumerate() {
            w[i] = u32::from_be_bytes(*c);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let mut v = h;
        for i in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
            let t1 = v[7]
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let t2 = s0.wrapping_add(maj);
            v = [
                t1.wrapping_add(t2),
                v[0],
                v[1],
                v[2],
                v[3].wrapping_add(t1),
                v[4],
                v[5],
                v[6],
            ];
        }
        for (a, b) in h.iter_mut().zip(v) {
            *a = a.wrapping_add(b);
        }
    }
    h.iter().map(|x| format!("{x:08x}")).collect()
}

#[test]
fn sha256_of_known_messages() {
    assert_eq!(
        sha256(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        sha256(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    let long = vec![b'a'; 1000];
    assert_eq!(
        sha256(&long),
        "41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3"
    );
}

pub mod model;
