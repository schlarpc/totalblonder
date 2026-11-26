//! Captions made from timed text, for the `cc_data()` of the encoder's pictures; and the
//! captions a source already carries, read and merged with ours.
//!
//! Pure (docs/ARCHITECTURE.md §1): no clock, file or thread; time is an argument. Everything
//! is cross-checked against the standards in `docs/external/standards/` (CTA-608-E S-2019, CTA-708-E
//! S-2023), with clause, table and page in the comments; `notes/captions-log.md` records the
//! decisions and the checks against real decoders.
//!
//! ```text
//!  SRT / WebVTT / tb_media Text packets ──► Cue ──► layout: Screens (rows, 32 columns)
//!        (srt, vtt, cue)                           │
//!                                                   ▼
//!               Captioner (schedule): CEA-608 slots per field, DTVCC packets in slots
//!                                                   │  picture(fields, top_first)
//!                                                   ▼
//!        cc_data() of one picture  ──► (ccdata::Merger with a source's) ──► Encoder::submit_shown
//! ```
//!
//! - [`srt`], [`vtt`], [`markup`]: parsers into [`Cue`]s (times in milliseconds,
//!   [`Millis`]); malformed input is reported per block, never a panic.
//! - [`layout`]: what is on the screen when, as rows of at most 32 cells on rows 1 to 15, at
//!   most four rows (the rules are in the module).
//! - [`cea608`]: odd-parity bytes, codes, the character sets, rows encoded into control
//!   codes sent twice; [`cea608::decode`] reads them back.
//! - [`cea708`]: commands, code sets G0 to G3, service blocks and caption channel packets;
//!   [`cea708::decode`] reads one service back.
//! - [`schedule`]: the [`Captioner`]: pop-on (load early, EOC on the start frame) or roll-up,
//!   both services, late cues reported. Its [`Captioner::picture`] writes exactly the
//!   constructs `tb_m2v_syntax::captions::validate_shown` accepts for a picture of one to three
//!   field periods, in the field order of `tb_timeline::captions::CaptionCfg::top_first`.
//! - [`ccdata`]: constructs, the parser of a source's `cc_data()`
//!   (`tb_media::VideoFrameDesc::cc_data`), and the [`ccdata::Merger`]: a source's captions win
//!   per service while it sends any, ours fill the rest.
//! - [`descriptor`]: the `caption_service_descriptor()` entries that announce the services
//!   (ATSC A/65 §6.9.2).
#![forbid(unsafe_code)]

pub mod ccdata;
pub mod cea608;
pub mod cea708;
pub mod cue;
pub mod descriptor;
pub mod layout;
pub mod markup;
pub mod schedule;
pub mod srt;
pub mod vtt;

pub use cue::{Align, Cue, CueStyle, FieldRate, Line, Millis, Span, SpanStyle, Vertical};
pub use schedule::{Captioner, CaptionerCfg, Presentation, Track};
