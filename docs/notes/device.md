# EQAM-420B-4-64 live device survey (10.42.0.184)

Read-only survey of the running unit. Date of survey: 2026-11-24 (device clock: 2026-11-25 UTC).
Raw captures are in `work/device/` (gitignored): `web/` (every page as served), `config_file.dat`
(web config export), `ftp/mirror/fs/` (read-only filesystem pull), `telnet/` (shell transcripts).
Scripts: `tools/eqam_login.sh`, `tools/eqam_get.sh`, `tools/eqam_forms.py`,
`tools/eqam_ftp_mirror.py`, `tools/eqam_telnet.py`. All read credentials from `rbw` at run time.

Legend: **[V]** verified by observation, **[I]** inferred.

## 1. Surface summary

| Port | Service | Notes |
|------|---------|-------|
| 21/tcp | GNU inetutils ftpd 1.4.1 (`/bin/ftpd -l` from inetd), uClinux 2.6.26-rc6 | Anonymous login works but is chrooted to an empty `/home/ftp`. **Web Admin credentials log in with full read access to `/` as root.** [V] |
| 23/tcp | telnetd from inetd, `uClinux login:` | **Web Admin credentials give a root (uid 0) `sash` 1.1.1 shell.** [V] |
| 80/tcp | Boa 0.93.15, DocumentRoot `/home/httpd`, runs as root | CGI web UI. [V] |
| 161/udp | not running | `snmpd` start is commented out in `/etc/rc`; no process and no socket on 161 in `/proc/net/udp`. The config still carries SNMP fields. [V] |
| UDP listeners | 5050 (0x13BA) and 59810 (0xE9A2) on 0.0.0.0 | Owner process not identified. Likely `eas_soft_trigger`, `logging_server` or `web_command_monitor`. [I] |

Other TCP ports are closed. A UDP scan was not possible (`nmap -sU` needs root). The UDP list comes from `/proc/net/udp` instead.

**Running firmware = EPCS_1_1113 image [V]:** all 96 non-empty files pulled over FTP (`/bin`, `/etc`, `/home/httpd` incl. every CGI, `/sbin`, `/usr`, `/lib`) are byte-identical to `work/firmware/epcs1/rootfs`. The only exception is the generated `/home/httpd/config_file.dat`.

Platform [V]: Altera **NIOS II** soft CPU at 125 MHz, no MMU (uClinux), 128 MB RAM. Kernel `uClinux 2.6.26-rc6 #11804 PREEMPT Thu May 14 16:01:07 EDT 2015`, gcc 3.4.6, built by `rsv@linux2.blondertongue`. `/etc/version`: `Altera/nios2 Version 3.2.0 -- Thu May 14 09:43:40 EDT 2015`. Root filesystem is an in-RAM rootfs (initramfs inside the kernel image). Persistent storage is a single 2 MB JWFFS2 MTD partition (`mtd0 "uClinux"`, mounted on `/mnt`). An `epcs` tool runs at boot (`/bin/epcs -k`). EPCS is Altera's serial config flash, which fits the `EPCS_1/2_*.bin` image names.

Network interfaces (Linux view) [V]:

| Linux if | UI name | MAC | IP | Notes |
|---|---|---|---|---|
| eth1 | "Remote Control" | 00:14:39:00:35:C3 | 10.42.0.184/22 gw 10.42.0.1 | management; only interface with traffic |
| eth0 | "Local Control 1" | 00:14:39:00:35:C4 | 192.168.253.2/24 | multicast route 224/4 is pinned here by `/etc/rc` |
| eth2 | "GIGE1" | 00:14:39:00:35:C6 | 10.42.0.183/22 | 0 packets at Linux level, so the TS data plane is in the FPGA [I] |
| eth3 | "GIGE2" | 00:14:39:00:35:C7 | 192.168.253.4/24 | the UI shows a junk mask/gw (`0.1.0.2` / `0.3.0.4`) |

## 2. Web session mechanism [V]

- `GET /cgi-bin/login.cgi` returns a fresh random 31-bit `session_id`. It comes in a hidden field and in JS that sets a `session_id` cookie.
- Login: `POST /cgi-bin/login.cgi` with `txtUserName`, `txtPassword` (both maxlength 10), `session_id=<the id from the form>` and `btnSubmit=Submit`. Success gives "Welcome Admin!" and a meta refresh to `status.cgi?session_id=<id>`.
- **Authentication uses only the `?session_id=` query parameter.** The cookie is used only by the page JS to fill the hidden field. Without the parameter, or with a bogus or not-logged-in id, the server returns a "Logged Out" page that redirects to login.
- Server-side session table: `/tmp/user_info.dat`. It has 5 lines of `user,client_ip,session_id,last_activity_uptime_seconds` (unused lines are `none,0.0.0.0,0,0`). The client IP is recorded. IP binding is not tested, but it is likely [I].
- **Single session per user:** a second login invalidates the first session id immediately.
- Idle timeout: "Login Timeout" on network.cgi (5/15/30/60 min). The current value is 15 (`Web Timeout=15` in the config).
- Logout is `<page>.cgi?logout=1&session_id=…` (never fetched).
- Navigation passes extra query params: `slotNumber=` (qamconfig) and `boxNumber=`. The `gobox()` helper exists on every page but no page in MPTS mode uses it. `status.cgi?boxNumber=2` is ignored in MPTS mode. Pagination is probably for SPTS mode [I].
- One GET to `statusedit.cgi` timed out once after 30 s and then worked on retry in under 1 s.
- **Side effect of a GET:** `GET config.cgi` regenerates `/home/httpd/config_file.dat` (its mtime changes to the request time). It writes only to RAM (rootfs), not to flash.

