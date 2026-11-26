# "Corrupts the config above 48 channels" — static root-cause hunt (fw 1.1.13)

Scope: static analysis of the 1.1.13 userland (byte-identical on the live unit), plus the saved
device captures in `work/device/`. Nothing was sent to the device.

Legend: **[P]** proven from the objdump disassembly. **[D]** read from the Ghidra decompilation and
spot-checked. **[L]** matches saved live-device data. **[I]** inference. Addresses are in the
`tools/bflt.py` images (base 0x10000000). EEPROM offsets are offsets in the 8 KB image.

## 0. Verdict

No code path in 1.1.13 fails at a stream *count* of 48, and nothing is dimensioned 48, 12 QAMs or
3 modules. The model string is a constant 64 [P streamconfig.cgi 0x10006eec `movi r17,64`]. The
report is therefore not explained by one "48" constant. Three real defects were found that an
installer would describe that way. Which of them the friend saw is **inference**.

| # | Mechanism | Status | Why it reads as "over 48" |
|---|---|---|---|
| A | `qamconfig.cgi` uses one mode-dependent offset for both RF-channel tables. In SPTS mode a QAM Config save for RF module 2, 3 or 4 writes 4 bytes outside the MPTS table. For **module 4 (streams 49–64) they land in Unit Name and Unit Location**. In MPTS mode it overwrites SPTS RF channels of other modules, and `mux_sync` later retunes modulators from those bytes. | **Confirmed [P]**, fingerprint matches the live export exactly [L] | Module 4 is the only one whose stray bytes hit fields shown on every page. |
| B | `update_config` (boot, Automatic mode) parses `/tmp/config_file.dat` as CSV, zero-fills every stream that has no line, and writes the result over the EEPROM stream table. The live unit holds a **48-line** file and is in Automatic mode. | **Confirmed [P]** for the code; effect on this unit in SPTS mode is [I] (unit is in MPTS mode now) | Streams 49–64 are wiped at every boot; streams 1–48 are replaced by mis-parsed values. |
| D | `mux_sync` rebuilds its per-stream program/ES table every 30 s but clears only `count*4` bytes of it, and appends ES entries with no limit. The write position of every streaming program moves forward 8 bytes per elementary stream per pass, through the following records and then past the end of the 0x24200-byte heap block. This CPU has no MMU, so that is other processes' or kernel memory. | **Defect confirmed [P]**; that it ends in config corruption is [I] | The table is laid out for 64 streams. Streams 1–48 have 16 unused records (37 KB) behind them; streams 49–64 have little or none. In MPTS mode more programs mean more moving write positions. |
| C | Vendor-acknowledged FPGA I2C fault, "EEPROM corruption of IP address", fixed in 1.1.13. | Not analyzable here (FPGA). Residue in the EEPROM suggests it, see 4.3 | Unknown. Could be the original source of the folklore. |

Operational context added later by the owner: the friend ran her unit in **MPTS mode** and probably
made the 48-line file herself on 2026-09-07 while setting this unit up. That makes D the best fit
for her report (it is the only defect found that acts in MPTS mode during streaming and scales
with the number of programs), demotes B to "what her file does to this unit", and leaves A as a
separate, real bug.

Confidence: A, B and D exist as described — high. That A or B is *the* reported bug — medium/low;
A is my first pick for a unit that was configured through the web UI, B for this particular unit
as it stands today.

## 1. Mechanism A — QAM Config save writes through the wrong table index

### Code
`qamconfig.cgi` main (0x100000ec), handler for `qam_saved=1` + `outputChannelA`. `r21` = slot
(0..3), `r19` = stream count (64 in SPTS, 16 in MPTS, from 0x10005a20), `fp` = sp+4, EEPROM image
at sp+688 = fp+684.

