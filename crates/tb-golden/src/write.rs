//! Writing golden records: one JSON object per line, numbers as the C++ encoder's `printf`
//! wrote them (integers as integers, doubles with `%.17g`), so that a record written here equals
//! the one the C++ encoder wrote, byte for byte. The writers of the vectors are the encoder's
//! recorder (`tb_m2v_encoder::golden`) and the conversion tables' (`tb_convert::golden`).

use std::fmt::Write as _;

/// `printf("%.17g", x)`: 17 significant digits, the shorter of fixed and exponent notation by
/// the C rule (exponent below -4 or at least 17: exponent notation), trailing zeros removed.
pub fn g17(x: f64) -> String {
    if x.is_nan() {
        return if x.is_sign_negative() { "-nan" } else { "nan" }.to_owned();
    }
    if x.is_infinite() {
        return if x < 0.0 { "-inf" } else { "inf" }.to_owned();
    }
    if x == 0.0 {
        return if x.is_sign_negative() { "-0" } else { "0" }.to_owned();
    }
    // Rust's exact formatting: 17 correctly rounded significant digits and the exponent.
    let sci = format!("{x:.16e}");
    let (mant, exp) = sci.split_once('e').unwrap_or((sci.as_str(), "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let digits: String = mant.chars().filter(char::is_ascii_digit).collect();
    let mut out = String::with_capacity(24);
    if x < 0.0 {
        out.push('-');
    }
    if !(-4..17).contains(&exp) {
        let (first, rest) = digits.split_at(1);
        let rest = rest.trim_end_matches('0');
        out.push_str(first);
        if !rest.is_empty() {
            out.push('.');
            out.push_str(rest);
        }
        out.push('e');
        out.push(if exp < 0 { '-' } else { '+' });
        let _ = write!(out, "{:02}", exp.unsigned_abs());
    } else if exp >= 0 {
        let (int, frac) = digits.split_at(exp.unsigned_abs() as usize + 1);
        let frac = frac.trim_end_matches('0');
        out.push_str(int);
        if !frac.is_empty() {
            out.push('.');
            out.push_str(frac);
        }
    } else {
        out.push_str("0.");
        for _ in 0..(exp.unsigned_abs() - 1) {
            out.push('0');
        }
        out.push_str(digits.trim_end_matches('0'));
    }
    out
}

/// One record being written: `{"k":"kind"` and the fields in the order they are added.
#[derive(Debug)]
pub struct Rec(String);

const HEX: &[u8; 16] = b"0123456789abcdef";

impl Rec {
    /// A record of this kind.
    pub fn new(kind: &str) -> Self {
        let mut s = String::with_capacity(256);
        s.push_str("{\"k\":\"");
        s.push_str(kind);
        s.push('"');
        Self(s)
    }

    fn key(&mut self, n: &str) {
        self.0.push_str(",\"");
        self.0.push_str(n);
        self.0.push_str("\":");
    }

    /// An integer.
    pub fn i(&mut self, n: &str, v: i64) -> &mut Self {
        self.key(n);
        let _ = write!(self.0, "{v}");
        self
    }

    /// An unsigned integer (the C++ encoder's `long long` casts of sizes and counts).
    pub fn u(&mut self, n: &str, v: u64) -> &mut Self {
        self.key(n);
        let _ = write!(self.0, "{v}");
        self
    }

    /// A flag: 0 or 1.
    pub fn b(&mut self, n: &str, v: bool) -> &mut Self {
        self.i(n, i64::from(v))
    }

    /// A double, `%.17g`.
    pub fn d(&mut self, n: &str, v: f64) -> &mut Self {
        self.key(n);
        self.0.push_str(&g17(v));
        self
    }

    /// A string (quotes and backslashes escaped, as the C++ encoder escaped them).
    pub fn s(&mut self, n: &str, v: &str) -> &mut Self {
        self.key(n);
        self.0.push('"');
        for c in v.chars() {
            if c == '"' || c == '\\' {
                self.0.push('\\');
            }
            self.0.push(c);
        }
        self.0.push('"');
        self
    }

    /// Bytes as lowercase hex.
    pub fn hex(&mut self, n: &str, v: &[u8]) -> &mut Self {
        self.key(n);
        self.0.push('"');
        for b in v {
            self.0.push(char::from(HEX[usize::from(b >> 4)]));
            self.0.push(char::from(HEX[usize::from(b & 15)]));
        }
        self.0.push('"');
        self
    }

    /// An array of integers.
    pub fn ia(&mut self, n: &str, v: impl IntoIterator<Item = i64>) -> &mut Self {
        self.key(n);
        self.0.push('[');
        for (k, x) in v.into_iter().enumerate() {
            if k > 0 {
                self.0.push(',');
            }
            let _ = write!(self.0, "{x}");
        }
        self.0.push(']');
        self
    }

    /// An array of doubles.
    pub fn da(&mut self, n: &str, v: impl IntoIterator<Item = f64>) -> &mut Self {
        self.key(n);
        self.0.push('[');
        for (k, x) in v.into_iter().enumerate() {
            if k > 0 {
                self.0.push(',');
            }
            self.0.push_str(&g17(x));
        }
        self.0.push(']');
        self
    }

    /// The record as one line, with its newline.
    pub fn finish(mut self) -> String {
        self.0.push_str("}\n");
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::g17;

    #[test]
    fn doubles_print_as_printf_17g() {
        for (x, s) in [
            (0.0, "0"),
            (1.0, "1"),
            (-1e18, "-1e+18"),
            (1e18, "1e+18"),
            (0.1001, "0.10009999999999999"),
            (0.49152, "0.49152000000000001"),
            (19060.66666666668, "19060.666666666679"),
            (45.599998474121094, "45.599998474121094"),
            (0.0002646809443831444, "0.00026468094438314438"),
            (1.5e-5, "1.5e-05"),
            (1.5e-25, "1.4999999999999999e-25"),
            (1e16, "10000000000000000"),
            (1e17, "1e+17"),
            (123456789.0, "123456789"),
            (-2.5, "-2.5"),
            (0.0001, "0.0001"),
        ] {
            assert_eq!(g17(x), s, "{x}");
        }
    }
}