## 3. Page / form map

All forms are `name=configForm`, `method=post`, `action=<same page>.cgi`, and include a hidden `session_id`. They are submitted to the same CGI that renders them. A save is a POST of the whole form. No POSTs were sent in this pass.

Top nav: Main(`status.cgi`) · Network · Time · Event Log(`errorlog.cgi`) · Config · Logout.
Sub-nav (Main): Status · Stream Info · QAM Config(`qamconfig.cgi?slotNumber=1`) · Diagnostics · Mode · EAS.

### status.cgi (view)
Table of **16 rows in MPTS mode**, one per QAM carrier, grouped 4 per RF module with a "Locked" status cell for each module. Columns: #, Stream (group URL or "No IP Assigned"), Source (2 lines: SSM source 1/2), Name, IP, Bitrate, Pro:Idiom, RF Chan. ("<ch> / On"), Status. Two extra rows: "GIGE1 EAS" and "GIGE2 EAS". Header: "QAM Output (40dBmV)". The **Edit** button opens `statusedit.cgi`.

### statusedit.cgi — input stream table (MPTS mode: 16 slots)
Per slot n=1..16:
- `streamGroupAddress{n}`: text, maxlength 27. The format is a URL like `UDP://a.b.c.d:port`, as seen in the EAS page default `UDP://0.0.0.0:0`. `RTP://` is implied by the config key `UDP/RTP` [I]. There is no client-side validation.
- `streamSourceAddress{n}A`, `streamSourceAddress{n}B`: text, maxlength 27, **disabled**. These are SSM sources for IGMPv3 only. The unit is in IGMPv2 mode.
- `streamName{n}`: text, maxlength 10.
- `proIdiomEnable{n}`: checkbox. JS `manageProIdDisable()` allows at most 2 checked per GigE. **Slots 1–8 belong to GIGE1 and 9–16 to GIGE2.** The server hard-codes `G1numChecked=2; G2numChecked=2`, so every unchecked box is disabled. Pro:Idiom is effectively locked out, probably because it is unlicensed [I].
- `btnSave` (submit).
RF channel and bitrate are display-only here. The input to RF mapping is fixed: slot n maps to carrier n.

### streaminfo.cgi (view)
"Streams Detected" table. Currently empty, because no inputs are configured.

### qamconfig.cgi?slotNumber=1..4 — RF modules
`slotNumber` select [1..4], labeled "RF Module #". Each module is one RF output port with **4 adjacent carriers A–D**.
- `outputChannelA`: select of cable channel numbers. Labels are "ch / center MHz". The STD plan is shown (2 / 57 MHz … 158 / 993 MHz). A's list stops at 155 so that 3 more channels fit. B, C and D are **disabled** and are set by JS `updateChannel()` from the `cable_channel_table_after[]` "next channel up in frequency" table: …6→95→96→97→98→99→14…, 94→100, 13→23, 22→7, 158→0.
- `outputChannelControl{A..D}`: 0=Off, 1=On.
- `chkCW1`: checkbox "Enable CW for QAM Module" (CW test carrier). When checked, it forces all channel controls to On and disables them.
- `outputLevel`: 28..41 dBmV. **Editable only on module 1** (disabled on 2–4), so the level is global [I].
- `QAMModeA`: 0=64B, 1=256B, 2=16A, 3=32A, 4=64A, 5=128A, 6=256A. The letter is ITU-T J.83 Annex A or B. Annex C is not offered.
- `QAMMap`: 10=STD, 11=IRC, 12=HRC.
- Read-only rows: QAM Data Rate (5.360500 Mbaud), Interleaver (128-1), Alpha (12%), QAM Lock State.
- Hidden `qam_saved` (JS sets it to 1 before submit). Save button: `btnQAMSave` → `validate_qam()` → `check_duplicates()`. That check compares this module's 4 channels against the other 12. The other modules' channels are **baked into the JS as server-side constants** (2, 10, 6, 140 plus their successors).

### diagnostic.cgi
Columns: #, Stream, IP, Ping (2 columns), Oversubscription, TS Sync Errors. There is a `btnClear` "Clear Errors" submit button (not pressed). No rows are shown, since no inputs are configured.

### mode.cgi
`tsMode` select: 0 = Single Program Transport Stream, 1 = Multiple Program Transport Stream (current value: 1, MPTS). `btnSave` submit. The JS references an `autoRepid` control that is not in the form; Auto Re-PID was removed from the UI. `Auto Re-PID=0` remains in the config.

