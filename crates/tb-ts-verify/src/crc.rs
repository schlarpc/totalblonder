//! CRC_32 of H.222.0 Annex A (p.152): generator polynomial 0x04C11DB7, register initialized to
//! all ones, bits processed most significant first, no final inversion. A section with a
//! correct CRC_32 field leaves the register at zero.

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

/// The register after processing `data` from the initial all-ones state.
pub fn crc32(data: &[u8]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for &b in data {
        c = (c << 8) ^ TABLE[usize::from((c >> 24) as u8 ^ b)];
    }
    c
}

#[cfg(test)]
mod tests {
    use super::crc32;

    #[test]
    fn check_value() {
        // The usual check value of this parameter set over "123456789".
        assert_eq!(crc32(b"123456789"), 0x0376_E6E7);
    }

    #[test]
    fn section_with_its_crc_gives_zero() {
        let mut s = vec![
            0x00, 0xB0, 0x0D, 0x00, 0x01, 0xC1, 0x00, 0x00, 0x00, 0x01, 0xE0, 0x30,
        ];
        let c = crc32(&s);
        s.extend_from_slice(&c.to_be_bytes());
        assert_eq!(crc32(&s), 0);
    }
}