```
100002a8  mov  r2,r19            ; count
100002c0  srai r2,r2,2           ; count/4  -> 16 (SPTS) or 4 (MPTS)
100002cc  mul  r2,r21,r2         ; slot * (count/4)
100002d8  slli r2,r2,5           ; * 0x20
100002e8  add  r2,r2,fp
100002f8  stb  r17,829(r2)       ; 829-684  = 0x091  SPTS rec RF CH, 16 stores 0x091..0x271 step 0x20
10000344  stb  r17,2877(r2)      ; 2877-684 = 0x891  MPTS rec RF CH (channel A)
100002ec  stb  r7,2909(r2)       ; 0x8B1 (B)
100002f4  stb  r6,2941(r2)       ; 0x8D1 (C)
1000030c  stb  r3,2973(r2)       ; 0x8F1 (D)
```
Decompiled: `iVar3 = slot * (count >> 2) * 0x20;` then 16 stores at `0x91 + iVar3 + k*0x20` and 4
stores at `0x891 + iVar3 + q*0x20`. The same offset is used for both tables, but the tables need
`slot*0x200` (SPTS) and `slot*0x80` (MPTS). There is no bound check. `slotNumber` itself is
`atoi()-1` with no range check [D] (UI sends 1–4).

### Bytes damaged
| Mode at save | RF module | SPTS RF CH bytes written | MPTS-table bytes written |
|---|---|---|---|
| SPTS | 1 | recs 0–15 (right) | 0x891,0x8B1,0x8D1,0x8F1 (right) |
| SPTS | 2 | recs 16–31 (right) | **0xA91, 0xAB1, 0xAD1, 0xAF1** = EAS 1 name[2], EAS 1 rec+0x29, EAS 2 name[6], EAS 2 rec+0x2D |
| SPTS | 3 | recs 32–47 (right) | **0xC91, 0xCB1, 0xCD1, 0xCF1** = no descriptor; use unknown |
| SPTS | 4 | recs 48–63 (right) | **0xE91, 0xEB1 = Unit Name[1], [33]; 0xED1, 0xEF1 = Unit Location[1], [33]** |
| MPTS | m (1..4) | **recs 4(m-1) .. 4(m-1)+15** (wrong for m>1) | recs 4(m-1)..+3 (right) |

The value written is the channel number of carrier A, B, C, D. Example: SPTS mode, module 4,
channel 140: "Ball Holdings" becomes `B\x8Cll Holdings`; a location "Rack 3" becomes `R\x8Eck 3`.
Channel 10 puts a line feed into the name, which also splits the `Unit Name=` line of the
Key=Value export.