### eas.cgi
Shows the IP information again (read-only). Fields: `g1EASstreamName1`/`g2EASstreamName1` (maxlength 10), `g1groupIP`/`g2groupIP` (maxlength 27, value `UDP://0.0.0.0:0`), `g{1,2}sourceIP{1,2}` (maxlength 15, disabled, "Source IP for use in IGMPv3 Multicast only"), `easTriggerEthInterface` (1=Remote Control, 0=Local Control 1 (current)) and `easTriggerPort` (maxlength 5, current 0). There is also a test section: `testDuration` (10/15/30/45/60 s), `btnEasTest`, `btnEASForceOn` and `btnEASForceOff`. The hidden flags `saveRequest`, `easTestRequest`, `easForceOn` and `easForceOff` are set to 1 by the button JS. **All EAS action buttons are POSTs to the same form. They were not touched.**

### network.cgi
Display only: MACs, **Software Version 1.1.13**, FPGA/DDQ versions, serial, all four interface IPs, syslog destination, IGMP version.
Editable fields: `unitName` (64), `unitLocation` (64), `loginTimeout` (5/15/30/60). `btnSave`.
**IP addresses are not editable in the web UI.** They are probably set from the front panel (LCD `/dev/lcd16207` plus `/dev/button`) [I]. Do not change them.

### time.cgi
`cboCurrentMonth/Day/Year` (year range 2012..2035), `txtCurrentHour/Minute/Second`, `radAutoManual` (0=Manual (checked), 1=Automatic/NTP), `radDSTEnable` (1/0, current 0), `cboDST{Begin,End}{Month,Day,Year(2012..2034),Hour(1..24)}`. Save is `validate()` → submit. The NTP server is not editable here; the config has 129.6.15.28. The device clock is correct, but how it is set is unclear: cron is inert, so it is not set by the `get_ntp_time` cron entry [I].

### errorlog.cgi (Event Log)
Shows the syslog destination 10.42.0.53:514 (read-only here). `btnClearLog` (submit, not pressed). `showLines` 1..65535 (current 1000), `btnSave`. Log entries are shown below.

### config.cgi
- Upload: `multipart/form-data` POST with `fileConfig` and `btnSubmitConfig`. **This is a config restore and was not touched.**
- Download: link to `/config_file.dat`, a static file regenerated by the GET of config.cgi. It was downloaded; see section 5.

### login.cgi
See section 2.

### Not linked from the UI, present on disk (never fetched over HTTP)
`/home/httpd/cgi-bin/`: `admin.cgi` (160 KB, probably user/password admin), `admin_redir.cgi`, `license.cgi`, `license_redir.cgi`, `debug.cgi`, `streamconfig.cgi` (probably the SPTS-mode stream editor [I]), `cgi_demo`. Static files: `Admin.html` → `admin_redir.cgi`, `License.html` → `license_redir.cgi`, `qamchart.html` (25 KB, channel/frequency chart). The binaries were pulled over FTP for offline RE.

## 4. Current state snapshot [V]

- Model banner `EQAM-420B-4-64`. ESN/serial `2013090274`. Headend Name "Ball Holdings". Location empty.
- **Software 1.1.13**. This matches the "1113" in `EPCS_1_1113.bin` / `EPCS_2_1113.bin`. FPGA1 1.5, FPGA2A/2B 1.3M, FPGA3A/3B 1.2, DDQ (RF outputs 1–4) 1.4 each. Kernel built 2015-05-14.
- Uptime at survey about 6 d 18 h. Temperature about 81 °F. Load is near zero. 114 MB RAM free.
- Mode: **MPTS**. IGMP: v2.
- RF: 4 modules × 4 carriers = **16 QAM carriers, all On, all Locked, 256-QAM Annex B (5.3605 Mbaud, I=128 J=1, α=12 %), STD plan, 40 dBmV**:
  - Module 1: ch 2, 3, 4, 5 (57/63/69/79 MHz)
  - Module 2: ch 10, 11, 12, 13 (195–213 MHz)
  - Module 3: ch 6, 95, 96, 97 (85/93/99/105 MHz; this crosses the FM-band channel gap)
  - Module 4: ch 140, 141, 142, 143 (891–909 MHz)
- Inputs: **none configured**. All 16 MPTS slots are "No IP Assigned". Pro:Idiom is disabled everywhere. EAS streams are unset.
- Syslog to 10.42.0.53:514 (`syslogd -R`). SNMP trap/dest 10.42.0.53:161 is configured but snmpd is not running. Community strings are present in the config export (values not recorded here).
- Event log (only a few entries since boot, all with 1970 timestamps from before NTP), among them:
  1. `Power Up Started`
  2. `ERROR: Difference detected in Config File versus current system settings.  Correcting differences.`
