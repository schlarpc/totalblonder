//! PSI and ATSC PSIP for a US cable MPTS (`docs/SPEC.md` §7, `notes/standards-digest.md` §1.3,
//! §2–§4).
//!
//! - [`lineup`]: the description of one carrier's programs from which every table is built
//!   (so the CVCT, the service location descriptors and the PMTs cannot disagree), validated
//!   when it is made: PID ranges and uniqueness, channel numbers, names, source ids;
//! - [`tables`]: one function per table, each returning finished sections: PAT, PMT, CVCT, MGT,
//!   STT, EIT, ETT;
//! - [`guide`]: the program guide's three-hour windows, EIT-k / ETT-k PIDs, events split
//!   across windows with stable ids;
//! - [`gps`]: GPS time, the GPS–UTC offset and the daylight-saving field of the STT;
//! - [`version`]: version numbers that change exactly when a table's content does;
//! - [`build`]: the whole table set of a carrier with its MGT, ready for the mux's carousel.
//!
//! Citations: "A/65" is ATSC A/65:2013; "Rec." is ITU-T H.222.0 (06/2012), printed pages.
#![forbid(unsafe_code)]

pub mod build;
pub mod descriptor;
pub mod gps;
pub mod guide;
pub mod lineup;
pub mod rating;
pub mod tables;
pub mod text;
pub mod version;

/// A table or descriptor that cannot be built.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// Not an ISO 639 code.
    #[error("not a language code: {0:?}")]
    Lang(String),
    /// Not a valid short name.
    #[error("short_name must be 1-7 UTF-16 units without NUL: {0:?}")]
    ShortName(String),
    /// A length field would overflow.
    #[error("{0} too long")]
    TooLong(&'static str),
    /// A value outside its field's range or a rule of the standard.
    #[error("invalid: {0}")]
    Invalid(&'static str),
    /// A lineup rule (uniqueness, ranges) is broken; the text says which.
    #[error("lineup: {0}")]
    Lineup(String),
    /// A section is too long.
    #[error(transparent)]
    Section(#[from] tb_ts::section::SectionError),
}