### Knock-on: mux_sync retunes modulators from the damaged table [D]
`mux_sync` main loop (0x1000047c), every 5th pass, for each module with status 0:
`ch = eeprom[slot*(count>>2)*0x20 + (mode ? 0x891 : 0x91)]`; if `ch != 0` and it differs from the
modulator's channel it logs `ERROR: Difference detected in modulator settings.  Changing RF output
channel. (%d)` and reprograms the modulator. This is unconditional (not tied to Automatic mode).
So after QAM saves in one mode, a later mode switch makes `mux_sync` push stale or foreign channel
numbers into the modulators. On the live unit SPTS rec 16 holds 141 while module 2 is on channel
10: **switching this unit to SPTS mode would retune RF module 2 to channel 141, on top of module
4** [I from D+L].

### Live fingerprint [L]
`work/device/config_file.dat`: SPTS RF CH = `2×4,3×4,4×4,140×4,141×4,142×4,143×4,13×4,6×4,95×4,
96×4,97×4, 0×16`; MPTS RF CH = `2,3,4,5,0×8,140,141,142,143`. The harness reproduces both arrays
exactly with this history: SPTS-mode saves of modules 1, 2 (ch 10), 3 (ch 6), then MPTS-mode saves
of modules 1 and 4 (`eqam_corrupt_sim.py qam --replay-live`). The "module 4 at SPTS recs 12–27" and
"MPTS recs 4–11 = 0" features have no other explanation in the code. Prediction for a raw EEPROM
dump: 0xA91/AB1/AD1/AF1 = 10,11,12,13 and 0xC91/CB1/CD1/CF1 = 6,95,96,97. Unit Name is intact
because module 4 was never saved in SPTS mode (SPTS recs 48–63 are 0).

### Aftermath and recovery
No guard, no reboot, no network loss. The damage is 4 bytes per save. Re-entering Unit Name,
Location (network.cgi) and the EAS names repairs the visible fields. The stale RF CH bytes are
repaired by saving QAM Config for all four modules in the current mode.

## 2. Mechanism B — boot-time CSV reconcile in Automatic mode

### Code [P]
`update_config` main 0x100000ec. Frame 13840 bytes: file records sp+0 (64×0x28), EEPROM records
sp+2560, EEPROM image sp+5220, line buffer sp+13412.
* Runs only if `eeprom[0x40] == 0` (Automatic) and `/tmp/config_file.dat` opens
  (`10000154 bne r2,zero` / `10000170 beq`).
* Zero-fills `count` file records (0x10000180 loop), then reads lines: `fgets(buf,255)`,
  `strtok` with delimiters `",-."` (0x10013424), fields in order: UDP/RTP, IP0..IP3, port,
  name (`strncpy` 10), RF CH, and in SPTS mode program number. No NULL checks on `strtok`
  results (`atoi(NULL)`/`strncpy(dst,NULL,10)` read address 0 on this no-MMU CPU).
* Loop exit is `1000029c blt r19,r22` with r22 = lines read, so it reads `count+1` lines; record
  64 lands on the first EEPROM-side record, which is rebuilt afterwards (harmless).
* Compares IP, port, UDP/RTP, name, RF CH (and program number in SPTS) for `count` streams. Any
  difference logs "Difference detected in Config File…" and then **all `count` records are
  overwritten from the file records** (0x80+0x20·n in SPTS, 0x880+0x20·n in MPTS), byte 0x10DE is
  cleared, and the whole 8 KB is written.

The file format it expects is the one `admin.cgi` "Commit Changes to Config File" writes [D,
admin.cgi 0x100000ec]: `"%d,"` `"%d.%d.%d.%d,"` `"%d,"` then `"%s,"` or `"0,"` for an empty name,
then `"%d-%d\n"` (SPTS: RF channel-program number) or `"%d\n"` (MPTS). It always writes 64 (or
16) lines. The Key=Value file (`/home/httpd/config_file.dat`, config.cgi upload) is a different
format handled by a different parser; `update_config` contains that parser only as dead code.

### Triggers and damage
1. **File with fewer lines than streams.** Streams without a line are compared against zeros and
   then zeroed (IP, port, UDP/RTP, name, RF CH, program number). The live `/mnt/config_file.dat` is
   48 lines of `0,0,<ch>-<k>` (created 2026-09-07, not a format 1.1.13 writes). In SPTS mode this
   file gives: streams 1–48 → IP `0.<ch>.<k>.0`, port 0, RF CH 0, program 0, name = bytes at
   address 0; streams 49–64 → all zero (`eqam_corrupt_sim.py csv --mode spts <file>`). So on this
   unit, in SPTS mode, anything configured above 48 disappears at each boot, and the rest is
   damaged too.
2. **Stream names containing `,` `-` or `.`** (e.g. `ESPN-HD`). The name is split by `strtok`; the
   fields shift: name is cut, RF CH = `atoi` of the name tail (0), program number = RF channel.
3. **Empty names** come back as the name `0`.
4. A Key=Value file left in `/tmp/config_file.dat` and committed would parse as all-zero records
   [I]; `commitConfig` rewrites `/tmp` first, so this needs manual copying.

When it runs: only at boot. `/etc/rc` has `#crontab /etc/cronfile` commented out in 1.1.13 (the
420A 1.0.15 image has it active, every 5 minutes), no other binary names `update_config`, and the
live PID counter was near 213 after 6 days [L]. The manual's "every 30 minutes" does not apply to
this build.