- Running daemons: `status_led`, `inetd`, `logging_server`, `syslogd`, `crond`. `/etc/cronfile` lists `update_config` every 5 min and `get_ntp_time` every 20 min, but `crontab /etc/cronfile` is commented out in `/etc/rc` and `/var/spool/cron/crontabs/` is empty, so **cron runs nothing** [V]. `update_config` therefore runs only once at boot, `mux_sync`, `web_command_monitor`, `boa`, `join_group`, 3× `eas_soft_trigger`, `getty` on ttyS0 at 115200.

## 5. Config export / persistence

### Web export `/config_file.dat` (31,808 bytes, saved as `work/device/config_file.dat`) [V]
This is a pure GET of a static file. No auth is needed once it exists: Boa serves it to anyone. Visiting config.cgi regenerates it. The format is ASCII `Key=Value` lines in sections:
- Banner `420B Configuration File Format`, `Config_Version=1`.
- `# SPTS Settings`: **64 blocks** `SPTS_ID=0..63`. Each block has `SPTS Group IP0..3` (dotted octets split into 4 keys), `SPTS Port`, `SPTS UDP/RTP`, `SPTS RF CH`, `SPTS Program Number`, `SPTS Source 1 IP0..3`, `SPTS Source 2 IP0..3`, `SPTS Major Channel`, `SPTS Minor Channel`, `SPTS Name`, `SPTS Short Name`. **Major/minor channel fields (PSIP virtual channels) exist only for SPTS.**
- `# MPTS Settings`: **16 blocks** `MPTS_ID=0..15` with the same fields, minus the major/minor/short name fields.
- `# EAS Settings`: 2 EAS inputs (group, port, UDP/RTP, 2 sources, name).
- `# Generic Settings`: event log/syslog, SNMP dest and communities, `IGMP Mode=2`, `Web Timeout=15`, time/NTP/DST, `Watchdog=1`, `TS Mode=1`, `Auto Re-PID=0`, `Path Select=0`, `Band Select=1`, `SPTS Cable Flag=0`, `MPTS Cable Flag=2`, EAS trigger, `MPTS P:I Enable 1..16`, **`SPTS P:I Enable 1..16` (only 16, not 64)**, Local/GIGE IPs, unit name/location.
- **Generator bugs visible in the file:**
  - The Local 1 gateway's first octet is written under the key `Local 1 Subnet 0=10`, which duplicates the subnet key. There is no `Local 1 Gateway 0`.
  - The MPTS blocks write `MPTS Source 1 IP0` twice and have no `MPTS Source 2 IP0`.
  - These typos could break a round trip through config upload [I].
- **The export disagrees with the live state:**
  - MPTS RF CH by slot is `2,3,4,5, 0,0,0,0, 0,0,0,0, 140,141,142,143`, but the live UI shows slots 5–12 on ch 10–13 and 6/95/96/97.
  - SPTS RF CH by slot 0..47 is `2×4, 3×4, 4×4, 140×4, 141×4, 142×4, 143×4, 13×4, 6×4, 95×4, 96×4, 97×4`, and **slots 48..63 are all 0**.

### Persistent flash `/mnt/config_file.dat` (JFFS2, 432 bytes, mtime Sep 7) [V]
This is **not** the key=value format. It holds exactly **48 lines** of the form `0,0,<ch>-<k>`, with k=1..3 for each of 16 channels: 2,3,4,5, 67,68,69,70, 58,59,60,61, 140,141,142,143. `/etc/rc` copies it to `/tmp/config_file.dat` at boot, and `update_config` runs once at boot (cron is inert). According to docs/notes/firmware.md, `update_config` compares that text file with the 8 KB EEPROM settings image and corrects the differences; the EEPROM is the real live config store. The boot log reports a "Difference detected … Correcting differences". This file looks like either a different (older or compact) format or a corrupted or overwritten config [I]. The channel list (67–70, 58–61) does not match the live modules 2 and 3 (10–13, 6/95–97). Other `/mnt` contents: `channels.dat` (45,519 bytes, dated 2013), an empty `results.dat` and an empty `nas/` directory. `channels.dat` is a CRLF text list of 4291 `number,NAME` lines (`96,GAMES`, `99,SXM`, `118,A&E`, ...). It looks like a cable lineup uploaded by an installer, and nothing in it is clearly used [I]. All were pulled over FTP into `work/device/ftp/mirror/fs/mnt/`.

Safety net: we hold the web export, the raw `/mnt` files, and the full `/home/httpd` and `/bin` contents. We do **not** hold a raw image of mtd0. Reading `/dev/mtd0` was deliberately not done.

## 6. 48 vs 64 — everything visible

