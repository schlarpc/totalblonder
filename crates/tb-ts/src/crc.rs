//! CRC-32 of PSI and PSIP sections (Rec. Annex A, eq. A-1, p.152; A/65 §4.1).
//!
//! Polynomial 0x04C11DB7, most significant bit first, register preset to all ones, no final
//! inversion. The decoder's register is zero after a whole section including its CRC.

const fn table() -> [u32; 256] {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = (i as u32) << 24;
        let mut k = 0;
        while k < 8 {
            c = if c & 0x8000_0000 != 0 {
                (c << 1) ^ 0x04C1_1DB7
            } else {
                c << 1
            };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
}

static TABLE: [u32; 256] = table();

/// CRC-32/MPEG-2 of `data`.
pub fn crc32_mpeg2(data: &[u8]) -> u32 {
    data.iter().fold(0xFFFF_FFFF, |c, &b| {
        (c << 8) ^ TABLE[usize::from(((c >> 24) as u8) ^ b)]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_value() {
        // The cataloged check value of CRC-32/MPEG-2 over "123456789".
        assert_eq!(crc32_mpeg2(b"123456789"), 0x0376_E6E7);
    }

    #[test]
    fn zero_after_whole_section() {
        let mut v = b"any section body".to_vec();
        let c = crc32_mpeg2(&v);
        v.extend_from_slice(&c.to_be_bytes());
        assert_eq!(crc32_mpeg2(&v), 0);
    }
}
