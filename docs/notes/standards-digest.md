# Standards digest for the MPTS server

An implementer's digest of the broadcast standards that govern what our Rust server must emit:
16 constant-bitrate MPEG-2 multi-program transport streams, one per 256-QAM Annex B carrier of
the EQAM-420B in MPTS pass-through mode, carrying everything a US clear-QAM TV needs. Written
2026-11-24.

## How to read this

- Every normative number carries a citation: document, clause, table, printed page ("p.").
  For every document listed here the printed page number equals the PDF page number, except
  ITU-T H.222.0, where both are given when they differ.
- Labels: **[K]** = general knowledge, not from a document read for this digest. Kept minimal
  and listed again in "Claims resting on [K]". **[I]** = our inference from cited text.
  **[Rec]** = our recommendation, not a requirement.
- Requirements written "shall" are the standard's; "should"/"recommended" are the standard's
  recommendations; our own design choices are marked [Rec].

### Sources read

| Short name | Document | Local file |
|---|---|---|
| H.222.0 | ITU-T H.222.0 (06/2012) = ISO/IEC 13818-1:2013 | `docs/external/standards/2026-11-24-atsc-itu-etsi/ITU-T_H.222.0-201206.pdf` |
| A/65 | ATSC A/65:2013 PSIP | `docs/external/standards/ATSC_A65-2013.pdf` |
| A/53-1 / -3 / -3am1 / -4 / -5 / -6 | ATSC A/53 Part 1:2013, Part 3:2013, Part 3 Amendment 1 (2023, AVC), Part 4:2009, Part 5:2014, Part 6:2013 | `docs/external/standards/2026-11-24-atsc-itu-etsi/` |
| A/52 | ATSC A/52:2012 (AC-3/E-AC-3), Annex A | same |
| A/72-1 | ATSC A/72 Part 1:2015 (AVC video) | same |
| A/85 | ATSC A/85:2013 with Corrigendum 1 (loudness RP) | same |
| TR 101 290 | ETSI TR 101 290 V1.4.1 (2020-06) | same |
| SCTE 54 | ANSI/SCTE 54 2024 (primary) and 2009 (for the PID table it dropped) | `docs/external/standards/2026-11-24-scte/SCTE_54_2024.pdf`, `docs/external/standards/SCTE_54_2009_lawresource.pdf` |
| SCTE 07 | ANSI/SCTE 07 2018 (R2024) | `docs/external/standards/SCTE_07_2018_R2024.pdf` |
| SCTE 128-1 / -2 | ANSI/SCTE 128-1 2020 (AVC coding), SCTE 128-2 2018 (AVC transport) | `docs/external/standards/2026-11-24-scte/` |
| SCTE 18 | CTA-814-C / J-STD-042-C (2018) = SCTE 18 | `docs/external/standards/J-STD-042-C_SCTE_18_EAS.pdf` |
| SCTE 65 | SCTE 65 2008 (out-of-band SI; not needed, see §2.9) | `docs/external/standards/SCTE_65_2008_lawresource.pdf` |

Text extractions are in `work/standards/` (gitignored); `work/standards/paged/` has each line
prefixed with its PDF page.

---

## 1. Transport layer (ITU-T H.222.0 / ISO 13818-1, A/53-3, SCTE 54)

H.222.0 page numbers below are the **printed** page numbers; the PDF page is about 14 higher
(e.g. printed p.109 = PDF p.123). SCTE 54 governs where it differs from A/53-3 for cable
(SCTE 54 §7.1, p.11: full 13818-1 compliance plus its constraints, and conformance to the
T-STD).

### 1.1 Packet

188 bytes (H.222.0 §2.4). Header (Table 2-2, p.21; semantics §2.4.3.3, p.21–23):

| Field | Bits | Rule for us |
|---|---|---|
| sync_byte | 8 | 0x47 |
| transport_error_indicator | 1 | 0 always |
| payload_unit_start_indicator | 1 | PES: 1 when a PES packet starts in this packet (exactly one may start). Sections: 1 when a section starts in this packet, and then the first payload byte is a pointer_field. Null packets: 0 |
| transport_priority | 1 | 0 |
| PID | 13 | §2.2 |
| transport_scrambling_control | 2 | 00 everywhere (SCTE 54 Table 5, p.24; A/65 for PSIP) |
| adaptation_field_control | 2 | 01 payload only, 10 AF only (adaptation_field_length must then be 183), 11 AF + payload; 00 reserved (Table 2-5, p.22) |
| continuity_counter | 4 | §1.4 |

Adaptation field (Table 2-6, p.23; semantics §2.4.3.5, p.24–32): adaptation_field_length (8;
0–182 with payload, 183 without); flags discontinuity_indicator, random_access_indicator,
elementary_stream_priority_indicator, PCR_flag, OPCR_flag, splicing_point_flag,
transport_private_data_flag, adaptation_field_extension_flag (1 each); PCR (33 base + 6
reserved + 9 extension); OPCR (same; only with a PCR; never modified by a mux); splice_countdown
(8, two's complement); private data (length 8 + bytes); extension (ltw, piecewise_rate,
seamless_splice with splice_type 4 + DTS_next_AU 33). Stuffing bytes are 0xFF, and
adaptation-field stuffing is **the only stuffing allowed in packets carrying PES** (p.24, p.32).

- PAT and PMT PIDs: no adaptation field except to set discontinuity_indicator for a version
  discontinuity (A/53-3 §5.4.1, p.12; SCTE 54 §7.5, p.14–15). **So never put a PCR on a PAT or
  PMT PID.** PSIP PIDs: AFC always 01 (§2.4).
- Private data in adaptation fields needs SCTE 128-2's tag/length format, a registration
  descriptor and the SCTE_adaptation_field_data_descriptor (A/53-3 §5.6.2, p.14; SCTE 54
  §7.8.3, p.18; SCTE 128-2 §6.3.2.3, §6.4.3, p.10, p.13). [Rec] Don't use it.
- elementary_stream_priority_indicator: MPEG-2 video only if the payload carries intra-slice
  bytes; H.264 only for I/SI slices (p.26). SCTE 128-2 requires it on the packet with the first
  slice of each random access picture (§1.7).

### 1.2 PCR

| Item | Value | Source |
|---|---|---|
| Format | PCR = base·300 + ext; base = (27 MHz·t / 300) mod 2^33; ext = (27 MHz·t) mod 300 | H.222.0 §2.4.2.2 eq. 2-1..2-3, p.13 |
| Timing reference | the arrival time of the byte containing the **last bit of program_clock_reference_base** | p.13, p.26 |
| system_clock_frequency | 27 000 000 Hz ± 810 Hz (±30 ppm) | §2.4.2.1, p.12 |
| Drift | ≤ 0.075 Hz/s | §2.4.2.1, p.12 |
| Accuracy (value vs. its byte position) | ±500 ns, network jitter excluded | §2.4.2.2, p.14 |
| Max interval | **≤ 0.1 s** between PCRs of a program on its PCR_PID | §2.7.2, p.109 |
| After a time-base discontinuity | ≥ 2 PCRs before the next discontinuity | §2.7.2, p.109; §2.4.3.5, p.24 |
| Audio clocks | locked to 27 MHz in a constant ratio (48 kHz = 27 MHz × 2/1125) | §2.7.10, p.112; A/53-5 §5.2, p.6–7 |

Neither A/53-3 nor SCTE 54 tightens the 100 ms interval. ETSI TR 101 290 also now uses 100 ms
everywhere (the DVB 40 ms limit was removed, TR 101 290 Table 5.0b note 2, p.24).

PCR_PID: the PMT's 13-bit PCR_PID names the PID whose packets carry the program's PCR
(§2.4.4.9, p.51). It may be the video PID or a separate PCR-only PID (AFC `10`; CC does not
increment) — §2.6.30 (p.81) lists "PCR_PID of the program" separately from elementary PIDs [I].
Programs sharing one clock may share a PCR_PID (Annex C.7, p.164, informative). Our server runs
all six programs on one output clock, so either works [I].

[Rec] PCR on each program's video PID every 20–40 ms (always ≤ 100 ms including the box's
re-timing), all programs derived from one 27 MHz output clock; compute each PCR from the
packet's exact byte position at our nominal CBR rate so PCR_AC is ~0.

At a CBR rate R, a PCR placed in a packet whose PCR-base last byte is at byte index i satisfies
PCR(i) = PCR(i₀) + (i − i₀)·8·27 000 000 / R (H.222.0 eq. 2-4/2-5, p.13; TR 101 290 §5.3.2,
p.30–31).

### 1.3 PSI: PAT and PMT

Section framing (H.222.0 §2.4.4, p.45–47):
- pointer_field (8 bits) is present exactly when PUSI = 1 on a section PID; its value is the
  byte offset to the first section start (0 = immediately). One pointer_field per packet;
  sections packed back-to-back.
- After the end of a section, once a 0xFF byte follows, the rest of the packet is 0xFF; the next
  packet on that PID starts with pointer_field 0 (p.45; Annex C.4, p.163). table_id 0xFF is
  forbidden for that reason.
- PSI sections (PAT, PMT, CAT) ≤ 1024 bytes: section_length ≤ 1021, top two bits `00`
  (§2.4.4.5 p.48; §2.4.4.9 p.50). Private sections ≤ 4096 bytes.

PAT (Table 2-30, p.47): table_id 0x00 (8), section_syntax_indicator 1, `0`, reserved 2,
section_length 12, transport_stream_id 16, reserved 2, version_number 5,
current_next_indicator 1, section_number 8, last_section_number 8; loop {program_number 16,
reserved 3, program_map_PID 13}; CRC_32. Each program_number once. Avoid program_number 0
(network_PID; ATSC has no NIT — A/53-3 §5.4.1, p.12; A/65 footnote 7, p.20).

PMT (Table 2-33, p.50): table_id 0x02, section_syntax_indicator 1, `0`, reserved 2,
section_length 12, program_number 16, reserved 2, version_number 5, current_next 1,
section_number **0**, last_section_number **0**, reserved 3, PCR_PID 13, reserved 4,
program_info_length 12 (top 2 bits 00), program descriptors; loop {stream_type 8, reserved 3,
elementary_PID 13, reserved 4, ES_info_length 12, ES descriptors}; CRC_32.
- A program definition fits in one section. H.222.0 also says it "is never longer than 1016
  (0x3F8)" (§2.4.4.9, p.50–51) without explaining the figure; stay under 1016 [Rec].
- The PMT PID must not change while the program exists (p.46).
- **One program per PMT PID** (A/53-3 §5.4.1, p.12; SCTE 54 §7.5, p.14). SCTE 54 allows other
  private sections on a PMT PID (p.15); we will not use that.
- A new PAT/PMT version takes effect when its last byte leaves Bsys (p.45–46).

CRC_32 (H.222.0 Annex A, eq. A-1, p.152): polynomial 0x04C11DB7, MSB-first (not reflected),
register initialized to all ones, no final XOR; the decoder's register is zero after the whole
section including the CRC. This is the parameter set commonly called CRC-32/MPEG-2 [K]. Same
CRC for all PSIP tables (A/65 §4.1, p.20) and the EAS section.

Version numbers: +1 mod 32 when the table changes; current_next = 1 means applies now.
If a version jump is unavoidable (e.g. restart), set discontinuity_indicator on the PAT/PMT
packet; then a single version discontinuity is allowed (H.222.0 §2.4.3.5, p.25). [Rec]
Persist PAT/PMT/PSIP version counters across server restarts so they always increment.

Repetition: H.222.0 has none (Annex C.5, p.163). Cable: **PAT ≤ 100 ms** (≤ 140 ms only if the
80 kbit/s system-data limit would otherwise be exceeded); **each PMT ≤ 400 ms** (SCTE 54 §7.5,
p.14; A/53-3 §5.4.1, p.12).

### 1.4 Continuity counter

H.222.0 §2.4.3.3 p.22; §2.4.3.5 p.24–25:
- 4-bit, per PID, +1 per packet with payload, wraps 15→0. **Does not increment** when AFC is
  `00` or `10` (adaptation-only packets, e.g. PCR-only).
- A packet may be sent twice, as two consecutive packets with the same CC (byte-identical
  except a valid PCR). Never three.
- A CC jump is allowed only in a packet with discontinuity_indicator = 1. On a PCR_PID, only in
  the packet where the time-base discontinuity happens.
- Null packets: CC undefined, never checked (also TR 101 290 Annex A.2, p.108).
- [Rec] Our mux owns every output PID and keeps its own CC counters, so input switching never
  causes an output CC jump.

### 1.5 Null packets and CBR

- PID 0x1FFF, PUSI 0, scrambling `00`, AFC `01`, CC undefined, payload any value (H.222.0
  p.21–23); [Rec] header 0x47 1F FF 10 + 184 × 0xFF (TR 101 290 uses 0x00 payload in its test
  pattern, Annex A.2 p.108–109; either is legal).
- Null packets never enter the T-STD (p.15).
- A multi-program TS with consistent T-STD behavior must be constant rate; VBR programs inside
  a CBR multiplex is the normal case (§2.4.2.2, p.14).
- The cable TS output rate is constant at T256 = 38.81070 Mbps, locked to the symbol rate
  (SCTE 54 §11, p.25). Null insertion/deletion absorbs an unlocked source rate (A/53-3 §7.2,
  p.18–19 describes this for 8-VSB). §8 covers our target rate.

### 1.6 T-STD essentials a mux must respect

H.222.0 §2.4.2.3–2.4.2.6, p.14–19:

| Buffer / rate | Value |
|---|---|
| TBn, TBsys (transport buffers) | 512 bytes each |
| Rx video MPEG-2 | 1.2 × Rmax(profile, level) |
| Rx video H.264 | 1.2 × BitRate[SchedSelIdx] (§2.14.3.1, p.137–139) |
| Rx audio (non-AAC) | 2 × 10⁶ bit/s |
| Rx systems (Rxsys) | 1 × 10⁶ bit/s |
| Bsys | 1536 bytes |
| Rsys (leak from Bsys) | max(80 000 bit/s, transport_rate·8/500) — 80 kbit/s at our rate |
| What feeds Bsys for a program | PAT, CAT, TSDT, IPMP and that program's own PMT |
| MPEG-2 video EB | vbv_buffer_size; MB = BSmux + BSoh + VBVmax − vbv_buffer_size (Main level) or BSmux + BSoh (High level); BSmux = 0.004 s·Rmax, BSoh = Rmax/750 |
| H.264 EB | CpbSize from the NAL HRD (else 1200·MaxCPB); MB = BSmux + BSoh + 1200·MaxCPB − cpb_size; leak Rbx = 1200·MaxBR unless the AVC timing/HRD descriptor says hrd_management_valid |
| AC-3 audio buffer BSn | **2592 bytes** (A/53-5 §5.7, p.7; A/52 Annex A §A4.4, p.120: BSmux 736 + BSpad 64 + BSdec) — ATSC/SCTE use this, not 13818-1's generic 3584 bytes (A/53-3 §5.1.2, p.10; SCTE 54 §7.1.2, p.12) |
| PSIP PIDs | smoothing buffer: leak 250 kbit/s, size 1024 bytes (§2.3) |

