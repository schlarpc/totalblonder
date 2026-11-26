# EQAM-420B — external research (Part B)

Companion to `notes/manual.md` (digest of our manualslib copy of the EQAM-420B User Manual, doc 651234700A
issue 1). Everything downloaded is under `docs/` in its own subdirectory and is **untrusted data**.
**[S]** = stated by the cited source; **[I]** = my inference. Research date 2026-11-24.

---

## 1. Headline findings

1. **Official Blonder Tongue firmware share is public and holds exactly our firmware.** Linked from
   https://www.blondertongue.com/support/firmware-updates/ ("FIRMWARE DOWNLOAD SITE") →
   https://blondertonguecom.sharepoint.com/:f:/s/BTWebFirmware/Eu8XaD7kWzdKgTJg9lKNcqoB65dDYg8F7kXziag8ZiIgqw?e=guAr2E
   Folder `Firmware/BLONDER TONGUE/E -/EQAM-420B_Stk# 6522B/` contains `EPCS_1_1113.bin` (4,602,784 B),
   `EPCS_2_1113.bin` (16,777,216 B) and `Readme.txt`. **SHA-256 of the SharePoint copies equals our
   `docs/external/eqam-420b.zip` contents** (EPCS_1 b31aa3c2…b7f, EPCS_2 975a580a…38c). So 1113 (= v1.1.13) is the latest
   (and only) published 420B firmware. No newer build exists publicly. [S, verified]
2. **The 420B changelog (Readme.txt) — verbatim:**
   ```
   v1.1.13 changes
     added ability to use octet '0' & '255'
     added update I2C controller for reading/writing to EEPROM (EEPROM corruption of IP addess)
   v0.1.12 changes
     Added router alert to join group requests.
   v0.1.11 changes
     Admin & Network Pages: Added gateway functionality for GigE Ping behavior. Removed references to SNMP.
     Status Edit Page: enter octet of 0 in address bar of incoming stream (status/edit page)
     Misc: LED orange/red issue constants were reversed
     License: Pro:Idiom is licensed, unit will be "420B-IP" instead of "420B" with vaild license
   v0.1.10  inital firmware release
   ```
   (`docs/external/bt-firmware-sharepoint/EQAM-420B_6522B/Readme.txt`)
   - Implications [I]: (a) **config/IP settings live in an I2C EEPROM, and BT acknowledged an EEPROM corruption
     bug** fixed in 1.1.13 — the only vendor-acknowledged "config corruption" issue found anywhere; if our unit runs
     <1.1.13, upgrade first. (b) The manual (0.1.0 Prerelease 1 screenshots) predates all of these. (c) **SNMP was
     removed in 0.1.11** — the 2014 sales deck's "SNMPv2 traps" claim is stale. (d) Pre-0.1.12 builds sent IGMP joins
     without Router Alert (some switches/snoopers drop such reports). (e) Pre-1.1.13, stream/IP addresses containing
     an octet 0 or 255 were rejected.
3. **EQAM-420A changelog (same codebase, 2 QAM modules/32 SPTS)** is much more detailed and describes the
   platform internals (`docs/external/bt-firmware-sharepoint/EQAM-420A_6522A/readme.txt`) [S]:
   - v1_0_16: same I2C/EEPROM-corruption fix and octet 0/255 fix as 420B 1.1.13 (→ the two lines share code).
   - v1_0_15: "fixed bug with generating new pmt table in SPTS mode." → **the box generates PMTs in SPTS mode.**
   - V1_0_14: "Fixed bug in crc generator used by **VCT and MGT** tables." → **box generates VCT + MGT.**
     Also: firmware ping block in FPGA1 on the GigE; previously GigE subnet mask was effectively
     255.255.255.252 and off-subnet traffic went via **uClinux** out the 10/100 port.
   - V1_0_13: handle `data_alignment_indicator` set in audio PES without AC-3/MPEG sync word following.
   - V1_0_12: fixed IGMP join packets not being sent; fixed PMTs with adaptation field; TS-packet watchdog.
   - v1_0_11: MPTS **multiplex buffer leak-rate model** fixed (used max instead of min — caused tiling).
   - v1_0_10: SPTS "post ddr TS errors" — bad DDR2 controller. v1_0_8: "system buffer models work on the individual
     programs". v1_0_7: avg bitrate summed over 64 s; "MPTS add register control to prohibit forced stuffing packets
     except when Pro:Idiom is enabled". V1_0_6p8: AC-3 audio ES buffer 3584 B without ATSC framework, 2592 B with.
   - [I] So even in "passthrough" MPTS mode the FPGA **re-schedules packets against T-STD buffer models and parses
     PES/audio sync** — it is not a dumb bit pipe. Feed it ATSC/SCTE-54-conformant T-STD streams (AC-3 with
     stream_type 0x81, MPEG-2 or H.264 video); exotic codecs (HEVC, E-AC-3, AAC) are unverified/likely unsupported.
