//! A field's value.

use std::borrow::Cow;
use std::fmt;

/// A field's value: what an OTLP attribute can hold, less arrays and maps.
#[derive(Debug, Clone, PartialEq)]
pub enum Val {
    /// A flag.
    Bool(bool),
    /// A signed integer.
    I64(i64),
    /// An unsigned integer (exported as a signed one, saturating).
    U64(u64),
    /// A real number.
    F64(f64),
    /// Text.
    Str(Cow<'static, str>),
}

impl Val {
    /// A duration in milliseconds: name the field `…_ms`.
    pub fn ms(d: std::time::Duration) -> Val {
        Val::F64(d.as_secs_f64() * 1e3)
    }

    /// `self + other` when both are numbers, else `other`.
    pub(crate) fn plus(&self, other: Val) -> Val {
        match (self, other) {
            (Val::U64(a), Val::U64(b)) => Val::U64(a.saturating_add(b)),
            (Val::I64(a), Val::I64(b)) => Val::I64(a.saturating_add(b)),
            (Val::U64(a), Val::I64(b)) => Val::I64((*a as i64).saturating_add(b)),
            (Val::I64(a), Val::U64(b)) => Val::I64(a.saturating_add(b as i64)),
            (Val::F64(a), Val::F64(b)) => Val::F64(a + b),
            (Val::F64(a), Val::U64(b)) => Val::F64(a + b as f64),
            (Val::F64(a), Val::I64(b)) => Val::F64(a + b as f64),
            (Val::U64(a), Val::F64(b)) => Val::F64(*a as f64 + b),
            (Val::I64(a), Val::F64(b)) => Val::F64(*a as f64 + b),
            (_, b) => b,
        }
    }
}

impl fmt::Display for Val {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Val::Bool(b) => write!(f, "{b}"),
            Val::I64(n) => write!(f, "{n}"),
            Val::U64(n) => write!(f, "{n}"),
            Val::F64(x) => write!(f, "{}", (x * 1e3).round() / 1e3),
            Val::Str(s) => {
                if s.is_empty() || s.chars().any(|c| c.is_whitespace() || c == '"' || c == '=') {
                    write!(f, "{s:?}")
                } else {
                    f.write_str(s)
                }
            }
        }
    }
}

macro_rules! from_as {
    ($variant:ident, $as:ty: $($t:ty),*) => {
        $(impl From<$t> for Val {
            fn from(v: $t) -> Val {
                Val::$variant(v as $as)
            }
        })*
    };
}
from_as!(I64, i64: i8, i16, i32, i64, isize);
from_as!(U64, u64: u8, u16, u32, u64, usize);
from_as!(F64, f64: f32, f64);

impl From<bool> for Val {
    fn from(v: bool) -> Val {
        Val::Bool(v)
    }
}
impl From<&str> for Val {
    fn from(v: &str) -> Val {
        Val::Str(Cow::Owned(v.to_owned()))
    }
}
impl From<String> for Val {
    fn from(v: String) -> Val {
        Val::Str(Cow::Owned(v))
    }
}
impl From<&String> for Val {
    fn from(v: &String) -> Val {
        Val::Str(Cow::Owned(v.clone()))
    }
}
impl From<Cow<'static, str>> for Val {
    fn from(v: Cow<'static, str>) -> Val {
        Val::Str(v)
    }
}
impl From<std::net::SocketAddr> for Val {
    fn from(v: std::net::SocketAddr) -> Val {
        Val::Str(Cow::Owned(v.to_string()))
    }
}
impl From<&std::path::Path> for Val {
    fn from(v: &std::path::Path) -> Val {
        Val::Str(Cow::Owned(v.display().to_string()))
    }
}