Buffer rules (§2.4.2.6, p.19): TBn/TBsys never overflow and empty at least once per second;
Bsys never overflows; EBn never underflows (except low_delay); MBn never overflows and (leak
method) empties at least once per second. **End-to-end delay through the buffers ≤ 1 s for
MPEG-2 video and audio, ≤ 10 s for H.264** (p.19; §2.14.3.1, p.138; SCTE 128-2 §6.4.2.1 note 2,
p.12).

[I] Practical consequences for the mux scheduler:
- Never send more than 512 bytes of one PID faster than its Rx drains it: a program's video
  PID must not get long back-to-back packet bursts at full TS rate. At 38.8 Mbps a packet
  slot is 38.75 µs; at Rx = 18 Mbit/s (MP@ML) TB drains one packet in 83.6 µs, so roughly every
  other slot at most, sustained only briefly.
- Each PSIP PID ≤ 250 kbit/s with ≤ 1 KB burst; PAT+PMT for each program within 80 kbit/s.
- Deliver each access unit to the decoder model before its DTS, and no more than 1 s
  (MPEG-2/audio) earlier.

### 1.7 Timestamps and PES

- PTS at least every 0.7 s per audio/video stream; PTS on the first AU of each stream and after
  any decoding discontinuity; DTS only when it differs from PTS (H.222.0 §2.7.4–2.7.5,
  p.109–110).
- All PES (cable): PES_scrambling_control `00`; ESCR, ES_rate, PES_CRC, PES_private_data,
  pack_header, program_packet_sequence_counter and P-STD_buffer flags 0 (SCTE 54 §7.7,
  p.15–16; A/53-3 §5.5, p.13).
- MPEG-2 video PES: each PES starts with an access unit aligned to the header, **every PES
  header carries a PTS** (plus DTS when needed), PES_packet_length 0, data_alignment_indicator
  1 (A/53-3 §5.5.1, p.13; Amend. 1 §5.5.1, p.5). One frame per PES is required for terrestrial;
  [Rec] do the same.
- H.264 PES: one AU start per PES; the AU start in the same TS packet as the PES header;
  **PTS on every PES**, DTS if different (SCTE 128-2 §6.5, p.14; A/53-3 Amend. 1 §5.5.3, p.5).
- AC-3: stream_id 0xBD (private_stream_1) (SCTE 54 §7.7.1, p.16; A/53-3 §5.5.2, p.14).
  Audio services meant to be decoded together should be frame-synchronous with identical PTS
  (SCTE 54 §7.7.1, p.16).
- SCTE 54 §9.2 (p.25, informative): send PTS on every picture and repeat the sequence header
  about every 0.5 s for fast acquisition.

### 1.8 Discontinuities, splices and source switching

System time-base discontinuity (H.222.0 §2.4.3.5, p.24; §2.4.2.2, p.13):
1. Set discontinuity_indicator = 1 on the PCR_PID packet carrying the first PCR of the new time
   base (it may be set earlier and then stays 1 on every PCR_PID packet up to that one).
2. No packet with a new-time-base PTS/DTS may arrive before that point; none with an
   old-time-base PTS/DTS after it.
3. Send ≥ 2 new PCRs before any further discontinuity; buffers may hold at most two time bases.

CC discontinuity on an ES PID (p.24–25): discontinuity_indicator = 1 in that packet, and the
first ES byte after it must be an access point — MPEG-2: a sequence header (optionally preceded
by sequence_end_code); H.264: the first byte of an AU, with the SPS/PPS it needs following;
audio: the first byte of a frame.

random_access_indicator = 1 means the next PES starting on that PID contains an access point;
for video the first picture after it carries a PTS (p.25–26). SCTE 128-2 makes it mandatory at
every H.264 random access point (§4.2).

Splicing (p.27–28; Annex K p.190–192, informative): splice_countdown counts down to the packet
that ends on the last byte of a picture or audio frame; the next payload packet starts a PES
with an access point. A non-seamless splice puts a PTS on the new stream's first AU, inserts
sequence_end_code + sequence_header if any sequence parameter changes, and adds dead time
Td ≥ max(0, (t_out − t_in)·(1 − R_old/R_new)) if the new rate is higher (K.3.1–K.3.3, p.191–192).

**What our source switch must do** [Rec, from the rules above]:
- Keep the output continuous: one output time base per carrier, our own CC counters, and
  re-stamp every input PTS/DTS into the output time base. Then **no discontinuity_indicator is
  ever needed** on the output, and TVs never see a time-base jump.
- Because we re-encode, switch the *encoder's input* (slate/live) rather than splicing
  compressed streams: the encoder then emits a new closed GOP / IDR with sequence header or
  SPS/PPS at the switch, and the AC-3 encoder keeps a continuous frame cadence.
- If compressed pass-through is added later: switch only at an access point of the new source
  (sequence_end_code + sequence_header + closed-GOP I picture, or IDR/SRAP for H.264), set
  random_access_indicator, put a PTS on the first AU, keep audio frame-aligned, and leave
  Td dead time against EB/MB overflow.

---

---

## 2. Cable framing (SCTE 54, A/65 cable rules)

### 2.1 Which tables, where

| | Terrestrial (A/65) | Cable in-band (SCTE 54 + A/65) |
|---|---|---|
| Required PSIP | STT, TVCT, MGT, EIT-0..EIT-3 (A/65 §5.1 Req. 4, p.23) | MGT, STT, and CVCT **or** TVCT (SCTE 54 §7.8.1.1, p.17; A/65 §5.2 Req. 6, p.23) |
| EIT/ETT | EIT-0..3 required; ETT optional | Optional, "at the option of the cable operator" (SCTE 54 §7.8.1, p.16); "no such requirement exists for the cable" (A/65 Annex G.2, p.136) |
| RRT | Required for any region referenced by a content_advisory_descriptor unless that region's system is defined by its own standard (A/65 §5.1, p.23) | Required for any region other than 0x01 that is referenced; **region 0x01 (US) is optional** because CEA-766 standardizes it (SCTE 54 §7.8.1.1, p.17) |
| What the tables must describe | All digital channels in the TS (A/65 §5.1 Req. 5) | "the unscrambled digital services multiplexed in the Transport Stream carrying those tables"; other TSs, analog and scrambled channels optional (SCTE 54 §7.8.1.1, p.17; A/65 §5.2 Req. 7, p.23) |
| Navigation | — | "In the absence of descrambling capability, receivers are expected to support navigation only to unscrambled services that are referenced in an in-band Virtual Channel Table present in the Transport Stream that carries the program" (SCTE 54 §7.8.1 informative note, p.16) |

The only rating regions defined for cable are 0x01 (US + possessions) and 0x02 (Canada)
(SCTE 54 §7.9.3.11, p.22).

### 2.2 PIDs

| PID | Use | Source |
|---|---|---|
| 0x0000 | PAT | H.222.0 Table 2-3 |
| 0x0001 | CAT (do not emit; we are unscrambled) | H.222.0 Table 2-3 |
| 0x0002–0x000F | MPEG reserved | SCTE 54 2009 Table 5.7, p.23 |
| 0x0010–0x001F | DVB SI; 0x0020–0x002F ARIB — avoid | SCTE 54 2009 Table 5.7, p.23 |
| 0x0030–0x1FEF | **The only range for PMT PIDs and elementary-stream PIDs** | SCTE 54 2024 §7.9.4, p.23 ("shall not be set less than 0x0030 or greater than 0x1FEF") |
| 0x1FFB | base_PID: STT, MGT, CVCT/TVCT, RRT, DCCT, DCCSCT, cable_emergency_alert; "reserved exclusively for this purpose" | A/65 §5, p.20; SCTE 54 §7.8.1, p.16 and §7.8.2, p.18 |
| per MGT | EIT-k and ETT-k PIDs, any value, each unique among the MGT's table_type_PIDs | A/65 §6.5, p.41; §6.6, p.44 |
| 0x1FFE | Reserved for DOCSIS — never use | SCTE 54 2009 Table 5.7, p.23 |
| 0x1FFF | Null packets | H.222.0 Table 2-3 |

A/65 footnote 7 (p.20): do not put program_number 0 (network_PID) in the PAT pointing at
0x1FFB; ATSC defines no NIT.

[Rec] PID plan per carrier: PMT for program *n* (1..6) at 0x0030 + 0x10·(n−1) (the layout the
box's own example uses, manual p.20), video at PMT+1, audio at PMT+4..6; EIT-k at 0x1D00+k,
event ETT-k at 0x1E00+k, channel ETT at 0x1E80. All inside 0x0030–0x1FEF.

### 2.3 Repetition and rate limits (cable)

SCTE 54 2024 §7.8.1.2 Table 1 (p.17), Table 2 (p.18):

| Table | Max cycle (cable) | Terrestrial for comparison (A/65 Table 7.1, p.83) |
|---|---|---|
| STT | 10 000 ms | 1 000 ms |
| MGT | 150 ms | 150 ms |
| CVCT / TVCT | 400 ms | 400 ms |
| RRT (when present) | 60 000 ms | 60 000 ms |
| EIT-0 | 500 ms **recommended** (SCTE 54 p.17; A/65 §7.1 p.83) | same |
| PAT | ≤100 ms; may stretch to ≤140 ms only if needed to stay under the 80 kbit/s system-data limit (SCTE 54 §7.5, p.14) | — |
| PMT | ≤400 ms per program (SCTE 54 §7.5, p.14) | — |

Max rate of each PSIP packet stream (base_PID, each EIT PID, each ETT PID): **250 000 bit/s**,
and each obeys a T-STD smoothing buffer with **sb_leak_rate 625** (250 kbit/s) and
**sb_size 1024 bytes** (SCTE 54 Table 2 and text, p.18; same numbers terrestrial, A/65 §7.1,
p.83). [I] The 1024-byte buffer means a burst on one PSIP PID may not exceed ~1 KB ahead of the
250 kbit/s leak: pace large EIT/ETT sections across packets rather than emitting them
back-to-back.

[Rec] Send STT every 1 s (the terrestrial figure; many receivers are designed around it [K]),
MGT every 100 ms, CVCT every 200–300 ms, PAT every ≤90 ms, each PMT every ≤300 ms, EIT-0 every
≤500 ms, EIT-1..3 every 2–3 s, later EITs and ETTs every 10–60 s. Leave margin below every
maximum because the box's re-timing (null insertion, PCR correction) can shift packets.

Annex G.8 (p.141–142) estimates required cable PSIP at ~25 kbit/s without EIT/ETT.

### 2.4 Packet-level constraints on PSIP

For STT, MGT, TVCT/CVCT, RRT, EIT, ETT: transport_scrambling_control `00`,
adaptation_field_control `01` — no adaptation field (A/65 §6.1 p.23, §6.2 p.25, §6.3.1 p.29,
§6.3.2 p.34, §6.4 p.37, §6.5 p.41, §6.6 p.44). The MGT must additionally **start a TS packet**:
PUSI=1 and pointer_field=0x00 on the packet carrying its table_id (A/65 §6.2, p.25). Other
sections may be packed after one another (Annex D.5, p.117); unused bytes after the last
section in a packet are 0xFF stuffing (H.222.0 §2.4.4.2 [see §1]).

PAT and PMT packets may carry an adaptation field **only** to set discontinuity_indicator when
version_number may be discontinuous (SCTE 54 §7.5, p.14–15).

### 2.5 CVCT versus TVCT

The CVCT (table_id 0xC9) has the TVCT's syntax and semantics except where stated (A/65
§6.3.2, p.35). Differences:

| Field | TVCT (0xC8) | CVCT (0xC9) | Source |
|---|---|---|---|
| bits after `hidden` | `reserved '11'` | `path_select` (0 = path 1), `out_of_band` (0 = in a tuned multiplex) | A/65 Table 6.8 p.36, §6.3.2 p.37 |
| major_channel_number | 1–99 (US: Annex B) | two-part: 0–999; or one-part form (below) | A/65 §6.3.1 p.32; §6.3.2 p.35; Annex G.3.3 p.137 |
| minor_channel_number | 0 for analog, 1–99 for DTV/audio, 1–999 data | 0–999 for any service type | A/65 §6.3.1 p.32; Annex G.3.3 p.137 |
| source_id 0 | reserved | "programming source is not identified" — no EPG link | A/65 §6.3.2 p.37; Annex G.3.4 p.137 |
| service_location_descriptor | mandatory for each active channel | not required (PMT carries the same data); if sent it must match the PMT | A/65 §6.9.5 p.71–72; Annex G.5.3 p.139 |

path_select: unidirectional cable-ready devices are not required to support it (Annex G.3.1,
p.136). Set path_select=0, out_of_band=0 [Rec].

### 2.6 Channel numbers on cable

- **Two-part**: both major_channel_number and minor_channel_number < 1000 (SCTE 54 §7.8.1.1,
  p.17; A/65 §6.3.2, p.35).
