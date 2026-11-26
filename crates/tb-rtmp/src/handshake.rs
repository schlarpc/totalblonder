//! The RTMP handshake (Adobe RTMP 1.0 §5.2), both roles.
//!
//! ```text
//!  client                    server
//!  C0 (version 3) + C1 ──►
//!                      ◄──   S0 + S1 + S2
//!  C2                  ──►
//! ```
//!
//! C1/S1 are 1536 bytes: time (4), zero (4), random (1528) (§5.2.3). S2 echoes C1 with the
//! time at which C1 was read in the second word (§5.2.4); C2 echoes S1.
//!
//! **Digest ("complex", Flash Player 9) handshakes.** ffmpeg's and librtmp's clients send a C1
//! whose second word is a non-zero version and which carries an HMAC-SHA256 digest. This
//! server always answers with the simple handshake: S1's second word is zero, and S2 echoes C1.
//! A zero version in S1 is how a server says "no digest", and every client we care about falls
//! back on it: ffmpeg (`libavformat/rtmpproto.c`, `rtmp_handshake`) only validates the server
//! digest when playing *and* S1's version byte is at least 3, and otherwise echoes S1 as C2;
//! librtmp (`handshake.h`, `HandShake`) clears its FP9 flag when `serversig[4] == 0` and then
//! only logs a warning if S2 differs from C1 (OBS uses librtmp). A publisher never needs the
//! digest (it exists to authorize Flash *players* for SWF verification), so there is no
//! HMAC here.
//!
//! The random bytes need not be cryptographically strong (§5.2.3: "sufficiently random"); they
//! come from a seed the caller provides, which keeps the state machine deterministic.

/// Size of C1, S1, C2, S2.
pub const PACKET_SIZE: usize = 1536;
/// The only protocol version accepted (§5.2.2): 3 is plain RTMP. 6 and 8 are RTMPE, refused.
pub const VERSION: u8 = 3;

/// A handshake failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HandshakeError {
    /// C0/S0 carried a version other than 3 (§5.2.2). 6 and 8 are RTMPE; 32 and up are not
    /// RTMP at all (an HTTP request starts with 'G' = 71, for example).
    #[error("unsupported RTMP version {0} (only 3)")]
    Version(u8),
}

/// Where the server side of the handshake is.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ServerState {
    /// Collecting C0 + C1.
    AwaitC0C1 { buf: Vec<u8> },
    /// S0/S1/S2 sent; collecting C2.
    AwaitC2 { have: usize },
    /// Finished; later bytes are chunks.
    Done,
}

/// The server side of the handshake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerHandshake {
    state: ServerState,
    seed: u64,
}

/// What a call to `push` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    /// Bytes of the input the handshake consumed; the rest (if `done`) are chunk bytes.
    pub consumed: usize,
    /// The handshake is complete.
    pub done: bool,
}

impl ServerHandshake {
    /// A server handshake whose S1 random bytes derive from `seed`.
    pub fn new(seed: u64) -> Self {
        Self {
            state: ServerState::AwaitC0C1 {
                buf: Vec::with_capacity(1 + PACKET_SIZE),
            },
            seed,
        }
    }

    /// The handshake has completed.
    pub fn is_done(&self) -> bool {
        self.state == ServerState::Done
    }

    /// Feed bytes from the client. `now_ms` is the server's handshake-epoch time in ms (it goes
    /// into S1's time field and S2's time2 field). Response bytes are appended to `out`.
    pub fn push(
        &mut self,
        input: &[u8],
        now_ms: u32,
        out: &mut Vec<u8>,
    ) -> Result<Progress, HandshakeError> {
        let mut consumed = 0;
        loop {
            let rest = &input[consumed..];
            match &mut self.state {
                ServerState::AwaitC0C1 { buf } => {
                    let need = 1 + PACKET_SIZE - buf.len();
                    let take = need.min(rest.len());
                    buf.extend_from_slice(&rest[..take]);
                    consumed += take;
                    if let Some(&v) = buf.first()
                        && v != VERSION
                    {
                        return Err(HandshakeError::Version(v));
                    }
                    if buf.len() < 1 + PACKET_SIZE {
                        return Ok(Progress {
                            consumed,
                            done: false,
                        });
                    }
                    let c1 = &buf[1..];
                    out.reserve(1 + 2 * PACKET_SIZE);
                    // S0
                    out.push(VERSION);
                    // S1: time, zero (no digest: see the module documentation), random.
                    out.extend_from_slice(&now_ms.to_be_bytes());
                    out.extend_from_slice(&[0; 4]);
                    let mut rng = XorShift::new(self.seed ^ fnv(c1));
                    for _ in 0..(PACKET_SIZE - 8) / 8 {
                        out.extend_from_slice(&rng.next().to_le_bytes());
                    }
                    // S2: C1's time, the time C1 was read, C1's random echo (§5.2.4).
                    out.extend_from_slice(&c1[0..4]);
                    out.extend_from_slice(&now_ms.to_be_bytes());
                    out.extend_from_slice(&c1[8..]);
                    self.state = ServerState::AwaitC2 { have: 0 };
                }
                ServerState::AwaitC2 { have } => {
                    // C2 is not checked: a digest client fills it differently, and nothing in
                    // it matters to a server (§5.2.4 lets the peer verify, not require).
                    let take = (PACKET_SIZE - *have).min(rest.len());
                    *have += take;
                    consumed += take;
                    if *have < PACKET_SIZE {
                        return Ok(Progress {
                            consumed,
                            done: false,
                        });
                    }
                    self.state = ServerState::Done;
                }
                ServerState::Done => {
                    return Ok(Progress {
                        consumed,
                        done: true,
                    });
                }
            }
        }
    }
}

