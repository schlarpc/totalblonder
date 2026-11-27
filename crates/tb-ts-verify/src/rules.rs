//! The rule catalog: every check the checker makes, with its clause, page, scope and
//! severity. `docs/notes/ts-conformance-rules.md` is generated from this table
//! (`tb-ts-verify --rules-md`).
//!
//! Page numbers are the printed ones. For ITU-T H.222.0 (06/2012) the printed page is the PDF
//! page minus 14; for every other document printed and PDF pages agree. Sources: ITU-T H.222.0
//! (06/2012), ETSI TR 101 290 V1.4.1, ANSI/SCTE 54 2024 (and 2009 for its PID table), ATSC
//! A/65:2013, A/53 Part 3:2013 (and its Amendment 1), A/53 Part 5:2014, A/52:2012 Annex A,
//! ITU-T H.262 (02/2000) for the video buffer constants.

use serde::Serialize;

/// How bad a violation is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub enum Severity {
    /// A receiver may fail to tune, lose sync or misdecode: TR 101 290 first-priority
    /// indicators and the second-priority ones that corrupt data (transport and CRC errors,
    /// PCR timing).
    S1,
    /// Illegal per H.222.0 (or a TR 101 290 indicator that receivers commonly tolerate).
    S2,
    /// Legal MPEG-2 systems, but outside ATSC A/53 Part 3, A/65, A/52 Annex A or SCTE 54: the
    /// rules for a US cable carrier.
    S3,
    /// A recommendation, a target of this project, or a legal construct worth knowing about.
    S4,
}

impl Severity {
    /// "S1" to "S4".
    pub fn name(self) -> &'static str {
        match self {
            Severity::S1 => "S1",
            Severity::S2 => "S2",
            Severity::S3 => "S3",
            Severity::S4 => "S4",
        }
    }

    /// Parses "S1" to "S4".
    pub fn from_name(s: &str) -> Option<Severity> {
        match s {
            "S1" => Some(Severity::S1),
            "S2" => Some(Severity::S2),
            "S3" => Some(Severity::S3),
            "S4" => Some(Severity::S4),
            _ => None,
        }
    }
}

/// The layer a rule applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub enum Scope {
    /// Transport packets: sync, header, adaptation field, continuity.
    Packet,
    /// Section framing and the generic section header: pointer field, length, CRC, versions.
    Section,
    /// PAT, PMT and CAT: their contents and repetition.
    Psi,
    /// Descriptors in the PMT.
    Descriptor,
    /// PES packets and timestamps.
    Pes,
    /// Program clock references and the stream's rate.
    Pcr,
    /// ATSC PSIP tables (MGT, VCT, STT, EIT, ETT, RRT) and their packets.
    Psip,
    /// Relations between tables and the PIDs present: PAT, PMT, MGT, VCT, EIT, ETT.
    CrossCheck,
    /// The transport-stream system target decoder of H.222.0 §2.4.2 and the PSIP smoothing
    /// buffer.
    BufferModel,
    /// Elementary-stream framing the buffer model depends on.
    Elementary,
}

impl Scope {
    /// A short name for tables.
    pub fn name(self) -> &'static str {
        match self {
            Scope::Packet => "packet",
            Scope::Section => "section",
            Scope::Psi => "PSI",
            Scope::Descriptor => "descriptor",
            Scope::Pes => "PES",
            Scope::Pcr => "PCR and rate",
            Scope::Psip => "PSIP",
            Scope::CrossCheck => "cross-check",
            Scope::BufferModel => "buffer model",
            Scope::Elementary => "elementary stream",
        }
    }

    /// Every scope, in catalog order.
    pub const ALL: [Scope; 10] = [
        Scope::Packet,
        Scope::Section,
        Scope::Psi,
        Scope::Descriptor,
        Scope::Pes,
        Scope::Pcr,
        Scope::Psip,
        Scope::CrossCheck,
        Scope::BufferModel,
        Scope::Elementary,
    ];
}

/// Whether the checker evaluates a rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Check {
    /// Checked on the stream; a test in `tests/rules.rs` makes it fire.
    Yes,
    /// Not checked, or only partly; the text says why.
    No(&'static str),
}

/// One entry of the catalog.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct RuleInfo {
    /// Stable identifier, for example `CC-ERROR`.
    pub id: &'static str,
    /// Scope.
    pub scope: Scope,
    /// Severity.
    pub severity: Severity,
    /// Whether it is checked.
    pub check: Check,
    /// Standard and clause.
    pub clause: &'static str,
    /// Printed page.
    pub page: &'static str,
    /// One-line statement of the rule.
    pub statement: &'static str,
}

macro_rules! rules {
    ($( $ident:ident, $id:literal, $scope:ident, $sev:ident, $check:expr, $clause:literal, $page:literal, $stmt:literal; )*) => {
        /// A rule of the catalog.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[allow(missing_docs, clippy::upper_case_acronyms)]
        pub enum Rule { $( $ident, )* }

        /// Every rule, in catalog order.
        pub const ALL_RULES: &[Rule] = &[ $( Rule::$ident, )* ];

        impl Rule {
            /// The catalog entry.
            pub fn info(self) -> &'static RuleInfo {
                match self {
                    $( Rule::$ident => {
                        const INFO: RuleInfo = RuleInfo {
                            id: $id,
                            scope: Scope::$scope,
                            severity: Severity::$sev,
                            check: $check,
                            clause: $clause,
                            page: $page,
                            statement: $stmt,
                        };
                        &INFO
                    } )*
                }
            }
        }
    };
}

use Check::Yes;