- "-64" in the model is matched by **64 SPTS slots** (`SPTS_ID 0..63`), i.e. 16 carriers × 4 SPTS per carrier [I]. MPTS mode has 16 slots (1 per carrier). Pro:Idiom enables are 16 in both modes. There are 2 EAS inputs.
- In MPTS mode the UI groups everything as 4 RF modules × 4 carriers, and inputs 1–8 / 9–16 by GigE port. The SPTS-mode UI was not visible because the unit is in MPTS mode, and it was not switched. `gobox(boxNumber)` plus `streamconfig.cgi` probably page through the 64 SPTS entries [I].
- **Web export: SPTS slots 48–63 have `SPTS RF CH=0`, while slots 0–47 are all assigned.** The 12 channels assigned to slots 0–47 do not match the live 16-carrier layout. Every other field of every SPTS slot is empty or 0.
- **Persistent flash config: exactly 48 entries** (16 channels × 3, `ch-1..ch-3`). Compare with 16 × 4 = 64 SPTS slots. The 4th sub-slot of each channel is absent. If the 4th per carrier is the 49th–64th, that lines up with the "corrupts above 48" report [I, speculative].
- No JS on any page branches on an index of 48 or 64. Client-side loops use 16 (`check_duplicates`) and 8 (Pro:Idiom per GigE). Any 48/64 handling is server-side, in the CGI binaries, `update_config` or `configure_eqam_mode`.
- Current highest in-use index: no inputs are configured, so none. RF channels in use: 16 of 16 carriers.

## 7. What the box accepts (from the UI)

- Inputs are IP multicast, and probably unicast too [I]: the group URL can presumably be the GigE's own IP. This is not verified; check the manual. Transport is `UDP://` or RTP (config key `UDP/RTP`). The input is on GIGE1 (inputs 1–8) or GIGE2 (inputs 9–16), with IGMPv2 (v3/SSM source fields exist but are disabled).
- Payload: MPEG-2 TS. There is per-input "Program Number" in the config. The UI shows TS sync errors, bitrate and oversubscription. The UI says nothing about codecs; it is a TS remux/modulator, not a transcoder.
- MPTS mode: 1 input TS → 1 QAM carrier. SPTS mode: up to 64 single-program inputs muxed 4 (or 3?) per carrier, with PSIP major/minor channel, name and short name per program.
- Output: J.83 Annex B 64/256 or Annex A 16–256 QAM, STD/IRC/HRC plans, 28–41 dBmV.

## 8. FTP / telnet details

- FTP: anonymous login → empty chroot; uploads were not tried. `Admin` plus the rbw credential → full root filesystem access. `/bin/ls` is absent, so `LIST` returns nothing; `NLST`, `SIZE` and `RETR` work. Passive mode only (active mode is blocked on our side). After about 15–50 passive data connections on one control session, ftpd returns `425 Can't open passive connection: Cannot assign requested address`. Reconnecting the control session fixes it, and `tools/eqam_ftp_mirror.py` does this automatically. After many rapid reconnects, inetd briefly **refuses** port 21 (ECONNREFUSED for about a minute, then it recovers by itself). This is probably inetd's rate limiting [I]. `SIZE` over-reports text files: it counts LF as CRLF even in TYPE I. `/dev`, `/sys` and most of `/proc` were listed but not read.
- Telnet: `uClinux login:`. `Admin` plus the rbw credential → `Sash command shell (version 1.1.1)` as uid 0. `/etc/passwd` has the `Admin` uid 0 MD5-crypt hash (not copied here), plus `root:x` and `ftp:x`. `/etc/config/config` holds another `passwd` hash. **Web, FTP and telnet all share one credential (the rbw credential), and it is root.**
- Commands run (all read-only): `help ps mount df free date printenv ifconfig`, `cat` of `/proc/{version,cpuinfo,meminfo,mtd,partitions,uptime,loadavg,devices,misc,interrupts,iomem,net/*}`, `/etc/*`, `/mnt/config_file.dat`, `/tmp/{user_info,syslog}.dat`, and `ls -l` of the main dirs. Transcripts are in `work/device/telnet/` (local only).

## 9. Not visited / dangerous (deliberately untouched)

- Any `*.cgi?logout=1`.
- All POST forms: every Save button, `config.cgi` upload (restore), `errorlog.cgi` Clear Log, `diagnostic.cgi` Clear Errors, and all EAS Test / Force On / Force Off actions.
- Unlinked CGIs: `admin.cgi`, `license.cgi`, `debug.cgi` and `streamconfig.cgi` were later fetched with a bare GET (section 11). `cgi_demo`, `admin_redir.cgi` and `license_redir.cgi` were never fetched.
- **Dangerous GET URLs (actions keyed on query parameters, never fetch):** `admin.cgi?…&btnReboot=reboot` (reboot), `admin.cgi?…&btnFlush=Flush` (deletes all stream addresses), `admin.cgi?…&btnDeleteConfig=Flush` (deletes the config file), and `admin.cgi` `commitConfig` (copies `/tmp/config_file.dat` to `/mnt`). The CGIs read action fields from the query string as well as the POST body, so **any GET that carries a `btn*` parameter must be treated as an action**. `tools/eqam_get.sh` refuses `*btn*`.
- Device nodes: `/dev/mtd0` was read exactly twice to dump the flash (section 11). Nothing else under `/dev` was read, including `/dev/mtdblock*`, `/dev/mmcblk*`, `/dev/i2c-*`, `/dev/mem`.
- Binaries with obvious side effects (never executed): `reboot`, `defaults`, `erase`, `eraseall`, `epcs`, `eeprom_rw`, `license_clear`, `password_control`, `update_config`, `configure_eqam_mode`, `set_eth*_ip`, `remote_logout`.
- Switching mode to SPTS, which would reveal the 64-slot UI.

