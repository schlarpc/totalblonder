//! A streaming checker of MPEG-2 transport streams for a US cable QAM carrier.
//!
//! It evaluates the TR 101 290 indicators (priority 1, 2 and the applicable priority 3), the
//! cable and ATSC rules of SCTE 54, A/53 Part 3 and A/65 (table cycles, section syntax,
//! PSIP packet rules, the PSIP smoothing buffer and rate), the PSIP cross-checks (MGT
//! against the tables on air, VCT against PAT and PMT, EIT and ETT structure), the PMT and
//! PES rules for ATSC programs, and the transport-stream system target decoder of H.222.0
//! §2.4.2 for MPEG-2 video, AVC, AC-3 and system data. Every check is a rule of the
//! catalog ([`rules::ALL_RULES`]) with an identifier, a severity and a clause and page.
//!
//! The parsers are written from the standards, not from the mux of this workspace, and share
//! no code with it, so that a misreading cannot hide in both.
//!
//! It is sans-io: [`Checker::push`] takes bytes in any chunking, [`Checker::report`] and
//! [`Checker::finish`] return a [`Report`]. The stream's byte positions are its clock (at the
//! nominal rate, or at the rate its PCRs give). Memory is bounded by the PIDs and tables of
//! the stream and the number of findings kept per rule.
//!
//! ```
//! use tb_ts_verify::{Checker, Options};
//! let mut c = Checker::new(Options::default());
//! c.push(&[0x47, 0x1F, 0xFF, 0x10]); // a fragment: nothing to report yet
//! let report = c.finish();
//! assert_eq!(report.stats.packets, 0);
//! ```
#![forbid(unsafe_code)]

mod bytes;
mod checker;
mod crc;
mod es;
mod mss;
mod pes;
mod psi;
mod psip;
mod report;
pub mod rules;
mod section;
mod time;
mod tstd;

pub use checker::{Checker, Options, check, passes};
pub use crc::crc32;
pub use report::{
    BufferStats, Finding, JSON_SCHEMA, PcrStats, PidStats, Position, Report, Stats, TableStats,
};
pub use rules::{Check, Rule, RuleInfo, Scope, Severity};