rules! {
    // ------------------------------------------------------------------ packet layer
    TsSyncLoss, "TS-SYNC-LOSS", Packet, S1, Yes, "TR 101 290 §5.2.1 item 1.1; H.222.0 Annex G.1", "22",
        "Synchronization is lost after two consecutive corrupted sync bytes and regained after five consecutive correct ones (188-byte packets); bytes outside sync are not analyzed.";
    TsSyncByte, "TS-SYNC-BYTE", Packet, S1, Yes, "TR 101 290 §5.2.1 item 1.2; H.222.0 §2.4.3.3", "22; 21",
        "Every packet starts with sync_byte 0x47, 188 bytes after the previous one.";
    TsTei, "TS-TEI", Packet, S1, Yes, "TR 101 290 §5.2.2 item 2.1; H.222.0 §2.4.3.3", "23; 21",
        "transport_error_indicator is 0 (1 marks an uncorrectable error; nothing else is derived from such a packet).";
    TsAfcReserved, "TS-AFC-RESERVED", Packet, S2, Yes, "H.222.0 §2.4.3.3, Table 2-5", "22",
        "adaptation_field_control 00 is reserved; decoders discard the packet.";
    TsAfLength, "TS-AF-LENGTH", Packet, S1, Yes, "H.222.0 §2.4.3.4, §2.4.3.5", "23-24",
        "adaptation_field_length is 183 when adaptation_field_control is 10 and 0 to 182 when it is 11, and the fields its flags announce fit inside it.";
    TsOpcr, "TS-OPCR", Packet, S2, Yes, "H.222.0 §2.4.3.5", "26",
        "An OPCR is coded only in a packet that also carries a PCR.";
    TsNull, "TS-NULL", Packet, S2, Yes, "H.222.0 §2.4.3.3", "21-22",
        "Null packets (PID 0x1FFF) have payload_unit_start_indicator 0, transport_scrambling_control 00 and adaptation_field_control 01.";
    TsPidReserved, "TS-PID-RESERVED", Packet, S2, Yes, "H.222.0 §2.4.3.3, Table 2-3", "22",
        "PIDs 0x0004 to 0x000F are reserved.";
    TsPcrPid, "TS-PCR-PID", Packet, S2, Yes, "H.222.0 §2.4.3.3, Table 2-3 note", "22",
        "Only packets of PIDs 0x0000, 0x0001 and 0x0010 to 0x1FFE may carry a PCR.";
    TsDiscontinuity, "TS-DISCONTINUITY", Packet, S4, Yes, "H.222.0 §2.4.3.5; SPEC §5.2", "24-25",
        "discontinuity_indicator is set: a time-base or continuity discontinuity is announced (legal; this project's output never sets it).";
    TsDvbPid, "TS-DVB-PID", Packet, S4, Yes, "TR 101 290 §5.2.3 items 3.1-3.8; SCTE 54 2009 Table 5.7", "25-26; 23",
        "PIDs 0x0010 to 0x0014 (DVB NIT, SDT, EIT, RST, TDT) carry packets. ATSC has no NIT or SDT, so TR 101 290 indicators 3.1, 3.2 and 3.5 to 3.8 do not apply; their PIDs should be unused.";
    CcError, "CC-ERROR", Packet, S1, Yes, "TR 101 290 §5.2.1 item 1.4; H.222.0 §2.4.3.3", "22-23; 22",
        "continuity_counter increments by one (mod 16) per packet with payload on each PID other than 0x1FFF; it does not increment in adaptation-only packets; a jump is legal only with discontinuity_indicator set.";
    CcDuplicate, "CC-DUPLICATE", Packet, S1, Yes, "TR 101 290 §5.2.1 item 1.4; H.222.0 §2.4.3.3", "22-23; 22",
        "A packet may be sent twice (two consecutive packets with the same continuity_counter), never three or more times.";
    CcDupContent, "CC-DUP-CONTENT", Packet, S2, Yes, "H.222.0 §2.4.3.3", "22",
        "A duplicate packet repeats every byte of the original, except that a PCR, if present, carries a valid value.";
    PidReservedScte, "PID-RANGE", Packet, S3, Yes, "SCTE 54 2024 §7.9.4; SCTE 54 2009 Table 5.7", "23; 23",
        "PMT PIDs and elementary PIDs are within 0x0030 to 0x1FEF; PID 0x1FFE (DOCSIS) is not used.";

    // ------------------------------------------------------------------ sections
    SectCrc, "SECT-CRC", Section, S1, Yes, "TR 101 290 §5.2.2 item 2.2; H.222.0 Annex A; A/65 §4.1", "23-24; 152; 20",
        "CRC_32 (polynomial 0x04C11DB7, initial value all ones, no reflection) of every long-form section (PAT, CAT, PMT and all PSIP tables) leaves the decoder register at zero. Nothing else is derived from a section that fails.";
    SectPointer, "SECT-POINTER", Section, S1, Yes, "H.222.0 §2.4.4.1, §2.4.4.2", "46",
        "pointer_field points inside the payload, and exactly at the end of the section that is in progress on the PID.";
    SectPusi, "SECT-PUSI", Section, S1, Yes, "H.222.0 §2.4.3.3", "21",
        "A packet that carries the first byte of a section has payload_unit_start_indicator 1; a packet with it set carries a section start.";
    SectStuffing, "SECT-STUFFING", Section, S2, Yes, "H.222.0 §2.4.4.2", "46",
        "Once a stuffing byte 0xFF follows a section in a packet, every remaining byte of that packet is 0xFF.";
    SectIncomplete, "SECT-INCOMPLETE", Section, S1, Yes, "H.222.0 §2.4.4", "46",
        "A section is not cut short by the start of the next one (a section lost to a continuity error is counted under CC-ERROR only).";
    SectLength, "SECT-LENGTH", Section, S1, Yes, "H.222.0 §2.4.4.5, §2.4.4.7, §2.4.4.9, §2.4.4.11; A/65 §6.1-6.6", "48-53; 24-45",
        "section_length does not exceed the table's maximum: 1021 (first two bits 00) for PAT, CAT, PMT, STT, VCT and RRT; 4093 for MGT, EIT, ETT and other private sections; and the section is long enough for its fixed fields.";
    SectSyntax, "SECT-SYNTAX", Section, S2, Yes, "H.222.0 §2.4.4.5, §2.4.4.9; A/65 §4.1", "48, 50-51; 18-19",
        "section_syntax_indicator is 1 for PAT, CAT, PMT and PSIP tables; the bit after it is '0' for PAT, CAT and PMT and private_indicator '1' for PSIP; section_number does not exceed last_section_number; loops and descriptors fit the section.";
    SectSingle, "SECT-SINGLE", Section, S2, Yes, "H.222.0 §2.4.4.9; A/65 §6.1, §6.2, §6.4, §6.6", "50; 24, 27, 37, 45",
        "PMT, STT, MGT, RRT and ETT are single sections: section_number and last_section_number are 0.";
    SectLastSection, "SECT-LAST-SECTION", Section, S2, Yes, "H.222.0 §2.4.4.5; A/65 §6.3.1", "48; 31",
        "All sections of one version of a table carry the same last_section_number.";
    SectVersionStep, "SECT-VERSION-STEP", Section, S2, Yes, "H.222.0 §2.4.4.5, §2.4.4.9, §2.4.3.5; A/65 §6.2, §6.3.1, §6.5, §6.6", "48, 51, 25; 27, 31, 42, 45",
        "version_number changes by +1 modulo 32 (a single jump is allowed after discontinuity_indicator on the table's PID).";
    SectVersionStale, "SECT-VERSION-STALE", Section, S2, Yes, "H.222.0 §2.4.4.5, §2.4.4.9; A/65 §6.2, §6.3.1, §6.5, §6.6", "48, 51; 27, 31, 42, 45",
        "The content of a table changes only together with a new version_number (the STT, whose version is always 0, excepted).";
    SectVersionNoop, "SECT-VERSION-NOOP", Section, S4, Yes, "H.222.0 §2.4.4.5", "48",
        "version_number changed although the content of the table is identical: receivers re-acquire the table for nothing.";
    SectCurrentNext, "SECT-CURRENT-NEXT", Section, S3, Yes, "A/65 §6.1, §6.2, §6.5, §6.6, §6.4", "24, 27, 43, 45, 38",
        "current_next_indicator is 1 in STT, MGT, EIT, ETT and RRT sections (\"next\" tables exist only for PAT, CAT, PMT and VCT).";
    SectTablePid, "SECT-TABLE-PID", Section, S2, Yes, "H.222.0 §2.4.4.4, Table 2-31; A/65 §5, §6.1-§6.6", "47; 20-45",
        "Each table is on its PID: PMT only on PMT PIDs, STT, MGT, VCT and RRT only on 0x1FFB, EIT only on EIT PIDs and ETT only on ETT PIDs of the MGT, and no PAT, CAT or PMT on 0x1FFB.";

    // ------------------------------------------------------------------ PSI
    PatIntervalTr, "PAT-INTERVAL-TR", Psi, S1, Yes, "TR 101 290 §5.2.1 item 1.3a", "22",
        "A section with table_id 0x00 occurs on PID 0x0000 at least every 0.5 s.";
    PatTableId, "PAT-TABLE-ID", Psi, S1, Yes, "TR 101 290 §5.2.1 item 1.3a; H.222.0 §2.4.4.4", "22; 47",
        "PID 0x0000 carries only sections with table_id 0x00.";
    PatScrambled, "PAT-SCRAMBLED", Psi, S1, Yes, "TR 101 290 §5.2.1 item 1.3a", "22",
        "transport_scrambling_control is 00 on PID 0x0000.";
    PatInterval, "PAT-INTERVAL", Psi, S3, Yes, "SCTE 54 §7.5; A/53-3 §5.4.1", "14; 12",
        "The last bytes of successive program_association_sections are at most 100 ms apart (140 ms only when needed to stay under the 80 kbit/s system data rate).";
    PatProgramDup, "PAT-PROGRAM-DUP", Psi, S2, Yes, "H.222.0 §2.4.4.5", "48",
        "A program_number occurs at most once in one version of the PAT, with one program_map_PID.";
    PatNit, "PAT-NIT", Psi, S4, Yes, "A/53-3 §5.4.1; A/65 §5 footnote 7", "12; 20",
        "program_number 0 (network_PID) should be avoided: ATSC defines no NIT.";
    PatPmtPidShared, "PAT-PMT-PID-SHARED", Psi, S3, Yes, "A/53-3 §5.4.1; SCTE 54 §7.5", "12; 14",
        "A PMT PID carries one program definition: two programs of the PAT do not share a program_map_PID.";
    PatTsid, "PAT-TSID", Psi, S3, Yes, "TR 101 290 §5.3.4.1; A/65 Annex G.5.2", "36; 138-139",
        "transport_stream_id equals the configured value (option --tsid) and does not change: receivers match it against the VCT's channel_TSID and remember it with the frequency.";
    PsiAdaptation, "PSI-AF", Psi, S3, Yes, "A/53-3 §5.4.1; SCTE 54 §7.5", "12; 14-15",
        "Packets of PID 0x0000 and of PMT PIDs carry an adaptation field only to set discontinuity_indicator (a version discontinuity).";
    PmtIntervalTr, "PMT-INTERVAL-TR", Psi, S1, Yes, "TR 101 290 §5.2.1 item 1.5a", "22",
        "A section with table_id 0x02 occurs on each program_map_PID of the PAT at least every 0.5 s.";
    PmtScrambled, "PMT-SCRAMBLED", Psi, S1, Yes, "TR 101 290 §5.2.1 item 1.5a", "22",
        "transport_scrambling_control is 00 on every packet of a PMT PID.";
    PmtInterval, "PMT-INTERVAL", Psi, S3, Yes, "SCTE 54 §7.5; A/53-3 §5.4.1", "14; 12",
        "The last bytes of successive TS_program_map_sections of a program are at most 400 ms apart.";
    PmtProgram, "PMT-PROGRAM", Psi, S2, Yes, "H.222.0 §2.4.4.9; A/53-3 §5.4.1; SCTE 54 §7.5", "51; 12; 14",
        "A PMT PID carries the TS_program_map_section of the program_number the PAT assigns to it, and no other program's.";
    PmtPrivateReg, "PMT-PRIVATE-REG", Psi, S3, Yes, "SCTE 54 §7.2.4; A/53-3 §5.2.1.3, §5.6.2", "13; 11, 14",
        "An elementary stream with a user-private stream_type (0xC4 to 0xFF) carries a registration descriptor in its ES_info loop.";
    PmtStreamType, "PMT-STREAM-TYPE", Psi, S4, Yes, "SCTE 54 §7.9.2, Table 3", "19",
        "stream_type is one of those SCTE 54 lists (0x01, 0x02, 0x05, 0x06, 0x0F, 0x11, 0x1B, 0x24, 0x2D, 0x2E, 0x80, 0x81, 0x87, 0x88) or user private; others are worth a look.";
    PmtVideoCount, "PMT-VIDEO-COUNT", Psi, S3, Yes, "SCTE 54 §7.4; A/53-3 §5.1.3.1", "14; 10",
        "A program carries at most one video elementary stream.";
    PmtAudioCm, "PMT-AUDIO-CM", Psi, S3, Yes, "SCTE 54 §7.3; A/53-3 §5.1.3.1", "14; 10-11",
        "A program with AC-3 audio has at least one complete main service (bsmod 0 in its AC-3 audio descriptor).";
    PmtDsa, "PMT-DSA", Psi, S3, Yes, "A/53-3 §5.4.1; A/53-3 Amendment 1 §5.4.1", "12; 4-5",
        "Every MPEG-2 video (0x02) and AVC video (0x1B) elementary stream has a data_stream_alignment_descriptor of length 1 with alignment_type 0x02.";
    PmtAudioDesc, "PMT-AUDIO-DESC", Psi, S3, Yes, "A/53-3 §5.8.1.1, §5.8.1.3; SCTE 54 §7.9.3.1, §7.9.3.3", "15-16; 19-20",
        "Every AC-3 (0x81) elementary stream has an AC-3_audio_stream_descriptor (0x81) and every E-AC-3 (0x87) one an E-AC-3 descriptor (0xCC) in its ES_info loop.";
    PcrMissing, "PCR-MISSING", Psi, S1, Yes, "H.222.0 §2.4.4.9, §2.7.2", "51; 109",
        "The PCR_PID of each program (other than 0x1FFF) carries PCRs.";

    // ------------------------------------------------------------------ descriptors
    DescLoop, "DESC-LOOP", Descriptor, S1, Yes, "H.222.0 §2.4.4.9, §2.6", "50-51, 68",
        "Descriptors exactly fill their loop (program_info_length, ES_info_length, and the descriptor loops of PSIP tables).";
    DescDup, "DESC-DUP", Descriptor, S3, Yes, "A/53-3 §5.8", "15",
        "No descriptor tag occurs twice in one PMT descriptor loop (registration descriptors are counted by DESC-REG-DUP).";
    DescRegDup, "DESC-REG-DUP", Descriptor, S3, Yes, "SCTE 54 §7.2.1; A/53-3 §5.2.1", "12; 11",
        "At most one registration descriptor in a descriptor loop.";
    DescGa94, "DESC-GA94", Descriptor, S4, Yes, "A/53-3 §5.2.1.1; SCTE 54 §7.2.2", "11; 12",
        "The program loop has no registration descriptor \"GA94\" (optional; it marks the program as ATSC).";
    DescAc3, "DESC-AC3", Descriptor, S2, Yes, "A/52 Annex A §A4.3, Tables A4.1-A4.6", "116-120",
        "AC-3_audio_stream_descriptor: its fields fit the length; sample_rate_code is not 011; surround_mode is not 11; num_channels is not 1110 or 1111; priority is not 00; langcod and langcod2 are 0xFF; language_flag_2 is 1 only with num_channels 0000.";
    DescAc3Atsc, "DESC-AC3-ATSC", Descriptor, S3, Yes, "A/53-3 §5.8.1.1; SCTE 54 §7.9.3.1", "15; 20",
        "AC-3 descriptor on ATSC/cable: bit_rate_code signals at most 448 kbit/s (0 to 15 or 32 to 47) and num_channels is 1 to 13.";
    DescAc3Stream, "DESC-AC3-STREAM", Descriptor, S3, Yes, "A/53-3 §5.8.1.1 item 4; SCTE 54 §7.9.3.1 item 5; A/52 §A4.3", "15; 20; 117-118",
        "The AC-3 descriptor matches the elementary stream: bsmod, bsid, sample rate, acmod (num_channels below 8) and the exact bit rate (bit_rate_code below 32).";
    DescAc3Language, "DESC-AC3-LANGUAGE", Descriptor, S3, Yes, "A/53-3 §5.8.1.1; SCTE 54 §7.9.3.6", "15; 21",
        "When two or more AC-3 streams of a program carry the same type of service (bsmod), each AC-3 descriptor carries its language.";
    DescAc3Primary, "DESC-AC3-PRIMARY", Descriptor, S3, Yes, "SCTE 54 §7.9.3.6", "21",
        "At most one AC-3 stream of a program is labeled primary (priority 01).";
    DescAc3Mainid, "DESC-AC3-MAINID", Descriptor, S3, Yes, "SCTE 54 §7.6; A/53-3 §5.4.2", "15; 13",
        "Main audio services (bsmod below 2) of a program carry distinct mainid values.";
    DescIso639, "DESC-ISO639", Descriptor, S2, Yes, "H.222.0 §2.6.18, §2.6.19", "78",
        "ISO_639_language_descriptor: the length is a multiple of 4 (language code and audio_type).";
    DescIso639Atsc, "DESC-ISO639-ATSC", Descriptor, S3, Yes, "A/53-3 §5.8.1.2; SCTE 54 §7.9.3.2", "16; 20",
        "For AC-3 audio: the ISO 639 language equals the AC-3 descriptor's language, audio_type is 0x00, and when it is used and two or more AC-3 streams share a bsmod every one of them carries it.";
    DescComponentName, "DESC-COMPONENT-NAME", Descriptor, S3, Yes, "SCTE 54 §7.9.3.9; A/65 §6.9.7", "21; 74",
        "Audio components of a program with the same language and the same service type (bsmod) each carry a component_name_descriptor (0xA3).";
    DescCaption, "DESC-CAPTION", Descriptor, S3, Yes, "A/65 §6.9.2, Table 6.26", "68-70",
        "caption_service_descriptor: length 1 + 6 x number_of_services; number_of_services 1 to 16; caption_service_number 1 to 63 when digital_cc is 1.";

    // ------------------------------------------------------------------ PES
    PesStart, "PES-START", Pes, S1, Yes, "H.222.0 §2.4.3.3, §2.4.3.6", "21, 33",
        "On a PES PID, the payload of a packet with payload_unit_start_indicator 1 begins with packet_start_code_prefix 0x000001.";
    PesHeader, "PES-HEADER", Pes, S1, Yes, "H.222.0 §2.4.3.6 Table 2-21, §2.4.3.7", "33-37",
        "PES header: the '10' bits; PES_header_data_length covers the optional fields its flags announce; PTS_DTS_flags is not 01.";
    PesTsMarker, "PES-TS-MARKER", Pes, S2, Yes, "H.222.0 §2.4.3.6 Table 2-21", "34",
        "PTS and DTS fields carry their 4-bit prefix ('0010' PTS only, '0011' PTS with DTS, '0001' DTS) and the three marker bits.";
    PesLength, "PES-LENGTH", Pes, S1, Yes, "H.222.0 §2.4.3.7", "35",
        "A non-zero PES_packet_length equals the bytes of the PES packet after the field, up to where the next PES packet starts on the PID.";
    PesLengthZero, "PES-LENGTH-ZERO", Pes, S2, Yes, "H.222.0 §2.4.3.7", "35",
        "PES_packet_length 0 (unbounded) is allowed only for video elementary streams in transport streams.";
    PesLengthVideo, "PES-LENGTH-VIDEO", Pes, S3, Yes, "A/53-3 §5.5.1", "13",
        "MPEG-2 video (stream_type 0x02) PES packets have PES_packet_length 0x0000.";
    PesStreamId, "PES-STREAM-ID", Pes, S3, Yes, "A/52 Annex A §A4.2; A/53-3 §5.5.2; SCTE 54 §7.7.1; H.222.0 Table 2-22", "115; 14; 16; 36",
        "AC-3 and E-AC-3 PES packets use stream_id 0xBD (private_stream_1); MPEG-2 and AVC video use 0xE0 to 0xEF.";
    PesFlags, "PES-FLAGS", Pes, S3, Yes, "A/53-3 §5.5; SCTE 54 §7.7", "13; 15-16",
        "PES_scrambling_control is 00 and ESCR_flag, ES_rate_flag, PES_CRC_flag, PES_private_data_flag, pack_header_field_flag, program_packet_sequence_counter_flag and P-STD_buffer_flag are 0.";
    PesVideoPts, "PES-VIDEO-PTS", Pes, S3, Yes, "A/53-3 §5.5.1", "13",
        "Every MPEG-2 video PES header carries a PTS.";
    PesVideoAlign, "PES-VIDEO-ALIGN", Pes, S3, Yes, "A/53-3 §5.5.1", "13",
        "MPEG-2 video PES packets have data_alignment_indicator 1 and their payload begins with an access unit (sequence header, group of pictures header or picture start code).";
    PesDtsRedundant, "PES-DTS-REDUNDANT", Pes, S2, Yes, "H.222.0 §2.7.5", "110",
        "A DTS appears only when the decoding time differs from the presentation time: a DTS equal to the PTS is not coded.";
    PesDtsOrder, "PES-DTS-ORDER", Pes, S2, Yes, "H.222.0 §2.4.2.4", "18",
        "The decoding time is not after the presentation time (DTS <= PTS, modulo 2^33).";
    PesPtsNoAu, "PES-PTS-NO-AU", Pes, S2, Yes, "H.222.0 §2.7.5", "110",
        "A PTS is coded only in a PES packet in which a picture start code (video) or an audio frame (AC-3) begins.";
    PtsRepetition, "PTS-REPETITION", Pes, S2, Yes, "TR 101 290 §5.2.2 item 2.5; H.222.0 §2.7.4", "23-24; 109",
        "PES packets with a PTS arrive at most 700 ms apart on every video and audio PID.";

    // ------------------------------------------------------------------ PCR and rate
    PcrRepetition, "PCR-REPETITION", Pcr, S1, Yes, "TR 101 290 §5.2.2 item 2.3a; H.222.0 §2.7.2", "23; 109",
        "The bytes holding the last bit of successive PCR bases on a PCR_PID arrive at most 100 ms apart (measured on the stream's byte positions at its rate).";
    PcrDiscontinuity, "PCR-DISCONTINUITY", Pcr, S1, Yes, "TR 101 290 §5.2.2 item 2.3b; H.222.0 §2.4.3.5", "23; 24",
        "The difference of successive PCR values is within 0 to 100 ms unless discontinuity_indicator is set.";
    PcrAccuracy, "PCR-ACCURACY", Pcr, S2, Yes, "TR 101 290 §5.2.2 item 2.4, §5.3.2.6; H.222.0 §2.4.2.2", "23, 33; 14",
        "Each PCR is within +-500 ns of the value its byte position gives on a straight line fitted to the PID's PCRs over about two seconds (the TS rate taken as constant).";
    PcrAccuracyNominal, "PCR-ACCURACY-NOMINAL", Pcr, S3, Yes, "H.222.0 §2.4.2.2; TR 101 290 §5.3.2.6; SPEC §5.1", "14; 33",
        "With --rate: each PCR is within +-500 ns of the value its byte position gives at the nominal rate (offset fitted over the same window).";
    PcrInterval40, "PCR-INTERVAL-40", Pcr, S4, Yes, "TR 101 290 Table 5.0b note 2; standards-digest §1.2", "24",
        "PCRs more than 40 ms apart: the former DVB limit, and the upper end of this project's recommended 20 to 40 ms.";
    PcrIntervalTarget, "PCR-INTERVAL-TARGET", Pcr, S4, Yes, "SPEC §6.3", "-",
        "PCRs further apart than the configured target (option --pcr-target-ms, default 30 ms, this project's mux setting).";
    TsRate, "TS-RATE", Pcr, S3, Yes, "H.222.0 §2.4.2.1; SCTE 54 §11", "12; 25",
        "With --rate: the rate measured from the PCRs is within +-30 ppm of the nominal rate (the system clock is 27 MHz +-810 Hz, so a correct clock and a correct rate agree to that).";
    TsVbr, "TS-VBR", Pcr, S4, Yes, "H.222.0 §2.4.2.2; TR 101 290 §5.2.2 item 2.4", "14; 24",
        "The rate between successive PCRs varies by more than 0.5 %: the stream is not constant bit rate, so PCR accuracy and the time base of the other checks are approximate.";
    TimeBase, "TIME-BASE", Pcr, S4, Yes, "H.222.0 §2.4.2.2", "13",
        "No time base: the stream has no PCR and no --rate was given, so no interval, rate or buffer check was made.";

    // ------------------------------------------------------------------ PSIP
    PsipAf, "PSIP-AF", Psip, S3, Yes, "A/65 §6.1, §6.2, §6.3.1, §6.3.2, §6.4, §6.5, §6.6", "23, 25, 29, 34, 37, 41, 44",
        "Packets of 0x1FFB and of EIT and ETT PIDs have adaptation_field_control 01: no adaptation field.";
    PsipScrambled, "PSIP-SCRAMBLED", Psip, S3, Yes, "A/65 §6.1, §6.2, §6.3.1, §6.3.2, §6.4, §6.5, §6.6", "23, 25, 29, 34, 37, 41, 44",
        "Packets of 0x1FFB and of EIT and ETT PIDs have transport_scrambling_control 00.";
    MgtPacketStart, "MGT-PACKET-START", Psip, S3, Yes, "A/65 §6.2", "25",
        "The packet carrying the MGT's table_id has payload_unit_start_indicator 1 and pointer_field 0x00.";
    PsipProtocol, "PSIP-PROTOCOL-VERSION", Psip, S3, Yes, "A/65 §6.1-§6.6", "24, 27, 31, 39, 43, 45",
        "protocol_version is 0 in every PSIP section.";
    PsipRequired, "PSIP-REQUIRED", Psip, S3, Yes, "SCTE 54 §7.8.1, §7.8.1.1; A/65 §5", "16-17; 21-23",
        "System information is present: an MGT, an STT and a CVCT or TVCT on 0x1FFB (checked when the stream is longer than the table's maximum cycle).";
    MgtInterval, "MGT-INTERVAL", Psip, S3, Yes, "SCTE 54 §7.8.1.2 Table 1; A/65 Table 7.1", "17; 83",
        "The MGT repeats at most every 150 ms.";
    VctInterval, "VCT-INTERVAL", Psip, S3, Yes, "SCTE 54 §7.8.1.2 Table 1; A/65 Table 7.1", "17; 83",
        "Each section of the CVCT (or TVCT) repeats at most every 400 ms.";
    SttInterval, "STT-INTERVAL", Psip, S3, Yes, "SCTE 54 §7.8.1.2 Table 1", "17",
        "The STT repeats at most every 10 s (cable).";
    SttInterval1s, "STT-INTERVAL-1S", Psip, S4, Yes, "A/65 Table 7.1; standards-digest §2.3", "83",
        "The STT repeats more often than every 1 s (the terrestrial maximum, and what receivers are commonly designed around).";
    RrtInterval, "RRT-INTERVAL", Psip, S3, Yes, "SCTE 54 §7.8.1.2 Table 1; A/65 Table 7.1", "17; 83",
        "An RRT, when present, repeats at most every 60 s.";
    RrtSyntax, "RRT-SYNTAX", Psip, S3, Yes, "A/65 §6.4, Table 6.10", "38-40",
        "An RRT: rating_region not 0 and the reserved byte of table_id_extension 0xFF, at least one dimension of 1 to 15 values, value 0 of each dimension with null strings, every string well formed, and nothing after the descriptors.";
    AdvisorySyntax, "ADVISORY-SYNTAX", Psip, S3, Yes, "A/65 §6.9.3, Table 6.27", "69-70",
        "A content_advisory_descriptor: 1 to 8 rating regions, each once, rating dimensions in increasing order, rating_description_length at most 80 and its text well formed, and its length exactly what its fields take.";
    AdvisoryNoRrt, "ADVISORY-NO-RRT", Psip, S3, Yes, "A/65 §6.4", "38",
        "A content advisory that rates in a region other than 1 (US) and 2 (Canada), whose systems CTA-766 defines, needs that region's RRT in the transport stream.";
    AdvisoryUndefined, "ADVISORY-UNDEFINED", Psip, S3, Yes, "A/65 §6.9.3", "70",
        "A content advisory's dimensions and values in a region with an RRT are ones that RRT defines.";
    Eit0Interval, "EIT0-INTERVAL", Psip, S4, Yes, "SCTE 54 §7.8.1.2; A/65 §7.1", "17; 83",
        "Each section of EIT-0 repeats at most every 500 ms (recommended).";
    PsipRate, "PSIP-RATE", Psip, S3, Yes, "SCTE 54 §7.8.1.2 Table 2; A/65 Table 7.2", "18; 83",
        "0x1FFB and every EIT and ETT PID carry at most 250,000 bit/s over any 1 s (measured in 100 ms slices).";
    SttHeader, "STT-HEADER", Psip, S3, Yes, "A/65 §6.1, Table 6.1", "24",
        "The STT has table_id_extension 0x0000 and version_number 0.";
    SttTime, "STT-TIME", Psip, S3, Yes, "A/65 §6.1", "24-25",
        "system_time advances with the stream: between two STTs it moves by the elapsed stream time within +-2 s (each value is accurate to +-1 s).";
    SttGpsUtc, "STT-GPS-UTC", Psip, S4, Yes, "A/65 §6.1; standards-digest §3.1 [K]", "25",
        "GPS_UTC_offset is 18 s, the value since 2017-01-01 (no leap second has been announced since).";
    SttDs, "STT-DS", Psip, S3, Yes, "A/65 Annex A, Tables A1, A2", "84-85",
        "daylight_saving: DS_hour is 0 to 18, and 0 when DS_day_of_month is 0.";
    MgtHeader, "MGT-HEADER", Psip, S3, Yes, "A/65 §6.2", "27",
        "The MGT has table_id_extension 0x0000 and tables_defined 2 to 370 (cable).";
    MgtTableType, "MGT-TABLE-TYPE", Psip, S3, Yes, "A/65 §6.2 Table 6.3, §6.5, §6.6", "27, 41, 44",
        "table_type values are not reserved and occur once; each EIT and ETT PID is unique among the table_type_PIDs and is not 0x1FFB; VCT and RRT are on 0x1FFB.";
    MgtVersion, "MGT-VERSION", Psip, S3, Yes, "A/65 §6.2", "27-28",
        "table_type_version_number equals the version_number of the table on table_type_PID.";
    MgtNumberBytes, "MGT-NUMBER-BYTES", Psip, S3, Yes, "A/65 §6.2", "28",
        "number_bytes equals the total size of all sections of all instances of the table type (compared once the table's sections have all been seen twice, and at the end of the stream).";
    MgtUnlisted, "MGT-UNLISTED", Psip, S3, Yes, "A/65 §6.2", "25",
        "Every VCT, EIT, ETT and RRT present is listed in the MGT.";
    VctTsid, "VCT-TSID", Psip, S3, Yes, "A/65 §6.3.1", "31",
        "The VCT's transport_stream_id equals the PAT's.";
    VctNumber, "VCT-NUMBER", Psip, S3, Yes, "A/65 §6.3.1, §6.3.2; SCTE 54 §7.8.1.1", "32, 35; 17",
        "Channel numbers: TVCT major 1 to 99 and minor 0 to 999 (1 to 99 for service types 0x02 and 0x03); CVCT two-part numbers both below 1000, or a one-part number (major's six high bits 111111).";
    VctNumberDup, "VCT-NUMBER-DUP", Psip, S3, Yes, "A/65 §6.3.1, §6.3.2", "32, 35",
        "No major/minor channel number pair occurs twice in the VCT.";
    VctShortName, "VCT-SHORT-NAME", Psip, S3, Yes, "A/65 §6.3.1", "31-32",
        "short_name is one to seven valid UTF-16 code values padded with 0x0000.";
    VctCarrier, "VCT-CARRIER-FREQ", Psip, S3, Yes, "A/65 §6.3.1", "32",
        "carrier_frequency is 0.";
    VctFields, "VCT-FIELDS", Psip, S3, Yes, "A/65 §6.3.1, §6.3.2, Tables 6.5, 6.6", "32-34, 37",
        "modulation_mode is not reserved (0x00, 0x06 to 0x7F); ETM_location is not 11; service_type is not 0x00; source_id 0 only in a CVCT; inactive channels (program_number 0) are hidden.";
    VctSourceDup, "VCT-SOURCE-ID-DUP", Psip, S3, Yes, "A/65 §6.3.1, §6.3.2", "34, 37",
        "source_id values 0x0001 to 0x0FFF are unique within the transport stream (channels of different programs do not share one).";
    EitWindow, "EIT-WINDOW", Psip, S3, Yes, "A/65 §5, §6.5", "21-22, 40",
        "Every event in EIT-k overlaps the k-th 3-hour UTC window after the one containing the STT's time (checked away from window boundaries).";
    EitOrder, "EIT-ORDER", Psip, S3, Yes, "A/65 §6.5", "41, 43",
        "Events of an EIT instance are in start-time order and do not overlap: each start_time is at least the previous event's start_time plus length_in_seconds.";
    EttEtmId, "ETT-ETM-ID", Psip, S3, Yes, "A/65 §6.6, Table 6.14", "45",
        "ETM_id: a channel ETM has bits 15 to 0 zero and is on the channel ETT PID; an event ETM has bits 1 to 0 '10' and is on an event ETT PID.";
    EttTidExt, "ETT-TID-EXT", Psip, S3, Yes, "A/65 §6.6 and footnote 13", "45",
        "ETT instances on one PID have distinct ETT_table_id_extension values (A/65 warns that some deployed receivers fail on non-zero values).";
    MssSyntax, "MSS-SYNTAX", Psip, S3, Yes, "A/65 §6.10, Tables 6.39-6.41", "79-82",
        "multiple_string_structure fits its field; compression_type is 0x00 to 0x02 (or 0xB0 and above); Huffman only with mode 0x00; mode is not reserved; UTF-16 segments have an even length and valid surrogates.";
    MssHuffman, "MSS-HUFFMAN", Psip, S4, Yes, "A/65 §6.10, Annex C", "80",
        "A string segment is Huffman compressed (compression_type 0x01 or 0x02): legal, but this checker does not decode it (the tables of Annex C are not implemented), so its text is not checked.";

    // ------------------------------------------------------------------ cross-checks
    PidMissing, "PID-MISSING", CrossCheck, S1, Yes, "TR 101 290 §5.2.1 item 1.6", "22-23",
        "A PID referenced by a PMT (elementary and PCR PIDs) or by the MGT carries packets: elementary and PCR PIDs at least every --pid-timeout (1 s), MGT PIDs at least every --psip-pid-timeout (60 s).";
    PidUnreferenced, "PID-UNREFERENCED", CrossCheck, S3, Yes, "TR 101 290 §5.2.3 item 3.4a", "25",
        "Every PID other than 0x0000 to 0x001F, 0x1FFB and 0x1FFF is referenced by the PAT, a PMT, the CAT or the MGT within 0.5 s of appearing.";
    PidConflict, "PID-CONFLICT", CrossCheck, S2, Yes, "H.222.0 §2.4.3.3, §2.4.4.5, §2.4.4.9", "21, 48, 51",
        "A PID has one role: it is not both a PMT PID and an elementary PID, nor an elementary PID of two stream types, nor a PSI or PSIP PID used for PES.";
    CatError, "CAT-ERROR", CrossCheck, S1, Yes, "TR 101 290 §5.2.2 item 2.6", "24",
        "Scrambled packets (transport_scrambling_control not 00) appear only with a CAT present; PID 0x0001 carries only table_id 0x01.";
    MgtPidTable, "MGT-PID-TABLE", CrossCheck, S3, Yes, "A/65 §6.2, §6.5, §6.6", "25, 41, 44",
        "Each table_type_PID of the MGT carries the table its table_type names (EIT PIDs table_id 0xCB, ETT PIDs 0xCC).";
    VctProgram, "VCT-PROGRAM", CrossCheck, S3, Yes, "A/65 §6.3.1", "33-34",
        "An active virtual channel of this transport stream (channel_TSID equal to the PAT's, program_number neither 0 nor 0xFFFF) names a program of the PAT.";
    VctCoverage, "VCT-COVERAGE", CrossCheck, S3, Yes, "SCTE 54 §7.8.1.1; A/65 §5.2", "17; 23",
        "Every program of the PAT is described by a virtual channel of the VCT.";
    VctChannelTsid, "VCT-CHANNEL-TSID", CrossCheck, S4, Yes, "A/65 §6.3.1; SCTE 54 §7.8.1.1; standards-digest §2.8", "33; 17",
        "A virtual channel names another transport stream (channel_TSID differs from the PAT's): legal, but this project lists only its own programs.";
    VctSld, "VCT-SLD", CrossCheck, S3, Yes, "A/65 §6.9.5", "72-73",
        "A service_location_descriptor equals the PMT on air: the same PCR_PID, and every elementary stream with the same stream_type, PID and language (zeros when the PMT gives none).";
    VctSldMissing, "VCT-SLD-MISSING", CrossCheck, S3, Yes, "A/65 §6.9.5; Annex G.5.3", "71-72; 139",
        "Every active channel of a TVCT carries a service_location_descriptor (not required in a CVCT).";
    EitInstance, "EIT-INSTANCE", CrossCheck, S3, Yes, "A/65 §6.5", "40",
        "Every virtual channel (other than data-only service_type 0x04) has an instance (its source_id) in each EIT-k the MGT lists, with num_events_in_section 0 when it has no event.";
    EitSource, "EIT-SOURCE", CrossCheck, S4, Yes, "A/65 §6.5", "40",
        "An EIT instance names a source_id that no virtual channel of the VCT has.";
    EitSpanId, "EIT-SPAN-ID", CrossCheck, S3, Yes, "A/65 §6.5", "40",
        "An event that extends over two EITs has the same event_id in both.";
    EtmMissing, "ETM-MISSING", CrossCheck, S3, Yes, "A/65 §6.3.1 Table 6.6, §6.5 Table 6.12", "33, 43",
        "A channel or event with ETM_location 01 has its ETM in this transport stream (checked once the ETT PIDs have cycled, and at the end).";

    // ------------------------------------------------------------------ buffer model
    TstdTbOverflow, "TSTD-TB-OVERFLOW", BufferModel, S2, Yes, "H.222.0 §2.4.2.3, §2.4.2.6; TR 101 290 §5.2.3 item 3.3", "14-15, 19; 25",
        "The transport buffer TBn (512 bytes, emptied at Rxn: 1.2 x Rmax for MPEG-2 video, 2 Mbit/s for audio) never overflows.";
    TstdTbsysOverflow, "TSTD-TBSYS-OVERFLOW", BufferModel, S2, Yes, "H.222.0 §2.4.2.3, §2.4.2.6; TR 101 290 §5.2.3 item 3.3", "14-15, 19; 25",
        "TBsys (512 bytes, emptied at 1 Mbit/s; fed by PIDs 0 to 3 and the program's PMT PID) never overflows.";
    TstdTbEmpty, "TSTD-TB-EMPTY", BufferModel, S2, Yes, "H.222.0 §2.4.2.6; TR 101 290 §5.2.3 item 3.9", "19; 26",
        "TBn and TBsys are empty at least once every second.";
    TstdBsysOverflow, "TSTD-BSYS-OVERFLOW", BufferModel, S2, Yes, "H.222.0 §2.4.2.3, §2.4.2.6", "16, 18, 19",
        "Bsys (1536 bytes, emptied at Rsys = max(80 kbit/s, rate / 500)) never overflows.";
    TstdMbOverflow, "TSTD-MB-OVERFLOW", BufferModel, S2, Yes, "H.222.0 §2.4.2.3, §2.4.2.6; H.262 Tables 8-13, 8-14", "15-17, 19",
        "The MPEG-2 video multiplexing buffer MBn (BSmux + BSoh, plus VBVmax - vbv_buffer_size at Main level and below) never overflows (leak method, Rbx = Rmax, or min(1.05 Res, Rmax) at High-1440 and High level).";
    TstdMbEmpty, "TSTD-MB-EMPTY", BufferModel, S2, Yes, "H.222.0 §2.4.2.6", "19",
        "MBn empties at least once every second (leak method).";
    TstdEbUnderflow, "TSTD-EB-UNDERFLOW", BufferModel, S2, Yes, "H.222.0 §2.4.2.3, §2.4.2.6", "18-19",
        "Every byte of a video access unit has reached EBn (size vbv_buffer_size) by its decoding time (DTS, else PTS), unless low_delay is set.";
    TstdBOverflow, "TSTD-B-OVERFLOW", BufferModel, S3, Yes, "A/52 Annex A §A4.4; A/53-5 §5.7; A/53-3 §5.1.2; SCTE 54 §7.1.2", "120; 7; 10; 12",
        "The AC-3 main buffer Bn (2592 bytes, holding PES headers and frames) never overflows.";
    TstdBUnderflow, "TSTD-B-UNDERFLOW", BufferModel, S2, Yes, "H.222.0 §2.4.2.3, §2.4.2.6", "18-19",
        "Every byte of an audio frame has reached Bn by its presentation time.";
    TstdDelay, "TSTD-DELAY", BufferModel, S2, Yes, "H.222.0 §2.4.2.6; TR 101 290 §5.2.3 item 3.10", "19; 26",
        "No byte of an access unit spends more than 1 s in the buffers: the decoding time is at most 1 s after the arrival of the access unit's first byte (10 s for AVC).";
    PsipSbOverflow, "PSIP-SB-OVERFLOW", BufferModel, S3, Yes, "SCTE 54 §7.8.1.2; A/65 §7.1; H.222.0 §2.6.30", "18; 83; 81",
        "The smoothing buffer of 0x1FFB and of every EIT and ETT PID (1024 bytes, whole packets in at the TS rate, out at 250,000 bit/s) never overflows.";

    // ------------------------------------------------------------------ elementary streams
    EsAc3Sync, "ES-AC3-SYNC", Elementary, S2, Yes, "A/52 §5.3, §5.4.1, Table 5.18; H.222.0 §2.4.3.7", "30, 37, 51-52; 37",
        "AC-3 syncframes follow each other: each starts with syncword 0x0B77 where the previous one's frmsizecod ends it, fscod is not 11 and frmsizecod is at most 37; the first frame after a PES start with data_alignment_indicator begins the payload.";
    EsVideoSeq, "ES-VIDEO-SEQ", Elementary, S4, Yes, "H.262 §6.2.2.1, §6.2.2.3; H.222.0 §2.4.2.3", "25-27; 14-15",
        "The MPEG-2 video profile and level (sequence_extension) are ones the buffer model has constants for (Simple, Main, 4:2:2 profiles at their levels); otherwise its MBn and EBn are not modeled.";
}