## 10. Open questions

1. What exactly do `update_config` and `configure_eqam_mode` do? What format is the 48-line `/mnt/config_file.dat`, and why does it hold 48 entries and not 64? Is it the result of the >48 corruption? (Firmware RE of the pulled `/bin` binaries can answer this.)
2. Why does the web export disagree with the live carrier layout? (RF CH 0 for MPTS slots 5–12 and SPTS 48–63, and stale 12-channel SPTS mapping.)
3. What are the SPTS-mode UI (`streamconfig.cgi`, `boxNumber` paging) and the per-carrier program count (3 or 4)?
4. Is the session bound to the client IP? What happens when the 5-entry user table fills?
5. Which daemon owns UDP 5050 and 59810? Is 5050 the EAS soft trigger or a `web_command_monitor` control channel? `/bin/web_command_monitor` suggests the CGIs hand commands to a daemon. That could be a cleaner automation API than form POSTs.
6. What is the address/URL syntax accepted by `streamGroupAddress` (unicast? `RTP://`?)? Check the manual and RE `statusedit.cgi`.
7. Is it worth taking a raw dump of mtd0 (2 MB, read-only via FTP `RETR /dev/mtd0`) before any write experiments?

## 11. Follow-up pass: flash dump, hidden pages, UDP owners, EEPROM

All actions in this pass were reads. The only writes were 2 MB temporary files in RAM `/tmp`, removed straight after.

### 11.1 Raw flash (mtd0) [V]
- `/proc/mtd`: `mtd0: 00200000 00040000 "uClinux"`. dmesg: `m25p80 spi0.0: m25p128 (16384 Kbytes)`, `0x00e00000-0x01000000 : "uClinux"`. mtd0 is therefore the **last 2 MB of the 16 MB EPCS serial flash** (m25p128 on altspi at 0x0F000400). The first 14 MB (the FPGA/kernel images) have no MTD partition, so the OS cannot read them. The physmap CFI probe at 0x11000000 fails (no NOR flash fitted).
- Method: ftpd refuses `RETR /dev/mtd0` ("not a plain file"), and sash has no `>` redirection. I ran `cp /dev/mtd0 /tmp/mtd0_X.bin` in telnet, fetched the copy over FTP, then ran `rm` (the copy lived in RAM). I did this twice.
- Both reads are 2,097,152 bytes and identical: sha256 `85054757e18c1ed530ad530a43e5407ffe50f5dfc13b954fb389fdd301f76df9`. Saved as `work/device/flash/mtd0.bin` (plus `mtd0_a.bin`, `mtd0_b.bin`). Telnet transcripts are in `work/device/flash/transcript_*.txt` (`/proc/{mtd,cpuinfo,meminfo,iomem,ioports}`, `ps`, `mount`, full `dmesg`).
- `/proc/iomem`: altspi at 0x0F000400–0x0F00041F, physmap-flash.0 at 0x11000000–0x11FFFFFF. `/proc/ioports`: eth0–eth3 OpenCores MACs at 0x14004000/5000/6000/7000. All four PHYs report 100BASE-TX full duplex. eth2 and eth3 ("GIGE") are Linux 100 Mb/s MACs with no traffic. The GigE TS input path is separate FPGA logic [I].

### 11.2 JFFS2 contents, including obsolete nodes [V]
Unpacked with `tools/jffs2_dump.py` (new; writes every node version) into `work/device/flash/jffs2/{nodes.txt,current,versions,history}`. Only about 20 KB of the 2 MB is used, in a single erase block at 0x180000. The other blocks are erased with cleanmarkers. Timestamps are JFFS2 mtimes (UTC):

| ino | name | versions | dates | notes |
|---|---|---|---|---|
| 2 | `stb.dat` | 2, dirent **obsolete (deleted)** | 2013-09-21 11:02 | 60 bytes, recovered: `R0083577644-46 192.168.253.23` / `R0083595162-78 192.168.253.22`. These look like set-top-box (Pro:Idiom/STB) IDs paired with IPs. Factory or installer leftover [I] |
| 3 | `channels.dat` | 12 data nodes (one write in 4 KB chunks) | 2013-09-21 11:02 | 45,519 bytes, the same as the live file. Channel lineup `num,NAME` |
| 4 | `config_file.dat` | v1 (empty, obsolete) + **v2 = the 432-byte 48-line file** | **2026-09-07 01:30:48** | **No older content exists.** The file was created that day and never rewritten. Flash holds no earlier Key=Value config |
| 5 | `results.dat` | 1 (empty) | 2026-09-07 01:40:40 | |
| 6 | `nas/` | dir | 2026-09-07 01:53:56 | |