- **One-part**: the six MSBs of the 10-bit major_channel_number are `11 1111` (major
  0x3F0–0x3FF). The 14-bit number is "`(major_channel_number & 0x00F) << 10 +
  minor_channel_number`" (A/65 §6.3.2, p.35; SCTE 54 §7.8.1.1, p.17).
  - As written in C, `+` binds tighter than `<<`, so the literal expression would be
    `(major & 0xF) << (10 + minor)`. The intent is clearly
    `((major & 0xF) << 10) | minor`, giving 0–16383 with minor carrying the low 10 bits [I].
    So one-part channel *N* encodes as major = 0x3F0 | (N >> 10), minor = N & 0x3FF.
    Minor values 1000–1023 are then legal inside a one-part number [I].
- A CVCT may mix one-part and two-part entries, and the same program may appear under both
  forms in separate loop iterations (A/65 §6.3.2 p.35 and footnote 11).
- No major/minor pair may repeat within the CVCT (A/65 §6.3.2, p.35). SCTE 54 adds nothing
  further; for MPTS mode the box's manual asks for unique pairs per MPTS (manual Appendix C,
  p.45).
- [Rec] Use two-part numbers (e.g. 20.1–20.6 on carrier 1, 21.1–21.6 on carrier 2). SCTE 18
  notes that receivers built to the original J-STD-042 understand only two-part numbers
  (SCTE 18 notes p.16, p.18), and two-part numbers are what terrestrial-derived UIs are built
  around [K]. One-part numbers remain an option to test.

### 2.7 Other CVCT field rules

| Field | Rule | Source |
|---|---|---|
| transport_stream_id (header) | The TSID in this TS's PAT | A/65 §6.3.1, p.31 |
| channel_TSID | TSID of the TS carrying the program. Receivers record each TS's PAT TSID with the frequency it was found on and match channel_TSID against it | A/65 §6.3.1 p.32–33; Annex G.5.2 p.138 |
| TSID uniqueness | "The TSID for each TS referenced by a CVCT needs to be unique on that cable system" | A/65 Annex G.5.2, p.139 |
| modulation_mode | 0x02 = SCTE mode 1 (5.057 Msps, 64-QAM); **0x03 = SCTE mode 2 (5.361 Msps, 256-QAM)**; 0x04 8-VSB; 0x01 analog | A/65 Table 6.5, p.32 |
| carrier_frequency | "These 32 bits shall be set to zero." Receivers find a TS by scanning and matching TSID | A/65 §6.3.1, p.32–33 |
| program_number | Matches PAT/PMT; 0 for an inactive channel; 0xFFFF for analog | A/65 §6.3.1, p.33 |
| ETM_location | `00` none, `01` in this PTC, `10` in the PTC given by channel_TSID | A/65 Table 6.6, p.33 |
| access_controlled | 0 for our clear services | A/65 §6.3.1, p.33 |
| hidden / hide_guide | hidden=1: not reachable by number or surfing; hide_guide=0 lets a hidden channel appear in the EPG; hide_guide ignored when hidden=0 | A/65 §6.3.1, p.33–34 |
| service_type | 0x02 ATSC digital television (MPEG-2 **or AVC** video + AC-3 CM); 0x03 ATSC audio-only; 0x04 data; 0x07 parameterized (A/71) | A/53-3 §5.1.3.1–5.1.3.3, p.10–11; A/53-3 Amend. 1 §5.1.3.1, p.4 |
| source_id | 0x0001–0x0FFF unique within the TS; 0x1000+ regionally unique and issued by an ATSC-designated authority | A/65 §6.3.2, p.37 |
| short_name | 1–7 UTF-16 code units, padded with 0x0000 | A/65 §6.3.1, p.31–32 |
| section_length | ≤1021 (first two bits `00`) | A/65 §6.3.1, p.31 |
| sections | Up to 256 sections; a channel's record never spans sections | A/65 §6.3, p.28 |
| version_number | +1 mod 32 whenever the current VCT changes; equals the MGT entry. "Next" VCTs (current_next=0) are allowed but "strongly discouraged" for one-section tables | A/65 §6.3.1, p.31 |

[Rec] source_id: allocate 0x0001–0x0FFF **unique across all 16 carriers** and keep each
service's source_id constant forever (Annex G.3.4, p.137: changes break recordings).

### 2.8 Describing services on other carriers

SCTE 54 lets each carrier's CVCT also list "other digital channels available in different
Transport Streams" (§7.8.1.1, p.17). Rules if we do: channel_TSID must be the other carrier's
TSID (A/65 Annex G.5.2, p.138), ETM_location `10` if its ETT is only on its own carrier, and
modulation_mode 0x03.

[Rec] Each carrier's CVCT describes **only its own six programs**. Reasons: SCTE 54 requires
nothing more; carrier_frequency is zero, so TVs must scan every carrier anyway; and one
operator found that PSIP describing other streams interfered with how one TV brand resolved
clear-QAM channels (TV Technology, 2009-07-09,
https://www.tvtechnology.com/news/update-on-limited-vizio-clearqam-channel-choices — the
operator's fix was to stop sending PSIP on other streams; the cause was not fully understood).
Exception: an EAS details channel (§9).

### 2.9 SCTE 65

SCTE 65 is out-of-band SI for set-top boxes and CableCARD hosts (A/65 Annex G.4 and G.7,
p.137, p.141). Clear-QAM TVs without a CableCARD navigate from in-band PSIP only (SCTE 54
§7.8.1 note, p.16). We do not emit SCTE 65.

---

## 3. PSIP bit syntax and semantics

All PSIP tables use the MPEG long-form private section (A/65 §4.1 Table 4.1, p.18):

| Field | Bits | Value |
|---|---|---|
| table_id | 8 | per table |
| section_syntax_indicator | 1 | 1 |
| private_indicator | 1 | 1 |
| reserved | 2 | 11 |
| section_length | 12 | bytes after this field up to and including CRC_32; ≤4093 generally, ≤1021 for STT, TVCT, CVCT, RRT |
| table_id_extension | 16 | per table |
| reserved | 2 | 11 |
| version_number | 5 | +1 mod 32 on any change |
| current_next_indicator | 1 | 1 (always, except an optional "next" VCT) |
| section_number | 8 | 0 for the first section, +1 per section |
| last_section_number | 8 | highest section_number |
| protocol_version | 8 | **0** (only valid value) |
| … table data … | | |
| CRC_32 | 32 | MPEG-2 CRC (H.222.0 Annex A), over the whole section |

Reserved bits are set to 1 (A/65 §4.2, p.20). The version_number applies to the collection of
sections, not to each section (A/65 footnote 6, p.17). Field semantics: A/65 §4.1, p.18–20.
CRC details: §1.4.

Table IDs (A/65 Table 4.2, p.19): 0x00 PAT, 0x02 PMT, **0xC7 MGT, 0xC8 TVCT, 0xC9 CVCT, 0xCA
RRT, 0xCB EIT, 0xCC ETT, 0xCD STT**, 0xD3 DCCT, 0xD4 DCCSCT; 0xD8 cable_emergency_alert
(SCTE 54 2009 Table 5.3, p.17; SCTE 18 p.8).

Maximum section sizes (total bytes = section_length + 3):

| Table | section_length max | Total | Sections allowed | Source |
|---|---|---|---|---|
| STT | 1021 | 1024 | 1 | A/65 §6.1, p.24 |
| MGT | 4093 | 4096 | 1 | A/65 §6.2, p.27 |
| TVCT/CVCT | 1021 | 1024 | ≤256 | A/65 §6.3, p.28; §6.3.1, p.31 |
| RRT | 1021 | 1024 per region | 1 per region | A/65 §6.4, p.37, p.39 |
| EIT | 4093 | 4096 | ≤256 per instance | A/65 §6.5, p.40, p.42 |
| ETT | 4093 | 4096 | 1 per ETM | A/65 §6.6, p.44–45 |
| PAT, PMT | 1021 | 1024 | — | H.222.0 §2.4.4.3/2.4.4.8 [see §1] |

### 3.1 STT (table_id 0xCD) — A/65 §6.1 Table 6.1, p.24–25

| Field | Bits | Value |
|---|---|---|
| header | | table_id_extension **0x0000**, version_number **0**, current_next 1, section 0/0, protocol_version 0 |
| system_time | 32 | GPS seconds since 1980-01-06 00:00:00 UTC |
| GPS_UTC_offset | 8 | whole seconds; UTC = GPS − offset |
| daylight_saving | 16 | DS_status (1), reserved `11` (2), DS_day_of_month (5), DS_hour (8) |
| descriptors | var | none needed |
| CRC_32 | 32 | |

- Accuracy: GPS seconds count and leap-second count "accurate and correct to within plus or
  minus one second", timed at arrival of the packet carrying the last CRC byte (p.24–25).
- Send the **next** second's value about 2T ms before the second increments, where T is the
  mean spacing of base_PID packets (p.25).
- PID 0x1FFB, scrambling `00`, AFC `01` (p.23).
- Leap seconds: "method A" (ignore future leap seconds, recompute EITs after one occurs) is
  adequate for program start times (Annex D.7, p.120–121).
- GPS_UTC_offset is currently 18 s [K: leap second of 2016-12-31; no leap second since]. Take
  it from a configured value or the host's leap-second table, never hard-code silently.

**daylight_saving** (Annex A, Tables A1–A2, p.84–85):

| Period | DS_status | DS_day_of_month | DS_hour |
|---|---|---|---|
| Standard time, transition more than a month away | 0 | 0 | 0 |
| From one month before the switch into DST until it happens | 0 | day_in (local day 1–31) | hour_in (local hour 0–18, US: 2) |
| After all zones in the network have switched in | 1 | 0 | 0 |
| From one month before the switch out until it happens | 1 | day_out | hour_out |
| After all zones have switched out | 0 | 0 | 0 |

In the US the status bit must flip no later than 7 p.m. Pacific on the transition day (Table
A2). Receivers get the time-zone offset from the user, not from the STT (Annex A, p.84).
[K] US DST: second Sunday in March to first Sunday in November, 02:00 local.

### 3.2 MGT (table_id 0xC7) — A/65 §6.2 Table 6.2, p.26–28

| Field | Bits | Value |
|---|---|---|
| header | | table_id_extension 0x0000; version_number +1 mod 32 whenever anything in it changes; current_next 1; section 0/0 |
| tables_defined | 16 | 2–370 for cable (6–370 terrestrial) |
| loop: table_type | 16 | Table 6.3 below |
| reserved | 3 | 111 |
| table_type_PID | 13 | PID carrying that table |
| reserved | 3 | 111 |
| table_type_version_number | 5 | equals the version_number of the referenced table instance |
| number_bytes | 32 | total bytes of that table type (all sections, all instances) |
| reserved | 4 | 1111 |
| table_type_descriptors_length | 12 | usually 0 |
| descriptors | var | |
| reserved | 4 | 1111 |
| descriptors_length | 12 | usually 0 |
| CRC_32 | 32 | |

table_type values (Table 6.3, p.27): 0x0000 TVCT current, 0x0001 TVCT next, **0x0002 CVCT
current**, 0x0003 CVCT next, **0x0004 channel ETT**, 0x0005 DCCSCT, **0x0100–0x017F EIT-0..127**,
**0x0200–0x027F event ETT-0..127**, **0x0301–0x03FF RRT for rating_region 1–255**,
0x0400–0x0FFF user private (needs a registration descriptor and may not use PID 0x1FFB),
0x1400–0x14FF DCCT.

- The STT is not listed in the MGT (§6.2, p.25).
- Version numbers are scoped per PID: a receiver must track (table_type_PID, version), not
  (EIT-k, version). Example of correct handling at a 3-hour boundary: A/65 Annex D.9,
  p.123–125.
- [Rec] A minimal cable MGT lists CVCT current (PID 0x1FFB) and nothing else (tables_defined
  ≥2 is the stated minimum; [I] list CVCT plus EIT-0 at least, or CVCT plus channel ETT, to
  satisfy it; see Open questions).

### 3.3 CVCT (table_id 0xC9) — A/65 §6.3.2 Table 6.8, p.36

| Field | Bits | Notes |
|---|---|---|
| header | | table_id_extension = **transport_stream_id**; section_length ≤1021 |
| num_channels_in_section | 8 | |
| loop: short_name | 7×16 | UTF-16, NUL padded |
| reserved | 4 | 1111 |
| major_channel_number | 10 | §2.6 |
| minor_channel_number | 10 | §2.6 |
| modulation_mode | 8 | 0x03 |
| carrier_frequency | 32 | 0 |
| channel_TSID | 16 | |
| program_number | 16 | |
| ETM_location | 2 | |
| access_controlled | 1 | 0 |
| hidden | 1 | |
| path_select | 1 | 0 |
| out_of_band | 1 | 0 |
| hide_guide | 1 | |
| reserved | 3 | 111 |
| service_type | 6 | 0x02 or 0x03 |
| source_id | 16 | |
| reserved | 6 | 111111 |
| descriptors_length | 10 | |
| descriptors | var | extended_channel_name, service_location (optional on cable) |
| reserved | 6 | 111111 |
| additional_descriptors_length | 10 | 0 |
| additional descriptors | var | |
| CRC_32 | 32 | |

Size per channel without descriptors: 32 bytes [I from field widths]; header+trailer 16 bytes
(13 + 3, consistent with Annex G.8's "16 + 52·n" estimate, p.141). Six channels with a
service_location_descriptor (5 + 6·n bytes) and an extended_channel_name fit easily in one
1024-byte section.

Inactive channels: hidden=1, hide_guide=0, program_number=0, no service_location_descriptor,
no PMT in the TS (A/65 §6.3.1, p.34; §6.9.9, p.75).

### 3.4 RRT (table_id 0xCA) — A/65 §6.4 Table 6.10, p.38–40

Header: table_id_extension = 0xFF (8) + rating_region (8); current_next 1; section 0/0;
protocol_version 0; ≤1024 bytes, one section per region (p.37). Body:
rating_region_name_length (8) + MSS (≤32 display chars); dimensions_defined (8, 1–255); per
dimension: dimension_name_length (8) + MSS (≤20 chars), reserved `111` (3), graduated_scale (1),
values_defined (4, 1–15); per value: abbrev_rating_value_length (8) + MSS (≤8 chars; value 0 is
the empty string), rating_value_length (8) + MSS (≤150 chars; value 0 empty); reserved `111111`
(6), descriptors_length (10), descriptors, CRC_32.

**Must region 1 be sent?** No. CTA-766-D §3.1 (p.2): "transmission of the U.S. RRT,
rating_region 0x01, is not required", and the US system cannot be implemented from a
transmitted RRT anyway. Also SCTE 54 §7.8.1.1 (p.17) and A/65 §6.4 (p.37). [Rec] Do not send
an RRT; send content_advisory_descriptors against region 0x01 using the indices below.
(CTA-766-D page numbers are the printed ones; PDF page = printed + 6.)

**US region 0x01** (CTA-766-D §3.1 Table 1, p.3): version 0, name "US (50 states +
possessions)", dimensions_defined 8. Value [0] of every dimension is the blank string.

| dim | Name | grad | Values |
|---|---|---|---|
| 0 | Entire Audience | 1 | 1 None, 2 TV-G, 3 TV-PG, 4 TV-14, 5 TV-MA |
| 1 | Dialogue | 0 | 1 D |
| 2 | Language | 0 | 1 L |
| 3 | Sex | 0 | 1 S |
| 4 | Violence | 0 | 1 V |
| 5 | Children | 1 | 1 TV-Y, 2 TV-Y7 |
| 6 | Fantasy Violence | 0 | 1 FV |
| 7 | MPAA | 0 | 1 N/A ("MPAA Rating Not Applicable"), 2 G, 3 PG, 4 PG-13, 5 R, 6 NC-17, 7 X, 8 NR ("Not Rated by MPAA") |

"None" (dim 0 value 1) means "no blocking is intended per the program rating criteria"
(Table 1 footnote, p.3). The earlier unsourced version of this table omitted MPAA 8 = NR.

**Descriptor rules for region 0x01** (CTA-766-D §4 and §4.1, p.5–9):
- rated_dimensions 1–8; rating_dimension 0–7; **rating_description_length = 0** (no text;
  receivers show the RRT 0x01 text) (§4.1, p.6). A/65 allows up to 80 bytes of
  rating_description_text (§3.7); for region 0x01 send none.
- **Only the 54 messages of Table 3 (p.7–8) are allowed**, the same 54 as the 608 XDS rating
  packet; "no other combinations ... are allowed" (p.6). In Table 3, "X" means the dimension
  shall not be present; "0" means it should be omitted.
  - TV-None: dim0 = 1. TV-Y: dim5 = 1. TV-Y7: dim5 = 2. **TV-Y7-FV: dim5 = 2 + dim6 = 1**
    (FV only with TV-Y7). TV-Y/TV-Y7 send no dim 0.
  - TV-G: dim0 = 2 only (no flags).
  - TV-PG (dim0 = 3) and TV-14 (dim0 = 4): any subset of D, L, S, V (dims 1–4 = 1), 16 each.
  - **TV-MA (dim0 = 5): any subset of L, S, V only — no D** (8 messages).
  - **MPAA (dim7 = 1–8) is sent alone**: every TV dimension is "X" in MPAA messages, and dim 7
    is "X" in every TV message. So a program carries either a TV rating or an MPAA rating,
    never both, in region 0x01.
  - Example: TV-14-L-V = rated_dimensions 3: (0,4) (2,1) (4,1).
- **No descriptor = not rated**: absence from the EIT means no rating; receivers by default
  should not block unrated programs (p.5, p.8).
- **Digital receivers ignore 608 XDS rating data** when the program is described by the EIT
  (§4, p.5): XDS ratings are not a substitute for the descriptor.
- The descriptor's contents shall not change during a program, including its commercials,
  promos and station IDs (p.9). [Rec] Rate per EIT event; do not try to re-rate inside an
  event.
- Region 0x02 is Canada (§3.2, §4.2); 0x05 is reserved for a downloadable US RRT (§3.3, p.5).

### 3.5 EIT (table_id 0xCB) — A/65 §6.5 Table 6.11, p.40–43

| Field | Bits | Notes |
|---|---|---|
| header | | table_id_extension = **source_id**; version_number per EIT-k instance, equals the MGT entry; current_next 1; section_length ≤4093 |
| num_events_in_section | 8 | 0 allowed |
| loop: reserved | 2 | 11 |
| event_id | 14 | |
| start_time | 32 | GPS seconds |
| reserved | 2 | 11 |
| ETM_location | 2 | 0 none, 1 in this PTC, 2 in the PTC carrying this event (Table 6.12, p.43) |
| length_in_seconds | 20 | |
| title_length | 8 | 0 = no title |
| title_text | var | MSS |
| reserved | 4 | 1111 |
| descriptors_length | 12 | |
| descriptors | var | content_advisory, caption_service, AC-3 audio, rc, genre |
| CRC_32 | 32 | |

Rules:
- **3-hour windows**: each EIT covers exactly 3 hours starting 00, 03, 06, … 21 h **UTC**; EIT-0
  is the window containing now, EIT-1 the next, and so on, non-overlapping (A/65 §5 Req. 1–3,
  p.22).
- At a window boundary, EIT-1 becomes EIT-0 by **shifting the PIDs in the MGT**; content
  corrections other than shifting bump the version number (A/65 §5, p.22; Annex D.9).
- Up to 128 EITs (EIT-0..127 = 16 days) (§6.5, p.40).
- **Every virtual channel in the VCT has an EIT-k instance** (one per source_id); a channel with
  no events in that window gets one section with num_events_in_section = 0 (§6.5, p.40).
- An event that spans windows appears in **each** window's EIT with the **same event_id**
  (§6.5, p.40).
- event_id uniquely identifies an event within the 3-hour window for that channel (§6.5,
  p.40). Since it forms part of the ETM_id, keep it stable for an event's lifetime [I]. Use
  the 14-bit space (0–16383) per source_id; [Rec] allocate monotonically per channel and wrap.
- Events in start-time order, within and across sections; no overlap: each start_time ≥
  previous start_time + length (§6.5, p.41; p.43).
- One event is never split across sections (§6.5, p.40).
- Data-only (service_type 0x04) channels have no EIT (§6.5, p.40).
- Descriptors that **must** be in the EIT event loop when the EIT is present on cable (Table
  6.25b, p.67):
  - AC-3 audio descriptor, one per audio service, with the language field when languages
    differ (§6.9.1.1, p.68)
  - caption_service_descriptor for each event with captions (§6.9.2, p.68)
  - content_advisory_descriptor for each event with ratings (§6.9.3, p.70)
  - rc_descriptor if the program carries one (§6.9.12, p.77–78)
- For the **current** event, EIT-0 must repeat the PMT's caption_service and
  content_advisory descriptors (SCTE 54 §7.9.3.10–11, p.22).

**How many EITs?** Cable: none required (SCTE 54; A/65 G.2). Terrestrial: EIT-0..3 = 12 hours
(A/65 §5.1). A/65 footnote 12 (p.40) quotes the FCC expecting a correct guide for the next
"[9 to] 12 hour period". [Rec] Send EIT-0..EIT-7 (24 hours) as the default, configurable up to
EIT-55 (7 days); EIT-0..3 is the floor. Which TVs use more than 12 hours is untested.

### 3.6 ETT (table_id 0xCC) — A/65 §6.6 Table 6.13, p.44–45

| Field | Bits | Notes |
|---|---|---|
| header | | table_id_extension = **ETT_table_id_extension**; section 0/0 always; section_length ≤4093 |
| ETM_id | 32 | Table 6.14 below |
| extended_text_message | var | MSS |
| CRC_32 | 32 | |

ETM_id (Table 6.14, p.45): channel ETM = `source_id << 16` (bits 15..0 zero); event ETM =
`(source_id << 16) | (event_id << 2) | 0b10`.

- Channel ETT: table_type 0x0004 in the MGT, one PID. Event ETT-k: table_type 0x0200+k, one PID
  per k, paired with EIT-k (Fig 5.2, p.22). ETTs are carried on the TS that carries the
  channel or event ("home physical transmission channel") (§6.6, p.44).
- version_number: for event ETT-k, +1 when any ETM in ETT-k changes; equals the MGT entry
  (§6.6, p.45).
- **ETT_table_id_extension**: must be unique among ETT instances sharing a PID (p.45), **but**
  footnote 13 (p.45): "some deployed receivers may not operate correctly with a non-zero value
  for this field. Some such receivers may have been deployed in the United States." See Open
  questions.

### 3.7 Descriptors

Allowed locations on cable (A/65 Table 6.25b, p.67; "M" = must be there when used; "O" =
optional second location):

| Tag | Descriptor | PMT | CVCT | EIT |
|---|---|---|---|---|
| 0x0A | ISO_639_language (MPEG) | ES loop | | |
| 0x05 | registration (MPEG) | program / ES loop | | |
| 0x81 | AC-3 audio (A/52 Annex A) | M (ES loop) | | M |
| 0x86 | caption_service | M (video ES loop) | | M |
| 0x87 | content_advisory | M (program loop) | | M |
| 0xA0 | extended_channel_name | | M | |
| 0xA1 | service_location | | M (optional on cable, §2.5) | |
| 0xA3 | component_name | M (ES loop) | | |
| 0xAA | redistribution_control | M (program loop) | | M |
| 0xAB | genre | | | M |
| 0x80 | stuffing | anywhere | | |
| 0xAD | ATSC_private_information | anywhere | | |

Receivers must tolerate unknown descriptors anywhere (A/65 §6.9, p.67).

**extended_channel_name_descriptor (0xA0)** — A/65 §6.9.4 Table 6.28, p.71: tag (8),
length (8), long_channel_name_text (MSS).

**service_location_descriptor (0xA1)** — A/65 §6.9.5 Table 6.29, p.72–73: tag (8), length (8),
reserved `111` (3), PCR_PID (13), number_elements (8); per element: stream_type (8), reserved
`111` (3), elementary_PID (13), ISO_639_language_code (24; 0x000000 when none, e.g. video).
Must match the PMT now on air: same PCR_PID, stream types, PIDs and languages; must list the
video and all audio streams (p.72–73). Not present for inactive channels.

**content_advisory_descriptor (0x87)** — A/65 §6.9.3 Table 6.27, p.70–71: tag, length,
reserved `11` (2), rating_region_count (6, 1–8); per region: rating_region (8),
rated_dimensions (8, ≤ RRT's dimensions_defined); per dimension: rating_dimension_j (8,
strictly increasing), reserved `1111` (4), rating_value (4); then rating_description_length
(8, 0–80) + rating_description_text (MSS, ≤16 display chars). For region 0x01,
CTA-766-D requires length 0 (§3.4 below). An absent
dimension or descriptor means rating 0 (none). Placement: PMT program_info loop for the current
program and EIT-0 current event (SCTE 54 §7.9.3.11, p.22); future events in their EIT.

**caption_service_descriptor (0x86)** — A/65 §6.9.2 Table 6.26, p.69–70: tag, length,
reserved `111` (3), number_of_services (5, 1–16); per service: language (24, ISO 639-2/B,
Latin-1), digital_cc (1; 0 = 608, 1 = 708), reserved `1` (1); if digital_cc=0: reserved
`11111` (5) + line21_field (1, receivers ignore it); else caption_service_number (6, 1–63,
0 prohibited); easy_reader (1); wide_aspect_ratio (1; 0 = 4:3, 1 = 16:9); reserved 14 bits of 1.
Placement: PMT video ES loop **and** EIT-0 for the program when captions are in the picture
user data (SCTE 54 §7.9.3.10, p.22; A/65 §6.9.2, p.68). Absent from events without captions.
[Rec] for one English caption track: two entries — 608 CC1 (digital_cc=0, line21_field=0,
language "eng") and 708 service 1 (digital_cc=1, number 1, "eng", wide_aspect_ratio per video).

**component_name_descriptor (0xA3)** — A/65 §6.9.7 Table 6.32, p.74–75: tag, length,
component_name_string (MSS). **Required** in the PMT ES loop of each audio component when two
or more audio services share the same language and the same bsmod (A/65 §6.9.7, p.74; SCTE 54
§7.9.3.9, p.21). Should also label a full-service ME track (SCTE 54 p.22).

**redistribution_control (rc) descriptor (0xAA)** — A/65 §6.9.12 Table 6.37, p.77–78: tag,
length, rc_information bytes (none defined; length 0). Its presence means "technological
control of consumer redistribution is signaled". If used: PMT program loop **and** EIT-0
current event, and as far ahead as possible in later EITs (p.78). [Rec] never send it.

**genre_descriptor (0xAB)** — A/65 §6.9.13 Table 6.38, p.78–79: tag, length, reserved `111`
(3), attribute_count (5, 1–31), attribute bytes (0x01–0xFE) referencing the Categorical Genre
Code Assignments Table (A/65 Table 6.20, in §6.7); include at least one "basic" attribute.
EIT event loop only. Optional.

**AC-3 audio descriptor (0x81)**, **ISO_639_language_descriptor (0x0A)**, registration
descriptors: see §4.4.

### 3.8 multiple_string_structure — A/65 §6.10 Table 6.39, p.79–82

```
number_strings            8
for each string:
  ISO_639_language_code   24   (ISO 639-2/B, e.g. "eng", "spa")
  number_segments         8
  for each segment:
    compression_type      8    0x00 none; 0x01/0x02 Huffman (Annex C), only with mode 0x00
    mode                  8    0x00 = Unicode U+0000–00FF (Latin-1); 0x3F = UTF-16BE; 0x3E = SCSU
    number_bytes          8
    bytes                 8 × number_bytes