### Open discrepancy
The live event log shows the reconcile fired at the last boot. In MPTS mode my model says MPTS
records 0–15 became `0.<ch>.<k>.0` with RF CH 0. The export shows IP 0.0.0.0 and RF CH
2,3,4,5,…,140–143. That is consistent only if, after that boot, someone saved QAM Config for
modules 1 and 4 and cleared the addresses (Status Edit save; a flush would have been logged). I
could not confirm this. Ask the owner, or read 0x880–0xA7F from an EEPROM dump after the next boot.

## 2a. Mechanism D — unbounded table growth in mux_sync (acts in MPTS mode too)

### Code [P]
`mux_sync` main (0x1000047c) mallocs the table once: `FUN_1000ca38(0x24200)` = 64 records × 0x908
bytes. Each main-loop pass (the loop ends in `sleep(30)`) calls 0x10001450(eeprom, table):

```
10001490  call 0x10001424        ; count (64 SPTS / 16 MPTS)
10001494  slli r6,r2,2           ; size = count*4   <- should be count*0x908
100014a8  call 0x1000c320        ; memset(table, 0, count*4)
...                              ; per stream, per program slot (16), per ES slot (16):
10001614  beq  r4,zero,...       ; skip if the FPGA reports PID 0
1000161c  slli r2,r2,3           ; r2 = es_count*8          (es_count loaded from record)
10001628  stw  r4,204(r2)        ; pid
1000163c  stw  r3,208(r2)        ; stream type
10001648  stw  r2,0(r5)          ; es_count++               (never reset, never compared to 16)
```
Record layout: +0 program count, +4 16×12-byte program entries, +0xC4 active-program count,
+200 16 × 0x84-byte program slots (ES count + 16 × 8-byte ES entries). Only stream 0's first
64 (MPTS) or 256 (SPTS) bytes are ever cleared. Every other counter keeps its value from the
previous pass, so each pass appends the same ES list again, 8 bytes further on.

### Consequences [I]
* A program with 2 elementary streams overruns its own 16-entry slot after 8 passes (4 minutes),
  then walks through the following program slots and stream records at 16 bytes per pass.
* When the walk crosses the ES counter of another *active* program it overwrites that counter
  with a PID or stream-type value, and that program's write position jumps ahead by up to
  0x1FFF×8 = 64 KB. High PIDs therefore shorten everything below.
* Distance from the last active stream to the end of the block: stream 64 about 2 KB, stream 49
  about 37 KB, stream 48 about 39 KB, MPTS 16 about 111 KB (plus whatever slack the kernel
  allocator added, unknown). At 16–24 bytes per 30 s that is on the order of an hour for stream
  64 and one to several days for the other cases, less with jumps.
* Past the end the writes hit unrelated memory. Possible results: hangs and watchdog reboots,
  damaged in-RAM files (the root filesystem and `/tmp/config_file.dat` live in RAM), or a damaged
  8 KB image in a CGI that is about to write the EEPROM. None of this was observed; the live unit
  has no inputs, so its table does not grow (uptime 6 d 18 h is consistent).
* There is no 48- or 96-entry flat program table in mux_sync. The per-stream limits are 16
  programs × 16 ES, taken from FPGA registers.

### Workarounds
None by configuration, except fewer programs/ES and lower stream indices to buy time. A scheduled
reboot (or `kill` + restart of `/bin/mux_sync` from telnet) resets the table. Binary patch
(untested): change one instruction so the whole table is cleared each pass —
`mux_sync` image 0x1494 `slli r6,r2,2` (100C90BA) → `muli r6,r2,2312` (11824224). bFLT file
offset = 0x40 + 0x1494 (the file is an uncompressed bFLT, flags=1; the old word is there).

## 2b. Who reads or writes `0,0,<ch>-<k>` and results.dat

No binary in the 1.1.13 root filesystem contains `results.dat` or `nas`. The only writer of
`config_file.dat` in CSV form is admin.cgi "Commit", and it always prints the IP and name fields
(`0,0.0.0.0,0,0,2-1` for an empty slot), 64 or 16 lines. So `0,0,<ch>-<k>` is a hand-made or
tool-made file. The only reader is `update_config`, which mis-parses it as described in section 2
(`<ch>` and `<k>` become IP octets 2 and 3). If the lines were meant as a major-minor lineup, the
firmware has no input for that in MPTS mode at all.

