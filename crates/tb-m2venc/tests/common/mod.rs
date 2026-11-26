//! Helpers shared by the integration tests of the driver: clips, a scratch directory, the
//! reference device with the host-arena rule checked, SHA-256, and a look into a coded stream.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    dead_code,
    reason = "test helpers outside #[test] functions; each test file uses a part"
)]

use std::path::{Path, PathBuf};

use tb_device_ref::RefDevice;
use tb_engine::device::Guarded;
use tb_m2venc::clip::{ClipKind, make_clip};
use tb_m2venc::opts::{self, Opts};
use tb_m2venc::{Outcome, arena_sizes, run};

/// A scratch directory of one test, removed when dropped.
pub struct Scratch(pub PathBuf);

impl Scratch {
    pub fn new(test: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("tb-m2venc-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    pub fn path(&self, name: &str) -> String {
        self.0.join(name).to_str().unwrap().to_owned()
    }
    pub fn read(&self, name: &str) -> Vec<u8> {
        std::fs::read(self.0.join(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
    }
    pub fn exists(&self, name: &str) -> bool {
        self.0.join(name).exists()
    }
    /// The clips of `crates/tb-golden/vectors/generate.sh`, by the name it gives them.
    pub fn clip(&self, name: &str) -> String {
        let (w, h, n, seed, kind) = match name {
            "qcif" => (176, 144, 30, 1, ClipKind::Plain),
            "qcif-noisy" => (176, 144, 24, 2, ClipKind::Noisy),
            "w184" => (184, 144, 12, 3, ClipKind::Plain),
            "qcif-i" => (176, 144, 24, 4, ClipKind::Interlaced),
            // not a golden clip: the smallest picture the clip generator makes, for speed
            "tiny" => (64, 48, 12, 5, ClipKind::Plain),
            "tiny-i" => (64, 48, 12, 6, ClipKind::Interlaced),
            // the adversarial clips of the conformance tests
            "white" => (176, 144, 8, 7, ClipKind::White),
            "patch" => (176, 144, 8, 8, ClipKind::Patch),
            "noisy352" => (352, 240, 6, 6, ClipKind::Noisy),
            _ => panic!("no clip {name}"),
        };
        let path = self.0.join(format!("{name}.yuv"));
        if !path.exists() {
            std::fs::write(&path, make_clip(w, h, n, seed, kind)).unwrap();
        }
        path.to_str().unwrap().to_owned()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub fn words(s: &str) -> Vec<String> {
    s.split_whitespace().map(str::to_owned).collect()
}

/// Parse a command line; the reference device, with the host-arena rule checked. The profile
/// is `h262` unless the line names one: the tests' pictures are small.
pub fn parse(args: &str) -> Opts {
    let mut w = words(args);
    if !w.iter().any(|a| a == "--conformance") {
        w.extend(words("--conformance h262"));
    }
    let mut o = opts::parse(&w).unwrap_or_else(|e| panic!("{args}: {e}"));
    o.device = opts::DeviceKind::Ref;
    o.check_arena = true;
    o
}

/// Run a command line on the reference device.
pub fn encode(o: &Opts) -> Outcome {
    let dev = RefDevice::new(arena_sizes(o).unwrap()).unwrap();
    let mut console = Vec::new();
    run(o, Guarded::new(dev), &mut console).unwrap()
}

/// Run a command line given as text.
pub fn encode_args(args: &str) -> Outcome {
    encode(&parse(args))
}

/// What a coded stream contains, by its start codes.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Shape {
    pub sequence_headers: usize,
    pub gop_headers: usize,
    /// picture_coding_type of every picture, in coded order (1 I, 2 P, 3 B).
    pub types: Vec<u8>,
    /// The stream ends with exactly one sequence_end_code.
    pub ended: bool,
}

pub fn shape(m2v: &[u8]) -> Shape {
    let mut s = Shape::default();
    let mut ends = 0;
    let mut i = 0;
    while i + 3 < m2v.len() {
        if m2v[i] == 0 && m2v[i + 1] == 0 && m2v[i + 2] == 1 {
            match m2v[i + 3] {
                0x00 => s.types.push((m2v[i + 5] >> 3) & 7),
                0xB3 => s.sequence_headers += 1,
                0xB8 => s.gop_headers += 1,
                0xB7 => ends += 1,
                _ => {}
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    s.ended = ends == 1 && m2v.ends_with(&[0, 0, 1, 0xB7]);
    s
}

pub fn file_exists(p: &str) -> bool {
    Path::new(p).exists()
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