/// Finds a rule by its identifier.
pub fn rule_by_id(id: &str) -> Option<Rule> {
    ALL_RULES.iter().copied().find(|r| r.info().id == id)
}

/// The catalog as a Markdown document (the body of `docs/notes/ts-conformance-rules.md`).
pub fn catalog_markdown() -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    let sevs = [Severity::S1, Severity::S2, Severity::S3, Severity::S4];
    let _ = writeln!(s, "## Counts\n");
    let _ = writeln!(s, "| Scope | S1 | S2 | S3 | S4 | Total | Checked |");
    let _ = writeln!(s, "|---|---|---|---|---|---|---|");
    let mut tot = [0usize; 6];
    for sc in Scope::ALL {
        let mut row = [0usize; 6];
        for r in ALL_RULES.iter().map(|r| r.info()).filter(|i| i.scope == sc) {
            row[r.severity as usize] += 1;
            row[4] += 1;
            if r.check == Check::Yes {
                row[5] += 1;
            }
        }
        for (t, r) in tot.iter_mut().zip(row) {
            *t += r;
        }
        let _ = writeln!(
            s,
            "| {} | {} | {} | {} | {} | {} | {} |",
            sc.name(),
            row[0],
            row[1],
            row[2],
            row[3],
            row[4],
            row[5]
        );
    }
    let _ = writeln!(
        s,
        "| **all** | {} | {} | {} | {} | {} | {} |\n",
        tot[0], tot[1], tot[2], tot[3], tot[4], tot[5]
    );
    for sc in Scope::ALL {
        let _ = writeln!(s, "## Scope: {}\n", sc.name());
        let _ = writeln!(s, "| ID | Sev | Clause | Page | Rule | Checked |");
        let _ = writeln!(s, "|---|---|---|---|---|---|");
        for sev in sevs {
            for r in ALL_RULES
                .iter()
                .map(|r| r.info())
                .filter(|i| i.scope == sc && i.severity == sev)
            {
                let chk = match r.check {
                    Check::Yes => "yes".to_string(),
                    Check::No(why) => format!("**no**: {why}"),
                };
                let _ = writeln!(
                    s,
                    "| `{}` | {} | {} | {} | {} | {} |",
                    r.id,
                    r.severity.name(),
                    r.clause,
                    r.page,
                    r.statement.replace('|', "\\|"),
                    chk.replace('|', "\\|")
                );
            }
        }
        let _ = writeln!(s);
    }
    s
}