```

- Modes 0x01–0x33 select the high byte of 16-bit Unicode; each byte is the low byte (Table
  6.41, p.82). Mode 0xFF = not applicable.
- For text outside 0x00–0xFF, prefer 0x3E (SCSU), else 0x3F (UTF-16, MSB first) (p.80–81).
- A null string is best encoded as number_strings = 0 (p.80).
- A receiver ignores a string whose mode it does not support (p.80).
- Length limits come from the enclosing field (title_length, descriptor_length: 8 bits). A
  segment holds ≤255 bytes; use several segments for longer ETT text [I].
- [Rec] Emit one string, language "eng" (or the event's language), one segment per ≤255 bytes,
  compression 0x00, mode 0x00, characters restricted to Latin-1. No Huffman. SCTE 18 receivers
  must support English and the Huffman tables (SCTE 18 §7.7 #37, p.31), but uncompressed Latin-1
  is the universally safe choice [K].

---

## 4. Stream types and PMT signaling

### 4.1 stream_type values we use

| stream_type | Content | Source |
|---|---|---|
| 0x02 | MPEG-2 video (0x80 also allowed on cable; use 0x02) | SCTE 54 §7.9.2.1, p.19; A/53-3 §5.7.2, p.14 |
| 0x1B | H.264/AVC video | SCTE 54 §7.9.2.1, p.19; SCTE 128-2 §6.3.1, p.9; A/53-3 Amend. 1 §5.7.4, p.5 |
| 0x81 | AC-3 audio | SCTE 54 §7.9.2.2, p.19; A/53-3 §5.7.1, p.14 |
| 0x87 | E-AC-3 (not planned) | SCTE 54 Table 3, p.19 |
| 0x86 | SCTE 35 cue messages [K]; not needed | — |
| 0xC4–0xFF | private; requires a registration descriptor | SCTE 54 §7.2.4, p.13 |

ATSC manages 0x80–0xC3 (SCTE 54 §7.9, p.18). Captions have no PID of their own: they are in the
video elementary stream (§6).

### 4.2 PMT contents per program [Rec, each line cited]

Program-info loop:
- registration_descriptor (tag 0x05) "GA94" (0x47413934) — optional; marks the program as
  ATSC-conformant (A/53-3 §5.2.1.1, p.11; SCTE 54 §7.2.2, p.12). At most one registration
  descriptor per loop (SCTE 54 §7.2.1, p.12). [Rec] include.
- content_advisory_descriptor (0x87) for the current program, if rated (SCTE 54 §7.9.3.11,
  p.22).
- No rc_descriptor (§3.7).

Video ES loop:
- **data_stream_alignment_descriptor**: tag 0x06, length 0x01, alignment_type 0x02 —
  **required** for 0x02 and 0x1B (A/53-3 §5.4.1, p.12; Amend. 1 §5.4.1, p.4–5).
- **caption_service_descriptor** (0x86) when captions are present (SCTE 54 §7.9.3.10, p.22).
- H.264 only: AVC_video_descriptor (tag 0x28: profile_idc 8, constraint_set0..5 flags 6,
  AVC_compatible_flags 2, level_idc 8, AVC_still_present 1, AVC_24_hour_picture_flag 1,
  Frame_Packing_SEI_not_present_flag 1, reserved 5) — H.222.0 Table 2-89, p.96–97; required by
  SCTE 128-2 only "when required by ISO/IEC 13818-1 or as otherwise appropriate" (§6.3.2.1,
  p.9), i.e. strictly only for stills. [Rec] include it with AVC_still_present = 0.
- H.264: AVC_timing_and_HRD_descriptor (0x2A, H.222.0 Table 2-90, p.98) is required unless
  every IDR and recovery-point SPS has VUI timing_info_present_flag = 1 (p.97). SCTE 128-1
  requires VUI timing (§5.2), so it may be omitted [I].

Each AC-3 ES loop:
- **AC-3_audio_stream_descriptor** (0x81) — **required** (SCTE 54 §7.9.3.1, p.19–20; A/53-3
  §5.8.1.1, p.15). Syntax in §4.4.
- registration_descriptor "AC-3" (0x41432D33) — optional (SCTE 54 §7.2.3, p.12–13; A/52 Table
  A3.1, p.115). [Rec] include (one per loop max).
- ISO_639_language_descriptor (0x0A) — **required on cable when two or more audio ESs share a
  bsmod** (SCTE 54 §7.9.3.2, p.20); optional but recommended in A/53-3 §5.8.1.2 (p.15–16).
- component_name_descriptor (0xA3) — required when two or more audio services share language
  and bsmod (§3.7).

At most one descriptor of each tag per loop unless a standard says otherwise (A/53-3 §5.8,
p.15).

### 4.3 Multi-audio: SAP and described video

| Track | AC-3 bsmod | full_svc | AC-3 descriptor language | ISO_639 descriptor | Source |
|---|---|---|---|---|---|
| Main (English) | 0 (CM) | 1 | "eng", priority 01 (primary), mainid 0 | "eng", audio_type 0x00 | A/53-5 §6–7.1.1, p.8; A/52 Table A4.6, p.119 |
| SAP (Spanish) | 0 (CM) | 1 | "spa", priority 10, mainid 1 | "spa", audio_type 0x00 | A/53-5 §7.1.1, p.8 |
| Described video | 2 (VI), complete mix | 1 | "eng"; asvcflags bit 0 set (associated with mainid 0) | see below | A/53-5 §7.1.2, p.8–9; SCTE 54 §8.2, p.23 |

- At least one CM per program (SCTE 54 §7.3, p.14; A/53-3 §5.1.3.1, p.10). At most one stream
  labeled "Primary" (SCTE 54 §7.9.3.6, p.21). Associated services must be complete mixes with
  full_svc = 1 (A/53-5 §6, p.8). ME, D, C, E, VO are deprecated (same).
- Language is signaled in the AC-3 descriptor's `language` field (SCTE 54 §7.9.3.1 item 4,
  p.20); the descriptor's `langcod` is reserved / 0xFF (SCTE 54 item 3; A/53-3 §5.8.1.1).
  Language must be present when ≥2 streams share language and bsmod; with mixed languages it
  must be in every descriptor (A/65 §6.9.1.1, p.68 for the EIT).
- **Described-video signaling conflict** (see Open questions): A/53-3 §5.8.1.2 (p.15–16) says
  ISO_639 audio_type shall be 0x00 and receivers should use bsmod. SCTE 54 §8.1 (p.23) says
  deployed set-top boxes ignore the AC-3 descriptor and select by ISO_639, and that the VI
  track is often labeled "spa" or given audio_type 0x03 (visual impaired commentary, H.222.0
  Table 2-60, p.78). SCTE 54 §8.2's example uses ISO_639 "spa" for an English VI track. With a
  real Spanish track present that trick collides. [Rec] AC-3 descriptor: eng/bsmod 2. ISO_639:
  "eng" + audio_type 0x03. component_name_descriptor "Described Video" on it. Test on target TVs.
- Encode all tracks of a program frame-synchronously with identical PTS (SCTE 54 §7.7.1,
  p.16; A/52 A6.1–6.2, p.124–125).
- mainid unique per program (SCTE 54 §7.6, p.15).

### 4.4 AC-3_audio_stream_descriptor — A/52 Annex A Table A4.1, p.116–119

| Field | Bits | Value for us |
|---|---|---|
| descriptor_tag | 8 | 0x81 |
| descriptor_length | 8 | |
| sample_rate_code | 3 | 000 (48 kHz) (Table A4.2, p.117) |
| bsid | 5 | 01000 (8) |
| bit_rate_code | 6 | MSB 0 = exact rate; low 5 bits index 32…640 kbit/s (Table A4.3); must mean ≤ 448 kbit/s (codes 0–15 or 32–47, A/53-3 §5.8.1.1, p.15). exact codes: 128 = 0x08, 192 = 0x0A, 256 = 0x0C, 384 = 0x0E, 448 = 0x0F (Table A4.3, p.117) |
| surround_mode | 2 | dsurmod, or 00 |
| bsmod | 3 | equals the bitstream's bsmod (SCTE 54 item 5, p.20) |
| num_channels | 4 | MSB 0: low 3 bits = acmod; must be 1–13 (Table A4.5, p.118; SCTE 54 item 2) |
| full_svc | 1 | 1 |
| langcod | 8 | 0xFF (reserved) |
| langcod2 | 8 | only if num_channels = 0 (1+1, prohibited) |
| if bsmod < 2: mainid 3, priority 2, reserved 3 `111`; else asvcflags 8 | 8 | priority: 01 primary, 10 other, 11 unspecified (Table A4.6, p.119) |
| textlen | 7 | 0, or a short label |
| text_code | 1 | 1 = Latin-1 |
| text | 8·textlen | |
| language_flag | 1 | 1 |
| language_flag_2 | 1 | 0 |
| reserved | 6 | `111111` |
| language | 24 | ISO 639-2/B, e.g. "eng" |
| additional_info | var | none |

The descriptor may legally end after full_svc when describing a main service with no
associated services (SCTE 54 2009 §5.8.3.1 item 5, p.19–20). [Rec] Always send the full form
through `language`, since language is required on cable.

### 4.5 Registration descriptor and the "program paradigm"

registration_descriptor (H.222.0 Table 2-51, p.74): tag 0x05, length, format_identifier 32,
optional bytes. Identifiers: "GA94" program-level (ATSC), "SCTE" (0x53435445, SCTE), "AC-3"
ES-level (SCTE 54 §7.2.2–7.2.3, p.12–13).

"Program paradigm": A/53-3:2013 contains no PID-allocation paradigm; a program is everything
referenced by one program_number (A/53-3 §3.3, p.8). [K] Older A/53 editions had an informative
paradigm deriving PIDs from program_number (PMT PID = program_number << 4, video +1, audio +4…).
It is not normative now, receivers must read the PMT anyway (A/65 Annex G.5.3, p.139), and
SCTE 54 only requires PIDs in 0x0030–0x1FEF. **It does not matter**; use the §2.2 plan.

### 4.6 H.264 specifics (SCTE 128-1/-2, A/72)

- Profiles/levels on cable: **High or Main, Level 3.0, 4.0 or 4.2** (level_idc 30/40/42)
  (SCTE 128-1 §7.2, p.10). SPS: profile_idc 100 or 77; constraint_set1 = 1 for Main;
  chroma_format_idc 1; gaps_in_frame_num 0; **vui_parameters_present_flag 1** (Table 6, p.11).
  Receivers are expected to decode profile 100 and should decode 77.
- **Access unit delimiter in every AU** (H.222.0 §2.14.1, p.135; shown first in SCTE 128-2's SRAP
  order, Figure 1).
- **SPS and PPS in-band**; every random access AU (SCTE "SRAP") contains exactly one active SPS
  (with VUI) and the PPS it needs, SPS before any SEI; order AUD → SPS → PPS → SEI → I/IDR
  picture (SCTE 128-2 §6.4.1, p.10–11; SCTE 128-1 §7.2.1.1, p.11). A PPS id's contents may not
  change between SRAPs (SCTE 128-1 p.12).
- SRAP picture is I or IDR (SCTE 128-2 §6.4.1). **SRAP spacing ≤ 1 s** (DTS to DTS; slack of
  up to two pictures at non-integer rates) where fast channel change matters (§6.4.2.3, p.12).
  Initial CPB delay ≤ 3 s, should be ≤ 1 s; pictures with PTS ≥ DTS_SRAP + 0.5 s must decode
  fully when starting at the SRAP (§6.4.2.2, p.12).
- TS flags at an SRAP: the TS packet carrying the SRAP's PES header has an adaptation field,
  PUSI 1 and **random_access_indicator 1**; **elementary_stream_priority_indicator 1** on the
  packet with the first slice of the SRAP picture (§6.4.2.1, p.11–12).
- VUI (SCTE 128-1 Table 7, p.12; Table 11, p.15): aspect_ratio_idc per format table; color
  description (BT.709 for HD, SMPTE 170M for SD preferred); fixed_frame_rate_flag 1;
  timing: 29.97 → time_scale 60000 / num_units_in_tick 1001 (P or I), 59.94p → 120000/1001,
  23.976 → 48000/1001, 30 → 60/1, 60p → 120/1, 24 → 48/1.
- Picture timing SEI required on every AU if pic_struct_present_flag = 1; buffering period SEI
  optional (SCTE 128-1 §7.2.1.3–7.2.1.4, p.12–13).
- No unpaired fields; all pictures displayable (p.12). Resolution changes ≥ 1 s apart
  (§7.2.1.1, p.11); avoid level, size, aspect, frame-rate or scan changes within a program
  (§7.2.1.7, p.16).
- service_type 0x02 with stream_type 0x1B is the signaling "reported to be received
  successfully on a large majority of 'smart' DTV sets" (A/53-3 Amend. 1 §1.2–1.3, p.3); don't
  use service_type 0x07 for AVC [I].

---

## 5. Video and audio constraints

### 5.1 MPEG-2 video (A/53 Part 4:2009)

- Main Profile, within MP@HL limits; profile_and_level_indication gives the lowest level that
  fits (§6, p.8–9; Table 6.3 note, p.10).
- Sequence header limits (Table 6.1, p.9): bit_rate_value ≤ 48 500 (×400 bit/s = 19.4 Mbit/s) or
  ≤ 97 000 (38.8 Mbit/s "high data rate" mode, §6.1.1); **vbv_buffer_size_value ≤ 488**
  (×16 384 bits = 7 995 392 bits [I]).
- vbv_delay ≤ 45 000 (0.5 s) unless 0xFFFF (§6.1.5, p.10).
- **Allowed formats (Table 6.2, p.9):**

| Lines | Width | aspect_ratio_information | frame_rate_code, progressive | frame_rate_code, interlaced |
|---|---|---|---|---|
| 1080 (coded 1088) | 1920 | 1 (square), 3 (16:9) | 1, 2, 4, 5 | 4, 5 |
| 720 | 1280 | 1, 3 | 1, 2, 4, 5, 7, 8 | — |
| 480 | 704 | 2 (4:3), 3 (16:9) | 1, 2, 4, 5, 7, 8 | 4, 5 |
| 480 | 640 | 1, 2 | 1, 2, 4, 5, 7, 8 | 4, 5 |

frame_rate_code: 1 = 23.976, 2 = 24, 4 = 29.97, 5 = 30, 7 = 59.94, 8 = 60. Not allowed: 1080p50/60,
720×480 coding (use 704 or 640), interlaced 720.
- Sequence extension after every sequence header: chroma 4:2:0, size/bit-rate/vbv extensions
  zero, frame_rate_extension 0 (Table 6.3, p.10).
- sequence_display_extension: video_format 000; colour_primaries, transfer_characteristics and
  matrix_coefficients **explicitly indicated** (§6.1.4, Table 6.4, p.10); [Rec] BT.709 for
  HD, SMPTE 170M for SD.
- frame_pred_frame_dct = 1 when progressive_frame = 1 (§6.1.6, p.11).
- GOP / sequence header repetition: **no normative rule** in A/53-4 or SCTE 54. SCTE 54 §9.2
  (p.25, informative) suggests a sequence header about every 0.5 s. [Rec] closed GOP, I-frame
  + sequence header every 0.5 s (≤ 1 s), matching H.264's ≤ 1 s SRAP rule.
- SCTE 43 2015 (R2021) is now held (docs/external/standards/2026-11-24-scte-43/) but not yet digested
  here. Its Table 3 (§7.1.2, p.3) lists allowed MPEG-2 formats; Table 2 (§7.1.1, p.2) caps
  bit_rate_value at 67500 (27.0 Mbit/s) for 64-QAM and 97000 (38.8 Mbit/s) for 256-QAM, and
  vbv_buffer_size_value at 488.

### 5.2 H.264 formats (SCTE 128-1 Tables 9A–9C, p.13–14; Table 10, p.15)

| Level | Size | Progressive rates | Interlaced rates | Max bit rate | MaxCPB |
|---|---|---|---|---|---|
| 3.0 | 720/704/640×480 (also 544/528/352×480 at 23.976/29.97) | 1, 2, 4, 5 | 4, 5 | 15 Mbit/s | 15 Mbit |
| 4.0 | 1920/1440×1080 | 1, 2, 4, 5 | 4, 5 | 30 Mbit/s | 37.5 Mbit |
| 4.0 | 1280×720 | 1, 2, 4, 5, 7, 8 | — | 30 | 37.5 |
| 4.0 | 720/704/640×480 | 7, 8 | — | 30 | 37.5 |
| 4.2 | 1920/1440×1080 | 7, 8 (1080p60) | — | 30 (capped at the 4.0 rate) | 37.5 |

1080 is coded as 1088 with frame_crop_bottom_offset = 2·(1 + frame_mbs_only_flag) (p.14–15).
DPB use ≤ MaxFrameBuffers (6 at L3.0, 4 at 1080, 9 at 720 and 480p60). Bit-rate/CPB units are
not printed in Table 10. They are H.264 High-profile **NAL HRD** limits in Mbit/s and Mbit:
Table A-1 gives MaxBR/MaxCPB 10 000/10 000 at L3, 20 000/25 000 at L4 and 50 000/62 500 at
L4.1–4.2 (H.264 V16 Annex A, p.298), and for High profile the NAL-HRD unit is cpbBrNalFactor
= 1500 bit/s and 1500 bits (Table A-2, p.302; A.3.3 h), p.301). 10 000 × 1500 = 15 Mbit/s,
20 000 × 1500 = 30 Mbit/s, 25 000 × 1500 = 37.5 Mbit, matching Table 10. At L4.2 H.264 would
allow 75 Mbit/s; SCTE 128-1 caps 1080p60 at the L4.0 values. (VCL HRD limits are 1250 × the
table values: 12.5 / 25 Mbit/s.)

### 5.3 AC-3 audio (A/53 Part 5:2014, A/52)

- 48 kHz (fscod 00), bsid 8, acmod ≥ 001 (1+1 dual mono prohibited) (A/53-5 §5.1, §5.4,
  p.6–7).
- **≤ 448 kbit/s** per main service or complete associated service (A/53-5 §5.1, §5.3, p.6–7).
  No combined limit in the 2014 edition. [Rec] 384 kbit/s 5.1 or 192 kbit/s stereo for main;
  128–192 kbit/s stereo for SAP and VI.
- Syncframe = 1536 samples = 32 ms; frame size at 48 kHz = 2 × kbit/s words (A/52 Table 5.18,
  p.51; Annex A §2, p.114).
- **dialnorm** (5 bits): 1–31 = −1…−31 dB; 0 reserved (decoders treat as −31) (A/52 §5.4.2.8,
  p.40). It **shall** indicate the content's loudness in LKFS (A/53-5 §5.5, p.7). Decoders
  normalize to −31 LKFS; content at −24 LKFS carries dialnorm 24 (A/85 §4 p.14, §7.3.5 p.21).
- DRC: dynrng (line mode) ±24 dB, compr (RF mode) ±48 dB (A/53-5 §5.6, p.7).
- Audio buffer BSn = 2592 bytes (A/53-5 §5.7, p.7).

### 5.4 Loudness (A/85:2013, Recommended Practice)

- Measure with ITU-R BS.1770 (BS.1770-3 in this edition; LFE excluded; gated: 400 ms blocks every
  100 ms, absolute gate −70 LKFS, relative gate −10 dB) (A/85 §2.1 p.10; Annex A p.41).
- **Target −24 LKFS** where there is no metadata agreement; about **±2 dB** is acceptable
  measurement variation; **true peak below −2 dBTP** (§6, p.18).
- **dialnorm must match the content loudness** — "mandatory for DTV broadcasting in the United
  States" (§7.1, p.19). Fixed, preset or agile dialnorm all allowed (§7.2, p.19).
- Long-form: measure the anchor element (dialogue), else the full mix; short-form: integrated
  full mix (§5.2, p.16–17).
- Don't use DRC to fix loudness jumps; choose a DRC profile other than None (§8.3, §9.1.4–9.1.5,
  p.26–30).
- [Rec] Our AC-3 encoders: loudness-normalize every input to −24 LKFS (BS.1770) with a
  −2 dBTP limiter and set dialnorm = 24 on all tracks, including slate silence.
- [K] CALM Act: FCC 47 CFR §76.607 incorporates A/85 for MVPDs.

### 5.5 A/V sync

ATSC IS-191 (26 June 2003, p.4): at the inputs to the DTV encoders, sound should never lead
video by more than **15 ms** nor lag it by more than **45 ms**; receivers should present audio
and video within **±15 ms** of the time PTS indicates. [Rec] Derive all PTS of a program
from one capture clock; keep audio within ±1 AC-3 frame (32 ms) of video and test with a
sync clip.

---

## 6. Captions

### 6.1 MPEG-2 video user data (A/53 Part 4 §6.2, p.11–13)

Placed in `extension_and_user_data(2)` after the picture header and picture coding extension
(picture user data); at most one user_data() of each user_data_type_code per picture (§6.2.1,
p.11):

```
user_data_start_code   32  0x000001B2
ATSC_identifier        32  0x47413934 'GA94'                 (Table 6.7, p.12)
user_data_type_code     8  0x03 = MPEG_cc_data()             (Table 6.9, p.12; 0x06 = bar_data)
cc_data()                  per CEA-708 Table 2               (§6.2.3.1, p.13)
marker_bits             8  0xFF
```

### 6.2 H.264 SEI (SCTE 128-1 §8.1–8.2.2, p.17–19; A/72-1 §7.4.2, p.9)

SEI message payloadType 4 (user_data_registered_itu_t_t35) in an SEI NAL unit of the same AU:

```
itu_t_t35_country_code   8  0xB5
itu_t_t35_provider_code 16  0x0031
user_identifier         32  0x47413934 'GA94'                (Table 13)
user_data_type_code      8  0x03 = cc_data()                  (Table 15; 0x06 bar_data)
cc_data()                   per CTA-708
marker_bits              8  0xFF                              (Table 14)
```

### 6.3 cc_data() (CTA-708-E S-2023 §4.3, p.9–17)

Table 2 (p.10):

```
reserved '1' 1 | process_cc_data_flag 1 | zero_bit '0' 1 | cc_count 5 (0–31)
reserved 8 '1111 1111'
cc_count × { one_bit '1' 1 | reserved '1111' 4 | cc_valid 1 | cc_type 2 | cc_data_1 8 | cc_data_2 8 }
```

process_cc_data_flag = 0 tells decoders to discard the whole structure (p.10).
cc_type (Table 3, p.11): 00 = 608 line-21 field 1, 01 = 608 field 2, 10 = DTVCC Caption Channel
Packet (CCP) data, 11 = start of a CCP (cc_data_1 is the CCP header byte).

- 608 pairs (cc_type 00/01) come **first**, only in the first one, two or three positions
  depending on frame rate; a 00/01 pair after any 10/11 pair is invalid (§4.3.1 rules 1–2 and 6,
  p.11; Figure 2 state table, p.12).
- cc_valid = 0 with cc_type 00/01 at the start is **not padding**: it means "generate no 608
  waveform for that field". Padding is cc_valid = 0 with cc_type 10 or 11, recommended bytes
  0x00 0x00 (§4.3.5, p.15).
- When 608 data is present, every cc_data() carries a pair for each field, even if
  cc_valid = 0 (§4.3.2.2.1, p.14). Decoder 608 buffers hold only two pairs per field, so send
  608 at a steady rate, no more than 60/1.001 × 2 bytes/s, also across splices (p.14; §4.3.6,
  p.16).
- A CCP may span several cc_data() structures; put at least one CCP pair in each, then pad
  (§4.3.3, p.14–15).
- **Bandwidth: 9600 bit/s fixed allocation** (÷1.001 at 23.98/29.97/59.94), 608 included:
  608 gets 960/1.001 bit/s, DTVCC at least 8640/1.001 (§4 and §4.1, p.9). cc_data() appears
  at **every** opportunity (every picture's user data) and cc_count = r × t / 16000 rounded
  (r = 9600, t = ms since the previous picture in presentation order) (§4.3.6, p.15–16).
- cc_count per picture (Table 4, p.16, 608 fields 1 and 2 present): **30i or 30p: 20**
  (2 pairs 608 + 18 pairs DTVCC); **60p: 10** (1 pair 608 + 9 pairs DTVCC);
  **24p: 25** (608 alternating between 2 and 3 pairs); 30i with 3:2 pull-down alternates
  between 20 and 30. Table 5 (p.17) gives the variants without 608.
- Decode order: cc_data() follows its coded picture through reordering; the decoder reorders
  cc_data() with the pictures before extracting CCPs (§4.3.3 NOTE, p.15; Annex D). So the
  encoder attaches each picture's own caption bytes and transmits them in decode order.
  Feeding captions through the encoder (ffmpeg's `a53_cc` side data [K]) handles this.

### 6.3.1 The 608 bytes inside cc_data() (CTA-608-E S-2019)

Page numbers are the printed ones (PDF page = printed + 12). The PDF's running header says
"CEA-608-E". Only what a 708 encoder with 608 compatibility needs:

- **Channels and fields** (§4.1 Table 1, p.10): field 1 (cc_type 00) carries CC1, CC2, T1, T2;
  field 2 (cc_type 01) carries CC3, CC4, T3, T4 and **XDS**. CC1 and CC3 are the synchronous
  caption services (CC3 usually a second language); CC2 and CC4 are non-synchronous. Data
  must not be duplicated in more than one channel or field (p.10).
- **Data channel 1 vs 2** is chosen by the first byte of each control pair: 0x10–0x17 for
  channel 1, 0x18–0x1F for channel 2 (PAC Table 53, p.101; e.g. special characters 0x11 xx /
  0x19 xx, Table 49, p.96; miscellaneous codes 0x14 xx / 0x1C xx, Table 52, p.99). On field 2
  the miscellaneous control codes use 0x15 / 0x1D in place of 0x14 / 0x1C (§8.4, p.32;
  B.11.6, p.69). So CC1 = field 1 ch 1, CC2 = field 1 ch 2, CC3 = field 2 ch 1, CC4 = field 2
  ch 2.
- **Byte format**: 7-bit characters plus odd parity (§5.3, p.15). The null pair is 0x00 0x00
  with odd parity, i.e. **0x80 0x80** (CTA-708-E §4.3.2.2, p.13). Codes 0x01–0x0F must never
  be sent on field 1; on field 2 they start XDS (§8.4 c, p.32; B.11.5, p.68; D.2, p.78).
- **Control-code doubling** (D.2, p.78, Normative/Regulatory): each two-byte control code whose
  first byte is 0x10–0x1F "should be transmitted twice in successive frames"; insert nulls so
  control pairs are frame-aligned; force odd parity on all data. Decoders act on the first of
  a redundant pair (§3.2.2 "Immediately", p.7), so a command that is really meant twice must
  be sent three times ("three CRs ... to result in two CRs", §8.3, p.32). On field 2, do not
  double unless necessary (§8.3, p.32; B.14, p.70), and **never repeat XDS control codes**
  (§8.6.2, p.33).
- **Field 1 whenever field 2**: when field 2 carries data, field 1 must carry at least run-in
  and nulls (§8.2, p.32; D.7, p.79). In cc_data() terms: when 608 is present, every picture
  carries one pair per field, cc_valid = 0 marking a field with no waveform (CTA-708-E
  §4.3.2.2.1, p.14); the decoder adds the field 1 run-in itself when field 2 has data
  (CTA-708-E p.13).
- **Timing**: one byte pair per field per frame. Pop-on captions: time the first EOC (not its
  repeat) for the frame on which the caption should appear (B.11.2, p.68). Delay through the
  chain: 1–4 frames unobjectionable, 5–6 occasionally a problem, 9 or more unacceptable; with
  unknown downstream delay, pre-advance captions by 2 frames (B.11.7, p.69). Each encoder must
  keep CC3/CC4 delay at or below 4 frames (E.7.1, p.86). Smart encoders may send an erase
  after 16 s with no caption data, to clear captions left on screen by an upcut (D.4, p.78–79).
- **XDS** (field 2 only; §8.6, p.33–35; §9.2–9.3, p.36–37): Start/Type pair (Start =
  0x01 Current, 0x05 Channel, 0x07 Misc ...; Table 14, p.37), informational pairs (≤ 32
  characters, padded with 0x00 to an even count), then End 0x0F + checksum (7-bit two's
  complement of the sum of everything except Continue pairs; §8.6.3, p.34). Content advisory
  is Current class type 0x05 (Tables 18–21, p.40–41: same TV-Y…TV-MA and D/L/S/(F)V
  combinations as CTA-766-D) and should repeat at least every 3 s (E.7.3, p.86–87). Network
  name and call letters are 2–5 s packets; Time of Day is low priority and "should only be
  generated when a program is ... going directly to the intended audience" (§9.6.2.10, p.57;
  E.7.3, p.86–87).
- **Is XDS worth emitting? No.** Digital receivers "shall ignore any analog content advisory
  data transmitted via digitally-carried" 608 when the program has an EIT (CTA-766-D §4, p.5),
  and the 608 stream inside DTV exists to feed NTSC outputs of DTV receivers (CTA-708-E §4.3.2,
  p.12). Our TVs take ratings from the content_advisory_descriptor, time from the STT and
  names from the VCT. [Rec] Generate CC1 (and CC3 only for a second language); generate no
  XDS; if a producer's source already carries 608 with XDS, pass it through unchanged, as
  CTA-708-E §4.3.2.1 (p.13) and CTA-608-E D.1 (p.78) recommend.

### 6.4 caption_service_descriptor

Syntax and placement in §3.7: PMT video ES loop and EIT-0 (and all EITs for events with
captions). number_of_services counts both 608 and 708 entries (A/65 §6.9.2, p.69).

---

## 7. AFD and bar data

MPEG-2 (A/53-4 §6.2.3.2, §6.2.4, p.13–17): AFD is user data with identifier **0x44544731
'DTG1'** → afd_data(): `0` (1), active_format_flag (1), reserved `000001` (6); if flag:
reserved `1111` (4), active_format (4). Bar data is GA94 type 0x06 → bar_data(): top, bottom,
left, right flags (1 each), reserved `1111`; for each set flag `11` + 14-bit line/pixel number.
Bars come in top/bottom **or** left/right pairs. After a sequence header, AFD/bar data must
appear before the next picture data and persist until replaced. AFD "should" be present
whenever the active area is not the full frame.

H.264 (SCTE 128-1 §8.2.3–8.2.6, p.19–23): same payloads in T.35 SEI (0xB5/0x0031) with
user_identifier 'DTG1' for AFD and 'GA94' + 0x06 for bar data; persists until the next SRAP or
a new value; missing AFD means 1000.

Common AFD codes (A/53-4 Table 6.14, p.17; SCTE 128-1 Table 19, p.22): 1000 = full frame
(same aspect as coded frame); 1001 = 4:3 content (pillarboxed in a 16:9 frame); 1010 = 16:9
content (letterboxed in a 4:3 frame); 1011 = 14:9; 1101–1111 = full with alternative center
crops; 0100 = wider than 16:9 (needs bar data). AFD takes precedence over bar data except for
0000 and 0100 (§6.2.5, p.17).

[Rec] Encode at the source's aspect ratio, send AFD 1000 always, and add AFD 1001 / 1010 only
when we pillarbox or letterbox a source ourselves. Low priority.

---

---

## 8. QAM physical layer (SCTE 07)

SCTE 07 Table 3 (§6.1, p.22) and Annex A (p.24–25):

| Parameter | 64-QAM (SCTE mode 1) | 256-QAM (SCTE mode 2) |
|---|---|---|
| Symbol rate | 5.056941 Msps ± 5 ppm | **5.360537 Msps ± 5 ppm** |
| Information (TS) bit rate | 26.97035 Mbps ± 5 ppm | **38.81070 Mbps ± 5 ppm** |
| Roll-off | ≈0.18 | ≈0.12 |
| RS code | (128,122) over GF(128), 7-bit symbols | same |
| FEC frame | 60 RS blocks + 42-bit sync trailer | 88 RS blocks + 40-bit sync trailer |
| Trellis rate | 14/15 (0.93333) | 19/20 (0.95) |
| Concatenated FEC rate | 0.8888889 | 0.9050097 |
| Channel bit rate | 30.34165 Mbps | 42.88430 Mbps |
| Interleaver | I=128, J=1 (level 1) or variable (level 2) | variable (level 2): I=128/64/32/16/8, J=1/2/3/4/8/16 (Table 4) |

Exact values from Annex A's formula, `RI = Rs · q · (122/128) · (L·896/(L·896+s)) · ((5q−2)/5q)`
[I, computed]:

| | 64-QAM | 256-QAM |
|---|---|---|
| RI (bit/s) | 26 970 352 (exactly, = Rs·6·8/9) | 38 810 701.02 |
| TS packets/s | 17 932.41 | 25 804.99 |
| ±5 ppm in bit/s | ±135 | ±194 |

SCTE 54 §11 (p.25) restates T64 = 26.97035 Mbps and T256 = 38.81070 Mbps as the constant TS
output rate, locked in frequency to the symbol rate.

On the wire the TS sync byte is replaced by a parity checksum over the preceding 187 bytes and
restored by the receiver (SCTE 07 §5.1, p.7); the FEC frame is not aligned to TS packets
(§5.3, p.15). Neither affects what we send.

**What this means for our CBR rate** [I]:
- The modulator owns the 38.81070 Mbps clock, locked to its symbol rate. Our server's clock is
  not locked to it. The box "provides null packet insertion and deletion" and "PCR correction"
  (manual p.7): it absorbs the difference by adding or dropping nulls and re-stamping PCRs.
- So our stream does **not** need to hit 38 810 701 bit/s exactly. It must stay **below** it
  at every instant (after the box's input buffer smooths jitter), or the box has no nulls to
  delete and overflows ("pixelation", manual p.17).
- [Rec] Nominal TS rate **38.80 Mbps** (the box's documented ceiling, manual p.10), ≈276 ppm
  below RI. This covers a free-running host clock error of ±100 ppm [K] with margin. Put
  ≥2–3 % of that as null packets so the box always has nulls to delete, and pace datagrams
  evenly (7 packets per UDP datagram ≈ every 271 µs at 38.8 Mbps [K: 7×188 = 1316-byte payload
  convention]).
- Our PCRs must be correct for *our* rate (PCR_AC ±500 ns, §1.2); the box then corrects them
  for its own null insertion.
- 64-QAM carriers, if ever used: same reasoning, nominal ≤26.95 Mbps [Rec].

---

## 9. EAS (SCTE 18 / J-STD-042-C)

### 9.1 cable_emergency_alert() — SCTE 18 §5 Table 1, p.9–10

Long-form private section on **PID 0x1FFB**, table_id **0xD8**, total ≤4096 bytes
(section_length ≤4093) (p.7–8, p.11). Out-of-band PID 0x1FFC is for CableCARD devices only (§5,
p.7).

| Field | Bits | Semantics |
|---|---|---|
| table_ID | 8 | 0xD8 |
| section_syntax_indicator | 1 | 1 |
| zero | 1 | 0 (note: not 1 as in PSIP) |
| reserved | 2 | 11 |
| section_length | 12 | ≤4093 |
| table_id_extension | 16 | 0x0000 |
| reserved | 2 | 11 |
| sequence_number | 5 | +1 mod 32 on any content change; receivers drop repeats with the same value |
| current_next_indicator | 1 | 1 |
| section_number / last_section_number | 8 / 8 | 0 / 0 |
| protocol_version | 8 | 0 (others discarded) |
| EAS_event_ID | 16 | new value whenever any later field changes except alert_message_time_remaining |
| EAS_originator_code | 24 | ASCII "PEP", "WXR", "CIV", "EAS" (Table 2, p.12) |
| EAS_event_code_length | 8 | |
| EAS_event_code | 8·n | ASCII, e.g. "RWT", "RMT", "EAN", "TOR" (Table 3, p.13) |
| nature_of_activation_text_length | 8 | 0 = absent |
| nature_of_activation_text | var | MSS |
| alert_message_time_remaining | 8 | 0–120 s from the last CRC bit; **0 = indefinite** |
| event_start_time | 32 | GPS seconds; 0 = now (receivers act on arrival regardless) |
| event_duration | 16 | minutes; 0 unknown, else 15–6000 |
| reserved | 12 | |
| alert_priority | 4 | Table 4, p.15 (below) |
| details_OOB_source_ID | 16 | 0 (OOB only) |
| reserved | 6 | |
| details_major_channel_number | 10 | in-band details channel; 0/0 = none |
| reserved | 6 | |
| details_minor_channel_number | 10 | |
| audio_OOB_source_ID | 16 | 0 |
| alert_text_length | 16 | 0 = none |
| alert_text | var | MSS; scrolled across the top of the screen |
| location_code_count | 8 | 1–31 |
| per location: state_code (8, 0 = all), county_subdivision (4, 0 = all), reserved (2), county_code (10, 0 = whole state) | 24 | p.16–17 |
| exception_count | 8 | |
| per exception: in_band_reference (1), reserved (7), then either reserved(6)+major(10)+reserved(6)+minor(10) or reserved(16)+OOB source_ID(16) | 40 | p.17–18 |
| reserved | 6 | |
| descriptors_length | 10 | |
| descriptors | var | 0x00 in-band details channel (RF channel 8 + program_number 16); 0x01 in-band exception channels (Table 6–8, p.19) |
| CRC_32 | 32 | |

### 9.2 Receiver behavior

| alert_priority | Meaning (Table 4, p.15) | Required action (§7.5, p.28–30) |
|---|---|---|
| 0 | Test | Discard (after sequence_number processing). Also used to **prime** sequence_number: a generator that does not know its last value shall first send a priority-0 message (p.11) |
| 3 (1–3) | Low | Text or audio, unless the current program has a CA_descriptor (ours never do) |
| 7 (4–7) | Medium | Text or audio, unless on PPV/VOD |
| 11 (8–11) | High | Text or audio, unconditionally |
| 15 (12–15) | Maximum | Must present alert audio; with no OOB audio, **"that details channel shall be acquired"** (§7.6 #29, p.30). For in-band alerts, a valid details major/minor is mandatory at 12–15 (§6 #4, p.25) |

- **Force-tune**: the TV resolves details_major/minor against the in-band VCT and tunes to it
  within 1000 ms of having the complete CVCT (§7.1 #3, p.26). Some TVs need that VCT entry **in
  the same TS as the alert** (note, p.16). The details channel may be hidden (#33, p.31).
  Channel locks and skips do not block it, content advisory blocking can (#32; §6 note, p.26).
- **Return**: the TV stays until time_remaining expires (0 = indefinitely, #31), then "should"
  restore the previous service (p.14). A later message with the same EAS_event_ID updates only
  time_remaining (#14, #16).
- **Exceptions**: if the currently tuned virtual channel matches an exception entry, the TV
  discards the alert (#23, p.28).
- **Per-carrier state**: sequence_number and event-ID memory reset on power-up and on every
  physical channel change (§7.1 #5, p.26; Annex A note 3). A viewer who changes carrier during
  a repeating priority-15 alert is force-tuned again.
- **Must be on every carrier**: "shall be included in-band in any Transport Stream containing
  unscrambled services unless the particular alert would exclude all unscrambled services on
  this Transport Stream" (§6 #6, p.25).
- **Location filtering** is optional and only when the TV knows its location (§7.8 #40, p.31);
  state 0 / county 0 / subdivision 0 reaches everyone (p.17).
- **Repetition**: informative — repeat for ~5 s, optionally for minutes for late joiners;
  identical repeats keep the same sequence_number (§8.2, p.32). Shares the base_PID 250 kbit/s,
  1024-byte smoothing buffer with MGT/CVCT/STT (SCTE 54 Table 2).
- Who must comply: retail digital TV receivers in scope (§1.1, p.4). Legal hook: a TV may be
  labeled "digital cable ready" only if it "responds to emergency alerts that are transmitted
  in compliance with ANSI/SCTE 54 2003" (47 CFR §15.123(b)(5), eCFR 2026-11-23). §76.640 (cable
  operators) does not mention emergency alerts; its (b)(1)(iv) requires in-band PSIP for clear
  transport streams "when available from the content provider": EIT-0 to EIT-3 (12 hours),
  one- or two-part channel number and a name, PSIP bandwidth may be capped at 80 kbit/s
  (27 Mbit/s mux) or 115 kbit/s (38.8 Mbit/s mux).

### 9.3 Could we emit it ourselves?

Yes, technically: it is one more section on PID 0x1FFB, which the box passes through in MPTS
mode (the manual says MPTS mode passes stream information through, Appendix C p.45; [I] not yet
verified for table 0xD8). Minimum per alert: a primer (priority 0) at startup per carrier, then
a ~47-byte section plus text on all 16 carriers, repeated for ~5 s; for priority 12–15 a
details channel listed in **every** carrier's CVCT (ideally a program present in every
multiplex, so the force-tune never changes RF).

Risks: time_remaining = 0 strands TVs (never send 0); repeated re-capture of channel surfers;
inconsistent TV support [K — many clear-QAM sets implement only text, or nothing]; one-part
details numbers not understood by older receivers (p.16 note).

Comparison with the box's EAS switch (program substitution, manual p.7, p.28–29): substitution
works on every TV and cannot strand one, but offers no text-only crawl, no exceptions and no
priority levels. SCTE 18 §1.1 (p.4) explicitly allows substitution as a compliant (if
"disruptive") method. [Rec] Use substitution (the box's, or our own program switching in the
mux) for must-reach alerts; consider SCTE 18 priority ≤11 text crawls as an optional extra
after testing on real TVs; if a priority-15 CEAM is ever sent, point details_major/minor at the
substituted program so both paths agree (#20).

---

## 10. Checklists

### 10.1 Minimum viable PSIP — scan, numbered and named channels, correct clock

Per carrier (all cited above):

1. **PAT** on PID 0, ≤ 100 ms; transport_stream_id unique across all 16 carriers; six
   program_numbers, no program 0 (§1.3, §2.7).
2. **PMT** per program on its own PID in 0x0030–0x1FEF, ≤ 400 ms; PCR_PID; video ES with
   data_stream_alignment_descriptor; each AC-3 ES with AC-3_audio_stream_descriptor carrying
   bsmod and language (§4.2).
3. **MGT** on 0x1FFB, ≤ 150 ms, starting a packet with pointer_field 0, listing the CVCT
   (table_type 0x0002, PID 0x1FFB, version, number_bytes) (§3.2).
4. **CVCT** on 0x1FFB, ≤ 400 ms: one entry per program with short_name, two-part
   major.minor unique system-wide, modulation_mode 0x03, carrier_frequency 0,
   channel_TSID = this TS's TSID, program_number, ETM_location 0, access_controlled 0,
   hidden 0, path_select 0, out_of_band 0, hide_guide 0, service_type 0x02, source_id (unique,
   stable) (§3.3).
5. **STT** on 0x1FFB, every 1 s [Rec] (≤ 10 s required), correct GPS time, GPS_UTC_offset,
   daylight_saving (§3.1).

That is the SCTE 54 minimum (MGT, STT, CVCT). Optional but cheap: extended_channel_name and
service_location descriptors in the CVCT.

### 10.2 Full set — EPG, ratings, captions, multi-audio

Everything in 10.1, plus:

6. MGT also lists EIT-0..EIT-N (0x0100+k), event ETT-0..N (0x0200+k) and the channel ETT
   (0x0004), each with its PID, version and number_bytes (§3.2).
7. **EIT-0..EIT-7** (24 h) at least EIT-0..3: an instance per source_id per window, empty
   instances where there are no events, 3-hour UTC windows, events in order without overlap,
   cross-window events repeated with the same event_id; EIT-0 ≤ 500 ms (§3.5).
8. Event descriptors: AC-3 audio descriptor per audio track (with language),
   caption_service_descriptor, content_advisory_descriptor, optional genre_descriptor (§3.5,
   §3.7).
9. **ETT** for event descriptions (ETM_location 1 in the EIT) and channel descriptions (ETM
   location 1 in the CVCT), ETM_id per Table 6.14 (§3.6).
10. **Ratings**: content_advisory_descriptor in the PMT program loop (current program) and in
    EIT-0's current event, region 0x01; no RRT for the US (§3.4, §3.7).
11. **Captions**: CEA-708 + 608 in picture user data / SEI at 9600 bit/s;
    caption_service_descriptor in the PMT video loop and the EIT (§6).
12. **Multi-audio**: up to 3 AC-3 ESs; distinct languages or bsmod; ISO_639 descriptors when
    two share a bsmod; component_name when two share language and bsmod (§4.3).
13. Loudness −24 LKFS with dialnorm 24 on every track (§5.4).
14. Optional: AFD (§7); extended_channel_name; genre.

### 10.3 Known real-world receiver quirks (with sources)

| Quirk | Source | Design response |
|---|---|---|
| Some deployed receivers (possibly in the US) misbehave with a non-zero ETT_table_id_extension | A/65 §6.6 footnote 13, p.45 | Open question; test both |
| Receivers built to the original J-STD-042 understand only two-part channel numbers | SCTE 18 notes, p.16 and p.18 | Use two-part numbers |
| Some cable-ready devices need the VCT entry for an EAS details channel in the same TS as the alert | SCTE 18 note, p.16 | If SCTE 18 is ever used, list the details channel on every carrier |
| Many deployed set-top boxes ignore the AC-3 audio descriptor and choose audio by ISO_639 descriptor; VI tracks often labeled "spa" | SCTE 54 §8.1, p.23 | Always send ISO_639 descriptors (§4.3) |
| AVC signaled as service_type 0x02 is what most smart TVs are reported to receive; strictly compliant alternatives may fail on legacy sets | A/53-3 Amend. 1 §1.2–1.3, p.3 | service_type 0x02 for H.264 |
| Receivers may meet a TVCT on cable and "should" handle it | A/65 Annex G.5.1, p.138 | CVCT is primary; TVCT is a fallback experiment |
| Receivers identify a TS by its PAT TSID and remember the frequency; duplicates break PSIP tuning | A/65 Annex G.5.2, p.138–139 | Unique TSID per carrier, stable forever |
| Receivers may cache PIDs per channel and only then check the PMT | A/65 Annex G.5.3, p.139 | Keep PIDs stable per service |
| One TV brand mis-resolved clear-QAM channels when PSIP was present on other (encrypted) streams; removing that PSIP helped | TV Technology, 2009-07-09, https://www.tvtechnology.com/news/update-on-limited-vizio-clearqam-channel-choices | Each carrier's CVCT describes only itself |
| Clear-QAM TVs showed numbers not matching broadcaster PSIP and no guide when cable PSIP was inconsistent | TV Technology, D. Lung, 2008-12-05, https://tvtechnology.com/news/cable-psip-part-2 | Consistent CVCT/EIT across carriers |
| Receivers are expected to re-scan in standby to refresh VCT and guide | A/65 Annex G.5.2 footnote 23, p.138 | Changes to the lineup may take a rescan to appear [I] |

---

## 11. Conformance self-tests

Basis: ETSI TR 101 290 V1.4.1 §5 (p.21–37). It has **no ATSC/PSIP annex**; its §5.3.5 / Table
5.1b (p.36–37) "reduced SI" profile (all priority 1 and 2 tests, CRC on PAT/PMT, plus 3.3, 3.4,
3.4.a, 3.9, 3.10) is the closest model for a non-DVB stream. "If indicator 1.1 is activated
then all other indicators are invalid" (§5.2.0, p.21). Run every test on our output before the
UDP sender, and again on HDHomeRun captures after the modulator.

### 11.1 TR 101 290 items that apply

| # | Indicator | Threshold (TR) | Our test (threshold to use) |
|---|---|---|---|
| 1.1 | TS_sync_loss | 5 good syncs to lock, 2 bad to lose (p.22) | every 188th byte is 0x47 |
| 1.2 | Sync_byte_error | sync ≠ 0x47 | same |
| 1.3a | PAT_error_2 | table_id 0x00 at least every 0.5 s on PID 0; only 0x00 on PID 0; not scrambled (p.22) | **≤ 100 ms** (SCTE 54) |
| 1.4 | Continuity_count_error | wrong order, packet more than twice, lost packet (p.22–23) | per PID except 0x1FFF; skip AFC 10 |
| 1.5a | PMT_error_2 | table_id 0x02 at least every 0.5 s on each PMT PID (p.22) | **≤ 400 ms** (SCTE 54) |
| 1.6 | PID_error | referenced PID absent > user period, ≤ 5 s for audio/video (p.22) | every PMT ES PID and MGT PID present within 1 s |
| 2.1 | Transport_error | TEI = 1 (p.23) | never set |
| 2.2 | CRC_error | CRC error in a table (p.24) | CRC of every PAT, PMT, MGT, CVCT, STT, RRT, EIT, ETT, EAS section |
| 2.3a | PCR_repetition_error | > 100 ms between PCRs (p.24) | ≤ 100 ms, target ≤ 40 ms |
| 2.3b | PCR_discontinuity_indicator_error | PCR delta outside 0–100 ms without DI (p.24) | never |
| 2.4 | PCR_accuracy_error | ±500 ns; CBR only (p.24) | |PCR − expected from byte index at R_nom| ≤ 500 ns (target ≤ 1 tick) |
| 2.5 | PTS_error | PTS interval > 700 ms (p.24) | ≤ 700 ms per PES PID; we expect PTS on every PES |
| 2.6 | CAT_error | scrambled packets without CAT (p.24) | assert no scrambling; PID 1 absent |
| 3.3 | Buffer_error | T-STD over/underflow (p.25) | run a T-STD model per program (§1.6) |
| 3.4a | Unreferenced_PID | PID not referenced within 0.5 s (p.25) | every PID ∈ {0, PMTs, ES PIDs, 0x1FFB, MGT PIDs, 0x1FFF} |
| 3.9 | Empty_buffer_error | TBn/TBsys not empty once per second (p.26) | T-STD model |
| 3.10 | Data_delay_error | delay > 1 s (stills 60 s) (p.26) | ≤ 1 s MPEG-2/audio, ≤ 10 s H.264 |
| — | TSID check | PAT TSID equals configured value (§5.3.4.1, p.36) | PAT TSID = CVCT transport_stream_id = configured, unique per carrier |
| — | "Mandatory descriptors present and tables consistent" (p.28) | — | PSIP cross-checks below |

DVB-only indicators (3.1 NIT, 3.2 DVB SI repetition, 3.5 SDT, 3.6 EIT, 3.7 RST, 3.8 TDT) don't
apply; [Rec] assert PIDs 0x0010–0x0014 are unused instead.

PCR clock measurements (TR §5.3.2, p.30–33): PCR_FO ≤ ±30 ppm (±810 Hz); PCR_DR ≤ ±75 mHz/s
(±10 ppm/hour); PCR_AC ±500 ns; PCR_OJ reported with its measurement filter profile (MGF1
10 mHz … MGF4; 2 mHz for the 13818-9 ±25 µs jitter limit) (Table 5.1, p.31–32). Measure FO/DR
against the host's disciplined clock, not the TS.

TS rate: measure with the "MGB2" profile (100 ms slices, 1 s window), recommended for CBR (TR
§5.3.3, Table 5.1a, p.35; Annex J.3.2, p.165). Assert the full TS rate equals our nominal
within ±1 packet per window, and report null share.

### 11.2 PSIP self-tests

| Test | Pass condition | Source |
|---|---|---|
| MGT cycle | ≤ 150 ms; MGT section starts a packet (PUSI 1, pointer 0) | SCTE 54 Table 1; A/65 §6.2 |
| CVCT cycle | ≤ 400 ms (all sections) | SCTE 54 Table 1 |
| STT cycle | ≤ 10 s required, ≤ 1 s [Rec] | SCTE 54 Table 1; A/65 Table 7.1 |
| STT accuracy | system_time within ±1 s of GPS time at last CRC byte | A/65 §6.1 |
| EIT-0 cycle | ≤ 500 ms (recommended) | SCTE 54 §7.8.1.2 |
| PSIP PID rates | each of 0x1FFB, every EIT PID, every ETT PID ≤ 250 kbit/s; 1024-byte smoothing buffer never overflows at 250 kbit/s leak | SCTE 54 Table 2 |
| PSIP packets | scrambling 00, AFC 01 | A/65 §6.x |
| Section sizes | STT/CVCT/RRT/PAT/PMT ≤ 1024 bytes, others ≤ 4096 | §3 table |
| protocol_version | 0 everywhere | A/65 §4.1 |
| MGT consistency | every table_type's PID carries that table; table_type_version_number equals the version in the carried table; number_bytes equals the bytes actually sent; PIDs unique | A/65 §6.2, §6.5, §6.6 |
| CVCT ↔ PAT/PMT | every PAT program has a CVCT entry; channel_TSID = PAT TSID; program_number matches; service_location (if sent) equals the PMT; no duplicate major.minor across the system | A/65 §6.3, §6.9.5 |
| EIT structure | windows on 3-hour UTC boundaries; instance per source_id per window; events sorted and non-overlapping; spanning events share event_id; ETM_location 1 ⇒ matching ETM exists | A/65 §5, §6.5 |
| EIT ↔ PMT | current event's caption_service and content_advisory descriptors equal the PMT's | SCTE 54 §7.9.3.10–11 |
| Versions | change ⇒ +1 mod 32; no change ⇒ unchanged; persisted across restarts | A/65 §4.1 |
| Text | all MSS parse; short_name ≤ 7 UTF-16 units; rating text ≤ 16 chars | A/65 §6.3.1, §6.9.3 |
| PID plan | PMT/ES PIDs in 0x0030–0x1FEF; 0x1FFE unused | SCTE 54 §7.9.4; SCTE 54 2009 Table 5.7 |
| Audio | AC-3 descriptor on every 0x81 ES; bsmod matches bitstream; ≥ 1 CM; ≤ 1 primary | SCTE 54 §7.9.3.1, §7.9.3.6 |
| Video | data_stream_alignment_descriptor on every video ES; H.264 SRAP ≤ 1 s with RAI and ES-priority flags; MPEG-2 sequence header ≤ 1 s [Rec] | A/53-3 §5.4.1; SCTE 128-2 §6.4 |

---

## Open questions / ambiguities

1. **ETT_table_id_extension.** A/65 §6.6 (p.45) requires a unique value per ETT instance on a
   PID; footnote 13 says non-zero values break some deployed receivers. With all zeros, a
   receiver that filters sections by (table_id, extension, version, section_number) would see
   every ETM on a PID as the same section. Test both on our TVs and the HDHomeRun.
2. **Minimum MGT.** tables_defined is "2–370 for cable" (A/65 §6.2, p.27), but a minimal cable
   multiplex has only the CVCT to list (the STT is never listed). Does a CVCT-only MGT violate
   the range, and do TVs care? Listing EIT-0 avoids the question.
3. **One-part channel number formula** precedence (§2.6). The intent is
   `((major & 0xF) << 10) + minor`; confirm with a TV before using one-part numbers at all.
4. **Described video labeling** (§4.3): A/53-3 (audio_type 0x00, rely on bsmod) conflicts
   with SCTE 54 (meaningful audio_type; legacy "spa" convention). What real TVs show for each
   variant is unknown.
5. **caption_service_descriptor trailing bits**: A/65:2013 has 14 reserved bits after
   wide_aspect_ratio (Table 6.26, p.69). Earlier A/65 editions defined the first of them as
   korean_code [K]; setting reserved bits to 1 per §4.2 would read as korean_code = 1 to an
   old parser. Decide 0x3FFF vs 0x1FFF after testing; flag as a risk.
6. **service_location_descriptor in the CVCT**: optional on cable (Annex G.5.3) but Table 6.25b
   marks it "M" for the CVCT. Send it — harmless if correct.
7. **H.222.0 PMT "never longer than 1016"** (p.50–51) vs section_length ≤ 1021; unexplained.
   Keep PMTs short (ours are ~100 bytes).
8. **STT cadence**: cable allows 10 s, terrestrial 1 s. TV behavior with sparse STT untested;
   send 1 s.
9. **How many EITs real TVs use**, and whether any need EIT on cable at all to show a guide.
   The friend's unit shows EPG working (design-inputs.md) — ask what was sent.
10. **Does the EQAM pass every PID untouched in MPTS mode** — EIT/ETT PIDs, table 0xD8 on
    0x1FFB, private sections, adaptation-field flags (RAI, ES priority)? "PCR correction" means
    it rewrites some packets; confirm it does not also rewrite PSIP or CC.
11. **The box's input buffer and null handling**: how far below 38.81070 Mbps it wants us, and
    how much jitter it absorbs. Measure with the HDHomeRun (PCR jitter, null share at output).
12. **MPEG-2 GOP/sequence-header interval**: no normative cable number. SCTE 43 2015 (R2021),
    now held, has no GOP or sequence-header interval rule either.
13. **A/72 Table 7.1 frame-rate legend** differs from SCTE 128-1's; follow SCTE 128-1 for cable.
14. **SCTE 54's informative claim** that AD is "often signaled as spa" describes set-top boxes,
    not retail TVs; our audience is TVs.
15. **service_type for audio-only channels**: 0x03 is defined (A/53-3 §5.1.3.2), but whether TVs
    list a 0x03 channel or need a video PID is untested; a still slate with service_type 0x02
    is the safe default (design-inputs.md).

## Documents still missing

Updated 2026-11-24 after the second acquisition pass and the owner's CTA downloads. CTA lists
608, 708, 766 and 542 at $0 ("Available to Everyone") on shop.cta.tech, delivered through cart
and checkout with a free account. Seattle Public Library has no standards database (no IEEE
Xplore, Techstreet, Knovel or similar); its nearest resource is "O'Reilly Complete Public
Library" (spl.org > Online Resources > A-Z, library card login).

| Document | Why we want it | Status / how to get it |
|---|---|---|
| CTA-708-E erratum (2023) | Corrections to the held 708-E | The held CDN copy lacks the erratum the shop says is appended; the $0 checkout at https://shop.cta.tech/products/cta-708 should give the version with it. Low priority. |
| ITU-T H.262 (02/2012), in-force edition | Current text; we hold the free 02/2000 edition | Not a free download from ITU. The 2000 edition's level tables (§8, Tables 8-11 to 8-14) cover what §1.6 needs. Low priority. |
| SCTE 35 | Only if we ever take ad-insertion cues from producers | Free from scte.org (needs a browser, as SCTE 43 did) |
| 47 CFR §76.607 (CALM Act) and §79.100 IBR list | Confirm the §5.4 rule number; which 608/708 editions the FCC incorporates | Free (eCFR API) |

Held CTA documents (docs/external/standards/2026-11-24-cta/; the PDFs' running headers use the older
CEA- names):

| Document | Edition | File |
|---|---|---|
| CTA-608-E | S-2019, 152 p. (owner download) | CTA-608-E-S-2019.pdf |
| CTA-708-E | S-2023 (March 2023), 116 p., no erratum | CTA-708-E-S-2023.pdf |
| CTA-766-D | R-2019, 24 p. (owner download) | CTA-766-D-R-2019.pdf |

Other documents obtained in the second pass (see each folder's SOURCES.txt):

| Document | Edition | Path |
|---|---|---|
| CTA-542-D (formerly CEA-542-D) | June 2013, 18 p. (Public.Resource.Org IBR copy) | docs/external/standards/2026-11-24-archive-org/CTA-542-D-2013.pdf |
| ANSI/SCTE 43 | 2015 (R2021), 9 p. | docs/external/standards/2026-11-24-scte-43/ |
| ITU-T H.262 | 02/2000, 220 p. | docs/external/standards/2026-11-24-itu-atsc-2/ |
| ITU-T H.264 | V16 (06/2026), 854 p. | docs/external/standards/2026-11-24-itu-atsc-2/ |
| ATSC A/54A | 4 Dec 2003 with Corrigendum 1 (2006), 110 p. | docs/external/standards/2026-11-24-itu-atsc-2/ |
| ATSC IS-191 | 26 June 2003, 4 p. (Wayback copy of ATSC's URL) | docs/external/standards/2026-11-24-itu-atsc-2/ |
| ATSC A/71 | A/71:2012, 15 p. | docs/external/standards/2026-11-24-itu-atsc-2/ |
| 47 CFR §15.120, §15.123, §76.640, §79.102 | eCFR, current to 2026-11-23 | docs/external/standards/2026-11-24-ecfr/ |
| 47 CFR §79.101 (former §15.119, line-21 decoders) | last text, 2025-10-02 (now [Reserved]) | docs/external/standards/2026-11-24-ecfr/ |

§15.119 and §15.122 no longer exist: they were [Reserved] from 2016-12-15 (their text moved to
§79.101 and §79.102) and were removed on 2026-03-27. §15.120 cites EIA/CEA-766-A and §79.102
cites EIA-708-B.

Not yet digested: CTA-542-D (cable channel plan), A/54A and A/71 are held but this digest does
not cite them yet; SCTE 43 is only summarized in §5.1.

## Claims resting on [K]

- GPS_UTC_offset is currently 18 s; US DST dates (§3.1).
- Older A/53 "program paradigm" PID derivation (§4.5).
- Host clock error of ±100 ppm and the 7-packets-per-datagram convention (§8).
- Receivers commonly expecting a 1 s STT and Latin-1 being the safest MSS mode (§2.3, §3.8).
- Old korean_code bit in the caption_service_descriptor (Open question 5).
- SCTE 35 stream_type 0x86 (§4.1).
- ffmpeg's `a53_cc` side data handling caption reordering (§6.3).
- CALM Act rule number, §76.607 (§5.4).
- CRC-32/MPEG-2 as the common name of the H.222.0 CRC (§1.3).
- Inconsistent SCTE 18 support among clear-QAM TVs (§9.3).

Resolved since the first digest: cc_data() layout and cc_count (§6.3, CTA-708-E); US region 1
dimension/value table and descriptor rules (§3.4, CTA-766-D); 608 carriage, doubling and XDS
(§6.3.1, CTA-608-E); IS-191 lip-sync figures (§5.5); SCTE 128-1 Table 10 units (§5.2, H.264
Tables A-1/A-2); FCC rule numbers for SCTE 18 (§9.2, 47 CFR §15.123(b)(5)).
