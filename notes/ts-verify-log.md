# tb-ts-verify: validation log

The transport-stream checker `crates/tb-ts-verify` (README there; rule catalog in
`notes/ts-conformance-rules.md`) run on streams from muxers that share nothing with it, on
deliberately damaged copies, and compared with TSDuck where TSDuck measures the same thing.
Written 2026-11-26, overnight, unattended; decisions taken without the owner are listed at the
end.

Tools: ffmpeg 6.1.6 (the host's), TSDuck 3.44-4676 (`nix build --inputs-from . nixpkgs#tsduck`),
the release build of tb-ts-verify, on bagel (Ryzen 7 5800X). Everything below can be redone
with the scripts in `crates/tb-ts-verify/tools/`:

    TSDUCK=.../bin tools/make_streams.sh DIR 20          # the streams (section 1)
    TSDUCK=.../bin tools/validate.sh DIR/atsc.ts 37000000 DIR/bad   # the damage table (3)
    TB_EXPORT_DIR=DIR cargo nextest run -p tb-ts-verify --run-ignored only export
                                                          # the synthetic streams (4)

## 1. Streams

| File | Made by | Content |
|---|---|---|
| `ff_cbr.ts` | ffmpeg `-muxrate 38800000 -pcr_period 30 -pat_period 0.1` | 2 programs, MPEG-2 720p59.94 8 Mbit/s + AC-3 192 kbit/s each, 20 s |
| `ff_vbr.ts` | ffmpeg without `-muxrate` | the same, variable rate |
| `ff_flags.ts` | ffmpeg `-muxrate 20M -mpegts_flags +resend_headers+pat_pmt_at_frames` | the same |
| `ff_system_b.ts` | ffmpeg `-mpegts_flags +system_b` (DVB carriage of AC-3) | the same |
| `ff_delay100ms.ts` | ffmpeg `-muxrate 20M -max_delay 100000` | the same, delivered at most 100 ms early |
| `atsc.ts` | ffmpeg at 37 Mbit/s, then TSDuck `filter` (drop SDT) and `inject` of MGT, CVCT, STT, EIT-0 (the spike of software-survey §2.6, to a file), then `tsmangle.py stt-advance` | an ATSC MPTS with PSIP |
| `ff_sd.ts` | ffmpeg 704x480 4 Mbit/s at 38.8 Mbit/s | one program, MPEG-2 Main@Main |
| `ff_avc.ts` | ffmpeg libx264 High@4.0 6 Mbit/s with access unit delimiters, at 38.8 Mbit/s | one program, AVC + AC-3 |
| `synthetic.ts` | the test builder of `tests/common` | 1 program, PSIP with EIT-0 and ETT-0, a conformant delivery schedule, 10 s at 38.8 Mbit/s |

TSDuck's `inject` repeats a fixed STT when run offline, so `tsmangle.py stt-advance` rewrites
each STT's system_time from its byte position (and its CRC).

## 2. Undamaged streams

| Stream | S1 | S2 | S3 | S4 | Rules reported |
|---|---|---|---|---|---|
| ff_cbr | 0 | 555 | 2567 | 992 | PAT-INTERVAL, PMT-DSA, PMT-AUDIO-DESC, PES-VIDEO-ALIGN, PSIP-REQUIRED, TSTD-TB-OVERFLOW (audio), TSTD-MB-OVERFLOW (video), TSTD-B-OVERFLOW; notes DESC-GA94, TS-DVB-PID, PCR-INTERVAL-TARGET |
| ff_vbr | 0 | 456 | 2432 | 1072 | as ff_cbr, plus TSTD-TBSYS-OVERFLOW, TSTD-MB-EMPTY; notes TS-VBR, PCR-INTERVAL-40 (PCRs 87 ms apart) |
| ff_flags | 0 | 517 | 2407 | 3 | as ff_cbr, plus TSTD-TBSYS-OVERFLOW (84) and TSTD-BSYS-OVERFLOW: PAT and PMT before every frame |
| ff_system_b | 0 | 17 | 2565 | 5 | **SECT-VERSION-STALE** (2), the video findings; no AC-3 rules (the audio is stream_type 0x06, not ATSC) |
| ff_delay100ms | 0 | 693 | 2565 | 3 | as ff_cbr, plus TSTD-EB-UNDERFLOW (42) and TSTD-B-UNDERFLOW (11) |
| atsc | 0 | 545 | 2712 | 328 | ffmpeg's findings, plus MGT-INTERVAL, VCT-INTERVAL; notes STT-INTERVAL-1S, EIT0-INTERVAL |
| ff_sd | 0 | 838 | 795 | 581 | ffmpeg's PMT and PES findings, TSTD-TB-OVERFLOW (806, video: up to 22 kB in a 512-byte TBn), TSTD-MB-OVERFLOW, TSTD-MB-EMPTY, TSTD-TBSYS-OVERFLOW (30), TSTD-B-OVERFLOW |
| ff_avc | 0 | 1434 | 202 | 604 | ffmpeg's PMT findings (PMT-DSA applies to 0x1B too), TSTD-TB-OVERFLOW (1418, video), TSTD-TBSYS-OVERFLOW, TSTD-B-OVERFLOW |
| synthetic | 0 | 0 | 0 | 0 | none |

Each finding class was checked by hand or with TSDuck:

- **PAT-INTERVAL on every ffmpeg stream**: ffmpeg's `-pat_period 0.1` gives PATs 100.02 ms
  apart (one packet late). SCTE 54 §7.5 says "less than or equal to 100 milliseconds" between
  the last bytes, so these are violations, by microseconds. TSDuck's section log gives the
  same maxima (100.0 ms, table in section 3).
- **PMT-DSA, PMT-AUDIO-DESC, DESC-GA94**: ffmpeg writes no data_stream_alignment_descriptor,
  no AC-3_audio_stream_descriptor and no GA94 registration (as software-survey §2.6 found in
  `mpegtsenc.c`); TSDuck's tstables shows the same PMTs.
- **PES-VIDEO-ALIGN on every ffmpeg video PES**: the PES header's flag byte is 0x80, so
  data_alignment_indicator is 0 (A/53-3 §5.5.1 requires 1). Checked in the bytes.
- **SECT-VERSION-STALE in system_b mode**: TSDuck's section log of PID 0x0030 shows the PMT
  at packet 2 with a one-byte DVB AC-3 descriptor (0x6A) and from packet 1,756 on with a
  three-byte one, both version 0. ffmpeg changes the PMT without a new version_number: a real
  defect, which receivers that cache by version would miss.
- **MGT-INTERVAL and VCT-INTERVAL on atsc.ts**: MGT up to 164 ms apart (limit 150), CVCT up to
  433 ms (limit 400) with injection periods set at the limits; the survey measured 170 and 440
  ms on the same pipeline. TSDuck's section log agrees to 0.1 ms (section 3).
- **Buffer model on ffmpeg's output**: ffmpeg writes a whole PES (an AC-3 frame, or a video
  frame) back to back at the multiplex rate, and sends audio up to `max_delay` (0.7 s) early.
  For AC-3 that is 5 packets at 38.8 Mbit/s into a 512-byte TBn drained at 2 Mbit/s (peak
  2.3 kB), and 0.7 s x 192 kbit/s = 17 kB in a 2,592-byte Bn. For video at Main@High-1440
  (profile_and_level_indication 0x46, read from the stream) MBn is BSmux + BSoh = 40 kB with
  a leak of 1.05 x 8 Mbit/s, so an I-picture at the multiplex rate overflows it; at Main@Main
  (ff_sd) TBn drains at 18 Mbit/s and the 38.8 Mbit/s bursts overflow it. These are the
  failures SPEC §6 anticipates ("writes whole access units back to back"). TSDuck has no
  buffer model to compare with; section 4 checks the model on streams with a known answer.