4. **Nothing public about a "48 channel" problem.** Searches of reddit, avsforum, dslreports, eevblog, hackaday,
   github, SiliconDust forum, videohelp, ffmpeg-user (via a sub-agent, ~34 queries/fetches) found **no
   discussion of the EQAM-420 family at all**, no config-corruption reports and no working ffmpeg/tsp recipes for
   it. Closest vendor-documented analogs:
   - The EEPROM-corruption fix above (1.1.13).
   - **Config-file "Automatic" mode** re-applies the stored config file every 30 min and zeroes minor channels if
     the file is deleted/updated (manual p.33; 420A manual §6.7.1). The **420A config-file format required exactly
     one line per slot** (16/32 lines SPTS, 4/8 MPTS) — "regardless of the actual number of active incoming streams"
     (`docs/external/eqam-420a-manual-avprosupply/eqam-420a-1-16-manual.pdf` p.32–33). A config file/EEPROM image covering
     fewer slots than in use is a plausible mechanism for partial loss. [I]
   - Sister AQT8 family has a **hard 20-program output limit**; adding a 21st silently drops the "last" program
     (Technical Bulletin 651245100A, 2017-06-05, `docs/external/bt-firmware-sharepoint/AQT8-QAM_IP_6281_6281A/…pdf`). Shows
     BT firmware of this era enforces limits poorly. [S]
   - [I] Hypothesis for the RE team: 48 = 3 modules × 16 slots, or an EEPROM page/size boundary (e.g. N bytes per
     SPTS record × 48 crossing a 24Cxx size/page limit). Worth checking the EEPROM layout in EPCS_1 (NIOS/uClinux
     image) — look for the I2C EEPROM driver touched in 1.1.13.

## 2. Model decoding and product family

| Model | Stock # | QAM modules / carriers | Inputs | Notes | Source |
|---|---|---|---|---|---|
| EQAM-400 | ? | 2 / 8 QAM | 1 GbE, 16 HD | first EdgeQAM (2009, World Cinema hospitality, Pro:Idiom) | BT FY2010 10-K https://www.sec.gov/Archives/edgar/data/1000683/000130794211000013/blonder10k031711.htm ; Toner Cable rebadge manual https://www.manualsdir.com/manuals/275585/toner-cable-eqam-400-edgeqam-with-proidiom.html |
| EQAM-420A-1-16 | 6522A-1-16 (P = Pro:Idiom) | 1 / 4 | 1 GbE, 16 SPTS or 4 MPTS | | 420A manual cover |
| EQAM-420A-2-32 | 6522A-2-32 (P) | 2 / 8 | 1 GbE, 32 SPTS or 8 MPTS | 54–996 MHz, +35 dBmV, SNMPv2 | 420A manual; https://files.avprosupply.com/files/attachments/2663/blonder-tongue-eqam-420a-1-16-manual.pdf |
| EQAM-420A-PIL | 6522-PIL | — | — | software option adding Pro:Idiom | same |
| **EQAM-420B-4-64** | **6522B-4-64** | **4 / 16** | **2 GbE, 64 SPTS or 16 MPTS (96 progs)** | adds EAS (SCTE-18 substitution), 2nd GbE, 54–1002 MHz, +40 dBmV | 420B spec sheet Rev 131216 (`docs/external/bt-wayback/EQAM-420B_Spec_2016.pdf`) |
| EQAM-430A | ? | 1–2 / 8 | 16× DIRECTV COM23-600 | Pro:Idiom only, 2 HD/QAM | `docs/external/bt-wayback/EQAM-430A_Spec.pdf` |
| AQT8-QAM / AQT8-IP / AQT8-QAM/IP | 6288 / 6280 / 6281(A) | 8 QAM | 8 RF tuners (8VSB/QAM) + IP | same NIOS/EPCS/DDQ platform; 20-program limit | SharePoint AQT8 folders; AQT8-QAM/IP manual (`docs/external/sister-aqt8-manual/`) |
| AQT8B | 6281B | | | **different** platform (Linux `.fw`, SSH) | SharePoint |
| IPAT | 6510 | — | GbE↔ASI | also EPCS_1/EPCS_2 files | SharePoint |

