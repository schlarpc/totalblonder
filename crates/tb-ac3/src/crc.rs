//! The two CRC words of a syncframe (A/52 §5.4.1.2, §5.4.5.2, §7.10.1, p.37, 52, 103–105).
//!
//! Generator x¹⁶ + x¹⁵ + x² + 1, registers reset to zero, bits shifted in in stream order
//! (A/52 §7.10.1, p.103–104). A decoder checks a syncframe by running the register over the
//! frame after the syncword: it must read zero at the 5/8 point (crc1) and again at the end
//! (crc2).
//!
//! crc2 is an ordinary CRC of the last 3/8 appended at the end: once crc1 has brought the
//! register to zero at the 5/8 point, the rest is a fresh computation. crc1 sits at the *front*
//! of the data it protects, so it is not the CRC of that data (§7.10.1, p.104 says as much). With
//! `D(x)` the L bits after crc1 up to the 5/8 point, the register reads zero iff
//! `crc1(x)·x^L + D(x) ≡ 0 (mod G)`, so `crc1 = D(x)·x^-L = crc(D)·x^-(L+16) (mod G)`, where
//! `crc(D) = D(x)·x^16 mod G` is the ordinary CRC. `x` is invertible because G has a constant
//! term.

/// The generator polynomial without its x¹⁶ term: x¹⁵ + x² + 1 (A/52 §7.10.1, p.103).
pub const POLY: u16 = 0x8005;

/// x⁻¹ mod G. From G ≡ 0: x·(x¹⁵ + x¹⁴ + x) = x¹⁶ + x¹⁵ + x² ≡ 1.
const X_INVERSE: u16 = 0xC002;

const fn make_table() -> [u16; 256] {
    let mut t = [0u16; 256];
    let mut i = 0;
    while i < 256 {
        let mut r = (i as u16) << 8;
        let mut b = 0;
        while b < 8 {
            r = if r & 0x8000 != 0 {
                (r << 1) ^ POLY
            } else {
                r << 1
            };
            b += 1;
        }
        t[i] = r;
        i += 1;
    }
    t
}

static TABLE: [u16; 256] = make_table();

/// The ordinary CRC of `data`: `data(x)·x¹⁶ mod G`, register starting at `crc`.
pub fn update(mut crc: u16, data: &[u8]) -> u16 {
    for &b in data {
        crc = (crc << 8) ^ TABLE[usize::from((crc >> 8) as u8 ^ b)];
    }
    crc
}

/// `a·b mod G` in GF(2)[x].
fn mul(a: u16, b: u16) -> u16 {
    let mut r: u16 = 0;
    let mut a = a;
    let mut b = b;
    while b != 0 {
        if b & 1 != 0 {
            r ^= a;
        }
        b >>= 1;
        a = if a & 0x8000 != 0 {
            (a << 1) ^ POLY
        } else {
            a << 1
        };
    }
    r
}

/// `x^(-n) mod G`.
fn x_inverse_pow(mut n: u64) -> u16 {
    let mut result: u16 = 1;
    let mut base = X_INVERSE;
    while n != 0 {
        if n & 1 != 0 {
            result = mul(result, base);
        }
        base = mul(base, base);
        n >>= 1;
    }
    result
}

/// crc1 for the bytes that follow it up to the 5/8 point (`after_crc1` excludes the syncword
/// and crc1 itself).
pub fn crc1(after_crc1: &[u8]) -> u16 {
    let bits = after_crc1.len() as u64 * 8;
    mul(update(0, after_crc1), x_inverse_pow(bits + 16))
}

/// crc2 for the bytes from the 5/8 point up to, not including, crc2.
pub fn crc2(last_three_eighths: &[u8]) -> u16 {
    update(0, last_three_eighths)
}

