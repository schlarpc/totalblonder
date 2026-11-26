//! The random numbers of the C++ driver, reproduced: `std::mt19937` and libstdc++'s
//! `std::uniform_real_distribution<double>` (`--rate-walk`, `--churn`).
//!
//! The rate walks and the churn schedule decide what is coded, so a stream can only equal the
//! C++ encoder's when these sequences do. `std::mt19937` is specified by the C++ standard; the
//! distribution is not, so this follows libstdc++ (`bits/random.tcc`): `generate_canonical`
//! takes two outputs of the generator, `(g0 + g1 * 2^32) / 2^64` rounded to nearest once, and
//! the distribution returns `canonical * (b - a) + a`. Both forms of `generate_canonical` in
//! libstdc++ (before and after GCC 16) give this value; they differ only when it rounds to 1.0
//! (once in 2^54 draws: the old form returns the largest double below 1, the new form draws
//! again, as this code does).

/// `std::mt19937`.
#[derive(Clone, Debug)]
pub struct Mt19937 {
    mt: [u32; 624],
    at: usize,
}

impl Mt19937 {
    /// `std::mt19937 rng(seed)`.
    pub fn new(seed: u32) -> Self {
        let mut mt = [0u32; 624];
        mt[0] = seed;
        for i in 1..624 {
            let p = mt[i - 1];
            mt[i] = 1_812_433_253u32
                .wrapping_mul(p ^ (p >> 30))
                .wrapping_add(i as u32);
        }
        Self { mt, at: 624 }
    }

    /// `rng()`.
    pub fn next_u32(&mut self) -> u32 {
        if self.at >= 624 {
            for i in 0..624 {
                let y = (self.mt[i] & 0x8000_0000) | (self.mt[(i + 1) % 624] & 0x7fff_ffff);
                let mut v = self.mt[(i + 397) % 624] ^ (y >> 1);
                if y & 1 != 0 {
                    v ^= 0x9908_b0df;
                }
                self.mt[i] = v;
            }
            self.at = 0;
        }
        let mut y = self.mt[self.at];
        self.at += 1;
        y ^= y >> 11;
        y ^= (y << 7) & 0x9d2c_5680;
        y ^= (y << 15) & 0xefc6_0000;
        y ^ (y >> 18)
    }

    /// `std::generate_canonical<double, 53>(rng)`: in `[0, 1)`.
    pub fn canonical(&mut self) -> f64 {
        loop {
            let lo = u64::from(self.next_u32());
            let hi = u64::from(self.next_u32());
            let r = ((hi << 32) | lo) as f64 / 18_446_744_073_709_551_616.0;
            if r < 1.0 {
                return r;
            }
        }
    }

    /// `std::uniform_real_distribution<double>(a, b)(rng)`.
    pub fn uniform(&mut self, a: f64, b: f64) -> f64 {
        self.canonical() * (b - a) + a
    }
}

/// The rate plan of `--rate-walk lo:hi:seed` for session `s` of a group: a rate drawn
/// log-uniformly for every key frame index from 1.
pub fn rate_walk(lo: f64, hi: f64, seed: i32, s: u32, gop: u32, frames: u64) -> Vec<(u64, u64)> {
    let mut rng = Mt19937::new(seed.wrapping_add(s as i32) as u32);
    let (a, b) = (lo.ln(), hi.ln());
    let mut plan = Vec::new();
    let mut k = 1u64;
    while k < 100_000 && k * u64::from(gop) < frames + u64::from(gop) {
        // (long)std::exp(u(rng))
        let r = rng.uniform(a, b).exp() as i64;
        plan.push((k, u64::try_from(r).unwrap_or(0)));
        k += 1;
    }
    plan
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The 10000th output of a default-constructed `std::mt19937` (seed 5489) is 4123659995
    /// (C++ standard, [rand.predef]).
    #[test]
    fn mt19937_is_the_standard_generator() {
        let mut r = Mt19937::new(5489);
        let mut last = 0;
        for _ in 0..10_000 {
            last = r.next_u32();
        }
        assert_eq!(last, 4_123_659_995);
    }

    /// Values printed by the C++ driver's rate walk (`std::mt19937`,
    /// `std::uniform_real_distribution<double>(log(lo), log(hi))`, `(long)std::exp(..)`) built
    /// with the flake's g++ 16.2 and its libstdc++, with and without
    /// `-D_GLIBCXX_USE_OLD_GENERATE_CANONICAL` (the two agree), on 2026-11-25.
    #[test]
    fn rate_walks_are_those_of_libstdcxx() {
        assert_eq!(
            rate_walk(3e6, 15e6, 3, 0, 15, 150)
                .iter()
                .map(|x| x.1)
                .collect::<Vec<_>>(),
            [
                3361675, 11593656, 3646920, 7499853, 6061979, 3091900, 3202734, 4470825, 3487473,
                9178720
            ]
        );
        assert_eq!(
            rate_walk(5e6, 15e6, 1, 1, 15, 60),
            [(1, 6127405), (2, 13913229), (3, 14162907), (4, 8516361)]
        );
        assert_eq!(
            rate_walk(2.4e6, 12e6, 3, 0, 30, 300)
                .iter()
                .map(|x| x.1)
                .collect::<Vec<_>>(),
            [
                2689340, 9274925, 2917536, 5999883, 4849583, 2473520, 2562187, 3576660, 2789978,
                7342976
            ]
        );
        assert_eq!(
            rate_walk(1_333_333.0, 4e6, 3, 3, 15, 60)
                .iter()
                .map(|x| x.1)
                .collect::<Vec<_>>(),
            [3775719, 1678227, 1430855, 2002284]
        );
    }

    /// `std::mt19937 crng(5); std::uniform_real_distribution<double> U(0, 1);` six draws,
    /// as `%.17g` prints them.
    #[test]
    #[allow(clippy::excessive_precision, reason = "the digits the C++ printed")]
    fn the_churn_generator_is_that_of_libstdcxx() {
        let mut r = Mt19937::new(5);
        let got: Vec<f64> = (0..6).map(|_| r.uniform(0.0, 1.0)).collect();
        assert_eq!(
            got,
            [
                0.055180120799223235,
                0.83132784019008565,
                0.36373689542680959,
                0.97944499783185568,
                0.089821034224799645,
                0.3967366066322997
            ]
        );
    }
}