What this means:
- The JFFS2 history cannot show a "before" version of the config. On 2026-09-07 something created `config_file.dat`, `results.dat` and `nas/` within 23 minutes. My guess is a person or tool working on the box, for example a config upload through `config.cgi`, which does `cp /home/ftp/%s /tmp/config_file.dat` [I].
- Since then, `update_config` at boot has not written `/mnt` (the mtime is unchanged across the boot about 7 days ago).
- The 2013 files show the factory or installer state.
- The live settings are in the EEPROM, not in this file (see docs/notes/firmware.md).

### 11.3 Unlinked CGIs (bare GET with only `session_id`) [V]
Safety check before fetching: the EEPROM/flash write helpers and `epcs -c` strings are a shared library that every CGI links in, including the pages already fetched safely. Actions are keyed on specific fields (`btnClear`, `btnSave`, `btnReboot`, `btnFlush`, `btnDeleteConfig`, `commitConfig`, `licenseValue`), and a bare GET carries none of them. `cgi_demo` (no useful strings) and the two `*_redir.cgi` (JS redirect to `admin.cgi`/`license.cgi` only) were not fetched.

- **debug.cgi** (`work/device/web/debug.cgi.html`). The one form field is `btnClear` "Clear Errors" (POST). The page shows:
  - Software `1.1.13.0`. FPGA2A/2B `81030000`, FPGA3A/3B `01020000` (raw register values). **`Pro:Idiom Enable 0`**.
  - FPGA2A↔FPGA1 and FPGA2B↔FPGA1 serial-link drop counters (all 0).
  - Per-stream counters for **FPGA2A streams 1–32 and FPGA2B streams 1–32**: pre-buffer and post-buffer TS sync errors, 100 ms timeout errors, buffer overflows.
  - **TS clock errors for FPGA2A clocks 1–8 and FPGA2B clocks 1–8**. FPGA2B clocks 5–8 show **2** errors each; all others are 0.
  - Average and peak bitrates and timestamp error counts for MPTS streams 1–8 per FPGA.
  - Real-time streaming values, audio/video PES header error registers, and **"IP Register 0..33" plus "RTP Flag & Port Register 0..33" per FPGA** (all 0.0.0.0 / 0).
  - **Inferred layout:** the 64 SPTS inputs are 2 × 32, one input FPGA per GigE port (FPGA2A = GIGE1, FPGA2B = GIGE2). Each FPGA has 34 address/port register pairs (32 streams plus EAS plus one more?). Each FPGA has 8 QAM clocks, one per carrier on 2 RF modules.
  - **Live check, 2026-11-27 [V for slot 1 only]:** with slot 1 = `UDP://239.10.0.1:5000` arriving on GIGE1, the page showed `FPGA2A IP Register 0 = 239.10.0.1`, `FPGA2A RTP Flag & Port Register 0 = 00001388` (port 5000), `FPGA2A Real-Time Streaming Value 1 = 00000001`, and bitrate and a Timestamp Error Count (83, steady) on `FPGA2A MPTS Stream 1` only. So slot *n* is FPGA2A register *n*−1 and MPTS stream *n*, at least for slot 1. The bitrate sections list MPTS streams 1–9 per FPGA, the timestamp section 1–8. Every per-stream error counter was 0, so which of the 32 counter rows belongs to slot 1 is still inferred, and so is FPGA2B for slots 9–16 (needs a stream in slot 9). Saved as `crates/tb-eqam/tests/fixtures/debug.cgi_stream1.html`.
  - So **stream indices 49–64 are FPGA2B streams 17–32**, the upper half of the second input FPGA [I]. The firmware-side 48-stream boundary is 32 + 16. Compare FPGA2B clocks 5–8 (the second half of FPGA2B's carriers), which have nonzero errors [I, weak].
- **admin.cgi** (`admin.cgi.html`). This is the full system admin page. Fields:
  - `adminUsername` (Admin), `adminPasswordCurrent/New/Confirm`, `guestUsername` (**Guest**), `guestPasswordCurrent/New/Confirm`. There is a second, Guest account.
  - `btnFlush` "Flush Stream Database" and `btnDeleteConfig` "Delete Config File", both GET actions.
  - `configMode` 0 = Automatic (current) / 1 = Manual. A `commitConfig` "Commit Changes to Config File" submit exists in the binary, probably shown only in Manual mode. It runs `cp /tmp/config_file.dat /mnt/config_file.dat`.
  - `watchdogEnable` (1 = Enabled), `btnReboot` (GET action).
  - **The editable network settings live here:** `ipAddress`/`subnetMask`/`defaultGateway` (Remote), `local1*`, `ipAddressGigE_2a`/`_2b` and their masks/gateways, `logDestination`, `logPort`, `timeServer`, `igmpType` (2/3), and the syslog `errors`/`info`/`feedback` radios (all currently Disabled). **Never submit this form.**
- **license.cgi**: `licenseValue` (16 characters) and `btnSave`. No license state is shown. Together with `Pro:Idiom Enable 0`, this points to an unlicensed encryption option [I].
- **streamconfig.cgi** ("Stream Configuration", the PSIP/VCT editor). It works in MPTS mode too. Fields:
  - `outOfBandCtrl` (0 = Disabled, 1 = Enabled; current 1).
  - `VCTtype` (0 = TVCT, 1 = CVCT; **neither option selected**, so the stored value is not 0 or 1).
  - Per row n: `programNumber{n}` (maxlength 5), `shortName{n}` (maxlength 7), `majorNumber{n}`, `minorNumber{n}` (maxlength 7).
  - The form has no `action`, so it posts to itself. JS `validate_inputs()` assumes **64 rows** (16 groups × 4; program numbers must be unique within a group of 4; major/minor pairs must be unique over all 64). In MPTS mode only 16 rows render, so the JS would throw.
  - **Current values are garbage.** Short names are raw bytes `01 02 03 04 01 02 03`, `02 03 04 01 …`, `03 04 01 02 …` and so on. Major/minor numbers are 768/1024, 1024/256, 256/512, 512/768 (that is 0x0300/0x0400 …), followed by values like 17920, 15104, 1280, 580, 573, and short names `` `a``, `C`, `<`. Every program number is 0.
  - The repeating `01 02 03 04` byte pattern across consecutive records (rows 1–5) looks like a per-sub-slot index table (1..4 per carrier) written over the VCT region [I]. **This is a strong lead for the corruption bug.** Compare it with the EEPROM VCT region (0x1160, 0x12 bytes per stream, per docs/notes/firmware.md) once someone reads the EEPROM image.

### 11.4 UDP listeners [V/I]
- No `netstat` or `readlink` exists. sash `ls -l /proc/<pid>/fd` does not show link targets. Instead I used inode order and strings.
- UDP **5050 (inode 358) → `eas_soft_trigger`** [V-ish]. The binary is the only one among `/bin` and the CGIs containing the Nios II instruction `movi r4, 5050` (offset 0x3b14). Its unique strings: `couldn't create server socket -- au revoir`, `unable to bind to specified Ethernet port %s`, `eth0`/`eth1` (picked by `EAS Trigger Eth Sel`, current 0 = Local Control 1), `2,EAS Event ID:%d, Event Code:%s,message time remaining:%d,Event duration:%d minutes`, `2,EAS message:%s`. It also links Sun RPC client code. Three `eas_soft_trigger` processes are running (PIDs 48/51/52, each with a pipe pair plus one socket).
  - The protocol is a datagram EAS trigger carrying an event ID, an event code string, time remaining and a duration in minutes. The wire format has not been decoded; it needs disassembly around 0x3b14. **No packets were sent.**
- UDP **59810 (inode 353)**: an ephemeral port created right after syslogd's `/tmp/log` unix socket (inode 342/352). Most likely busybox `syslogd -R 10.42.0.53:514`'s sending socket [I].
- Other strings confirm the stream URL syntax: `mux_sync` logs `ERROR: Stream %d from UDP://%d.%d.%d.%d:%d is assigned but not streaming.` and the same with `RTP://`. **Accepted forms are `UDP://a.b.c.d:port` and `RTP://a.b.c.d:port`** [V from strings].
- `web_command_monitor` has no sockets (fds 0–2 only), so it is not a network control channel. It probably talks through SysV message queues (`log_msg_qid`, `dtcp_msg_qid`) [I].

### 11.5 EEPROM image — not dumped (deliberately)
- `/bin/eeprom_rw` is an **interactive numbered menu**. Its options include "Write a byte", "Read a byte", "**Default entire EEPROM contents**", "Change MPTS/SPTS mode", "Force EAS mode", "Test BT interface functions", "Test front LEDs" and "Modify VCT/MGT Parameters". Reads are one byte at a time by decimal address. Driving 8192 menu reads over telnet next to a "default EEPROM" item was judged too risky, so I skipped it.
- No EEPROM image file exists in `/tmp` or `/mnt`. The CGI library's "Reading and Writing EEPROM to File … eeprom.hex" path is tied to license/admin actions and was not triggered.
- The mirror at physical 0x13010000 could be read via `/dev/mem`, but the only tools available (sash `hexdump [-s pos]`, `cp`, `cat`) read to EOF and have no length limit. They would run past the 8 KB window into other FPGA address space, which could hang the bus. Skipped.
- A safe way needs a bounded reader: either a tiny static Nios II binary that `memcpy`s 0x2000 bytes from 0x13010000 (this is what `update_config` itself does), or `dd`, which this busybox lacks. **Uploading a binary would be a write to the device (RAM `/tmp` only), so it needs a decision.**

### 11.6 Updated open questions
- Q5 is answered (5050 = eas_soft_trigger; 59810 = probably the syslogd sender). Q6 is answered (`UDP://` and `RTP://`; unicast is still untested). Q7 is done (mtd0 dumped).
- New: what wrote the 2026-09-07 files (`config_file.dat`, `results.dat`, `nas/`), and how did it get there?
- New: correlate the garbage VCT rows in streamconfig.cgi with the EEPROM VCT region. That needs a bounded EEPROM read (see 11.5).
- New: why do FPGA2B TS clocks 5–8 show 2 errors each?