/// Writes crc1 (bytes 2–3) and crc2 (the last two bytes) into a complete syncframe whose first
/// 5/8 is `five_eighths_bytes` long, and whose crcrsv bit (the bit before crc2) is 0. If crc2
/// would equal the syncword, crcrsv is set to 1 and crc2 recomputed (A/52 §5.4.5.1, p.52).
/// Returns crcrsv.
pub fn seal_frame(frame: &mut [u8], five_eighths_bytes: usize) -> bool {
    let n = frame.len();
    let p58 = five_eighths_bytes;
    let c1 = crc1(&frame[4..p58]);
    frame[2..4].copy_from_slice(&c1.to_be_bytes());
    let mut c2 = crc2(&frame[p58..n - 2]);
    let mut crcrsv = false;
    if c2 == crate::tables::SYNCWORD {
        frame[n - 3] |= 0x01;
        c2 = crc2(&frame[p58..n - 2]);
        crcrsv = true;
    }
    frame[n - 2..].copy_from_slice(&c2.to_be_bytes());
    crcrsv
}

/// The decoder's check: the register after shifting in `data` bit by bit from zero, as the LFSR
/// of A/52 §7.10.1 (p.104) does. Deliberately bitwise and table-free, so that it is an
/// independent implementation of what [`update`], [`crc1`] and [`crc2`] compute.
pub fn lfsr_register(data: &[u8]) -> u16 {
    let mut reg: u16 = 0;
    for &byte in data {
        for i in (0..8).rev() {
            let input = (byte >> i) & 1;
            let top = (reg >> 15) as u8 & 1;
            reg <<= 1;
            if top ^ input != 0 {
                reg ^= POLY;
            }
        }
    }
    reg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_crc_matches_bitwise_lfsr_with_appended_check() {
        // A message followed by its ordinary CRC leaves the decoder's register at zero.
        let msg: Vec<u8> = (0..200u32).map(|i| (i * 37 + 11) as u8).collect();
        let c = crc2(&msg);
        let mut framed = msg.clone();
        framed.extend_from_slice(&c.to_be_bytes());
        assert_eq!(lfsr_register(&framed), 0);
    }

    #[test]
    fn crc1_in_front_zeroes_the_register() {
        for len in [0usize, 1, 2, 37, 158, 478, 1118] {
            let data: Vec<u8> = (0..len).map(|i| (i * 131 + 7) as u8).collect();
            let c = crc1(&data);
            let mut framed = c.to_be_bytes().to_vec();
            framed.extend_from_slice(&data);
            assert_eq!(lfsr_register(&framed), 0, "len {len}");
        }
    }

    /// A frame whose crc2 would be the syncword gets crcrsv = 1 and a different crc2, and both
    /// checks still pass. Such a frame is made by choosing two aux bytes so that crc2 = 0x0B77.
    #[test]
    fn crcrsv_avoids_a_syncword_in_crc2() {
        let n = 768;
        let p58 = 480;
        let mut frame = vec![0u8; n];
        frame[0] = 0x0B;
        frame[1] = 0x77;
        for (i, b) in frame[4..n - 4].iter_mut().enumerate() {
            *b = (i * 29 + 3) as u8;
        }
        frame[n - 3] = 0; // auxdatae 0, crcrsv 0 in the last bits before crc2
        let found = (0..=u16::MAX).find(|&v| {
            frame[n - 5..n - 3].copy_from_slice(&v.to_be_bytes());
            crc2(&frame[p58..n - 2]) == crate::tables::SYNCWORD
        });
        assert!(found.is_some(), "no aux value gives crc2 = syncword");
        assert!(seal_frame(&mut frame, p58));
        assert_eq!(frame[n - 3] & 1, 1);
        assert_ne!(
            u16::from_be_bytes([frame[n - 2], frame[n - 1]]),
            crate::tables::SYNCWORD
        );
        assert_eq!(lfsr_register(&frame[2..p58]), 0);
        assert_eq!(lfsr_register(&frame[2..]), 0);
    }

    #[test]
    fn x_inverse_is_the_inverse() {
        assert_eq!(mul(X_INVERSE, 2), 1);
        assert_eq!(
            mul(x_inverse_pow(1000), mul(x_inverse_pow(0), 1)),
            x_inverse_pow(1000)
        );
    }
}