/// Where the client side of the handshake is.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ClientState {
    /// C0 + C1 written; collecting S0 + S1 + S2.
    AwaitS0S1S2 { buf: Vec<u8> },
    /// Finished.
    Done,
}

/// The client side of the simple handshake: for a future pull (play) mode and for tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientHandshake {
    state: ClientState,
}

impl ClientHandshake {
    /// Start: returns the handshake and C0 + C1 to send.
    pub fn start(seed: u64, now_ms: u32) -> (Self, Vec<u8>) {
        let mut out = Vec::with_capacity(1 + PACKET_SIZE);
        out.push(VERSION);
        out.extend_from_slice(&now_ms.to_be_bytes());
        out.extend_from_slice(&[0; 4]);
        let mut rng = XorShift::new(seed);
        for _ in 0..(PACKET_SIZE - 8) / 8 {
            out.extend_from_slice(&rng.next().to_le_bytes());
        }
        let hs = Self {
            state: ClientState::AwaitS0S1S2 {
                buf: Vec::with_capacity(1 + 2 * PACKET_SIZE),
            },
        };
        (hs, out)
    }

    /// The handshake has completed.
    pub fn is_done(&self) -> bool {
        self.state == ClientState::Done
    }

    /// Feed bytes from the server; C2 is appended to `out` when S1 is known.
    pub fn push(
        &mut self,
        input: &[u8],
        now_ms: u32,
        out: &mut Vec<u8>,
    ) -> Result<Progress, HandshakeError> {
        match &mut self.state {
            ClientState::AwaitS0S1S2 { buf } => {
                let need = 1 + 2 * PACKET_SIZE - buf.len();
                let take = need.min(input.len());
                buf.extend_from_slice(&input[..take]);
                if let Some(&v) = buf.first()
                    && v != VERSION
                {
                    return Err(HandshakeError::Version(v));
                }
                if buf.len() < 1 + 2 * PACKET_SIZE {
                    return Ok(Progress {
                        consumed: take,
                        done: false,
                    });
                }
                let s1 = &buf[1..=PACKET_SIZE];
                out.extend_from_slice(&s1[0..4]);
                out.extend_from_slice(&now_ms.to_be_bytes());
                out.extend_from_slice(&s1[8..]);
                self.state = ClientState::Done;
                Ok(Progress {
                    consumed: take,
                    done: true,
                })
            }
            ClientState::Done => Ok(Progress {
                consumed: 0,
                done: true,
            }),
        }
    }
}

/// FNV-1a over a few bytes of C1, so that two connections with the same seed differ.
fn fnv(data: &[u8]) -> u64 {
    data.iter().take(64).fold(0xcbf2_9ce4_8422_2325, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// xorshift64*: filler bytes, not cryptography.
struct XorShift(u64);

impl XorShift {
    fn new(seed: u64) -> Self {
        Self(seed | 1)
    }
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_and_server_meet_byte_by_byte() {
        let (mut c, c01) = ClientHandshake::start(7, 1000);
        let mut s = ServerHandshake::new(9);
        let mut s_out = Vec::new();
        for b in &c01 {
            let p = s.push(std::slice::from_ref(b), 5, &mut s_out).unwrap();
            assert_eq!(p.consumed, 1);
        }
        assert_eq!(s_out.len(), 1 + 2 * PACKET_SIZE);
        // S1's version word is zero; S2 echoes C1's time and random bytes.
        assert_eq!(&s_out[5..9], &[0; 4]);
        assert_eq!(&s_out[1 + PACKET_SIZE..][..4], &c01[1..5]);
        assert_eq!(&s_out[1 + PACKET_SIZE + 8..], &c01[9..]);
        let mut c2 = Vec::new();
        let p = c.push(&s_out, 6, &mut c2).unwrap();
        assert!(p.done && c.is_done());
        assert_eq!(c2.len(), PACKET_SIZE);
        // C2 plus a trailing chunk byte: the server consumes C2 only.
        c2.push(0x03);
        let p = s.push(&c2, 7, &mut s_out).unwrap();
        assert_eq!(p.consumed, PACKET_SIZE);
        assert!(p.done && s.is_done());
    }

    #[test]
    fn refuses_other_versions() {
        let mut s = ServerHandshake::new(0);
        let mut out = Vec::new();
        assert_eq!(
            s.push(b"GET / HTTP/1.1", 0, &mut out),
            Err(HandshakeError::Version(b'G'))
        );
        let mut s = ServerHandshake::new(0);
        assert_eq!(s.push(&[6], 0, &mut out), Err(HandshakeError::Version(6)));
        assert!(out.is_empty());
    }
}
