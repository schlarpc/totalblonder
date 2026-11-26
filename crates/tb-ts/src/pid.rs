//! PIDs and continuity counters.

use std::fmt;

/// A packet identifier: 13 bits (Rec. Table 2-2, p.21).
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Pid(u16);

/// A value that is not a PID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("PID {0:#x} is above 0x1FFF")]
pub struct PidError(pub u16);

impl Pid {
    /// Program association table (Rec. Table 2-3, p.21).
    pub const PAT: Pid = Pid(0x0000);
    /// Conditional access table.
    pub const CAT: Pid = Pid(0x0001);
    /// ATSC PSIP base PID: MGT, TVCT/CVCT, STT, RRT (A/65 §6, `docs/notes/standards-digest.md` §2.2).
    pub const PSIP_BASE: Pid = Pid(0x1FFB);
    /// Null packets (Rec. Table 2-3, p.21).
    pub const NULL: Pid = Pid(0x1FFF);

    /// `v` as a PID.
    pub const fn new(v: u16) -> Result<Self, PidError> {
        if v > 0x1FFF {
            Err(PidError(v))
        } else {
            Ok(Pid(v))
        }
    }

    /// The 13-bit value.
    pub const fn get(self) -> u16 {
        self.0
    }

    /// The PID of the low 13 bits of `v` (packet parsing).
    pub(crate) const fn from_bits(v: u16) -> Self {
        Pid(v & 0x1FFF)
    }
}

impl fmt::Debug for Pid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Pid({:#06x})", self.0)
    }
}

impl fmt::Display for Pid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:#06x}", self.0)
    }
}

/// A 4-bit continuity counter (Rec. §2.4.3.3, p.22).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Cc(u8);

impl Cc {
    /// Counter from the low 4 bits of `v`.
    pub const fn new(v: u8) -> Self {
        Cc(v & 0x0F)
    }

    /// The 4-bit value.
    pub const fn get(self) -> u8 {
        self.0
    }

    /// The value after this one, modulo 16.
    #[must_use]
    pub const fn next(self) -> Self {
        Cc((self.0 + 1) & 0x0F)
    }

    /// Returns the current value and advances: the counter of the next packet with payload.
    pub fn take(&mut self) -> Self {
        let c = *self;
        *self = self.next();
        c
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pid_range() {
        assert!(Pid::new(0x1FFF).is_ok());
        assert_eq!(Pid::new(0x2000), Err(PidError(0x2000)));
    }

    #[test]
    fn cc_wraps() {
        let mut c = Cc::new(14);
        assert_eq!(c.take().get(), 14);
        assert_eq!(c.take().get(), 15);
        assert_eq!(c.take().get(), 0);
    }
}
