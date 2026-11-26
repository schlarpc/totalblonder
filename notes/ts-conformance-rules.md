# MPEG-2 transport stream for a US cable carrier: the rule catalog

Every check the transport-stream checker `tb-ts-verify` (crates/tb-ts-verify) makes,
extracted from the text of the standards, not from any mux. The catalog is data in the
checker (`src/rules.rs`); everything below the marker line is printed by
`tb-ts-verify --rules-md`, and the flake check `ts-verify/rules-catalog` fails if this file
and the binary disagree. To change a rule, edit `src/rules.rs` and regenerate:

    nix develop -c cargo run -q -p tb-ts-verify -- --rules-md > /tmp/rules.md
    # then replace everything below the marker line with /tmp/rules.md

**Sources** (text extractions in work/standards/ of the main checkout). Page numbers are the
printed ones; for ITU-T H.222.0 the printed page is the PDF page minus 14, for every other
document the two agree.

- ITU-T H.222.0 (06/2012) = ISO/IEC 13818-1:2013: §2.4.2 (T-STD), §2.4.3 (packets, adaptation
  field, PES), §2.4.4 (sections, PAT, CAT, PMT), §2.6 (descriptors), §2.7 (timing), §2.14.3
  (AVC T-STD), Annex A (CRC).
- ETSI TR 101 290 V1.4.1 (2020-06): §5.2 (the indicators), §5.3.2 (PCR measurements).
- ANSI/SCTE 54 2024: §7 (cable transport), and SCTE 54 2009 Table 5.7 for the PID ranges the
  2024 edition dropped.
- ATSC A/65:2013 (PSIP), A/53 Part 3:2013 and its Amendment 1, A/53 Part 5:2014, A/52:2012
  (Annex A for AC-3 in the transport stream, §5 for the syncframe).
- ITU-T H.262 (02/2000) Tables 8-13 and 8-14 for the video buffer constants.
- SPEC.md for this project's own targets (PCR every 30 ms, the nominal rate).

**Columns.**

- *ID*: stable identifier; findings, the JSON report and `--allow` use it.
- *Sev*: S1 a receiver may fail to tune, lose sync or misdecode (the TR 101 290
  first-priority indicators, transport and CRC errors, PCR timing); S2 illegal per H.222.0
  but commonly tolerated; S3 legal MPEG-2 systems but outside A/53 Part 3, A/65, A/52 Annex A
  or SCTE 54, the rules of a US cable carrier; S4 a recommendation, a target of this project
  or a legal construct worth knowing about. The command line fails on S2 and worse by
  default; this project's mux is held to S3 (`--fail-on S3`).
- *Checked*: **yes** means a test in `tests/rules.rs` builds a stream that violates the rule
  and the checker reports it.

**Scopes.** packet (sync, header, adaptation field, continuity), section (framing, CRC,
versions), PSI (PAT, CAT, PMT and their repetition), descriptor (PMT descriptors), PES,
PCR and rate, PSIP (MGT, VCT, STT, EIT, ETT, RRT and their packets and repetition),
cross-check (between tables and PIDs), buffer model (H.222.0 T-STD, PSIP smoothing buffer),
elementary stream (the framing the buffer model depends on).

**What the text says, which is easy to get wrong** (checked while extracting):

