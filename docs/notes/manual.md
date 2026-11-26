# EQAM-420B User Manual — structured digest

Source: `docs/external/blonder tongue eqam-420b.pdf` (manualslib wkhtmltopdf scrape, 52 PDF pages; text layer is good,
screenshots are images). **Page refs below are the manual's printed page numbers ("p.N"); PDF page = p.N + 3.**
Manual identity: Blonder Tongue "EQAM-420B User Manual", **Stock No. 6522B-4-64**, Document No. **651234700A**,
Issue 1, Mar. 21 2014, author "MP" (p.1; PDF p.4). Revision history: "This is the first issue" (p.7).
Screenshots in the manual show firmware **"Software Version: 0.1.0 Prerelease 1"**, FPGA1 1.1, FPGA2A 1.1S,
FPGA2B 1.1S, FPGA3A 1.1, FPGA3B 1.1, DDQ 1.4 on all four modules, ESN 2013090270, MACs 00:14:39:00:35:AF..B3,
dated Dec 2013 (p.30, p.36, p.37). I.e. the manual documents a pre-release build; our unit is likely newer.

Legend: **[M]** = manual states it; **[I]** = my inference.

---

## 1. Product / model decoding

- Cover (p.1): "EdgeQAM with EAS and Pro:Idiom™ — EQAM-420B — Equipped with 4 Quad QAM output modules for a total
  of 16 QAM (accepts **64 HD Programs in SPTS mode** and **96 HD Programs in MPTS mode**)". Stock no. 6522B-4-64. [M]
- Web UI banner reads **"EQAM-420B-4-64"** on every screenshot (p.14 ff). [M]
- **"-4-64" decoding [I, strongly supported]:** `4` = number of Quad-QAM output modules installed (4 × 4 = 16 QAM
  carriers); `64` = SPTS input stream capacity (64 SPTS). The front-panel "Output Status Module" LEDs are "Off =
  corresponding RF module board is not installed" (p.8), implying depopulated variants exist (e.g. -1-16, -2-32;
  the EQAM-420A manual on the web is titled "eqam-420a-1-16", see research.md). 64 = 4 modules × 4 QAM × 4 SPTS/QAM.
- MPTS capacity: 16 MPTS × 6 programs = **96 programs** (p.1, p.7). [M]
- The "B" vs "A" difference is not explained in this manual. [See research.md.]
- Product family name on front panel: "EQAM SERIES Edge QAM" (p.8 photo). [M]

## 2. Hardware interfaces (p.8–9, p.10)

Front panel (left→right) [M]:
1. **GIGE #2 & #1 DATA** — RJ45 1000Base-T, streaming IP inputs. (Two independent ports, each with its own IP,
   subnet mask and default gateway on Admin.html, p.32.)
2. **-20 dB QAM RF TEST** — F female, combined RF output −20 dB.
3. **OUTPUT STATUS MODULE LEDs 1–4** — Off = module not installed; Red = module issue; **Orange = not all assigned
   channels have IP streams enabled and streaming**; Green = OK and all assigned channels streaming.
4. **LOCAL CONTROL #1 & #2 (10/100)** — RJ45. #1 usable as EAS trigger input (and web access per p.34 item 17);
   #2 "reserved for future use".
5. **REMOTE CONTROL (10/100)** — RJ45, web management + optional EAS trigger input. Static IP only; default 172.16.70.1.
6. **POWER LED** — green = AC present; off = no AC/switch off, or dead PSU ("must be sent to Blonder Tongue").
7. **STATUS LED** — **flashing green = OS valid and operational; solid green or off = OS not valid/not operational.**
8. **IP RESET button** (front panel, right of STATUS LED) — see §8 recovery.

Rear panel [M]: (9) IEC power inlet + rocker switch, 115–230 VAC 1.4/0.7 A, 3.0 A 250 V slo-blo fuse;
(10) **REMOTE DATA OUT** — "reserved for future use"; (11) **QAM OUTPUT** F-female: all four modules' outputs combined
internally onto this single connector.