- **AVC**: with no HRD parsing the model takes H.222.0 §2.14.3.1's defaults for High@4.0:
  Rx = 1.2 x 1500 x 20,000 = 36 Mbit/s, so frames sent back to back at 38.8 Mbit/s overflow
  TBn (peak 2.9 kB); MBn (16 kB) and EBn (3.75 MB) hold. The stream's own HRD says 6 Mbit/s,
  which would make Rx smaller still: the defaults are the lenient side.
- **Underflows with `-max_delay 100000`**: frames delivered less than their transfer time
  ahead of their decoding time; expected.
- **System buffers**: with `pat_pmt_at_frames` ffmpeg puts PAT and PMT before every video
  frame, close to the periodic ones. In ff_sd TSDuck's section log shows PAT and PMT at
  packets 36,211-36,212 and again at 36,221-36,222: four packets in 0.39 ms into TBsys
  (512 bytes, 1 Mbit/s) is 704 bytes, the overflow reported at packet 36,222.
- **PSIP-REQUIRED** on the ffmpeg-only streams: no MGT, VCT or STT, correct for cable.

Agreement with TSDuck on the undamaged atsc.ts:

| Measure | tb-ts-verify | TSDuck |
|---|---|---|
| Packets per PID (10 PIDs) | 242, 242, 242, 110,580, 2,709, 108,464, 2,709, 76, 203, 266,678 | identical (tsanalyze --json) |
| Rate from PCR | 36,999,999.997 bit/s (least squares) | 36,999,998 bit/s |
| PCRs per PID | 707 / 707 | 707 / 707 |
| PCRs off by more than 500 ns / 13 ticks | 0 (largest 21 ns) | 0 (pcrverify) |
| Continuity errors | 0 | 0 |
| Table cycles (count, largest interval) | PAT 242, 100.0 ms; PMTs 242, 100.0; MGT 133, 164.1; CVCT 50, 432.9; STT 20, 1023.5; EIT-0 38 and 37, 536.0 and 533.9 | the same, except EIT-0 source 2: 38 (TSDuck's `tables --packet-index` log, `tsduck_intervals.py`) |

The EIT-0 difference: the first section on PID 0x1D00 comes before the first MGT, and only the
MGT says that PID carries EITs; the checker starts there. On ff_vbr TSDuck's pcrverify reports
all 558 PCR intervals as jitter; the checker detects the variable rate (TS-VBR), does not
measure PCR accuracy, as TR 101 290 2.4 says, and says so.

## 3. Damaged streams

`validate.sh atsc.ts 37000000`: each row is atsc.ts with one kind of damage
(`tsmangle.py`), the counts of the relevant rules, and TSDuck's nearest measure.

| Damage | tb-ts-verify | TSDuck |
|---|---|---|
| none | CC-ERROR 0, SECT-CRC 0, PCR-ACCURACY 0, PCR-DISCONTINUITY 0, TS-TEI 0, TS-SYNC-BYTE 0 | continuity 0; pcrverify (jitter > 13 ticks) 0 |
| 3 video packets replaced by nulls | CC-ERROR 1 | continuity 1 ("missing 3 packets") |
| CC + 5 in one audio packet | CC-ERROR 2 (into and out of it) | continuity 2 |
| a bit flipped in one PAT section | SECT-CRC 1 | 241 PAT sections decoded instead of 242, longest gap 200.1 ms |
| no PAT from 5.0 to 5.6 s (nulls) | PAT-INTERVAL-TR 1, CC-ERROR 1 | longest PAT gap 699.7 ms; continuity 1 |
| PCR +-2 us on every 10th PCR | PCR-ACCURACY 71, PCR-ACCURACY-NOMINAL 71 | pcrverify 141 (two intervals per moved PCR) |
| PCR +-300 ns on every 10th PCR | none | pcrverify 0 |
| PCR +200 ms from the 300th on (no discontinuity_indicator) | PCR-DISCONTINUITY 1 | pcrverify 1 (tsanalyze's leap counters stay 0) |
| transport_error_indicator in one packet | TS-TEI 1 | tsanalyze: 1 with transport error |
| 3 consecutive sync bytes corrupted | TS-SYNC-BYTE 2, TS-SYNC-LOSS 1 | tsanalyze: 3 with invalid sync |
| 10 video packets scrambled (no CAT) | CAT-ERROR 1 | tsanalyze: 1 scrambled PID |

Differences, all by design:

- Sync: TR 101 290 1.1 loses sync after two bad sync bytes; the third packet is then part of
  the hunt for five good ones, not a sync_byte_error. TSDuck counts packets with a bad first
  byte. After a loss the checker restarts continuity and the table clocks ("if indicator 1.1
  is activated then all other indicators are invalid", TR 101 290 §5.2.0); TSDuck reported no
  continuity error either.
- PCR jitter: TSDuck compares each PCR interval with the bytes between; TR 101 290's PCR_AC
  (and the checker) measures each PCR against its byte position, so one moved PCR is one
  finding here and two intervals there.
- After damage the buffer-model counts change a little (other columns of the full reports):
  the model restarts after lost packets or a loss of sync, so an overflow that continues is
  counted again as a new episode.

## 4. The buffer model, on streams with a known answer

No tool at hand implements the T-STD (TSDuck does not), so the model is checked three ways:

1. `tests/rules.rs` builds streams whose buffer behavior follows from the schedule: audio
   packets of a frame back to back (TBn), two 72 kB frames at 17.3 Mbit/s against a
   15 Mbit/s leak (MBn), EBn full for more than a second (MBn not empty), video delivered
   20 ms before its decoding time (EBn underflow), audio 200 ms early (Bn overflow), 2 ms early
   (Bn underflow), 1.1 s early (delay), a PMT of five packets (TBsys) every 50 ms (Bsys), one
   packet every 752 us after two back to back (TBn of an E-AC-3 PID never empty, peak 282
   bytes as computed), an ETT of seven packets (smoothing buffer). Each reports exactly its
   rule; the undamaged synthetic stream reports nothing, with every model running.
2. `synthetic.ts` with every third AC-3 frame moved into the null slots right after its
   first packet (`tsmangle.py burst`, PCR packets untouched): TSTD-TB-OVERFLOW 101 times (the
   moved frames after the model starts), nothing else; TBn peaks at 892 bytes, which is
   5 x 188 bytes arriving in 5 x 38.76 us minus 2 Mbit/s x 194 us = 940 - 48 bytes. TSDuck's
   continuity and pcrverify stay at 0.
3. The constants: H.262 Tables 8-13 and 8-14 (Rmax, VBVmax), H.222.0 §2.4.2.3 (BSmux, BSoh,
   Rbx at Main and High levels, Rx), A/53 Part 5 §5.7 (2,592 bytes); a unit test checks the
   Main@Main values.

## 5. Cost and memory

| Input | Time | Peak RSS |
|---|---|---|
| atsc.ts, 492,145 packets (20 s at 37 Mbit/s) | 0.03 s user | 2.9 MB |
| atsc.ts 30 times through stdin, 14.8 M packets (10 min) | 1.13 s user (77 ns per packet) | 3.1 MB |
| the same without the buffer model | 0.99 s user (67 ns per packet) | 3.1 MB |

A 38.8 Mbit/s carrier is 25,800 packets/s: about 2 ms of one core per second (0.2 %), well
under the 5 % budget. Memory does not grow with the stream's length (the 30 loops also hit
the checker with a discontinuity at every loop).

## 6. Decisions taken overnight

- **Time base**: byte position at a constant rate (the nominal one, else a least-squares fit
  of the first PCR PID's PCRs) rather than piecewise PCR interpolation. It is what the task
  and TR 101 290's PCR_AC model ask for, and a bad PCR cannot move the clock of every other
  check. Variable-rate streams are noted (TS-VBR) and their PCR accuracy is not judged.
- **PCR accuracy window**: 64 PCRs (about 2 s at 30 ms), each PCR judged against a line
  through the window around it; a discontinuity (signaled or a jump outside 0 to 100 ms)
  starts a new window.
- **Interval semantics**: from the byte holding the last bit of one occurrence to that of the
  next (SCTE 54 §7.5's wording for PAT and PMT, used for every table); a table that stops
  is reported when it is overdue, once per gap. PSIP tables are timed from their first
  occurrence; a table never sent at all is PSIP-REQUIRED.
- **PID-MISSING periods**: 1 s for elementary and PCR PIDs, 60 s for EIT and ETT PIDs of the
  MGT (TR 101 290 leaves the period to the user, at most 5 s for audio and video).
- **Severity scale** as in tb-m2v-verify; the CLI fails on S2 by default like tb-m2v-verify;
  the project's mux should be held to S3.
- **ETT_table_id_extension**: the rule follows A/65's "shall be unique" (S3) although its
  footnote 13 warns about non-zero values; the conflict stays the digest's open question 1.
- **PSIP transport buffer**: A/65 and SCTE 54 define only the smoothing buffer and the
  250 kbit/s rate for PSIP PIDs (they are in no program's T-STD), so those two are checked
  and no TBn is invented for them.
- **Bsys input**: the payload bytes of PAT, CAT and PMT packets (H.222.0 says the bytes
  leaving TBsys enter Bsys; counting whole packets would double the header share).

## 7. Open items

- No independent T-STD implementation was available to compare the buffer model with;
  section 4 is the evidence. A capture from a professional analyzer would settle it.
- AVC buffers use the level defaults (no HRD parsing); AAC and E-AC-3 main buffers are not
  modeled; the vbv_delay method is not modeled.
- Not yet run on the project's own mux (crates tb-ts / tb-psip / tb-mux are being written) or
  on a capture after the modulator.
- `make_streams.sh` derives the EIT window from the host's clock; the streams are not
  reproducible byte for byte.