## 3. Other hypotheses

| Lead | Verdict | Evidence |
|---|---|---|
| L1 arrays sized 3 modules / 12 QAM / 48 | **Refuted** in admin, config, mode, qamconfig, statusedit, streamconfig CGIs, update_config, mux_sync, web_command_monitor | Stack arrays are 64×0x3C, 64×0x1B, 64×0x14, 64×3 ints, 4×0x9C; loops use `count` (64/16) or 4 modules [D]. Not checked: eas, network, time, status, license CGIs, eeprom_rw menus, join_group, eas_soft_trigger. |
| L2 image bounded at 0x14C0 | **Refuted** for analyzed programs | No 0x14C0 constant in any decompilation; all writers send 8192 bytes. Controller side unknown. |
| L3 MPTS-mode VCT base 0xF40 walks over the guard | **Refuted for normal use** | streamconfig.cgi 0x1000073c: MPTS records are major 0xF3C, minor 0xF3E, name 0xF40 + 0x12·n, loop bound `count` = 16 in MPTS → ends 0x105C. Only reachable if byte 0x7B is neither 0 nor 1: then count = 64 with the MPTS base, and a save zeroes 14 bytes per row up to 0x13C0, including 0x1128 → guard. `mode.cgi` stores `atoi(tsMode)` unchecked [D], so a crafted POST or a corrupted 0x7B does it. |
| Corruption guard (0x1128–0x112B) as the trigger | **No normal writer reaches it** | 0x1128 is the management (eth1) IP. Guard: copy 0x1F00-byte default template (172.16.70.1/24, Admin/pass, Guest/pass, SPTS mode), overwrite IP/mask/gateway from live `ifconfig eth1` and `route`, write, `epcs -c`. The last 0x100 bytes of that image are uninitialized stack, and if `ifconfig eth1` shows no address the IP words are uninitialized too [D, I]. Documented recovery: IP RESET button → 172.16.70.1. |
| L4 Key=Value parser limits | **No 48 limit**; other bugs confirmed [D] | config.cgi 0x10004fcc (same code as update_config 0x10001c6c). `SPTS_ID`/`MPTS_ID` values are used as indices with no bound. String values are copied with `memcpy(dst, val, strlen(val))`, not clamped to the field, and keep the line ending (delimiter is only `=`): a 10-character stream name writes LF into RF CH (0x91+0x20·n); with CRLF also into the program number. A 16-character SNMP read community writes into the admin user name. |
| L7 short-name size 0x12 vs 14 | **Confirmed, benign for a complete file** [D] | Type 7 does `memset(field,0,0x12)`: it zeroes the next stream's major/minor, which the next block rewrites. Net effect of a full 64-block file: 0x15E0–0x15E3 zeroed (unused as far as seen). A file with fewer blocks leaves the following stream's major/minor at 0. |
| Exporter key typos (`Local 1 Subnet 0`, `MPTS Source 1 IP0` twice) | **Round trip is safe if line order is kept** [D] | The parser first tries the descriptor after the last match, so the duplicate names resolve by position. Reordered lines fall back to first match and hit the wrong field. |
| POST truncation of 64 rows | **Refuted on the CGI side** | cgic mallocs CONTENT_LENGTH and fails the request on a short read (0x1000aa30). Boa not examined. Related real hazard: statusedit.cgi zeroes any stream whose `streamGroupAddress%d` is absent, and streamconfig.cgi zeroes any short name whose field is absent, so a partial POST wipes the missing rows [D]. |
| Lost-update races | **Refuted** | Every reader/writer holds `eeprom_sem_id` across read-modify-write; mux_sync writes the EEPROM only in the guard path [D]. |
| L6 per-GigE limits | Not a config writer | mux_sync programs 32 streams per input FPGA (`i < count/2`). No 24/48 split [D]. FPGA behavior for FPGA2B streams 17–32 is outside static reach. |
| Meaning "VCT major/minor > 48" or "EIA channel > 48" | **Refuted** | No value range check or table indexed by these numbers in the writers. |

