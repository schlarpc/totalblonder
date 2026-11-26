//! A strict conformance verifier for MPEG-2 video elementary streams.
//!
//! The verifier is written from the text of the standards (ITU-T H.262 (02/2000), ATSC A/53
//! Part 4:2009, ANSI/SCTE 43 2015, CTA-708-E), not from any encoder, and shares no code or
//! tables with the encoder crates of this workspace. It parses the whole syntax down to the
//! coefficients, runs the decoding process as far as the rules need it (variable length
//! decoding, DC and motion vector prediction, inverse quantization arithmetic, the video
//! buffering verifier), and reports every violation of a rule of its catalog
//! ([`rules::ALL_RULES`]) with the clause, the severity and the position.
//!
//! It is sans-io: [`verify`] takes the stream as a byte slice and returns a [`Report`].
//!
//! ```
//! use tb_m2v_verify::{verify, Options};
//! // a lone sequence_end_code is not a video sequence
//! let report = verify(&[0, 0, 1, 0xB7], &Options::default());
//! assert!(!report.conformant());
//! ```
#![forbid(unsafe_code)]

mod bits;
mod mb;
mod report;
pub mod rules;
pub mod tables;
mod userdata;
mod vbv;
mod verify;
mod vlc;

pub use report::{Features, Finding, Position, Report};
pub use rules::{Check, Rule, RuleInfo, Scope, Severity};
pub use vbv::Delivery;
pub use verify::{Options, Profile, verify};
