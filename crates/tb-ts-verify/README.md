# tb-ts-verify

A streaming checker of MPEG-2 transport streams for a US cable QAM carrier: a multi-program
transport stream with ATSC PSIP on cable (SCTE 54). It is the stream checker of SPEC §15
(layer 3) and §11.2: used in CI on files, continuously in production on a tap of each mux's
output, and on captures after the modulator.

It is written from the standards (ITU-T H.222.0, ETSI TR 101 290, SCTE 54, ATSC A/65, A/53
Part 3 and Part 5, A/52 Annex A, the buffer tables of H.262), not from the mux crates of this
workspace, and shares no code with them, so that one misreading cannot hide in both. Every
check is a rule with an identifier, a severity and a clause and page: 147 rules, listed with
their sources in `notes/ts-conformance-rules.md` (generated from `src/rules.rs`).

## What it checks

- **TR 101 290** priority 1 and 2 and the priority 3 items that apply to ATSC: sync and sync
  byte, PAT (0.5 s, table_id, scrambling), continuity (with the duplicate-packet and
  adaptation-only rules), PMT, referenced PIDs, transport errors, section CRCs, PCR
  repetition, discontinuity and accuracy, PTS repetition, CAT, unreferenced PIDs, and the
  buffer indicators 3.3, 3.9 and 3.10 through the buffer model. The DVB service-information
  indicators (3.1, 3.2, 3.5 to 3.8: NIT, SDT, EIT, RST, TDT) do not apply to ATSC, which has
  none of those tables; packets on their PIDs are noted (TS-DVB-PID).
- **Cable and ATSC table rules**: the maximum cycles of SCTE 54 and A/65 (PAT 100 ms, PMT
  400 ms, MGT 150 ms, VCT 400 ms, STT 10 s on cable with the terrestrial 1 s as a note, RRT
  60 s, EIT-0 500 ms recommended); section syntax, lengths, single-section tables,
  versions (+1 modulo 32, no change of content without a new version, no new version without
  a change), current_next; the PSIP packet rules (no adaptation field, no scrambling, the MGT
  at the start of a packet); the PSIP smoothing buffer (1024 bytes, 250 kbit/s) and the
  250 kbit/s limit per PSIP PID.
- **PSIP cross-checks**: the MGT against the tables on air (PIDs, versions, number_bytes,
  tables not listed); the VCT against the PAT and PMT (TSID, program numbers, every program
  described, service location descriptor equal to the PMT), channel numbers, short names,
  source ids; the STT advancing with the stream and its GPS-UTC offset; EIT windows, order,
  instances per channel, events spanning windows; ETM identifiers and missing ETMs;
  multiple string structures.
- **PMT and PES for ATSC programs**: stream types, the registration descriptors, the AC-3
  audio descriptor (its syntax, the ATSC limits, and against the AC-3 syncframes), ISO 639
  and component names, captions descriptor syntax, the data stream alignment descriptor;
  PES start codes, header syntax, timestamps and marker bits, stream_id per type,
  PES_packet_length, data alignment, the cable PES flag restrictions.
- **The T-STD** of H.222.0 §2.4.2: TBn, MBn and EBn for MPEG-2 video (sizes from the
  profile and level in the stream, leak method), TBn, MBn and EBn for AVC (defaults of
  §2.14.3.1 for the level), TBn and Bn for AC-3 (2592 bytes), TBsys and Bsys per program;
  overflow, underflow, buffers that never empty, and the 1 s delay.

## Command line

    tb-ts-verify [options] FILE...      # '-' reads standard input as it arrives
    tb-ts-verify --rules-md | --rules-json

| Option | Meaning |
|---|---|
| `--rate R` | nominal rate, bit/s (38800000 for this project): the time base, and PCR accuracy against it |
| `--tsid N` | transport_stream_id the PAT must carry |
| `--pcr-target-ms M` | PCR interval target (30) for PCR-INTERVAL-TARGET |
| `--pid-timeout S` | PID-MISSING period for elementary and PCR PIDs (1 s) |
| `--psip-pid-timeout S` | PID-MISSING period for EIT and ETT PIDs (60 s) |
| `--no-buffer-model` | skip the T-STD and smoothing-buffer model |
| `--max-examples N` | findings kept per rule with position and message (8) |
| `--details`, `--tables` | print every kept finding; print table cycles and buffer peaks |
| `--json` | one JSON document (below) |
| `--allow ID` | do not count this rule against the exit status |
| `--fail-on S` | S1, S2 (default), S3, S4 or none |

Exit status: 0 when no rule of severity `--fail-on` or worse was violated (allowed rules
apart), 1 when one was, 2 on a usage or input error. This project's mux is held to S3.

On the mux's own output give `--rate 38800000` (SPEC §5.1): PCR accuracy is then also
judged against the nominal rate, and TS-RATE checks the PCR clock against it. On a capture
after the modulator the rate is the modulator's (38,810,701 bit/s for 256-QAM Annex B, with
its PCRs restamped): give that rate, or none and let the PCRs set it.

## Library

```rust
use tb_ts_verify::{Checker, Options};
let mut c = Checker::new(Options { rate: Some(38_800_000.0), ..Options::default() });
c.push(&bytes);              // any chunking; positions and results do not depend on it
let now = c.report();        // the state so far, at any time
let events = c.drain_events(); // findings since the last call (Options::events_capacity)
let end = c.finish();        // plus the checks that need the end of the stream
```