- SCTE 54 §7.5 and A/53-3 §5.4.1 measure the PAT and PMT intervals between "the byte
  containing the last bit" of successive sections, and say "less than or equal to": a PAT
  every 100.02 ms (ffmpeg's `-pat_period 0.1`) violates the 100 ms limit. The 140 ms
  allowance is only for streams whose system data would otherwise exceed 80 kbit/s.
- A/65 requires the MGT to start a packet (pointer_field 0); no other PSIP table has to.
- PAT and PMT packets may carry an adaptation field only for discontinuity_indicator
  (A/53-3 §5.4.1, SCTE 54 §7.5); PSIP packets may not carry one at all (A/65 §6.x).
- The STT's version_number is always 0 (A/65 Table 6.1), so its content changes every
  second without a version change; every other table must change version with content.
- A/65 §6.6 requires distinct ETT_table_id_extension values on a PID, and its footnote 13
  warns that deployed receivers fail on non-zero values. The rule follows the text (S3); the
  conflict is the project's open question 1 in standards-digest.
- A/53-3 §5.5.1 requires PES_packet_length 0 and data_alignment_indicator 1 on MPEG-2 video;
  H.222.0 allows length 0 only for video. A/53-3 §5.4.1 requires the
  data_stream_alignment_descriptor (alignment_type 0x02) on every 0x02 stream.
- The AC-3 main buffer is 2592 bytes (A/53 Part 5 §5.7), not H.222.0's generic 3584 for
  "other audio"; Rx for AC-3 is H.222.0's 2 Mbit/s.
- H.222.0 §2.4.2.3 gives MPEG-2 video at High-1440 and High level an MBn of only
  BSmux + BSoh (40 kB at High-1440) and a leak of min(1.05 x ES rate, Rmax); at Main level
  the unused VBV space is added. A mux that sends an I-picture at the carrier rate overflows
  MBn at High level and TBn (Rx 18 Mbit/s) at Main level.
- TR 101 290's PCR_discontinuity_indicator_error covers any PCR delta outside 0 to 100 ms,
  so PCRs 110 ms apart are reported both as a repetition error and as a discontinuity.
- TR 101 290 has no ATSC profile: its NIT, SDT, EIT, RST and TDT indicators (3.1, 3.2,
  3.5 to 3.8) do not apply to a cable stream; TS-DVB-PID notes packets on their PIDs.
- The SCTE 54 2024 PID range (0x0030 to 0x1FEF) applies to PMT and elementary PIDs only;
  PSIP EIT and ETT PIDs may be anywhere the MGT says.

<!-- generated: tb-ts-verify --rules-md -->
## Counts

| Scope | S1 | S2 | S3 | S4 | Total | Checked |
|---|---|---|---|---|---|---|
| packet | 6 | 6 | 1 | 2 | 15 | 15 |
| section | 5 | 7 | 1 | 1 | 14 | 14 |
| PSI | 6 | 2 | 10 | 2 | 20 | 20 |
| descriptor | 1 | 2 | 10 | 1 | 14 | 14 |
| PES | 3 | 6 | 5 | 0 | 14 | 14 |
| PCR and rate | 2 | 1 | 2 | 4 | 9 | 9 |
| PSIP | 0 | 0 | 30 | 4 | 34 | 34 |
| cross-check | 2 | 1 | 9 | 2 | 14 | 14 |
| buffer model | 0 | 9 | 2 | 0 | 11 | 11 |
| elementary stream | 0 | 1 | 0 | 1 | 2 | 2 |
| **all** | 25 | 35 | 70 | 17 | 147 | 147 |

## Scope: packet

| ID | Sev | Clause | Page | Rule | Checked |
|---|---|---|---|---|---|
| `TS-SYNC-LOSS` | S1 | TR 101 290 §5.2.1 item 1.1; H.222.0 Annex G.1 | 22 | Synchronization is lost after two consecutive corrupted sync bytes and regained after five consecutive correct ones (188-byte packets); bytes outside sync are not analyzed. | yes |
| `TS-SYNC-BYTE` | S1 | TR 101 290 §5.2.1 item 1.2; H.222.0 §2.4.3.3 | 22; 21 | Every packet starts with sync_byte 0x47, 188 bytes after the previous one. | yes |
| `TS-TEI` | S1 | TR 101 290 §5.2.2 item 2.1; H.222.0 §2.4.3.3 | 23; 21 | transport_error_indicator is 0 (1 marks an uncorrectable error; nothing else is derived from such a packet). | yes |
| `TS-AF-LENGTH` | S1 | H.222.0 §2.4.3.4, §2.4.3.5 | 23-24 | adaptation_field_length is 183 when adaptation_field_control is 10 and 0 to 182 when it is 11, and the fields its flags announce fit inside it. | yes |
| `CC-ERROR` | S1 | TR 101 290 §5.2.1 item 1.4; H.222.0 §2.4.3.3 | 22-23; 22 | continuity_counter increments by one (mod 16) per packet with payload on each PID other than 0x1FFF; it does not increment in adaptation-only packets; a jump is legal only with discontinuity_indicator set. | yes |
| `CC-DUPLICATE` | S1 | TR 101 290 §5.2.1 item 1.4; H.222.0 §2.4.3.3 | 22-23; 22 | A packet may be sent twice (two consecutive packets with the same continuity_counter), never three or more times. | yes |
| `TS-AFC-RESERVED` | S2 | H.222.0 §2.4.3.3, Table 2-5 | 22 | adaptation_field_control 00 is reserved; decoders discard the packet. | yes |
| `TS-OPCR` | S2 | H.222.0 §2.4.3.5 | 26 | An OPCR is coded only in a packet that also carries a PCR. | yes |
| `TS-NULL` | S2 | H.222.0 §2.4.3.3 | 21-22 | Null packets (PID 0x1FFF) have payload_unit_start_indicator 0, transport_scrambling_control 00 and adaptation_field_control 01. | yes |
| `TS-PID-RESERVED` | S2 | H.222.0 §2.4.3.3, Table 2-3 | 22 | PIDs 0x0004 to 0x000F are reserved. | yes |
| `TS-PCR-PID` | S2 | H.222.0 §2.4.3.3, Table 2-3 note | 22 | Only packets of PIDs 0x0000, 0x0001 and 0x0010 to 0x1FFE may carry a PCR. | yes |
| `CC-DUP-CONTENT` | S2 | H.222.0 §2.4.3.3 | 22 | A duplicate packet repeats every byte of the original, except that a PCR, if present, carries a valid value. | yes |
| `PID-RANGE` | S3 | SCTE 54 2024 §7.9.4; SCTE 54 2009 Table 5.7 | 23; 23 | PMT PIDs and elementary PIDs are within 0x0030 to 0x1FEF; PID 0x1FFE (DOCSIS) is not used. | yes |
| `TS-DISCONTINUITY` | S4 | H.222.0 §2.4.3.5; SPEC §5.2 | 24-25 | discontinuity_indicator is set: a time-base or continuity discontinuity is announced (legal; this project's output never sets it). | yes |
| `TS-DVB-PID` | S4 | TR 101 290 §5.2.3 items 3.1-3.8; SCTE 54 2009 Table 5.7 | 25-26; 23 | PIDs 0x0010 to 0x0014 (DVB NIT, SDT, EIT, RST, TDT) carry packets. ATSC has no NIT or SDT, so TR 101 290 indicators 3.1, 3.2 and 3.5 to 3.8 do not apply; their PIDs should be unused. | yes |

## Scope: section

| ID | Sev | Clause | Page | Rule | Checked |
|---|---|---|---|---|---|
| `SECT-CRC` | S1 | TR 101 290 §5.2.2 item 2.2; H.222.0 Annex A; A/65 §4.1 | 23-24; 152; 20 | CRC_32 (polynomial 0x04C11DB7, initial value all ones, no reflection) of every long-form section (PAT, CAT, PMT and all PSIP tables) leaves the decoder register at zero. Nothing else is derived from a section that fails. | yes |
| `SECT-POINTER` | S1 | H.222.0 §2.4.4.1, §2.4.4.2 | 46 | pointer_field points inside the payload, and exactly at the end of the section that is in progress on the PID. | yes |
| `SECT-PUSI` | S1 | H.222.0 §2.4.3.3 | 21 | A packet that carries the first byte of a section has payload_unit_start_indicator 1; a packet with it set carries a section start. | yes |
| `SECT-INCOMPLETE` | S1 | H.222.0 §2.4.4 | 46 | A section is not cut short by the start of the next one (a section lost to a continuity error is counted under CC-ERROR only). | yes |
| `SECT-LENGTH` | S1 | H.222.0 §2.4.4.5, §2.4.4.7, §2.4.4.9, §2.4.4.11; A/65 §6.1-6.6 | 48-53; 24-45 | section_length does not exceed the table's maximum: 1021 (first two bits 00) for PAT, CAT, PMT, STT, VCT and RRT; 4093 for MGT, EIT, ETT and other private sections; and the section is long enough for its fixed fields. | yes |
| `SECT-STUFFING` | S2 | H.222.0 §2.4.4.2 | 46 | Once a stuffing byte 0xFF follows a section in a packet, every remaining byte of that packet is 0xFF. | yes |
| `SECT-SYNTAX` | S2 | H.222.0 §2.4.4.5, §2.4.4.9; A/65 §4.1 | 48, 50-51; 18-19 | section_syntax_indicator is 1 for PAT, CAT, PMT and PSIP tables; the bit after it is '0' for PAT, CAT and PMT and private_indicator '1' for PSIP; section_number does not exceed last_section_number; loops and descriptors fit the section. | yes |
| `SECT-SINGLE` | S2 | H.222.0 §2.4.4.9; A/65 §6.1, §6.2, §6.4, §6.6 | 50; 24, 27, 37, 45 | PMT, STT, MGT, RRT and ETT are single sections: section_number and last_section_number are 0. | yes |
| `SECT-LAST-SECTION` | S2 | H.222.0 §2.4.4.5; A/65 §6.3.1 | 48; 31 | All sections of one version of a table carry the same last_section_number. | yes |
| `SECT-VERSION-STEP` | S2 | H.222.0 §2.4.4.5, §2.4.4.9, §2.4.3.5; A/65 §6.2, §6.3.1, §6.5, §6.6 | 48, 51, 25; 27, 31, 42, 45 | version_number changes by +1 modulo 32 (a single jump is allowed after discontinuity_indicator on the table's PID). | yes |
| `SECT-VERSION-STALE` | S2 | H.222.0 §2.4.4.5, §2.4.4.9; A/65 §6.2, §6.3.1, §6.5, §6.6 | 48, 51; 27, 31, 42, 45 | The content of a table changes only together with a new version_number (the STT, whose version is always 0, excepted). | yes |
| `SECT-TABLE-PID` | S2 | H.222.0 §2.4.4.4, Table 2-31; A/65 §5, §6.1-§6.6 | 47; 20-45 | Each table is on its PID: PMT only on PMT PIDs, STT, MGT, VCT and RRT only on 0x1FFB, EIT only on EIT PIDs and ETT only on ETT PIDs of the MGT, and no PAT, CAT or PMT on 0x1FFB. | yes |
| `SECT-CURRENT-NEXT` | S3 | A/65 §6.1, §6.2, §6.5, §6.6, §6.4 | 24, 27, 43, 45, 38 | current_next_indicator is 1 in STT, MGT, EIT, ETT and RRT sections ("next" tables exist only for PAT, CAT, PMT and VCT). | yes |
| `SECT-VERSION-NOOP` | S4 | H.222.0 §2.4.4.5 | 48 | version_number changed although the content of the table is identical: receivers re-acquire the table for nothing. | yes |

## Scope: PSI

| ID | Sev | Clause | Page | Rule | Checked |
|---|---|---|---|---|---|
| `PAT-INTERVAL-TR` | S1 | TR 101 290 §5.2.1 item 1.3a | 22 | A section with table_id 0x00 occurs on PID 0x0000 at least every 0.5 s. | yes |
| `PAT-TABLE-ID` | S1 | TR 101 290 §5.2.1 item 1.3a; H.222.0 §2.4.4.4 | 22; 47 | PID 0x0000 carries only sections with table_id 0x00. | yes |
| `PAT-SCRAMBLED` | S1 | TR 101 290 §5.2.1 item 1.3a | 22 | transport_scrambling_control is 00 on PID 0x0000. | yes |
| `PMT-INTERVAL-TR` | S1 | TR 101 290 §5.2.1 item 1.5a | 22 | A section with table_id 0x02 occurs on each program_map_PID of the PAT at least every 0.5 s. | yes |
| `PMT-SCRAMBLED` | S1 | TR 101 290 §5.2.1 item 1.5a | 22 | transport_scrambling_control is 00 on every packet of a PMT PID. | yes |
| `PCR-MISSING` | S1 | H.222.0 §2.4.4.9, §2.7.2 | 51; 109 | The PCR_PID of each program (other than 0x1FFF) carries PCRs. | yes |
| `PAT-PROGRAM-DUP` | S2 | H.222.0 §2.4.4.5 | 48 | A program_number occurs at most once in one version of the PAT, with one program_map_PID. | yes |
| `PMT-PROGRAM` | S2 | H.222.0 §2.4.4.9; A/53-3 §5.4.1; SCTE 54 §7.5 | 51; 12; 14 | A PMT PID carries the TS_program_map_section of the program_number the PAT assigns to it, and no other program's. | yes |
| `PAT-INTERVAL` | S3 | SCTE 54 §7.5; A/53-3 §5.4.1 | 14; 12 | The last bytes of successive program_association_sections are at most 100 ms apart (140 ms only when needed to stay under the 80 kbit/s system data rate). | yes |
| `PAT-PMT-PID-SHARED` | S3 | A/53-3 §5.4.1; SCTE 54 §7.5 | 12; 14 | A PMT PID carries one program definition: two programs of the PAT do not share a program_map_PID. | yes |
| `PAT-TSID` | S3 | TR 101 290 §5.3.4.1; A/65 Annex G.5.2 | 36; 138-139 | transport_stream_id equals the configured value (option --tsid) and does not change: receivers match it against the VCT's channel_TSID and remember it with the frequency. | yes |
| `PSI-AF` | S3 | A/53-3 §5.4.1; SCTE 54 §7.5 | 12; 14-15 | Packets of PID 0x0000 and of PMT PIDs carry an adaptation field only to set discontinuity_indicator (a version discontinuity). | yes |
| `PMT-INTERVAL` | S3 | SCTE 54 §7.5; A/53-3 §5.4.1 | 14; 12 | The last bytes of successive TS_program_map_sections of a program are at most 400 ms apart. | yes |
| `PMT-PRIVATE-REG` | S3 | SCTE 54 §7.2.4; A/53-3 §5.2.1.3, §5.6.2 | 13; 11, 14 | An elementary stream with a user-private stream_type (0xC4 to 0xFF) carries a registration descriptor in its ES_info loop. | yes |
| `PMT-VIDEO-COUNT` | S3 | SCTE 54 §7.4; A/53-3 §5.1.3.1 | 14; 10 | A program carries at most one video elementary stream. | yes |
| `PMT-AUDIO-CM` | S3 | SCTE 54 §7.3; A/53-3 §5.1.3.1 | 14; 10-11 | A program with AC-3 audio has at least one complete main service (bsmod 0 in its AC-3 audio descriptor). | yes |
| `PMT-DSA` | S3 | A/53-3 §5.4.1; A/53-3 Amendment 1 §5.4.1 | 12; 4-5 | Every MPEG-2 video (0x02) and AVC video (0x1B) elementary stream has a data_stream_alignment_descriptor of length 1 with alignment_type 0x02. | yes |
| `PMT-AUDIO-DESC` | S3 | A/53-3 §5.8.1.1, §5.8.1.3; SCTE 54 §7.9.3.1, §7.9.3.3 | 15-16; 19-20 | Every AC-3 (0x81) elementary stream has an AC-3_audio_stream_descriptor (0x81) and every E-AC-3 (0x87) one an E-AC-3 descriptor (0xCC) in its ES_info loop. | yes |
| `PAT-NIT` | S4 | A/53-3 §5.4.1; A/65 §5 footnote 7 | 12; 20 | program_number 0 (network_PID) should be avoided: ATSC defines no NIT. | yes |
| `PMT-STREAM-TYPE` | S4 | SCTE 54 §7.9.2, Table 3 | 19 | stream_type is one of those SCTE 54 lists (0x01, 0x02, 0x05, 0x06, 0x0F, 0x11, 0x1B, 0x24, 0x2D, 0x2E, 0x80, 0x81, 0x87, 0x88) or user private; others are worth a look. | yes |

## Scope: descriptor

| ID | Sev | Clause | Page | Rule | Checked |
|---|---|---|---|---|---|
| `DESC-LOOP` | S1 | H.222.0 §2.4.4.9, §2.6 | 50-51, 68 | Descriptors exactly fill their loop (program_info_length, ES_info_length, and the descriptor loops of PSIP tables). | yes |
| `DESC-AC3` | S2 | A/52 Annex A §A4.3, Tables A4.1-A4.6 | 116-120 | AC-3_audio_stream_descriptor: its fields fit the length; sample_rate_code is not 011; surround_mode is not 11; num_channels is not 1110 or 1111; priority is not 00; langcod and langcod2 are 0xFF; language_flag_2 is 1 only with num_channels 0000. | yes |
| `DESC-ISO639` | S2 | H.222.0 §2.6.18, §2.6.19 | 78 | ISO_639_language_descriptor: the length is a multiple of 4 (language code and audio_type). | yes |
| `DESC-DUP` | S3 | A/53-3 §5.8 | 15 | No descriptor tag occurs twice in one PMT descriptor loop (registration descriptors are counted by DESC-REG-DUP). | yes |
| `DESC-REG-DUP` | S3 | SCTE 54 §7.2.1; A/53-3 §5.2.1 | 12; 11 | At most one registration descriptor in a descriptor loop. | yes |
| `DESC-AC3-ATSC` | S3 | A/53-3 §5.8.1.1; SCTE 54 §7.9.3.1 | 15; 20 | AC-3 descriptor on ATSC/cable: bit_rate_code signals at most 448 kbit/s (0 to 15 or 32 to 47) and num_channels is 1 to 13. | yes |
| `DESC-AC3-STREAM` | S3 | A/53-3 §5.8.1.1 item 4; SCTE 54 §7.9.3.1 item 5; A/52 §A4.3 | 15; 20; 117-118 | The AC-3 descriptor matches the elementary stream: bsmod, bsid, sample rate, acmod (num_channels below 8) and the exact bit rate (bit_rate_code below 32). | yes |
| `DESC-AC3-LANGUAGE` | S3 | A/53-3 §5.8.1.1; SCTE 54 §7.9.3.6 | 15; 21 | When two or more AC-3 streams of a program carry the same type of service (bsmod), each AC-3 descriptor carries its language. | yes |
| `DESC-AC3-PRIMARY` | S3 | SCTE 54 §7.9.3.6 | 21 | At most one AC-3 stream of a program is labeled primary (priority 01). | yes |
| `DESC-AC3-MAINID` | S3 | SCTE 54 §7.6; A/53-3 §5.4.2 | 15; 13 | Main audio services (bsmod below 2) of a program carry distinct mainid values. | yes |
| `DESC-ISO639-ATSC` | S3 | A/53-3 §5.8.1.2; SCTE 54 §7.9.3.2 | 16; 20 | For AC-3 audio: the ISO 639 language equals the AC-3 descriptor's language, audio_type is 0x00, and when it is used and two or more AC-3 streams share a bsmod every one of them carries it. | yes |
| `DESC-COMPONENT-NAME` | S3 | SCTE 54 §7.9.3.9; A/65 §6.9.7 | 21; 74 | Audio components of a program with the same language and the same service type (bsmod) each carry a component_name_descriptor (0xA3). | yes |
| `DESC-CAPTION` | S3 | A/65 §6.9.2, Table 6.26 | 68-70 | caption_service_descriptor: length 1 + 6 x number_of_services; number_of_services 1 to 16; caption_service_number 1 to 63 when digital_cc is 1. | yes |
| `DESC-GA94` | S4 | A/53-3 §5.2.1.1; SCTE 54 §7.2.2 | 11; 12 | The program loop has no registration descriptor "GA94" (optional; it marks the program as ATSC). | yes |

## Scope: PES

| ID | Sev | Clause | Page | Rule | Checked |
|---|---|---|---|---|---|
| `PES-START` | S1 | H.222.0 §2.4.3.3, §2.4.3.6 | 21, 33 | On a PES PID, the payload of a packet with payload_unit_start_indicator 1 begins with packet_start_code_prefix 0x000001. | yes |
| `PES-HEADER` | S1 | H.222.0 §2.4.3.6 Table 2-21, §2.4.3.7 | 33-37 | PES header: the '10' bits; PES_header_data_length covers the optional fields its flags announce; PTS_DTS_flags is not 01. | yes |
| `PES-LENGTH` | S1 | H.222.0 §2.4.3.7 | 35 | A non-zero PES_packet_length equals the bytes of the PES packet after the field, up to where the next PES packet starts on the PID. | yes |
| `PES-TS-MARKER` | S2 | H.222.0 §2.4.3.6 Table 2-21 | 34 | PTS and DTS fields carry their 4-bit prefix ('0010' PTS only, '0011' PTS with DTS, '0001' DTS) and the three marker bits. | yes |
| `PES-LENGTH-ZERO` | S2 | H.222.0 §2.4.3.7 | 35 | PES_packet_length 0 (unbounded) is allowed only for video elementary streams in transport streams. | yes |
| `PES-DTS-REDUNDANT` | S2 | H.222.0 §2.7.5 | 110 | A DTS appears only when the decoding time differs from the presentation time: a DTS equal to the PTS is not coded. | yes |
| `PES-DTS-ORDER` | S2 | H.222.0 §2.4.2.4 | 18 | The decoding time is not after the presentation time (DTS <= PTS, modulo 2^33). | yes |
| `PES-PTS-NO-AU` | S2 | H.222.0 §2.7.5 | 110 | A PTS is coded only in a PES packet in which a picture start code (video) or an audio frame (AC-3) begins. | yes |
| `PTS-REPETITION` | S2 | TR 101 290 §5.2.2 item 2.5; H.222.0 §2.7.4 | 23-24; 109 | PES packets with a PTS arrive at most 700 ms apart on every video and audio PID. | yes |
| `PES-LENGTH-VIDEO` | S3 | A/53-3 §5.5.1 | 13 | MPEG-2 video (stream_type 0x02) PES packets have PES_packet_length 0x0000. | yes |
| `PES-STREAM-ID` | S3 | A/52 Annex A §A4.2; A/53-3 §5.5.2; SCTE 54 §7.7.1; H.222.0 Table 2-22 | 115; 14; 16; 36 | AC-3 and E-AC-3 PES packets use stream_id 0xBD (private_stream_1); MPEG-2 and AVC video use 0xE0 to 0xEF. | yes |
| `PES-FLAGS` | S3 | A/53-3 §5.5; SCTE 54 §7.7 | 13; 15-16 | PES_scrambling_control is 00 and ESCR_flag, ES_rate_flag, PES_CRC_flag, PES_private_data_flag, pack_header_field_flag, program_packet_sequence_counter_flag and P-STD_buffer_flag are 0. | yes |
| `PES-VIDEO-PTS` | S3 | A/53-3 §5.5.1 | 13 | Every MPEG-2 video PES header carries a PTS. | yes |
| `PES-VIDEO-ALIGN` | S3 | A/53-3 §5.5.1 | 13 | MPEG-2 video PES packets have data_alignment_indicator 1 and their payload begins with an access unit (sequence header, group of pictures header or picture start code). | yes |

## Scope: PCR and rate

| ID | Sev | Clause | Page | Rule | Checked |
|---|---|---|---|---|---|
| `PCR-REPETITION` | S1 | TR 101 290 §5.2.2 item 2.3a; H.222.0 §2.7.2 | 23; 109 | The bytes holding the last bit of successive PCR bases on a PCR_PID arrive at most 100 ms apart (measured on the stream's byte positions at its rate). | yes |
| `PCR-DISCONTINUITY` | S1 | TR 101 290 §5.2.2 item 2.3b; H.222.0 §2.4.3.5 | 23; 24 | The difference of successive PCR values is within 0 to 100 ms unless discontinuity_indicator is set. | yes |
| `PCR-ACCURACY` | S2 | TR 101 290 §5.2.2 item 2.4, §5.3.2.6; H.222.0 §2.4.2.2 | 23, 33; 14 | Each PCR is within +-500 ns of the value its byte position gives on a straight line fitted to the PID's PCRs over about two seconds (the TS rate taken as constant). | yes |
| `PCR-ACCURACY-NOMINAL` | S3 | H.222.0 §2.4.2.2; TR 101 290 §5.3.2.6; SPEC §5.1 | 14; 33 | With --rate: each PCR is within +-500 ns of the value its byte position gives at the nominal rate (offset fitted over the same window). | yes |
| `TS-RATE` | S3 | H.222.0 §2.4.2.1; SCTE 54 §11 | 12; 25 | With --rate: the rate measured from the PCRs is within +-30 ppm of the nominal rate (the system clock is 27 MHz +-810 Hz, so a correct clock and a correct rate agree to that). | yes |
| `PCR-INTERVAL-40` | S4 | TR 101 290 Table 5.0b note 2; standards-digest §1.2 | 24 | PCRs more than 40 ms apart: the former DVB limit, and the upper end of this project's recommended 20 to 40 ms. | yes |
| `PCR-INTERVAL-TARGET` | S4 | SPEC §6.3 | - | PCRs further apart than the configured target (option --pcr-target-ms, default 30 ms, this project's mux setting). | yes |
| `TS-VBR` | S4 | H.222.0 §2.4.2.2; TR 101 290 §5.2.2 item 2.4 | 14; 24 | The rate between successive PCRs varies by more than 0.5 %: the stream is not constant bit rate, so PCR accuracy and the time base of the other checks are approximate. | yes |
| `TIME-BASE` | S4 | H.222.0 §2.4.2.2 | 13 | No time base: the stream has no PCR and no --rate was given, so no interval, rate or buffer check was made. | yes |

## Scope: PSIP

| ID | Sev | Clause | Page | Rule | Checked |
|---|---|---|---|---|---|
| `PSIP-AF` | S3 | A/65 §6.1, §6.2, §6.3.1, §6.3.2, §6.4, §6.5, §6.6 | 23, 25, 29, 34, 37, 41, 44 | Packets of 0x1FFB and of EIT and ETT PIDs have adaptation_field_control 01: no adaptation field. | yes |
| `PSIP-SCRAMBLED` | S3 | A/65 §6.1, §6.2, §6.3.1, §6.3.2, §6.4, §6.5, §6.6 | 23, 25, 29, 34, 37, 41, 44 | Packets of 0x1FFB and of EIT and ETT PIDs have transport_scrambling_control 00. | yes |
| `MGT-PACKET-START` | S3 | A/65 §6.2 | 25 | The packet carrying the MGT's table_id has payload_unit_start_indicator 1 and pointer_field 0x00. | yes |
| `PSIP-PROTOCOL-VERSION` | S3 | A/65 §6.1-§6.6 | 24, 27, 31, 39, 43, 45 | protocol_version is 0 in every PSIP section. | yes |
| `PSIP-REQUIRED` | S3 | SCTE 54 §7.8.1, §7.8.1.1; A/65 §5 | 16-17; 21-23 | System information is present: an MGT, an STT and a CVCT or TVCT on 0x1FFB (checked when the stream is longer than the table's maximum cycle). | yes |
| `MGT-INTERVAL` | S3 | SCTE 54 §7.8.1.2 Table 1; A/65 Table 7.1 | 17; 83 | The MGT repeats at most every 150 ms. | yes |
| `VCT-INTERVAL` | S3 | SCTE 54 §7.8.1.2 Table 1; A/65 Table 7.1 | 17; 83 | Each section of the CVCT (or TVCT) repeats at most every 400 ms. | yes |
| `STT-INTERVAL` | S3 | SCTE 54 §7.8.1.2 Table 1 | 17 | The STT repeats at most every 10 s (cable). | yes |
| `RRT-INTERVAL` | S3 | SCTE 54 §7.8.1.2 Table 1; A/65 Table 7.1 | 17; 83 | An RRT, when present, repeats at most every 60 s. | yes |
| `PSIP-RATE` | S3 | SCTE 54 §7.8.1.2 Table 2; A/65 Table 7.2 | 18; 83 | 0x1FFB and every EIT and ETT PID carry at most 250,000 bit/s over any 1 s (measured in 100 ms slices). | yes |
| `STT-HEADER` | S3 | A/65 §6.1, Table 6.1 | 24 | The STT has table_id_extension 0x0000 and version_number 0. | yes |
| `STT-TIME` | S3 | A/65 §6.1 | 24-25 | system_time advances with the stream: between two STTs it moves by the elapsed stream time within +-2 s (each value is accurate to +-1 s). | yes |
| `STT-DS` | S3 | A/65 Annex A, Tables A1, A2 | 84-85 | daylight_saving: DS_hour is 0 to 18, and 0 when DS_day_of_month is 0. | yes |
| `MGT-HEADER` | S3 | A/65 §6.2 | 27 | The MGT has table_id_extension 0x0000 and tables_defined 2 to 370 (cable). | yes |
| `MGT-TABLE-TYPE` | S3 | A/65 §6.2 Table 6.3, §6.5, §6.6 | 27, 41, 44 | table_type values are not reserved and occur once; each EIT and ETT PID is unique among the table_type_PIDs and is not 0x1FFB; VCT and RRT are on 0x1FFB. | yes |
| `MGT-VERSION` | S3 | A/65 §6.2 | 27-28 | table_type_version_number equals the version_number of the table on table_type_PID. | yes |
| `MGT-NUMBER-BYTES` | S3 | A/65 §6.2 | 28 | number_bytes equals the total size of all sections of all instances of the table type (compared once the table's sections have all been seen twice, and at the end of the stream). | yes |
| `MGT-UNLISTED` | S3 | A/65 §6.2 | 25 | Every VCT, EIT, ETT and RRT present is listed in the MGT. | yes |
| `VCT-TSID` | S3 | A/65 §6.3.1 | 31 | The VCT's transport_stream_id equals the PAT's. | yes |
| `VCT-NUMBER` | S3 | A/65 §6.3.1, §6.3.2; SCTE 54 §7.8.1.1 | 32, 35; 17 | Channel numbers: TVCT major 1 to 99 and minor 0 to 999 (1 to 99 for service types 0x02 and 0x03); CVCT two-part numbers both below 1000, or a one-part number (major's six high bits 111111). | yes |
| `VCT-NUMBER-DUP` | S3 | A/65 §6.3.1, §6.3.2 | 32, 35 | No major/minor channel number pair occurs twice in the VCT. | yes |
| `VCT-SHORT-NAME` | S3 | A/65 §6.3.1 | 31-32 | short_name is one to seven valid UTF-16 code values padded with 0x0000. | yes |
| `VCT-CARRIER-FREQ` | S3 | A/65 §6.3.1 | 32 | carrier_frequency is 0. | yes |
| `VCT-FIELDS` | S3 | A/65 §6.3.1, §6.3.2, Tables 6.5, 6.6 | 32-34, 37 | modulation_mode is not reserved (0x00, 0x06 to 0x7F); ETM_location is not 11; service_type is not 0x00; source_id 0 only in a CVCT; inactive channels (program_number 0) are hidden. | yes |
| `VCT-SOURCE-ID-DUP` | S3 | A/65 §6.3.1, §6.3.2 | 34, 37 | source_id values 0x0001 to 0x0FFF are unique within the transport stream (channels of different programs do not share one). | yes |
| `EIT-WINDOW` | S3 | A/65 §5, §6.5 | 21-22, 40 | Every event in EIT-k overlaps the k-th 3-hour UTC window after the one containing the STT's time (checked away from window boundaries). | yes |
| `EIT-ORDER` | S3 | A/65 §6.5 | 41, 43 | Events of an EIT instance are in start-time order and do not overlap: each start_time is at least the previous event's start_time plus length_in_seconds. | yes |
| `ETT-ETM-ID` | S3 | A/65 §6.6, Table 6.14 | 45 | ETM_id: a channel ETM has bits 15 to 0 zero and is on the channel ETT PID; an event ETM has bits 1 to 0 '10' and is on an event ETT PID. | yes |
| `ETT-TID-EXT` | S3 | A/65 §6.6 and footnote 13 | 45 | ETT instances on one PID have distinct ETT_table_id_extension values (A/65 warns that some deployed receivers fail on non-zero values). | yes |
| `MSS-SYNTAX` | S3 | A/65 §6.10, Tables 6.39-6.41 | 79-82 | multiple_string_structure fits its field; compression_type is 0x00 to 0x02 (or 0xB0 and above); Huffman only with mode 0x00; mode is not reserved; UTF-16 segments have an even length and valid surrogates. | yes |
| `STT-INTERVAL-1S` | S4 | A/65 Table 7.1; standards-digest §2.3 | 83 | The STT repeats more often than every 1 s (the terrestrial maximum, and what receivers are commonly designed around). | yes |
| `EIT0-INTERVAL` | S4 | SCTE 54 §7.8.1.2; A/65 §7.1 | 17; 83 | Each section of EIT-0 repeats at most every 500 ms (recommended). | yes |
| `STT-GPS-UTC` | S4 | A/65 §6.1; standards-digest §3.1 [K] | 25 | GPS_UTC_offset is 18 s, the value since 2017-01-01 (no leap second has been announced since). | yes |
| `MSS-HUFFMAN` | S4 | A/65 §6.10, Annex C | 80 | A string segment is Huffman compressed (compression_type 0x01 or 0x02): legal, but this checker does not decode it (the tables of Annex C are not implemented), so its text is not checked. | yes |

## Scope: cross-check

| ID | Sev | Clause | Page | Rule | Checked |
|---|---|---|---|---|---|
| `PID-MISSING` | S1 | TR 101 290 §5.2.1 item 1.6 | 22-23 | A PID referenced by a PMT (elementary and PCR PIDs) or by the MGT carries packets: elementary and PCR PIDs at least every --pid-timeout (1 s), MGT PIDs at least every --psip-pid-timeout (60 s). | yes |
| `CAT-ERROR` | S1 | TR 101 290 §5.2.2 item 2.6 | 24 | Scrambled packets (transport_scrambling_control not 00) appear only with a CAT present; PID 0x0001 carries only table_id 0x01. | yes |
| `PID-CONFLICT` | S2 | H.222.0 §2.4.3.3, §2.4.4.5, §2.4.4.9 | 21, 48, 51 | A PID has one role: it is not both a PMT PID and an elementary PID, nor an elementary PID of two stream types, nor a PSI or PSIP PID used for PES. | yes |
| `PID-UNREFERENCED` | S3 | TR 101 290 §5.2.3 item 3.4a | 25 | Every PID other than 0x0000 to 0x001F, 0x1FFB and 0x1FFF is referenced by the PAT, a PMT, the CAT or the MGT within 0.5 s of appearing. | yes |
| `MGT-PID-TABLE` | S3 | A/65 §6.2, §6.5, §6.6 | 25, 41, 44 | Each table_type_PID of the MGT carries the table its table_type names (EIT PIDs table_id 0xCB, ETT PIDs 0xCC). | yes |
| `VCT-PROGRAM` | S3 | A/65 §6.3.1 | 33-34 | An active virtual channel of this transport stream (channel_TSID equal to the PAT's, program_number neither 0 nor 0xFFFF) names a program of the PAT. | yes |
| `VCT-COVERAGE` | S3 | SCTE 54 §7.8.1.1; A/65 §5.2 | 17; 23 | Every program of the PAT is described by a virtual channel of the VCT. | yes |
| `VCT-SLD` | S3 | A/65 §6.9.5 | 72-73 | A service_location_descriptor equals the PMT on air: the same PCR_PID, and every elementary stream with the same stream_type, PID and language (zeros when the PMT gives none). | yes |
| `VCT-SLD-MISSING` | S3 | A/65 §6.9.5; Annex G.5.3 | 71-72; 139 | Every active channel of a TVCT carries a service_location_descriptor (not required in a CVCT). | yes |
| `EIT-INSTANCE` | S3 | A/65 §6.5 | 40 | Every virtual channel (other than data-only service_type 0x04) has an instance (its source_id) in each EIT-k the MGT lists, with num_events_in_section 0 when it has no event. | yes |
| `EIT-SPAN-ID` | S3 | A/65 §6.5 | 40 | An event that extends over two EITs has the same event_id in both. | yes |
| `ETM-MISSING` | S3 | A/65 §6.3.1 Table 6.6, §6.5 Table 6.12 | 33, 43 | A channel or event with ETM_location 01 has its ETM in this transport stream (checked once the ETT PIDs have cycled, and at the end). | yes |
| `VCT-CHANNEL-TSID` | S4 | A/65 §6.3.1; SCTE 54 §7.8.1.1; standards-digest §2.8 | 33; 17 | A virtual channel names another transport stream (channel_TSID differs from the PAT's): legal, but this project lists only its own programs. | yes |
| `EIT-SOURCE` | S4 | A/65 §6.5 | 40 | An EIT instance names a source_id that no virtual channel of the VCT has. | yes |

## Scope: buffer model

| ID | Sev | Clause | Page | Rule | Checked |
|---|---|---|---|---|---|
| `TSTD-TB-OVERFLOW` | S2 | H.222.0 §2.4.2.3, §2.4.2.6; TR 101 290 §5.2.3 item 3.3 | 14-15, 19; 25 | The transport buffer TBn (512 bytes, emptied at Rxn: 1.2 x Rmax for MPEG-2 video, 2 Mbit/s for audio) never overflows. | yes |
| `TSTD-TBSYS-OVERFLOW` | S2 | H.222.0 §2.4.2.3, §2.4.2.6; TR 101 290 §5.2.3 item 3.3 | 14-15, 19; 25 | TBsys (512 bytes, emptied at 1 Mbit/s; fed by PIDs 0 to 3 and the program's PMT PID) never overflows. | yes |
| `TSTD-TB-EMPTY` | S2 | H.222.0 §2.4.2.6; TR 101 290 §5.2.3 item 3.9 | 19; 26 | TBn and TBsys are empty at least once every second. | yes |
| `TSTD-BSYS-OVERFLOW` | S2 | H.222.0 §2.4.2.3, §2.4.2.6 | 16, 18, 19 | Bsys (1536 bytes, emptied at Rsys = max(80 kbit/s, rate / 500)) never overflows. | yes |
| `TSTD-MB-OVERFLOW` | S2 | H.222.0 §2.4.2.3, §2.4.2.6; H.262 Tables 8-13, 8-14 | 15-17, 19 | The MPEG-2 video multiplexing buffer MBn (BSmux + BSoh, plus VBVmax - vbv_buffer_size at Main level and below) never overflows (leak method, Rbx = Rmax, or min(1.05 Res, Rmax) at High-1440 and High level). | yes |
| `TSTD-MB-EMPTY` | S2 | H.222.0 §2.4.2.6 | 19 | MBn empties at least once every second (leak method). | yes |
| `TSTD-EB-UNDERFLOW` | S2 | H.222.0 §2.4.2.3, §2.4.2.6 | 18-19 | Every byte of a video access unit has reached EBn (size vbv_buffer_size) by its decoding time (DTS, else PTS), unless low_delay is set. | yes |
| `TSTD-B-UNDERFLOW` | S2 | H.222.0 §2.4.2.3, §2.4.2.6 | 18-19 | Every byte of an audio frame has reached Bn by its presentation time. | yes |
| `TSTD-DELAY` | S2 | H.222.0 §2.4.2.6; TR 101 290 §5.2.3 item 3.10 | 19; 26 | No byte of an access unit spends more than 1 s in the buffers: the decoding time is at most 1 s after the arrival of the access unit's first byte (10 s for AVC). | yes |
| `TSTD-B-OVERFLOW` | S3 | A/52 Annex A §A4.4; A/53-5 §5.7; A/53-3 §5.1.2; SCTE 54 §7.1.2 | 120; 7; 10; 12 | The AC-3 main buffer Bn (2592 bytes, holding PES headers and frames) never overflows. | yes |
| `PSIP-SB-OVERFLOW` | S3 | SCTE 54 §7.8.1.2; A/65 §7.1; H.222.0 §2.6.30 | 18; 83; 81 | The smoothing buffer of 0x1FFB and of every EIT and ETT PID (1024 bytes, whole packets in at the TS rate, out at 250,000 bit/s) never overflows. | yes |

## Scope: elementary stream

| ID | Sev | Clause | Page | Rule | Checked |
|---|---|---|---|---|---|
| `ES-AC3-SYNC` | S2 | A/52 §5.3, §5.4.1, Table 5.18; H.222.0 §2.4.3.7 | 30, 37, 51-52; 37 | AC-3 syncframes follow each other: each starts with syncword 0x0B77 where the previous one's frmsizecod ends it, fscod is not 11 and frmsizecod is at most 37; the first frame after a PES start with data_alignment_indicator begins the payload. | yes |
| `ES-VIDEO-SEQ` | S4 | H.262 §6.2.2.1, §6.2.2.3; H.222.0 §2.4.2.3 | 25-27; 14-15 | The MPEG-2 video profile and level (sequence_extension) are ones the buffer model has constants for (Simple, Main, 4:2:2 profiles at their levels); otherwise its MBn and EBn are not modeled. | yes |