- **"-4-64" = 4 Quad-QAM modules, 64 SPTS** [S: spec sheet "EdgeQAM-420B equipped with 4 Quad-QAM output modules
  (accepts 64 unencrypted HD programs)"; 420A pattern -1-16/-2-32]. Only -4-64 was ever listed for the 420B
  [S: spec sheet ordering info], although the 2014 deck says "Configurable with 1 to 4 QAM modules".
- **A vs B** [S, from the two manuals/specs]: B adds a second GbE input, EAS (SCTE-18), local-control ports, status
  LED, 4 modules (vs 2), 1002 MHz upper edge, +40 dBmV (vs +35), and drops SNMP. Same firmware lineage (shared
  fixes), same web UI (Admin.html, Config file, epcs/bootloader update tools).
- **"420B-IP"**: banner string shown when a valid Pro:Idiom license is installed (420B readme 0.1.11) [S].
- **OEM?** No evidence of a foreign OEM [I, medium confidence]. BT's 10-K says it "currently manufactures most of
  its digital products, including the latest encoder and EdgeQAM collections" in-house; licenses listed are LG
  (Pro:Idiom), DTLA, MPEG-2 encoder core. Platform clues from AQT8 readme [S]: Altera FPGAs ("CPU_fpga", "MUX_fpga"),
  NIOS + **uClinux** (`table_server`, `bcm_loop` processes), **DDQ** quad-QAM daughterboard with a microcontroller
  (updated with `bootloader`) and a **Commsonic** modulator core ("Firmware validating packets before Commsonic
  core") — Commsonic is a UK FPGA modem-IP vendor; TI GigE PHY (AQT8 bulletin 651244900A). The web UI pattern
  (Admin.html etc.) appears across BT's own HDE/SDE products. Toner Cable sold a rebadged EQAM-400 (Toner is a
  distributor → it's BT's design rebadged, not the reverse).
- **EPCS factory image**: AQT8 readme 1.0.3: "Epcs application updated with f-option. Use EPCS_Factory_1.bin to
  update factory image." [S for AQT8] → [I] the platform uses Altera remote-update with a **factory (golden) image +
  application image** in EPCS1; `epcs -e1` writes the application image, `epcs -f` the factory one. This matters
  for recovery: a bad application image may fall back to factory. Verify in our EPCS_1_1113 layout.
- AQT8 readme also: "If unit updated with wrong MUX fpga revision (EPCS_2.bin file), MUX fpga revision on EAS and
  Network pages will be displayed in red." [S]

## 3. Input contract (consolidated; see manual.md §4 for page refs)

| Item | Value | Source |
|---|---|---|
| Transport | clear MPEG-2 TS over **UDP or RTP**, unicast or multicast (224–239 = multicast) | manual p.7, p.19; spec sheet |
| IGMP | v2 or v3 (global), up to 2 SSM sources/stream; joins carry Router Alert since 0.1.12 | manual p.34; readme |
| Input ports | 2× GbE (each its own IP/mask/gw); 10/100 control port must be on a different subnet and must not see multicast | manual p.12, p.34 |
| SPTS mode | 64 SPTS, 4 per QAM, each **1 video + ≤1 audio**; box regenerates PAT/PMT/VCT/MGT; "PMT and PID modification support" | manual; 2014 deck slide 7 (`docs/external/bt-wayback/EQAM-420B-Clear_140207v2_Presentation.pdf`); 420A readme |
| MPTS mode | 16 MPTS, 1 per QAM, ≤6 programs, each 1 video + ≤3 audio; "PMT & PID values are passed thru without change"; "PSIP pass thru" | same |
| Rate | ≤ 38.8 Mbps per QAM (256-QAM Annex B); VBR and CBR accepted; avg/max bitrate shown (avg over 64 s) | manual; 420A readme v1_0_7 |
| Nulls / PCR | null insertion & deletion; "PCR correction" | spec sheet |
| Codecs | MPEG-2 / H.264 video; AC-3 audio (Pro:Idiom requires Dolby Digital) | spec sheet |
| Example densities (vendor) | SPTS: 2–3 HD MPEG-2, 2 HD + 2 SD MPEG-2, 4 SD MPEG-2, **4 HD H.264** per QAM; MPTS: up to 6 SD MPEG-2 or 5–6 HD H.264 per QAM | 2014 deck slide 8 |
| Not documented | packets per datagram, FEC (SMPTE 2022-1), jitter-buffer depth, VLAN, jumbo frames | — |

[I] Recommended feed from our server until live-verified: 7×188 = 1316-byte UDP payloads (no RTP initially), CBR
per stream with null padding (box deletes nulls anyway), PCR every ≤40 ms (ffmpeg default 20 ms in CBR),
PAT ≤100 ms / PMT ≤400 ms repetition, AC-3 ≤448 kbps (avoid 512–640 kbps: AQT8 notes needed a bigger AC-3 buffer for
those), distinct (group,port) per stream, sum per QAM ≤ ~37.5 Mbps to leave headroom for the box's own PSIP and
PCR jitter. **Never use ffmpeg `-mpegts_m2ts_mode 1`** (192-byte packets) — a 2015 ffmpeg-user thread shows a
modulator rejecting such a feed with PAT/PMT faults (https://ffmpeg.org/pipermail/ffmpeg-user/2015-July/027437.html,
follow-up https://ffmpeg.org/pipermail/ffmpeg-user/2015-July/027449.html, no resolution) [S for the symptom, I for
the cause].

ffmpeg CBR facts [S]: `-muxrate` switches the mpegts muxer to CBR with null stuffing and `pcr_period` defaults to
20 ms in CBR (https://lists.ffmpeg.org/pipermail/ffmpeg-devel/2016-March/192046.html,
https://lists.ffmpeg.org/pipermail/ffmpeg-cvslog/2019-August/118726.html); known CBR overshoot/undershoot bug
https://trac.ffmpeg.org/ticket/8048. TSDuck wiki: hardware modulators need CBR input, obtained by null stuffing
(https://github.com/tsduck/tsduck/wiki/Dektec-use-case:-Multicast-(CBR)-to-DVB-T). Untested template [I]:
`ffmpeg … -c copy -f mpegts -muxrate 9M -pcr_period 20 -mpegts_flags +resend_headers 'udp://239.x.y.z:5000?pkt_size=1316'`
(or `tsp -I … -P regulate … -O ip 239.x.y.z:5000 -p 7`).

## 4. Feature support matrix — who must do what

| Function | EQAM, SPTS mode | EQAM, MPTS mode | Our server must… | Basis |
|---|---|---|---|---|
| PAT/PMT | regenerated (program # from Stream Config) | passed through | MPTS: build correct PAT/PMT | 420A readme v1_0_15; deck |
| PID remap | yes ("PMT and PID modification") | no | MPTS: pick non-colliding PIDs | deck slide 7 |
| VCT (TVCT/CVCT), channel #, short name | generated from UI (major/minor 1–999, 7-char name) | passed through | MPTS: generate CVCT | manual p.22–23; readme v1_0_14 |
| MGT / STT | generated (has NTP + "ATSC compliant time") | passed through | MPTS: generate MGT+STT | readme v1_0_14 (MGT); manual p.36 [I for STT] |
| EIT/ETT (EPG) | **not documented; probably dropped** (box's MGT likely lists no EITs) | probably passed if on 0x1FFB/MGT-listed PIDs — **unverified** | MPTS: generate EIT-0..3 + ETT, list in MGT | [I]; AQT8 note that muxer passes "only packets with tables and video/audio data" |
| SAP / 2nd-language audio | **impossible** (1 audio/SPTS) | up to 3 audio ES/program | MPTS: multiple AC-3 ES with ISO 639 + AC-3 descriptors | manual/spec |
| Closed captions | pass (CEA-708 rides in video user data) [I] | pass [I] | keep 708 in video; add caption_service_descriptor | A/65, A/53 |
| Muxing several programs per QAM | box muxes 4 SPTS | server builds MPTS | MPTS: statmux/CBR to ≤38.8 | manual |
| Bitrate management / statmux / transrating | **none** — rejects nothing, just pixelates on overflow ("Oversubscription" column) | none | cap every QAM's sum; CBR | manual p.17, p.26 |
| Null stuffing, PCR restamp | box does | box does | still send clean PCR | spec sheet |
| EAS | box substitutes all programs on SCTE-18 trigger (UDP port, default 5050 in screenshot), EAS stream per GigE | same | optionally provide EAS A/V stream at ≤ lowest program bitrate [I from AQT8 manual: "EAS program bit rate must not exceed the lowest program video bit rate it will replace"] | manual p.28–29; AQT8 manual |
| Scrambling | Pro:Idiom only (licensed) | same | nothing | manual |
| SNMP | **removed** (0.1.11) | | use syslog (UDP 514) | readme |

## 5. Recovery / factory reset (cross-source)

- **IP RESET button** (front, by STATUS LED): hold ~10 s → IP 172.16.70.1, Admin/pass, Guest/pass (420B manual
  p.8). The **420A manual adds "followed by a power cycle"** and the AQT8-QAM/IP manual says "NOTE: Power must be
  reset for this to take effect" [S] → **hold 10 s, then power-cycle.**
- PC at 172.16.70.2/24 on the front "REMOTE CONTROL" 10/100 port, http://172.16.70.1, Admin/pass.
- Stream config wipe: http://172.16.70.1/Admin.html → "Flush Stream Database", "Delete Config File", Configuration
  Mode = Manual, "Reboot EQAM Unit".
- Telnet (Admin/pass) + anonymous FTP to /home/ftp exist; `epcs -c` reboots.
- Boot log string "Password file not found. Using default password." (manual p.37) → creds stored in a file;
  absence = defaults [I].
- No documented golden-image recovery; [I] check for EPCS factory image (see §2) and JTAG.

## 6. Firmware update procedure (all sources agree)

`ftp -A <ip>` (anonymous), `bin`, `put EPCS_1_<ver>.bin`, `put EPCS_2_<ver>.bin`; `telnet <ip>` Admin/pass;
`cd /home/ftp`; `epcs -e1 EPCS_1_<ver>.bin`; `epcs -e2 EPCS_2_<ver>.bin` (can run concurrently in two sessions;
on error repeat, never power off); `epcs -c` to reboot. DDQ modules: `bootloader -d<1..4> -w DDQ_v#.hex`.
Wrong file / ASCII-mode transfer can brick ("returned to BT for repair"). BT firmware page: "Use the Stock Number…
Installing the wrong files can crash the unit" (https://www.blondertongue.com/support/firmware-updates/). File naming
`EPCS_<prom#>_<ver w/o dots>.bin` — 1113 = v1.1.13; 420A uses 1015/1016 (v1.0.15/1.0.16); AQT8 uses suffixes like
`1010p17`, `_IP`, `_QAM`. No DDQ hex is published for the 420B.

## 7. Standards cheat-sheet (what the cable-ready TV needs)

- **ITU-T J.83 Annex B / ANSI/SCTE 07** (US cable QAM): 6 MHz, 64-QAM 5.056941 Msym/s → **26.97035 Mbps**
  TS payload; 256-QAM 5.360537 Msym/s → **38.81070 Mbps**; RS(128,122) + trellis + MPEG framing; variable
  interleaver (box shows 128-1 for 256B). The box displays "5.360500 Mbaud" for 256B. https://www.itu.int/rec/T-REC-J.83 ;
  SCTE 07 https://www.scte.org/documents/6863/SCTE_07_2018_R2024.pdf ; symbol-rate confirmation
  https://www.manualslib.com/manual/738761/Cisco-Goqam.html?page=171 . (Payload figures are the standard SCTE 07
  numbers; the search did not surface a page quoting them verbatim — verify against SCTE 07 §6/Table.)
- **SCTE 54** (in-band cable multiplex; `docs/external/standards/SCTE_54_2009_lawresource.pdf`, from
  https://law.resource.org/pub/us/cfr/ibr/005/scte.54.2009.pdf; current: https://www.scte.org/documents/6941/SCTE_54_2024.pdf) [S]:
  - PAT repeat ≤100 ms, PMT ≤400 ms, no adaptation fields in PAT/PMT packets, one program per PMT PID, ≤1 MPEG-2
    video per program, at least one complete-main (CM) audio.
  - SI "shall include … at a minimum: MGT, STT, and CVCT or TVCT" on PID 0x1FFB; RRT only for non-US regions.
    Max cycle: STT 10 s, MGT 150 ms, CVCT 400 ms, RRT 60 s; EIT-0 recommended ≤500 ms; base_PID/EIT/ETT streams
    ≤250 kbps each (T-STD sb_leak_rate 625, sb_size 1024).
  - "In the absence of a functioning POD module, receivers are expected to support navigation only to unscrambled
    services that are referenced in an in-band Virtual Channel Table" → **in-band CVCT is what makes a clear-QAM TV
    show channel names/numbers**; EPG data (EIT/ETT per A/65) is optional "at the option of the cable operator".
  - CVCT one-part numbers: major's top 6 bits = 111111, number = ((major & 0xF) << 10) + minor. (Sister AQT8
    readme: minor 0 + Cable ⇒ one-part number "5" [S, AQT8 only].)
  - Cable EAS: cable_emergency_alert() (SCTE 18) on PID 0x1FFB.
- **ATSC A/65** PSIP (MGT 0xC7, TVCT 0xC8, CVCT 0xC9, RRT 0xCA, EIT 0xCB, ETT 0xCC, STT 0xCD; service_location_descriptor
  0xA1 lists component stream_types/PIDs/ISO-639 languages; extended_channel_name 0xA0):
  https://www.atsc.org/standard/a652013-program-and-system-information-protocol-for-terrestrial-broadcast-and-cable
  (atsc.org PDF 403'd to curl). Overview https://en.wikipedia.org/wiki/Program_and_System_Information_Protocol
- **SCTE 65** — out-of-band SI for set-top boxes/CableCARD; **not used by clear-QAM TVs** without a CableCARD [I];
  `docs/external/standards/SCTE_65_2008_lawresource.pdf` (https://law.resource.org/pub/us/cfr/ibr/005/scte.65.2008.pdf).
  The EQAM's "Out of Band" checkbox only sets the CVCT out_of_band bit (manual p.23).
- **SAP / secondary audio** [I, from A/53-3, A/52 Annex A, A/65]: each audio is its own ES in the PMT —
  stream_type 0x81 (AC-3) with registration "AC-3", **AC-3 audio descriptor (tag 0x81)** carrying bsmod
  (0 = complete main, 2 = visually impaired, 3 = hearing impaired …) and language code, plus
  **ISO_639_language_descriptor (tag 0x0A)**; the CVCT service_location_descriptor lists each audio component with its
  ISO-639 code. TVs present additional CM-language tracks or VI (bsmod 2) as "SAP"/"Audio language".
  → only possible in **MPTS mode** (≤3 audio/program) on this box.
- **SCTE 18 / J-STD-042 / CEA-814** EAS: https://account.scte.org/standards/library/catalog/scte-18-emergency-alert-messaging-for-cable/
  (preview https://webstore.ansi.org/preview-pages/SCTE/preview_ANSI+SCTE+18+2007.pdf). The EQAM does
  "network-based EAS switching … by program substitution", so TVs need no SCTE-18 support; our server need not emit
  SCTE-18 tables [S manual; I for our responsibility].
- Channel plans STD/IRC/HRC = CEA-542 (box offers all three) [S manual p.25].

## 8. Documents obtained (all under `docs/`, untrusted)

| Path | What | Source URL |
|---|---|---|
| `docs/external/bt-firmware-sharepoint/EQAM-420B_6522B/Readme.txt` | **420B firmware changelog v0.1.10→v1.1.13** | BT SharePoint (link above), folder `Firmware/BLONDER TONGUE/E -/EQAM-420B_Stk# 6522B` |
| (not re-downloaded) `EPCS_1_1113.bin`, `EPCS_2_1113.bin` | identical to `docs/external/eqam-420b.zip` (SHA-256 verified) | same |
| `docs/external/bt-firmware-sharepoint/EQAM-420A_6522A/readme.txt` | 420A changelog v1.0.6p7→v1.0.16 | `…/EQAM-420A_Stk# 6522A` |
| `docs/external/bt-firmware-sharepoint/EQAM-420A_6522A/EPCS_{1,2}_1016.bin` | 420A firmware v1.0.16 (diff target vs 420B 1.1.13 — shares the EEPROM fix) | same |
| `docs/external/bt-firmware-sharepoint/EQAM-420A_6522A/Ver. 1015/EPCS_{1,2}_1015.bin` | 420A v1.0.15 (diff 1015→1016 isolates the EEPROM/I2C fix) | same |
| `docs/external/bt-firmware-sharepoint/AQT8-QAM_IP_6281_6281A/EPCS_{1,2}_1015.bin`, `Read me.txt`, two Technical Bulletin PDFs | sister platform firmware + very detailed changelog | `…/A -/AQT8-QAM_IP_Stk# 6281_6281A` |
| `docs/external/bt-firmware-sharepoint/AQT8-QAM_6288/Read me.txt` | AQT8-QAM changelog | `…/A -/AQT8-QAM _ Stk# 6288` |
| `docs/external/bt-firmware-sharepoint/AQT8B-QAM_IP_6281B/**.txt` | AQT8B (different Linux platform) release notes | `…/A -/AQT8B-QAM_IP_Stk# 6281B` |
| `docs/external/eqam-420a-manual-avprosupply/eqam-420a-1-16-manual.pdf` | EQAM-420A Instruction Manual, doc 651226500C issue 3, 2012-12-14 (real InDesign PDF) | https://files.avprosupply.com/files/attachments/2663/blonder-tongue-eqam-420a-1-16-manual.pdf |
| `docs/external/bt-wayback/EQAM-420B_Spec_2016.pdf` | **official 420B spec sheet, Rev. 131216** | https://web.archive.org/web/20160505175318/http://www.blondertongue.com/UserFiles/file/2014_Documents/Specs/EQAM-420B_Spec.pdf (also `/UserFiles/file/documents/specs/EQAM-420B_Spec.pdf`, identical) |
| `docs/external/bt-wayback/EQAM-420B-Clear_140207v2_Presentation.pdf` | BT sales deck 2014-02-07 (modes, densities, "PMT and PID modification") | https://web.archive.org/web/20150807033229/http://www.blondertongue.com:80/UserFiles/file/2014_Documents/Presentations/EQAM-420B-Clear_140207v2%20Presentation.pdf |
| `docs/external/bt-wayback/EdgeQAM420A_Spec.pdf`, `EdgeQAM420A_Presentation.pdf` | 420A spec + 2011 deck | wayback `/UserFiles/file/documents/specs/EdgeQAM420A_Spec(2).pdf`, `/UserFiles/file/Solutions/documents/EdgeQAM420A_Presentation.pdf` |
| `docs/external/bt-wayback/EQAM-430A_Spec.pdf` | DIRECTV sister | wayback `/UserFiles/file/documents/specs/EQAM-430A_Spec(1).pdf` |
| `docs/external/bt-wayback/EQAM_and_IPTV_Summary_Bifold_0912.pdf` | 2012 product bifold | wayback `/UserFiles/file/documents/EQAM%20and%20IPTV%20Summary%20Bifold_0912.pdf` |
| `docs/external/bt-wayback/product_page_eqam-420b-4-64_2021.html` | archived product page (Oct 2021, "Sale – Limited Availability") | https://web.archive.org/web/20211022133554/https://www.blondertongue.com/eqam-420b-4-64.html |
| `docs/external/sister-aqt8-manual/AQT8-QAM-IP_UserManual_fullcompass.pdf` | AQT8-QAM/IP manual issue 5 (2017) — same platform; IP-reset needs power cycle; "no CVCT/TVCT if the output TS has more than 3 programs"; EAS bitrate rule | https://www.fullcompass.com/common/files/32315-BlonderTongueAQT8QAMIPUserManual.pdf |
| `docs/external/standards/SCTE_54_2009_lawresource.pdf` | SCTE 54 2009 | https://law.resource.org/pub/us/cfr/ibr/005/scte.54.2009.pdf |
| `docs/external/standards/SCTE_65_2008_lawresource.pdf` | SCTE 65 2008 | https://law.resource.org/pub/us/cfr/ibr/005/scte.65.2008.pdf |

## 9. Documents that exist but could not be fetched

| Document | URL | Why |
|---|---|---|
| Official EQAM-420B **User Manual** (2.32 MB) and **Spec Sheet** (703.88 kB) attached to the old Magento product page | https://www.blondertongue.com/amfile/file/download/file/140/product/3/ (manual), …/file/28/product/3/ (spec) | product page now 404; attachments never archived by Wayback. The manual is very likely the same doc 651234700A we have (no later issue found anywhere). |
| manuals.online copy of 420B manual | https://manuals.online/blonder-tongue/i-o-system/eqam-420b/user-manual/1idkn | 403 to automated fetch; same document as ours per snippets |
| SCTE 07 2018 (R2024), SCTE 54 2024 | https://www.scte.org/documents/6863/SCTE_07_2018_R2024.pdf , https://www.scte.org/documents/6941/SCTE_54_2024.pdf | scte.org 403s curl and browser fetch; free with browser download/registration |
| ATSC A/65:2013 | https://www.atsc.org/wp-content/uploads/2015/03/Program-System-Information-Protocol-for-Terrestrial-Broadcast-and-Cable.pdf | 403 to curl; free from atsc.org in a browser |
| SCTE 18 2018 | https://account.scte.org/standards/library/catalog/scte-18-emergency-alert-messaging-for-cable/ | requires SCTE account |
| AQT8-QAM "RN_AQT8_20140620.pdf" release notes | referenced in AQT8-QAM `Read me.txt` | not on the share |
| Toner Cable EQAM-400 manual | https://www.manualsdir.com/manuals/275585/toner-cable-eqam-400-edgeqam-with-proidiom.html | HTML page-by-page viewer, not fetched (older 8-QAM product) |
| SiliconDust forum thread referencing a Cisco RFGW-1-D homelab video | https://forum.silicondust.com/forum/viewtopic.php?p=408889 | 403 |
| Other BT firmware on the share (HDE-*, SDE-*, IPAT EPCS, NXG, etc.) | BT SharePoint | not needed; IPAT (6510) also uses EPCS_1/EPCS_2 naming if a third same-platform sample is wanted |

## 10. Documents we still need

1. Any 420B **DDQ_v#.hex** (quad-QAM module firmware) — not on the share; manual screenshots show DDQ 1.4.
2. A later 420B manual issue (if one exists) documenting 1.1.x firmware behavior, esp. config-file format for MPTS
   and QAM sections — ask BT tech support (732-313-4217 / 800-523-6049) quoting stock 6522B-4-64.
3. Pro:Idiom restricted PID range document (Zenith/LG) — only if we care about the encrypted path.
4. SCTE 07, SCTE 54 2024, A/65:2013, A/53 Part 3, A/52 Annex A, SCTE 18 — fetch manually in a browser.
5. The SCTE-18 trigger packet format the EQAM expects on its EAS trigger UDP port (not in any BT doc found) —
   firmware RE target.
6. Any user report of the 48-channel / config-loss behavior — none exists publicly; our own observations will be
   the primary source.