Not present [M by omission / I]: no ASI input or output, no serial console documented, no SNMP agent (SNMP only
mentioned as "future option", p.37), no redundancy/second PSU. The Stream Input status column help text says
"IP: indicates the incoming **ASI** input transport stream rate" (p.17) — a copy-paste artifact; there is no ASI port.

Sec. 4 warning (p.12) [M]: **the 10/100 Control port and GigE Data ports must never be on the same subnet nor on the
same unmanaged switch; if on a managed switch, ensure the 10/100 Control port receives no multicast traffic.**
Repeated on p.34 with example (10/100 = 172.16.70.100/24 with GigE 172.16.70.110 → "you will not be able to
communicate with the unit"; use e.g. 192.168.253.1 or 172.16.100.98 for GigE).

p.11 [M]: "EQAM-420B and the GigE Switch are considered as elements of an integrated solution. When a power cycle or
re-boot is deemed necessary for one element, it may be necessary to power cycle one or all of the other elements."

## 3. Specifications (p.10, "2.3 Product Specification") [M]

Input IP: 2× RJ-45 1000Base-T; UDP/RTP (user-selectable); IGMPv3, IGMPv2.
Stream portfolio: SPTS & MPTS; null packet deletion & addition; muxing of input streams; bitrate variable and constant.
EAS: RJ-45 (trigger via Remote Control port or Local Control #1), 10/100, trigger mechanism per SCTE-18.

QAM output:
| Item | Spec |
|---|---|
| Output modules | 4 Quad-QAM (16 QAM channels) |
| Connector | 1× F female rear (combined) |
| Modulation | QAM 16, 32, 64, 128, 256 (UI offers **64B, 256B, 16A, 32A, 64A, 128A, 256A**, p.25) |
| Standards | ITU-T J.83 **Annex A and B** (no Annex C mentioned) |
| DVB symbol rate | Variable, up to 7 MSym/s (Annex A) |
| Frequency range | 54–1002 MHz |
| Tuning | CATV channel selectable, "CH. 2 to 158" (spec table) vs "NTSC channels 2 to 155" (QAM Config, p.25) — inconsistent |
| Channel bandwidth | 24 MHz per module (4 × adjacent 6 MHz) |
| RF level | +40 dBmV, ±1 dB increment; adjustment range +35 to +42 dBmV (spec) vs **28 to 41 dBmV** (UI, p.25) — inconsistent; recommended 40 dBmV |
| Freq tolerance / stability | ±0.5 kHz @25 °C / ±5 kHz 0–50 °C |
| Flatness | ±0.25 dB over 6 MHz |
| Phase noise | −98 dBc @10 kHz |
| Spurious | −60 dBc |
| Broadband noise | −70 dBc (@ +40 dBmV, 5.5 MHz BW) |
| Impedance / return loss | 75 Ω / 14 dB typ |
| Spectral inversion | Auto recognition |
| Carrier suppression | 45 dB |
| SNR / **MER** | 40 dB typ / **39 dB typ** |
| I/Q phase / amplitude | <1° / <1 % |

256B shows "Output QAM Data Rate 5.360500 Mbaud", interleaver "128-1", alpha "12%" (p.24 screenshot). [M]
(Annex B 256-QAM = 5.360537 Msym/s nominal, ~38.81 Mbps payload; 64B = 5.056941 Msym/s, ~26.97 Mbps — [I], see research.md.)

QAM map options: STD, IRC, HRC (p.25). CW mode per module ("Enable CW for QAM Module") for alignment (p.25).

General: 1RU, 19.0 × 18.125 × 1.75 in, 60 W, 12 lb, 0–50 °C operating.

**Adjacency constraint [M]:** each Quad-QAM module outputs 4 **adjacent** channels; user sets only the first channel,
the next three are automatically the next three channel numbers (p.25). So the 16 carriers are 4 blocks of 4 contiguous
6 MHz channels, each block placeable independently in 54–1002 MHz.

## 4. Input contract — what the box accepts

### 4.1 Transport (p.7, p.10, p.19) [M]
- **Clear (unencrypted) UDP or RTP** only. Stream URL syntax `UDP://a.b.c.d:port` or `RTP://a.b.c.d:port`
  ("The only acceptable protocols are UDP or RTP"). Upper-case in examples.
- **Unicast or multicast.** Addresses 224.0.0.0–239.255.255.255 treated as multicast (IGMP join); anything else is
  treated as unicast ("the unit will consider any address outside of this range to be a Unicast address"). For
  unicast, the address entered is the... (example 3 uses 172.160.80.70 — manual does not say whether this is the
  sender's or the EQAM GigE's own address; [I] it is most likely the destination = EQAM GigE address, or the
  firmware matches on dst IP+port. Needs live verification.)
- **"The address entered here MUST be identical to the address of the source stream"**; **no duplicates allowed** —
  duplicate URLs are highlighted yellow on the Edit screen (p.19, fig 6.2.4). [M] Note: this forbids two slots with
  the same group:port — our server must give every SPTS a distinct (ip,port).
- **IGMPv2 or IGMPv3** (global setting, Admin.html item 25). IGMPv3 SSM: up to **2 source IPs per stream**
  ("Source IP" fields, two per row on Edit screen; config file `SPTS Source 1/2 IP0..3`). "Source IP for use in
  IGMPv3 Multicast only" (EAS screen footnote, p.28).
- Which GigE port a stream arrives on: not configurable per stream in the UI shown [M by omission]; the EAS config is
  per-GigE (GIGE1/GIGE2 EAS stream). Pro:Idiom inputs "must be received on a dedicated GbE port" (p.7).
- **Not stated anywhere:** TS packets per datagram (7×188 = 1316 assumed [I]), FEC (Pro-MPEG/SMPTE 2022-1 — not
  mentioned → assume unsupported [I]), jitter-buffer size, max input bitrate per stream other than the 38.8 Mbps/QAM
  rule, RTP payload type requirements, multicast TTL, jumbo frames, VLANs (none mentioned).
- A "Ping" diagnostic exists for unicast sources only (p.27) — [I] suggests the unicast address is the *sender's*
  address (you ping a source), which conflicts with the dst-address interpretation above. Verify live.

### 4.2 Modes (p.7, p.15) [M]
| | SPTS mode | MPTS mode |
|---|---|---|
| Max input streams | **64** SPTS | **16** MPTS |
| Streams per QAM | up to 4 SPTS muxed into one QAM | 1 MPTS → 1 QAM (passthrough) |
| Programs per QAM | 4 | max 6 |
| Total programs | 64 | 96 |
| ES per program | **1 video + up to 1 audio** | **1 video + up to 3 audio** |
| Codecs | MPEG-2 / H.264 video (AC-3 shown in screenshots) | same |
| Rate limit | sum of the 4 SPTS on one QAM ≤ 38.8 Mbps | MPTS ≤ 38.8 Mbps |
| PSI/PSIP | "PSIP manipulation" — box generates VCT from Stream Config | "PSIP pass thru" |
| Web screens | Status/Edit, **Stream Config** (SPTS-only) | Status/Edit, **Stream Info** (MPTS-only) |

- Mode is a global choice on Main > Mode (p.14–15); "This is the first necessary step when configuring the unit for
  the first time." Status table shows up to 64 rows in SPTS mode, 16 in MPTS mode (p.17).
- Slot→QAM mapping in SPTS mode [M from fig 6.2.2/6.3.1]: rows are fixed slots; slot *n* (1-based) feeds RF module
  ⌈n/16⌉, QAM carrier ⌈n/4⌉, sub-program position ((n-1) mod 4)+1, shown as "50-1, 50-2, 50-3, 50-4, 51-1 …".
  Slot 53–56 → "59-1..59-4" in fig 6.2.2 (module 4 started at ch 56? [I] the screenshot montage is spliced).
  In MPTS mode stream row *n* → QAM carrier *n* (50/On, 51/On, …).
- **Codec agnosticism**: manual only claims MPEG-2/H.264; Stream Info screen labels "MPEG-2 Video", "AC3 Audio". [I]
  As a remux/modulator it is likely codec-agnostic for passthrough in MPTS mode, but in SPTS mode it parses PMTs and
  only keeps 1 video + 1 audio, so any extra ES (e.g. SAP audio, captions-as-separate-PID, data) will probably be
  dropped. HEVC/other stream_types: untested/unknown.
- "**Accepts variable and constant bitrate streams**", "**Provides Null packet insertion and deletion**",
  "**Provides PCR correction**" (p.7 features). [M] No detail on how much VBR burst / jitter is tolerated.
- Diagnostics screen has an **"Oversubscription"** column (screenshot p.26) not described in text. [M screenshot]
  [I] there is per-stream/QAM overflow detection; exceeding 38.8 Mbps per QAM → "pixelation issues" (p.17).
- Status screen shows per-stream **Bitrate "Avg/Max"** in Mbps (e.g. "8.4Avg/9.7Max") and IP green/red (p.16).
- TS Sync Error counter per stream (counts missing 0x47 sync bytes) (p.27).
- Stream Name: auto-detected from source "if assigned by the source" (likely SDT service name or VCT short name [I]),
  editable, **≤10 alphanumeric chars** (p.19).

### 4.3 PSI/PSIP handling
SPTS mode — Main > Status > Stream Config (p.22–23) [M]:
- **Out of Band**: Enable/Disable (global). "When Enabled, assigns the OOB bit in the TS packet" — i.e. the
  `out_of_band` flag in CVCT. Note: "Selecting the above two fields would allow the TS packets to be compliant with
  industry standards, but would not affect the input or output configuration."
- **VCT Type**: TVCT or CVCT ("CTCT" typo) (global).
- Per program: **Program Number** (must be unique; fig shows 1–4 repeating per QAM, so unique per QAM),
  **Short Name** (≤7 alphanumeric chars; VCT short_name is 7 UTF-16 chars per A/65), **Major Channel** 1–999,
  **Minor Channel** 1–999 (screenshot defaults are 0/0).
- [I] Therefore in SPTS mode the EQAM **regenerates PAT, PMT (remultiplexed program numbers) and a VCT (plus
  presumably MGT/STT)** per QAM. Whether it passes EIT/ETT (EPG) from inputs or regenerates MGT including them is
  **not stated**. PID remapping is not described but is implied when 4 SPTS with colliding PIDs are muxed [I].
- QAM Config note: "The RF Channel number will be displayed on TV only if the source stream does not carry any
  virtual channel number." (p.25) [M]
- Time screen: "To remain compliant with ATSC and cable standards, it is important to have the accurate date and
  time" → [I] the box generates STT (System Time Table) from its clock (NTP or manual).
- Config file per-SPTS fields include `SPTS Major Channel`, `SPTS Minor Channel`, `SPTS Name`, `SPTS Short Name`.

MPTS mode — Appendix C (p.45) [M]:
- "In MPTS mode, the unit will **pass thru the stream information**." User must ensure program numbers unique
  (1–65535) and major-minor pair unique per MPTS output. Different MPTS outputs may reuse PIDs/program numbers/
  channel pairs. [I] → in MPTS mode **our server must generate PAT/PMT/MGT/CVCT/STT/EIT itself**.
- Stream Info screen (p.20) parses and shows each PMT PID, program number, ES PIDs + types (e.g. 48 PMT 1 /
  49 MPEG-2 Video / 52,55,56 AC3 Audio). PMTs in example at PIDs 48,64,80,96,112,128 (0x30 + 0x10·n).

Not mentioned at all [M by omission]: EIT/ETT/EPG generation, SCTE 65 OOB SI, SDT/NIT (DVB), PID remap UI,
closed captions, SAP/secondary audio (other than "up to 3 audio" in MPTS), statmux/dynamic bitrate, transrating,
scrambling/CAS other than Pro:Idiom, SCTE-35, SNMP.

### 4.4 Pro:Idiom (p.7, p.10, p.45) [M]
Optional encryption of **all** QAM outputs. Requires a license ("An invalid license key was detected. Unit will
proceed without encryption." appears in example event log p.37). Inputs on a dedicated GbE port; null packets
**required**; stream ID user-assigned; Dolby Digital compliant audio; SPTS: 1 program ≤38.8 Mbps; MPTS: ≤6 programs,
inputs **paired** (MPTS #1&2, 3&4, 5&6, 7&8) with identical PMT PIDs, program numbers, ES PIDs, ES count/types;
PMT ≤ 1 TS packet; avoid Pro:Idiom restricted PID range. Status screen has a per-QAM "Pro:Idiom" checkbox (p.18).

### 4.5 EAS (p.7, p.28–29) [M]
- Network-based SCTE-18 EAS **by program substitution**: on an SCTE-18 trigger, *all* video+audio programs are
  replaced with the EAS stream; no SCTE-18-capable TV/STB required.
- Per GigE: EAS Stream Name, EAS Stream Group URL (e.g. `UDP://239.10.10.10:1234`), Source IP 1/2 (IGMPv3).
- EAS Trigger Ethernet Source: Remote Control or Local Control 1; **EAS Trigger Source Port Number** (example 5050).
  **"The unit must be power cycled after a change to EAS port."**
- EAS Test Duration 10/15/30/45/60 s; "Initiate EAS Test"; EAS Override Force Enabled/Force Disabled.
- The trigger message format on that port is not documented here [I: presumably SCTE-18 cable_emergency_alert
  section over UDP].
- Status screen shows rows "GIGE1 EAS / GIGE2 EAS" with "Not Triggered" (p.16).

## 5. All configuration screens & fields

Tabs (p.13): **Main** {Status, Stream Info (MPTS only), Stream Config (SPTS only), QAM Config, Diagnostics, Mode,
EAS, Refresh}, **Network**, **Time**, **Event Log**, **Config**, **Logout**. Hidden: **/Admin.html**.
Every page header: ESN (serial), Headend Name, Temperature (chipset), Mode (SPTS/MPTS), Uptime, Location.
**Every write screen requires clicking SAVE.**

| Screen | Fields (range / default) | Page |
|---|---|---|
| Main > Mode | Output Mode: Single Program Transport Stream / Multiple Program Transport Stream | 14–15 |
| Main > Status (RO) | #, Stream (proto://ip:port), Source (2 lines), Name, IP (green/red), Bitrate Avg/Max, Pro:Idiom, RF Chan (ch-sub / On), Status (Locked) per module; EAS rows; Edit button | 16–17 |
| Main > Status > Edit | per row: Stream URL, Source IP ×2, Name (≤10 chars); Pro:Idiom checkbox per QAM | 18–19 |
| Main > Stream Info (MPTS, RO) | Streams Detected; per stream: PMT PID, program #, ES PID + type | 20–21 |
| Main > Stream Config (SPTS) | Out Of Band (Disabled/Enabled), VCT Type (TVCT/CVCT); per slot: Program Number, Short Name (≤7), Major (1–999), Minor (1–999) | 22–23 |
| Main > QAM Config | RF Module # (1–4); Output Channel/Frequency (first ch; NTSC 2–155, e.g. "50 / 381MHz", others auto +1/+2/+3); Output Control On/Off per carrier; CW Control (per module); Final Output Level 28–41 dBmV (rec. 40); Output QAM Mode 64B/256B/16A/32A/64A/128A/256A; QAM Map STD/IRC/HRC; RO: Data Rate (Mbaud), Interleaver, Alpha, Lock State | 24–25 |
| Main > Diagnostics | #, Stream, IP, Ping (unicast only; Passed/Failed), Oversubscription, TS Sync Errors, Clear Errors | 26–27 |
| Main > EAS | see §4.5 | 28–29 |
| Main > Refresh | refreshes current sub-tab | 29 |
| Network | RO: MACs (Remote, Local 1, GIGE1, GIGE2), Software Version, FPGA1/2A/2B/3A/3B versions, DDQ versions (modules 1–4), Serial. RW: Headend Name, Location, Login Timeout 5/15/30/60 min (screenshot 60). RO mirrors of IPs, log dest, IGMP version | 30–31 |
| /Admin.html (Admin only) | see below | 32–35 |
| Time | Current date (mm/dd/yyyy dropdowns), time (24h), Configuration Method Manual/Automatic (NTP, refresh every 20 min), Auto DST Yes/No, DST start/end date & time | 36 |
| Event Log | Event Log Destination, Log Destination Port (514), SNMP Destination Port (514, "future option"), Clear Log, Lines to Display 1–65535 (screenshot 1000), Save Number of Displayed Lines. Red=error, blue=info, green=feedback. **Log cleared on power loss.** | 37 |
| Config | Select Configuration File [Browse][Submit]; "Download Configuration File: Right-click and choose Save-as … Config File" | 38 |

/Admin.html (p.32–35) [M], numbered as in manual:
1 Login (≤10 chars, default **Admin**) · 2 Current Password (default **pass**) · 3/4 New/Confirm password (≤10) ·
5 Guest Login (default **Guest**) · 6 Current Guest Password (default **pass**) · 7/8 New/Confirm guest password ·
9 **Database Control: "Flush Stream Database"** — removes all IP addresses and stream names ("Service will be
disrupted") · 10 **Config File Control: "Delete Config File"** · 11 **Configuration Mode: Manual / Automatic** +
"Commit Changes to Config File" button · 12 **System Watchdog** Enabled/Disabled (screenshot Enabled) — reboots unit
if OS stops · 13 **System Reboot: "Reboot EQAM Unit"** · 14–16 Remote Control IP/mask/gw (defaults
**172.16.70.1 / 255.255.255.0 / 172.16.70.254**) · 17–19 Local Control 1 IP/mask/gw (screenshot 192.168.253.1/24,
gw 172.16.70.254) · 20 GIGE1 IP/mask/gw (screenshot 192.168.4.3/24 gw 192.168.4.254) · 21 GIGE2 IP/mask/gw
(192.168.4.4/24 gw 192.168.4.254) · 22 Event Log Destination (default **172.16.70.2**) · 23 Log Destination Port
(**514**, fixed) · 24 Time Server IP (default 172.16.70.2; screenshot 129.6.15.28) · 25 IGMP Version Support
IGMPv2/IGMPv3 · 26–28 Syslog Errors / Informational / Feedback enable (defaults Disabled).

Login (p.13) [M]: **Admin / pass** (read-write; **only one Admin session at a time**), **Guest / pass** (read-only;
up to 4 concurrent). Case-sensitive. IE7+ recommended.

## 6. Config file (p.13, p.33, p.38)

- Text file, extension **.dat** (example `config_file2.dat`), downloaded via "Config File" link, uploaded via Browse +
  Submit. Used for backup/restore and cloning units. [M]
- Format (fig 6.11.2, transcribed) [M]:
```
###########################################
#        420B Configuration File Format   #
###########################################
Config_Version=1
###########################################
# SPTS Settings
SPTS_ID=0
SPTS Group IP0=239
SPTS Group IP1=16
SPTS Group IP2=180
SPTS Group IP3=72
SPTS Port=50000
SPTS UDP/RTP=0
SPTS RF CH=155
SPTS Program Number=1
SPTS Source 1 IP0=172
SPTS Source 1 IP1=16
SPTS Source 1 IP2=180
SPTS Source 1 IP3=72
SPTS Source 2 IP0=172
SPTS Source 2 IP1=16
SPTS Source 2 IP2=180
SPTS Source 2 IP3=72
SPTS Major Channel=3
SPTS Minor Channel=1
SPTS Name=QAM501
SPTS Short Name=ch1

SPTS_ID=1
SPTS Group IP0=239 ... IP3=73
... Program Number=2 ... Major Channel=3, Minor Channel=2, Name=QAM502, Short Name=ch2
SPTS_ID=2 ...
```
  SPTS_ID is 0-based (0..63). `UDP/RTP=0` → UDP [I: 1 = RTP]. "SPTS RF CH" appears per stream (value 155 in
  example, odd — [I] may be ignored/derived). Only the SPTS section is shown; MPTS/QAM/network sections unknown.
- **Configuration Mode = Automatic**: the unit re-reads the stored Config file **every 30 minutes** and overwrites any
  mismatching live configuration with the file's contents. To change config permanently: set Manual → edit →
  "Commit Changes to Config File" → set Automatic again (p.33). [M]
- Warning (p.33) [M]: **"While the unit is operating in Automatic Mode, and if the current Config file is deleted or
  updated then the QAM minor channels … will be defaulted to 'zero' and will have to be updated manually"**.
- p.13 [M]: "When EQAM-420B is operating in 'Automatic' Configuration Mode, the user must update the respective config
  file while switching between the SPTS and MPTS modes."
- [I] Highly relevant to "config corruption" symptoms: if Automatic mode is on with a stale/partial config file, the
  unit will silently revert edits every 30 min; and a file covering fewer slots than in use could blank the rest.
  Check Admin.html Configuration Mode first when diagnosing config loss.

## 7. Firmware update (Appendices A & B, p.39–44) [M]

Appendix A — server board / FPGAs ("EPCS" = Altera EPCS serial configuration flash [I]):
- Two PROMs: **PROM1 ← `EPCS_1_ver#.bin`**, **PROM2 ← `EPCS_2_ver#.bin`**. "Not every software update requires
  both PROMs … program both PROMs unless you get a written notice with Release notes to do otherwise." ~10 min.
  → Our files **EPCS_1_1113.bin** (4,602,784 B) and **EPCS_2_1113.bin** (16,777,216 B = exactly 16 MiB) match this
  naming; "1113" is the ver#.
- Step 1: `ftp -A 172.16.70.1` (anonymous FTP!), `bin`, `put EPCS_1_ver#.bin`, `put EPCS_2_ver#.bin`, `bye`
  (scriptable `ftp -A -s:ftpcmd 172.16.70.1`). **Must use binary mode** — ASCII mode corrupts and can brick.
- Telnet: `telnet 172.16.70.1`, login **Admin / pass** (manual's telnet creds — [I] may be separate from web creds),
  `cd home/ftp` (files land in **/home/ftp**), `ls`.
- Step 2: `epcs –e1 EPCS_1_ver#.bin` ("Update FPGA1 by programming EPCS1"); `epcs –e2 EPCS_2_ver#.bin` ("Update
  FPGA2 by programming EPCS2 (if necessary)"). Both can run concurrently in two telnet sessions. **"If you get errors
  during programming then DO NOT TURN OFF THE EQAM-420B, just repeat the epcs commands again."**
- Warning: typos or forgetting `bin` "could cause the EQAM-420B Flash memory to be corrupted and the unit needs to be
  sent back to Blonder Tongue for repair."
- Reboot: "(1) **Reset switch in the back of the unit**" or "(2) Telnet and type **`epcs –c`** — automatically
  reboots". (Rear-panel photo shows no reset switch other than the power rocker — [I] "reset switch" probably means
  the power switch.) The manual's en-dash `–e1` is a typesetting artifact; type ASCII `-e1`.
- [I] epcs flags: `-e1`/`-e2` = erase+program EPCS device 1/2, `-c` = reconfigure FPGA (nCONFIG) → reboot.

Appendix B — Quad-QAM modules ("DDQ"):
- One file **`DDQ_v#.hex`** (Intel HEX) for all four modules; FTP same way; telnet, `cd home/ftp`;
  `bootloader -d1 -w DDQ_v#.hex` … `bootloader -d4 -w DDQ_v#.hex`. ~20 min total. Same don't-power-off advice.
  Reboot via `epcs –c`.

## 8. Recovery / factory reset (precise) [M unless marked]

1. **IP RESET button** (front panel, right of the STATUS LED, recessed): **push and hold ~10 seconds** → resets
   **IP address, Usernames, and Passwords** to: IP **172.16.70.1** (Remote Control port), Username **Admin**,
   Password **pass** (p.8). It does **not** claim to reset stream/QAM config. [I] mask presumably 255.255.255.0 and
   gw 172.16.70.254. **Cross-ref (research.md §5):** the EQAM-420A and AQT8 manuals add "followed by a power
   cycle" / "Power must be reset for this to take effect" — so hold 10 s, then power-cycle.
2. Connect PC to **"10/100 Remote Control"** port (the manual says rear-panel in §4 but the port is on the front per
   p.8 photo) with the supplied crossover cable (BT p/n 741021800); set PC to **172.16.70.2 / 255.255.255.0**, browse
   to **http://172.16.70.1**, log in Admin/pass (p.12–13).
3. Stream/config reset: **/Admin.html → "Flush Stream Database"** (clears all IPs + stream names) and
   **"Delete Config File"**; set Configuration Mode to **Manual**; **Reboot EQAM Unit** (p.32–33).
4. Remote reboot alternatives: Admin.html "Reboot EQAM Unit"; telnet `epcs –c`; power switch.
5. **System Watchdog** (Admin.html) auto-reboots on OS hang (status LED steady/off).
6. Event log hint: "Password file not found. Using default password." appears in a boot log (p.37) — [I] credentials
   are stored in a separate password file; if it is missing/corrupt the unit falls back to Admin/pass.
7. There is **no documented full "factory default" of all settings**, no serial console, no safe-mode/golden image.
   A bad EPCS flash = RMA per manual. [I] Altera EPCS boards often have a JTAG header allowing reflash with a
   USB-Blaster — out of scope of manual.

## 9. Every mention of 48 or 64 [M]

- **64**: stock no. 6522B-**4-64**; UI banner "EQAM-420B-4-**64**"; "accepts **64** HD programs in SPTS mode";
  "up to sixty-four (**64**) … SPTS"; "up to **64** rows of input streams" in SPTS mode (p.17); "accept up to **64**
  programs in UDP or RTP format" (p.15).
- **48**: only appears as a PMT PID example ("48 PMT 1", p.20–21) and as row "48" in the Stream Config screenshot.
  **No stated limit, warning, or known issue involving 48** anywhere in the manual.
- Curiosity: fig 6.3.1 (p.22) is a spliced screenshot: rows 1–11 then jumps to 44–64, and the stream URLs are shown
  with **byte-reversed IPs** ("UDP://36.180.16.239:20675", "73.181.16.239" — i.e. 239.16.18x.yy reversed, and port
  20675 = 0x50C3, the byte-swap of 50000 = 0xC350). [I] This is a firmware **endianness display bug** in that
  pre-release build of the Stream Config page (raw little-endian u32/u16 printed). Worth checking whether our
  firmware's config storage has similar endianness inconsistencies.
- Fig 6.3.1 groups slots 4-per-QAM with RF channels 50, 51, 52, 55, 58, 59, 60, 61 shown in "/ CW" state and two
  "Locked" module groups — a spliced demo screenshot; not consistent with strict 4-adjacent blocks, don't read into it.

## 10. Things the manual says are NOT supported / limits [M]

- Encrypted input streams (inputs must be clear).
- Protocols other than UDP/RTP.
- Duplicate stream addresses.
- SPTS with >1 video or >1 audio; MPTS with >6 programs or >3 audio per program; >38.8 Mbps per QAM.
- Ping for multicast streams.
- Local Control #2, Remote Data Out, SNMP: "reserved for future use"/"future option".
- DHCP on the Remote Control port ("Only static IP address can be assigned").
- 10/100 control and GigE on same subnet / same unmanaged switch.
- Event log does not survive power loss; "Save Number of Displayed Lines" doesn't persist the log.

## 11. Responsibilities summary [I, derived from the above]

| Function | SPTS mode | MPTS mode |
|---|---|---|
| PAT/PMT | EQAM regenerates (program number from Stream Config) | passthrough — server |
| VCT (TVCT/CVCT), channel numbers, short names | EQAM generates from Stream Config | server |
| MGT/STT | EQAM presumably (has NTP/time) | server |
| EIT/ETT (EPG) | not documented — likely dropped; test | server (passthrough) |
| Secondary audio (SAP) | **not possible** (1 audio max per SPTS) | up to 3 audio ES per program |
| Mux of multiple programs into one QAM | EQAM (4 SPTS → 1 QAM) | server builds MPTS |
| Bitrate budget | sum of 4 SPTS ≤ 38.8 Mbps — server must cap | MPTS ≤ 38.8 Mbps — server must cap |
| Null stuffing / PCR | EQAM inserts/deletes nulls, "PCR correction" | same (presumably) |
| EAS | EQAM substitution on SCTE-18 trigger | same |
| Statmux / transrating | none — server must do rate control | none |