## 4. Things on the live unit that are already wrong

1. `/mnt/config_file.dat`: 48 lines in a format 1.1.13 neither writes nor parses correctly, with
   Automatic mode on (mechanism B at every boot). [I] It looks like a hand-made or tool-made file
   that already assumes 3 programs per carrier, i.e. it may be a product of the "48" belief, not
   its cause.
2. SPTS RF CH table is stale/foreign (mechanism A); a switch to SPTS mode will retune module 2.
3. Bytes 0xF38 onward hold a big-endian u16 sequence 1,2,3,4,… followed by RF channel numbers
   (67, 68, 70, 59, 60, 61, 96, 97) [L, from the streamconfig.cgi page and the export: Path
   Select=0, Band Select=1, cable flags 0 and 2 are the first two words]. **No routine in the
   analyzed 1.1.13 programs writes such a pattern.** Candidates: (a) residue of an older firmware's
   EEPROM layout (420B 0.1.10–0.1.12) that was never defaulted; (b) residue of the vendor's I2C
   fault (modulator traffic or mis-addressed writes landing in the EEPROM). 96/97 are module 3's
   *current* channels, which argues the writes are recent. Unresolved here; section 8 has the dump, which shows a structured
   table and changes this picture.

## 5. Workarounds, safest first

1. **Set Configuration Mode to Manual** (Admin.html) or delete the config file ("Delete Config
   File"; both copies are already backed up in `work/device/`). This disables mechanism B. If
   Automatic mode is wanted: Manual → finish all edits → "Commit Changes to Config File" →
   Automatic, and repeat the commit after every change. Never use `,` `-` `.` in stream names and
   give every stream a name.
2. **Order of operations for mechanism A:** pick the TS mode first. Save QAM Config for all four
   RF modules in that mode. Then re-enter Unit Name and Unit Location (and EAS stream names). After
   any later QAM Config save on modules 2–4 in SPTS mode, re-enter them again. After any mode
   change, re-save all four QAM pages before relying on the RF plan.
3. Do not restore Key=Value files that were edited on Windows (CRLF) or that hold 10-character
   stream names, 16-character SNMP names, or fewer than 64 SPTS blocks.
4. Drive the configuration only through the three per-page forms, always posting every row
   (statusedit and streamconfig wipe rows that are missing from the POST).
5. **Binary patch for A** (untested on hardware). `qamconfig.cgi` is an uncompressed bFLT; file
   offset = 0x40 + image offset. 11 words, little-endian:

   | Image off | Old | New | Meaning |
   |---|---|---|---|
   | 0x2A8 | 9805883A | A804927A | `slli r2,r21,9` (slot×0x200) |
   | 0x2AC | 9800010E | 1705883A | `add r2,r2,fp` |
   | 0x2B0 | 988000C4 | A81291FA | `slli r9,r21,7` (slot×0x80) |
   | 0x2C0 | 1005D0BA | 4F13883A | `add r9,r9,fp` |
   | 0x2CC, 0x2D8, 0x2E8 | A885383A, 1004917A, 1705883A | 0001883A | `nop` |
   | 0x2EC | 11C2D745 | 49C2D745 | `stb r7,2909(r9)` |
   | 0x2F4 | 1182DF45 | 4982DF45 | `stb r6,2941(r9)` |
   | 0x30C | 10C2E745 | 48C2E745 | `stb r3,2973(r9)` |
   | 0x344 | 1442CF45 | 4C42CF45 | `stb r17,2877(r9)` |

   Both tables are then written at their own stride in either mode. r9 is unused in the block and
   dead across the following call [P]. `tools/eqam_corrupt_sim.py patch SRC DST` applies it and
   checks the old words. The root filesystem is an initramfs, so the low-risk way to use it is to
   copy the patched file over `/home/httpd/cgi-bin/qamconfig.cgi` after each boot (RAM only, gone at
   reboot). Making it permanent means rebuilding the EPCS_1 image; not recommended yet.

## 6. Harness

`tools/eqam_corrupt_sim.py` (pure Python, synthetic 8 KB image):
* `qam --mode spts --slot 4 --channel 140` shows the Unit Name/Location overwrite; `--fixed` shows
  the patched behavior; `--replay-live` reproduces the live RF CH arrays.
* `csv --mode spts [FILE]` runs the boot reconcile (no FILE = round trip through the firmware's
  own writer; `--name ESPN-HD` shows the delimiter bug).
* `kv --name 0123456789 [--crlf]` shows the Key=Value string overflow and the short-name memset.

## 7. Still unverified, and the cheapest experiments

| Question | Experiment | Risk |
|---|---|---|
| Is A what happened on this unit? | Raw EEPROM dump: expect 0xA91.. = 10,11,12,13 and 0xC91.. = 6,95,96,97 | read-only |
| Is the model of B right (section 2 discrepancy)? | Same dump: look at 0x880–0xA7F; ask the owner what was saved since the last boot | read-only |
| Source of the 1,2,3,4 pattern at 0xF38+ | Same dump: where it starts and ends, its period, whether it crosses 0x10E0–0x115F | read-only |
| Does A corrupt the name live? | SPTS mode, set a Unit Name, save QAM Config on module 4, reload | writes 20 EEPROM bytes; reversible |
| FPGA I2C fault (C) still present? | Compare 420A 1.0.15 vs 1.0.16 FPGA1 bitstreams and userland (the vendor says only the I2C controller changed). `extract_firmware.py` fails on the 1016 header variant, so this was not done | none |
| Does D breach the heap block in practice? | With inputs running: note `free` and uptime over two days, watch for watchdog reboots; or run the bFLT under an emulator with fake FPGA registers | operational only |
| FPGA2B streams 17–32 behavior | Needs traffic on the unit | operational |

**EEPROM dump method.** A dump does discriminate (first three rows). BusyBox here has no `dd`,
`hexdump` or `od`; sash `hexdump -s` and `cp`/`cat` read to end of file, so on `/dev/mem` they
would run past the 8 KB mirror into other FPGA registers (and the kernel's range check may refuse
0x13010000 anyway). `eeprom_rw` is a byte-at-a-time menu next to "Default entire EEPROM". The
safe method is a tiny static bFLT that waits for `0x12000810 & 1` and does
`write(1, (void*)0x13010000, 0x2000)`, exactly the read every BT program performs (they take
`eeprom_sem_id` first; skipping it is safe if nobody uses the web UI during the read). Put it in
`/tmp` (RAM), run it with output to a file in `/tmp`, fetch by FTP. `nios2-elf-as`/`ld` are built
in `work/tools-build`; the bFLT header can be written by hand (no relocations needed for
position-independent code). It needs the owner's approval because it uploads and runs a binary.

## 8. Raw EEPROM dump (taken from the live unit) and what it settles

Method: `tools/eedump/` (source `eedump.S`, build and offline check `build.py`). A 260-byte bFLT
with no relocations that repeats the stock read (`ldwio` poll of bit 0 at 0x12000810, then 2048
word loads from 0x13010000 into a stack buffer), writes `/tmp/ee.bin` and exits. It stores to no
FPGA address. Before use: disassembly checked against the stock instruction words, and a small
interpreter in `build.py` ran it against fake hardware. On the device: uploaded to `/tmp` by FTP,
`chmod 755`, run twice from telnet, both outputs fetched, both files removed, `/tmp` listing back
to the original four entries, uptime unbroken, no new event-log lines. Both dumps are identical:
`work/device/eeprom.bin`, sha256 `2f27ba03…5f55` (not committed; it holds credentials).

| Item | Dump | Meaning |
|---|---|---|
| Guard 0x1128–0x1133 | 10.42.0.184 / 255.255.252.0 / 10.42.0.1 | Healthy. The guard is not involved in anything seen on this unit. |
| Mode 0x7B, config mode 0x40 | 1 (MPTS), 0 (Automatic) | As exported. |
| 0xA91, 0xAB1, 0xAD1, 0xAF1 | 10, 11, 12, 13 | **Defect A confirmed on this unit**: module 2 saved in SPTS mode. |
| 0xC91, 0xCB1, 0xCD1, 0xCF1 | 6, 95, 96, 97 | Same, module 3. Exactly the predicted bytes. |
| 0xE91.. | 'a', 0, 0, 0 (Unit Name intact) | Module 4 was never saved in SPTS mode. |
| SPTS / MPTS RF CH | identical to the export | |
| MPTS records 0x880–0xA7F | all zero except RF CH | The values defect B should have written at the last boot (`0.<ch>.<k>.0`, RF CH 0) are **not there**. Still two readings: QAM saves on modules 1 and 4 plus a Status Edit save happened after that boot, or my model of the reconcile write is wrong in some detail. The dump cannot tell them apart; the owner can. |
| SPTS VCT 0x1160–0x15DF | all zero | Never initialized by 1.1.13 defaults (those hold 3-1 … 18-4). |
| 0xF10–0xF8F | 64 big-endian u16: 1,2,3,4 repeated 16 times | |
| 0xF9A–0xF9D | EA60 EA61 (60000, 60001) | |
| 0xFA0–0x108F | 48 five-byte entries `00 00 <ch> <k> 00`, ch = 2,3,4,5,67,68,69,70,58,59,60,61,140,141,142,143, k = 1..3 | **Byte-for-byte the content of the 48-line `0,0,<ch>-<k>` file.** |
| 0x1090–0x10DF | zero | Room for 16 more five-byte entries before the SNMP names. |
| 0x1F00–0x1F2B | five MAC addresses 00:14:39:00:35:C3..C7 and the serial number | Factory data. The guard's default path rebuilds only 0x1F00 bytes and writes uninitialized stack above that [D], so a guard event would also destroy these. |

### The table at 0xF10–0x108F
* It is structured data, not random damage, and it is the same lineup as the 2026-09-07 file. So
  the file and the table come from the same setup session or tool.
* **No analyzed 1.1.13 program reads or writes this layout.** 1.1.13 uses the same bytes for other
  things: 0xF30/0xF34 = GIGE 2 subnet mask and gateway, 0xF38–0xF3B = path/band/cable flags,
  0xF3C + 0x12·n = MPTS-mode VCT records. That is why the live UI shows GIGE 2 mask `0.1.0.2`,
  gateway `0.3.0.4` (admin.cgi page), VCT type "neither", Out Of Band enabled, and garbage rows in
  streamconfig.cgi. **This is real, visible corruption of network and PSIP settings on this unit,
  and 1.1.13's own code did not cause it.** (The kernel refused the bad mask at boot, so eth3 runs
  with 255.255.255.0.)
* Layout arithmetic, if some other software owns this table: 48 entries end at 0x1090; entry 65
  would start at 0x10E0 (SNMP community), entry 71 at 0x1100 (web user names and passwords),
  entry 79 at 0x1128 (management IP, the guard bytes). A table of this shape that is allowed to
  grow past its slack is a direct way to get "config corruption above N channels" and the
  vendor's "EEPROM corruption of IP address". With five-byte entries the first damage is above 64,
  not 48; the 48 may simply be 16 carriers × 3 programs as set up. [I]
* Origin candidates: a different firmware build that was on the unit during setup, or a tool that
  wrote the EEPROM directly (`eeprom_rw` can write single bytes). The person who did the setup on
  2026-09-07 should know; that question now matters more than any further disassembly.

### Hypotheses after the dump
* Killed: "the guard fired / IP bytes were lost on this unit"; "the 0xF38 pattern is random I2C
  damage"; "the pattern is a side effect of defect A"; "defect A is only theoretical here".
* Confirmed: defect A, with the exact predicted bytes.
* Open: defect B's behavior at the last boot; who wrote the 0xF10 table; defect D's practical
  reach (needs live inputs).