It is sans-io: bytes in, findings out. Memory is bounded by the PIDs and tables of the
stream and the findings kept per rule (table instances not seen for ten minutes are
dropped); a ten-minute run uses the same 3 MB as a twenty-second one. Cost: about 80 ns per
packet with the buffer model, so a 38.8 Mbit/s carrier (25,800 packets/s) costs about 2 ms of
CPU per second (notes/ts-verify-log.md).

### Time base

The stream's byte positions are its clock. A byte at offset i arrives at i x 8 / R seconds,
where R is `--rate` or, without it, the rate the PCRs of the first PCR PID give (a running
least-squares line of PCR against byte position; PCRs more than 10 ms off the line are left
out, three in a row start a new line). Intervals are measured between the bytes holding the
last bit of what is timed (the PCR base, the section's last byte). A program's clock at byte
i is its latest PCR plus the elapsed bytes at that rate; PTS and DTS are placed on the stream
clock through it. Without PCRs and without `--rate` no timing check runs (TIME-BASE).

PCR accuracy is measured two ways: against a line fitted to the PID's own PCRs over a
window of 64 PCRs around each one (TR 101 290 PCR_AC; skipped on a stream found to be
variable-rate), and with `--rate` against the nominal rate with only the offset fitted.

## Severities

S1 a receiver may fail to tune, lose sync or misdecode; S2 illegal per H.222.0 but commonly
tolerated; S3 outside the ATSC and cable rules (A/53 Part 3, A/65, A/52 Annex A, SCTE 54);
S4 a recommendation, a target of this project, or a legal construct worth knowing.

## JSON (schema `tb-ts-verify/1`)

One object per file (an array for several files). Fields may be added without a change of
schema; a change of meaning or a removal changes it.

| Field | Type | Meaning |
|---|---|---|
| `schema` | string | `"tb-ts-verify/1"` |
| `path`, `pass`, `fail_on`, `allowed` | string, bool, string or null, [string] | the file, whether it passed, the threshold and allowed rules (CLI only) |
| `bytes`, `packets`, `bytes_out_of_sync` | int | bytes read, packets analyzed in sync, bytes skipped out of sync |
| `duration_s` | number or null | stream duration at the time base's rate |
| `rate` | object | `estimated` (from PCRs), `nominal` (`--rate`), `deviation_ppm`, `used`, `cbr` (false when the rate between PCRs varies by more than 0.5 %) |
| `null_share` | number | share of null packets |
| `tsid` | int or null | the PAT's transport_stream_id |
| `programs` | {string: int} | program_number to PMT PID |
| `channels` | [string] | "major.minor name (program n)" of the VCT |
| `severity` | {S1..S4: int} | violations by severity |
| `counts` | {rule id: int} | violations by rule |
| `findings` | [object] | the kept findings: `rule`, `severity`, `clause`, `page`, `packet` (byte / 188), `byte` (offset of the packet), `pid` (or null), `time` (s, or null), `message` |
| `pids` | [object] | `pid`, `role`, `packets`, `scrambled`, `cc_errors`, `bitrate` |
| `pcr` | [object] | per PCR PID: `pid`, `count`, `max_interval_ms`, `over_40ms`, `over_target`, `max_accuracy_ns`, `max_accuracy_nominal_ns`, `rate` |
| `tables` | [object] | per table (or section) tracked for repetition: `name`, `pid`, `count`, `max_interval_ms`, `limit_ms` |
| `buffers` | [object] | per buffer of the model: `buffer` (TB, MB, EB, B, TBsys, Bsys, SB), `pid` (TBsys, Bsys: the PMT PID), `size`, `max_fill` (bytes) |
| `features` | {string: int} | sections seen by table |

## Simplifications and limits

- Video transfer from MBn to EBn uses the leak method (the STD descriptor's vbv_delay
  method is not modeled). PES header bytes are not counted in MBn (at most one header).
  Bsys receives the payload bytes of system packets.
- Access units: MPEG-2 pictures (from the first sequence, GOP or picture start code before a
  picture), AVC access unit delimiters, AC-3 syncframes. An access unit without a timestamp
  is decoded one frame period (half for a field picture, 1536 samples for AC-3) after the
  previous one; repeat_first_field is not followed there. A model starts at the first PES with
  a timestamp once the stream's parameters and the program's clock are known, and restarts
  after lost packets.
- AVC buffers use the defaults of H.222.0 §2.14.3.1 for the level (no HRD parameters are
  parsed); E-AC-3 gets a transport buffer only; AAC and other audio are not modeled.
- Not checked: Huffman-compressed and SCSU strings are not decoded (MSS-HUFFMAN notes the
  former); RRT, DCCT and cable_emergency_alert contents; captions and other video user data
  (tb-m2v-verify checks the video elementary stream); the 140 ms PAT allowance is not
  computed (the message says it exists); the EIT window check skips 30 s around each 3-hour
  boundary, where the MGT's roll is due.
- EIT and ETT PIDs are known only from the MGT: sections on them before the first MGT are
  not analyzed.

## Validation

`notes/ts-verify-log.md`: the checker against TSDuck on streams from ffmpeg and TSDuck,
and on deliberately damaged ones. The tools are in `tools/`: `make_streams.sh` (the
streams), `tsmangle.py` (the damage), `validate.sh` (the comparison table),
`tsduck_intervals.py` (table cycles from TSDuck's section log). The tests need none of them:
`tests/rules.rs` builds a stream that violates each rule from a conformant synthetic stream
(`tests/common`), `tests/streaming.rs` checks the incremental interface.
