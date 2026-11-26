# EQAM-420B-4-64 firmware: first-pass map

Source: `docs/external/eqam-420b.zip` -> `EPCS_1_1113.bin` (4,602,784 B, sha256 b31aa3c2…b7f) and
`EPCS_2_1113.bin` (16,777,216 B, sha256 975a580a…38c). Firmware build date is 14 May 2015.
The live device may run something else.

Legend: **[V]** = verified from the bytes. **[I]** = inference. Offsets are file offsets unless a
note says otherwise.

## 0. TL;DR

* **[V]** Two Altera serial config PROM dumps (the device calls them "EPCS_1" and "EPCS_2"). They
  are in **.rbf byte order**: `FF×32, 6A` at 0x20. Any data that is not an FPGA bitstream must be
  **bit-reversed per byte** to get the bytes the CPU sees.
* **[V]** EPCS_1 = FPGA1 (the host FPGA) bitstream + **Nios II** software: an Altera EPCS
  boot-copier image -> uClinux zImage -> **Linux 2.6.26-rc6 uClinux/Nios II** (`rsv@linux2.blondertongue`,
  gcc 3.4.6, built 14 May 2015) with an embedded gzip **initramfs** (cpio newc).
  The userland is **bFLT v4** (uClibc, BusyBox 1.11.1, Boa, a custom "BT" app suite and CGIs).
* **[V]** EPCS_2 = two FPGA bitstreams only (FPGA2–4 per the `epcs` tool usage text). It holds no
  CPU code.
* **[V]** Runtime settings live in an **8 KB EEPROM image**. The CPU sees an 8 KB mirror at
  **0x13010000** and writes through an FPGA mailbox at **0x12000808/0x12000810**. A text
  "420B Configuration File" (`config_file.dat`) is also kept on a **JFFS2** filesystem
  (`/dev/mtdblock0` -> `/mnt`). [I] That JFFS2 is the "uClinux" MTD partition at 14 MB–16 MB of
  the 16 MB EPCS_1 chip. That area is **not in our dump**, because the dump is truncated at 4.6 MB.
* **[V]** Product geometry: **4 RF modules ("DDQ1–4" / slots) × 4 QAM channels (A–D) = 16 QAMs,
  × 4 programs = 64 SPTS streams.** There are 2 GigE inputs (GIGE1/2) and 16 MPTS entries. The web UI
  prints `EQAM-%s-4-%d`, so the "64" is the stream count.
* No Nios II support exists in Ghidra, radare2/rizin or the system binutils. I built
  `nios2-elf-objdump` (binutils 2.42) and wrote a validated Ghidra SLEIGH module in
  `tools/ghidra-nios2/` (see §7).

## 1. Flash layout

### EPCS_1_1113.bin (the dump is 0x463BA0 bytes; [I] the chip is an EPCS128/M25P128 = 16 MB)

| Offset | Size | Content |
|---|---|---|
| 0x000000 | 0x10B52E | FPGA1 bitstream (.rbf order). Header length field = 0x85A969 bits [V]. The data visibly ends at about 0x10B4E5, then FF pad. Extracted as `work/firmware/epcs1/fpga1.rbf`. |
| 0x10B52E | 8 | boot record 0 header (bit-reversed LE): len 0x2AC6, dest **0x00D00000** |
| 0x10B536 | 0x2AC6 | zImage head and decompressor (Nios II code, "Uncompressing Linux...") -> `rec0_00d00000.bin` |
| 0x10DFFC | 8 | boot record 1 header: len 0x355AE4, dest 0x00D02AC8 |
| 0x10E004 | 0x355AE4 | u32 piggy length 0x355AE0 + **gzip(vmlinux.bin)** at 0x10E008 -> `rec1_00d02ac8.bin` |
| 0x463AE8 | 8 | terminator record: len 0, **entry 0x00D00000** |
| 0x463AF0 | 0xB0 | FF (end of the dump file) |
| [I] 0xE00000 | 0x200000 | MTD partition "uClinux" (from kernel platform data: m25p128 on altspi @0x0F000400, 25 MHz, IRQ 9, partition size 0x200000 at offset 0xE00000) = JFFS2 mounted at `/mnt`. **Not captured.** |

Notes:
* **[V]** Boot-copier record format: `u32 LE length, u32 LE dest, payload…`. The terminator has
  length 0 and carries the entry address. These are the standard Altera EPCS boot-copier records
  after bit reversal. The FPGA's on-chip boot ROM finds the software start from the bitstream
  length field.
* **[V]** Bitstream length field: bit 2 of the header bytes, starting at file offset 0x30, holds a
  24-bit LSB-first value = config size in **bits**. EPCS_1 gives 0x85A969 bits ->
  0x10B52D bytes -> software at 0x10B52E. `tools/extract_firmware.py` implements this.
* binwalk 3.1 finds nothing in either raw file. Everything above came from bit reversal and manual
  parsing.
* **[I]** The dump file name matches the output of `epcs -e1 -r0` ("Read EPCS_x and write EPCS_x.bin
  field update programming file"). It was probably cut at the end of the used data, so it is a
  field-update image and not a full chip read. That would explain why EPCS_1 is 4.6 MB while
  EPCS_2 is a full 16 MB.

### EPCS_2_1113.bin (16 MB, EPCS128)

| Offset | Size | Content |
|---|---|---|
| 0x000000 | 0x661444 (+FF pad) | FPGA bitstream image 0 (`work/firmware/epcs2/fpga2_img0.rbf`) |
| 0x661444 | ~0x19EBDC | FF |
| 0x800000 | 0x67A92E | FPGA bitstream image 1 (`fpga2_img1.rbf`) |
| 0xE7A92E | 0x1856D2 | FF |

* **[V]** No software, no strings and no filesystem, in either bit order.
* **[V]** The two images differ in length (0x661444 vs 0x67A92E). The header byte layout is shifted
  by one compared to EPCS_1 (`6A FF F7…` vs `6A F7…`). Decoding with the EPCS_1 rule gives low bits
  that match ~0x66148C bytes, but the high bit is lost, so the field encoding differs a little.
* **[I]** Two possibilities: (a) the factory/recovery image plus the production image (the `epcs`
  tool says "NOTE: Updating both the Factory Recovery and Production PROM Images"), or (b) two
  different designs for FPGA2x and FPGA3x. Different sizes fit compressed bitstreams or different
  designs. Not resolved.
* **FPGA family [I]:** a Cyclone III/IV-style .rbf header (`…FF 6A`) at 0x20. I could **not**
  identify the exact device. EPCS_1 is 8.76 Mbit and the EPCS_2 images are about 53.5 Mbit each.
  Neither matches a known uncompressed size, so the bitstreams are likely compressed. Board
  photos or the manual are needed.

### Runtime address map (Nios II physical addresses, from kernel platform data and userland)

| Address | What |
|---|---|
| 0x00000000 | SDRAM. The kernel `vmlinux.bin` is linked and loaded here [V: string pointers resolve with base 0] |
| 0x00D00000 | zImage decompressor load/entry |
| 0x0F000400 | altspi SPI master -> m25p128 (EPCS_1) [V kernel data] |
| 0x11000000–0x11FFFFFF | physmap CFI flash, bankwidth 1, one partition "uClinux" of 0x500000 at offset 0 [V kernel data]. [I] Probably not fitted (an old template). |
| 0x12000000 | FPGA register window used by every BT app with `ldwio/stwio`. 0x12000808 = mailbox data/command write (value = `cmd<<16 | byte`). 0x12000810 = mailbox status bits (bit0 ready, bit3, bit14, bit16 …) [V disasm] |
| 0x13000000 | FPGA RAM window [V used by all BT apps] |
| 0x13010000 | **8 KB EEPROM mirror**. Apps `memcpy(buf, 0x13010000, 0x2000)` [V update_config 0x100022a4] |

On nommu Nios II, userland programs touch hardware directly. There is no driver layer for the
FPGA. This matters for any replacement firmware.

## 2. CPU / OS

* **[V]** CPU: Altera **Nios II** (R1, little-endian, no MMU: "uClinux/Nios II"). The kernel was
  built with HW multiply (there are `CONFIG_NIOS2_HW_MUL*` check strings). The `/etc/version`
  file says `Altera/nios2 Version 3.2.0 -- Thu May 14 09:43:40 EDT 2015` (nios2-linux / uClinux-dist).
* **[V]** Kernel: `Linux version 2.6.26-rc6 (rsv@linux2.blondertongue) (gcc version 3.4.6) #11804
  PREEMPT Thu May 14 16:01:07 EDT 2015`, built from `/home/rsv/nios2-linux/linux-2.6`. It has no
  kallsyms. Drivers seen: altera_uart, altera_jtaguart, altspi + m25p80, physmap CFI (Intel/AMD
  cmdsets), **open_eth (`oeth_*`, OpenCores 10/100 MAC)** ×4 (eth0–eth3), JFFS2, IGMP.
* **[V]** libc: uClibc (static, linked into each bFLT). BusyBox v1.11.1 (2015-05-14).
  Boa 0.93.15 per the live banner; the Boa binary itself is in the image. ntpclient, inetd,
  telnetd and ftpd come from uClinux-dist.
* **[V]** Userland: bFLT v4. Some files are gzip-compressed (`flags=0x5`). Nios II reloc types
  are in the top nibble of each reloc entry (0=R_32, 1=HI_LO, 2=HIADJ_LO, 4=CALL26).
  `tools/bflt.py` handles all of these.

## 3. Root filesystem (initramfs)

**[V]** It sits inside vmlinux.bin at +0x21C4AC (gzip). It expands to 0x54AA00 bytes of cpio newc.
It is unpacked to `work/firmware/epcs1/rootfs/`. The device nodes could not be made in the
extraction. They include mtd0–3, mtdblock0–3, mmcblk0p1–3, spi0–3, i2c-0/1, lcd16207, button,
fb0–3 and video0/1 (a generic template).

### Boot sequence ([V] `/etc/inittab` -> `/etc/rc`)
`init` (BusyBox) runs `/etc/rc`, then a getty on ttyS0 at 115200. `/etc/rc` runs these steps in
order:
1. `hostname uClinux`, mount proc/sysfs.
2. `status_led &`, `clear_registers`, `enable_temp_rtc`, then `epcs -k` (resets the Pro:Idiom cores).
3. `inetd &` (ftp and telnet), `ifconfig lo`, `logging_server &`.
4. `start_syslogd`, `set_eth0_ip`…`set_eth3_ip`, `set_route &`, `route add -net 224.0.0.0/4 dev eth0`.
5. **`mount -t jffs2 /dev/mtdblock0 /mnt`**, then `password_control -i` (writes /etc/passwd from
   the EEPROM). `crond` starts, but `crontab /etc/cronfile` is commented out.
6. **`cp /mnt/config_file.dat /tmp/config_file.dat`**, `update_config &`, `mux_sync &`,
   `web_command_monitor &`.
7. `boa &`, `join_group &`, `eas_soft_trigger &`, `clear_reconfig_bit &`.

The lines for `configure_eqam_mode`, `snmp_config` and `snmpd` are commented out. **No SNMP agent
binary is in the image [V].**

### Binary inventory (all bFLT v4 Nios II unless noted. Size = file size. "z" = gzip-compressed bFLT)

App suite (`/bin`). All are statically linked against the same "BT" library: EEPROM access,
semaphores, a msgq logger, settings-table parsing and the EEPROM-corruption check.

| Binary | Size | Purpose (guess unless marked [V]) |
|---|---|---|
| mux_sync | 114000 | Main control daemon. It syncs the EEPROM settings to the FPGAs and modulators: stream-assignment checks ("Stream %d from UDP:// … assigned but not streaming"), RF lock, FPGA2A/B PES-header error monitor, the FPGA1->FPGA3A/B serial-link register writes, and temperature [V strings] |
| update_config | 86804 | Compares `/tmp/config_file.dat` with the EEPROM settings and "corrects differences". It writes the config file (`/home/httpd/config_file.dat` too). Holds the settings descriptor table [V] |
| epcs | 106800 | EPCS/NAND programmer (v2.8): `-e1/-e2` file write+verify, `-r` dump, `-i` ID, `-c` reconfigure all FPGAs, `-k` Pro:Idiom core reset, `-p` key update. Knows EPCS-64/EPCS-128 and **Micron MT29F1G NAND** (as EPCS_2 on other hardware) [V] |
| bootloader | 69328 | Programs the **DDQ1–4 modulator boards** from Intel-HEX (FLASH+EEPROM+config bytes). [I] Each board has its own MCU. Uses the `modulator_sem_id` semaphore [V] |
| eeprom_rw | 102604 | **Hidden interactive EEPROM editor**: poke/peek bytes, default the EEPROM, MPTS/SPTS mode, force EAS, test the BT interface (`get_igmp_mode`, `get_max_streams_per_gige`, `get_stream_ip`…), front LEDs, VCT/MGT params (path/band/cable flag, major/minor, short name) [V] |
| defaults | 100512 | Factory defaults: `-a` all, `-m` mux, `-n` network+web users, `-c` delete the config file (`rm /mnt/config_file.dat`), `-p` telnet password, `-i` DHCP DB. Then `/bin/epcs -c` [V] |
| configure_eqam_mode | 85900 | Checks the license key for encryption (Pro:Idiom) [V strings]. Commented out of rc |
| password_control | 90552 | `-i` writes /etc/passwd from the EEPROM. `-s` stores it in the EEPROM [V] |
| pass.sh | 29 (text) | `passwd; password_control -s` |
| join_group | 89232 | **Software IGMP**: builds IGMP reports (224.0.0.22 = IGMPv3, IP router-alert option) per stream group/source and binds to an eth port [V strings] |
| eas_soft_trigger | 169788 | EAS listener (TCP/UDP server; "EAS Event ID…Event Code…"; version string 0.9.29). Possibly a SCTE-18 / CAP-like trigger [I] |
| web_command_monitor | 85436 | Small daemon [I]. Probably runs commands that CGIs queue for it |
| logging_server | 96832 | SysV msgq (`log_msg_qid`, `dtcp_msg_qid`) -> `/tmp/syslog.dat` event log |
| start_syslogd | 85484 | `syslogd -R ip:port` from the EEPROM |
| set_eth0..3_ip | 94180 each | ifconfig from the EEPROM. The four binaries are byte-identical |
| set_route | 94096 | default route from the EEPROM |
| get_ntp_time | 85684 | runs `ntpclient` against the EEPROM time server (cron) |
| enable_temp_rtc | 94492 | temperature sensor / RTC init |
| status_led | 85312 | front-panel LEDs |
| clear_registers / clear_reconfig_bit | 85336 / 85304 | FPGA register init / reconfig-bit clear |
| license_clear | 85896 | clears the encryption license through the "write FIFO" |
| remote_logout | 85460 | resets `/tmp/user_info.dat` |
| dhcpd | 146704 | DHCP server (iplist 192.168.253.20+, router .254). Lease DB on `/mnt`. [I] For settop boxes / Pro:Idiom TVs |
| busybox | 146510 z | BusyBox 1.11.1 (sh/msh, mount, grep, ping, login, crond, syslogd, …). Applets: cp, dmesg, egrep, fgrep, grep, kill, login, mount, nice, ping, rm |
| sh | 49516 z | shell |
| init | 20780 z | init |
| inetd / telnetd | 23907 / 28554 z | uClinux-dist inetd. telnetd -> `/bin/login` |
| ftpd / ftp | 66001 / 66448 z | uClinux ftpd (`-l`, runs as root) and an FTP client |
| boa | 60964 z | HTTP server (runs as **root**, port 80, docroot /home/httpd) |
| ntpclient | 56592 z | NTP client |
| erase / eraseall | 17152 / 19097 z | MTD erase utilities |
| reboot | 17993 z | reboot |

CGIs (`/home/httpd/cgi-bin`), all bFLT. Login state is a `session_id` (%u) passed in the query
string or POST, with 5 slots in `/tmp/user_info.dat` (`user,ip,session,time`):

| CGI | Size | Form fields (POST unless noted) |
|---|---|---|
| login.cgi | 133488 | txtUserName, txtPassword, btnSubmit, session_id. Users **Admin** and **Guest** (defaults "pass"/"pass" are in the BT lib data [V]) |
| status.cgi | 142204 | btnEdit (stream status, bitrates, PMT) |
| statusedit.cgi | 147300 | streamGroupAddress%d, streamSourceAddress%dA/B, streamName%d, proIdiomEnable%d (max 2 Pro:Idiom per GigE in JS), btnSave |
| streamconfig.cgi | 144584 | outOfBandCtrl, VCTtype (CVCT/TVCT), programNumber1..64, shortName1..64 (7 chars), majorNumber1..64, minorNumber1..64, btnSave. JS loops `i<65` and program groups of 4 × 16 |
| streaminfo.cgi | 134768 | read-only |
| qamconfig.cgi | 152632 | slotNumber (1–4), outputChannelA..D, outputChannelControlA..D (On/Off/CW), chkCW1, outputLevel (dBmV), QAMModeA, QAMMap, qam_saved, btnQAMSave |
| mode.cgi | 132904 | tsMode (SPTS/MPTS), autoRepid |
| eas.cgi | 147812 | g%dgroupIP, g%dsourceIP1/2, g%dEASstreamName1, easTriggerPort, easTriggerEthInterface, testDuration, easTestRequest, easForceOn/Off, saveRequest |
| network.cgi | 137588 | unitName, unitLocation, loginTimeout |
| admin.cgi | 160808 | ipAddress, subnetMask, defaultGateway, ipAddressGigE_2a/2b, subnetMaskGigE_2a/2b, defaultGatewayGigE_2a/2b, local1ipAddress/subnetMask/defaultGateway, timeServer, logDestination, logPort, snmpDestination, snmpRName, snmpRWName, igmpType (IGMPv2/v3), watchdogEnable, configMode, admin/guest Username + PasswordCurrent/New/Confirm, commitConfig, btnSave, btnReboot, btnFlush, btnDeleteConfig (the last 3 also as GET) |
| config.cgi | 142944 | fileConfig (multipart upload of config_file.dat; also `cp /home/ftp/%s /tmp/config_file.dat`), btnSubmitConfig. The download link is `/config_file.dat` |
| time.cgi | 151884 | radAutoManual, cboTimeZone, cboCurrentDay/Month/Year, txtCurrentHour/Minute/Second, radDSTEnable, cboDSTBegin*/End* |
| errorlog.cgi | 132820 | showLines, btnClearLog, btnSave |
| diagnostic.cgi | 134092 | btnPing%d, btnClear |
| debug.cgi | 129876 | btnClear. Dumps FPGA2A/2B/3A/3B versions, IP registers, RTP flag/port registers, per-stream bitrates, serial-link drops and PES errors. **Not linked from the menu [I]** |
| license.cgi | 131280 | licenseValue (encryption license key) |
| admin_redir.cgi / license_redir.cgi | 47076 | redirectors for Admin.html / License.html |
| cgi_demo | 19820 z | uClinux demo CGI (leftover) |

Every CGI accepts `logout=1`, `session_id`, `slotNumber` and `boxNumber` in the query string.

Config and assets: `/etc/rc`, `/etc/inittab`, `/etc/inetd.conf` (ftp: `ftpd -l`, telnet:
`telnetd`), `/etc/passwd`, `/etc/config/config` (`passwd abBqwxv.nBlS5`, DES crypt, not cracked
with rockyou), `/etc/config/dhcpd.*`, `/etc/cronfile` (every 20 min get_ntp_time, every 5 min
update_config; not installed), `/etc/boa.conf` + `/home/httpd/boa.conf`,
`/home/httpd/{index.html, Admin.html, License.html, qamchart.html (QAM channel/frequency chart),
eqam.css, images/logo.png}`, `/lib/liberror.txt`, and `/tmp/user_info.dat`.

## 4. Configuration persistence (and leads on the >48-channel corruption)

There are three layers [V]:
1. **The EEPROM (the main store).** The image is 8 KB (0x2000). The FPGA mirrors it at
   0x13010000. Apps read the whole 8 KB. Writes go as mailbox words to 0x12000808. The EEPROM
   write is at update_config 0x10002704: it waits for status bit 1 in 0x12000810, sends all 8192
   bytes as `0x00020000 | byte` (cmd 2), then the terminator 0x00030000. It re-reads through the
   mirror and runs a memcmp over 8192 bytes; a mismatch logs "EEPROM write verify failed!"
   [V disasm]. So the EEPROM is always rewritten whole. Other mailbox commands with unknown
   targets: cmd 6 / cmd 12 (1024 B each, terminators 0x70000 / 0xD0000) and cmd 0x11 (256 B,
   terminator 0x120000), at 0x100020e8 / 0x10002140 / 0x10002198. The access semaphore is
   `eeprom_sem_id`.
2. **`/tmp/config_file.dat` and `/mnt/config_file.dat`.** This is a text "420B Configuration File
   Format" (`Config_Version=%d`, `# SPTS Settings / # MPTS Settings / # EAS Settings / # Generic
   Settings`, lines `Name=value`). `update_config` builds it from the EEPROM and, if the file and
   the EEPROM differ, it rewrites the EEPROM ("Difference detected in Config File versus current
   system settings. Correcting differences."). admin.cgi saves it to JFFS2 with
   `cp /tmp/config_file.dat /mnt/config_file.dat`. config.cgi uploads it.
3. **JFFS2 at `/mnt`** (mtdblock0, [I] EPCS_1 0xE00000–0xFFFFFF). It also holds the dhcpd files.

**The settings descriptor table** is at update_config.img +0xD33C. All BT apps carry an
identical copy [V]. `python3 -I tools/settings_table.py X.img` dumps it. Each record is 0x44
bytes: `{u32 0; u32 size; u32 type; u32 eeprom_off; u32 stride; char name[0x30]}`. Type codes
seen: 1 = scalar, 2 = string, 3/8/0xd/0x10 = section markers (SPTS/MPTS/EAS/end), 4/9/0xe =
per-stream SPTS/MPTS/EAS fields, 5 = per-stream VCT u16, 6/0xb/0xf = per-stream names,
7 = per-stream VCT short name. The resulting EEPROM map:

| EEPROM offset | Item | Size | Stride / count |
|---|---|---|---|
| 0x2D… 0x7C | event log, syslog, SNMP dest, IGMP mode, web timeout, time server, DST, timezone, watchdog, TS mode | 1–4 | scalar |
| 0x80 + 0x20·n | **SPTS stream n**: group IP 0x80–0x83, UDP port 0x84 (2), UDP/RTP 0x86, name 0x87 (10), RF ch 0x91, program # 0x92 (2), source1 IP 0x94–0x97, source2 IP 0x98–0x9B | | stride 0x20; **64 streams -> 0x80–0x87F** |
| 0x880 + 0x20·m | **MPTS m**: same layout | | stride 0x20; 16 -> 0x880–0xA7F |
| 0xA80–0xA84 | EAS test duration, trigger port, eth sel | | |
| 0xA88 + 0x3C·k | EAS 1/2 streams | | stride 0x3C |
| 0xB00–0xB0F / 0xB10–0xB1F | MPTS / SPTS "P:I Enable 1..16" (Pro:Idiom) | 1 | |
| 0xE90 / 0xED0 | Unit Name / Unit Location | 0x40 | |
| 0xF38–0xF3B | Auto Re-PID, Path Select, Band Select, SPTS/MPTS cable flag | 1 | |
| **0xF40 + 0x12·n** | VCT records in MPTS mode (alternate base used in streamconfig.cgi 0x10000324) | 0x12 | |
| 0x10E0 / 0x10F0 | SNMP RW / R community | 0x10 | |
| **0x1128–0x112B** | not in the table. **Checked on every EEPROM read: if all four bytes are 0x00 or all 0xFF, the app logs "CRITICAL ERROR: Possible EEPROM corruption detected!", "Defaulting EEPROM!", "Reassigning IP addresses!", "Rebooting unit!"** [V update_config 0x100022b4…]. [I] This is the management IP. | 4 | |
| 0x1140–0x114B | Local 1 IP / subnet / gateway | | |
| 0x1158–0x115F | GIGE 1/2 IP | | |
| **0x1160 + 0x12·n** | **SPTS VCT record n**: major (2) @+0, minor (2) @+2, short name 14 B (7×UTF-16) @+4 [V mux_sync 0x10007ad0 `muli r3,r4,18` + `addi r5,r19,4452`; eeprom_rw 0x10000bd8] | 0x12 | 64 -> **0x1160–0x15DF** |

The image is 0x15E0 = 5600 bytes, and mux_sync's main frame is 5608 bytes. It fits in 8 KB.

**Leads for the ">48 channels corrupts config" report** (none confirmed):
* **L1 — the counts.** 64 streams = 16 QAM × 4 programs. 48 = 12 QAMs = 3 of the 4 RF modules.
  "Channels over 48" most likely means streams 49–64, which all map to RF module 4 (slot 4 / DDQ4).
  Look at code that works per slot (qamconfig slotNumber 1–4, `bootloader -d4`, mux_sync
  `slli r4,r17,2` = qam×4) for arrays sized for 3 slots or 12 QAMs.
* **L2 — the VCT region.** 0x1160 + 48·0x12 = **0x14C0**. Streams 48–63 use 0x14C0–0x15DF.
  Check whether any reader or writer still bounds the image at 0x14C0 (a 48-stream layout), or
  copies fewer bytes. The CPU-side EEPROM write is the full 8 KB (cmd 2), so any truncation would be
  in the FPGA/EEPROM controller (address wrap, a 4 KB part, page writes) or in a reader. Check the
  EEPROM part size on the board. Note that `addi r8,r8,5312` in
  network.cgi is a function address (0x100014C0), not an EEPROM offset.
* **L3 — the corruption trigger.** Every BT app tests EEPROM[0x1128..0x112B] for all-0 or all-FF
  and then **defaults the whole EEPROM and reboots**. Anything that clobbers those 4 bytes looks
  like "firmware config corruption". Find every writer whose computed offset can reach 0x1128:
  * The MPTS-mode VCT base 0xF40 + 0x12·n reaches 0x1128 at n = 27 (0xF40 + 27·0x12 = 0x1126).
    If MPTS-mode VCT indexing uses the stream index (up to 63) instead of the MPTS index (up to 15),
    it walks over 0x1128 (SNMP, local IP, GIGE IP), and at n ≥ 30 into the SPTS VCT region.
  * Look for this alternate base (3904 = 0xF40) in streamconfig.cgi, mux_sync and update_config.
* **L4 — the text config file.** `update_config` runs at boot (and every 5 min if cron is on). It
  rewrites the EEPROM when the file differs. The config-file parser and writer use fixed buffers
  (an 8660-byte stack frame in update_config 0x10002258). There are 64 × ~20 SPTS keys, so this is a
  long file. An overflow, or a truncated file on the 2 MB JFFS2, may turn into "corrections" that
  zero fields. Check the parse loop limits (does it stop at 48?).
* **L7 — the short-name size mismatch [V table].** The descriptor says "SPTS Short Name"
  size = **0x12 (18 bytes)** at 0x1164 with stride 0x12. But the record only has 14 bytes left after
  major/minor, and mux_sync/streamconfig copy 14 (`movi r6,14`). Any generic table-driven
  writer that uses the descriptor size (update_config's config-file "correction", defaults,
  config.cgi upload) would write 4 bytes past each name, into the **next stream's major/minor
  channel**. For stream 63 it would write past the 0x15E0 image end. Check how the generic
  set/get-by-descriptor code uses `size`.
* **L5 — the JS limits** in streamconfig: `i<65` for major/minor and 16 program groups. These are
  consistent with 64. The JS uses no 48.
* **L6 — `get_max_streams_per_gige()`** (eeprom_rw test menu). 64 streams over 2 GigE is 32 each.
  If the FPGA IP filter table (FPGA2A/2B "IP Register %d") or the per-GigE stream arrays are sized
  differently (for example 24 + 24 = 48), assignments past the limit may go out of bounds.
* Counting 48/64/0x30/0x40 immediates is useless: '0' = 48 and '@' = 64 in uClibc code drown the
  signal. Use Ghidra xrefs from the offsets above instead.

## 5. Network surface

* **HTTP:** Boa on :80 **as root**. The CGIs are listed above. Auth is a numeric `session_id` in
  the URL or form, plus `/tmp/user_info.dat` with 5 sessions, each recording the user and IP.
  Default web users are Admin/pass and Guest/pass [V strings in the BT lib data; the live device
  may differ]. `debug.cgi` is hidden but has an auth check [I]. The config download at
  `/config_file.dat` sits in the docroot. [I] It may be readable without auth, because Boa serves
  it as a static file. Check this on the live unit.
* **Telnet:** inetd -> `telnetd` -> BusyBox `/bin/login` with `/etc/passwd`. The image ships
  **`Admin:$1$LNywSUeB$PA0a4CKZ03PQIHH1Q8E3j.` uid 0 = password `pass`** [V, cracked].
  `password_control -i` rewrites /etc/passwd from the EEPROM at boot, so the live password may have
  been changed (`pass.sh` = `passwd; password_control -s`). `root:x` has no shadow file, so it is
  effectively locked.
* **FTP:** `ftpd -l` as root (uClinux ftpd), with the same /etc/passwd logins. Uploads to
  `/home/ftp` feed `config.cgi` and `epcs`.
* **Serial:** a getty on ttyS0 at 115200 (altera_uart). The kernel has a JTAG UART too.
* **SNMP:** no agent in this image (snmpd is commented out of rc and absent). Trap destination and
  community settings exist only in the EEPROM and UI.
* **Firmware update [V]:** there is no web upload. Upload `EPCS_1*.bin` / `EPCS_2*.bin` over FTP,
  then run `epcs -e1 file` / `epcs -e2 file` in telnet. The checks are: the **filename** must
  contain `EPCS_1`, `EPCS_Factory_1` or `EPCS_2` (strstr, epcs 0x10001c14…), the PROM device ID must
  be detected, and a page-by-page readback verify runs after writing. **There is no signature,
  CRC or header check** on the image. epcs unmounts `/mnt` first, writes only the sectors it needs,
  then `epcs -c` reconfigures all FPGAs. The DDQ modulator boards are programmed with
  `bootloader -dN -w file.hex` (Intel HEX).
* **Stream input:** the TS payload is handled **in FPGA** [I strong]. FPGA2A/FPGA2B (one per GigE)
  expose IP registers, RTP flag/port registers, per-stream bitrates, PES-header error counters and
  serial links to FPGA1/FPGA3A/3B. The software side is only `join_group`, which crafts IGMPv2/v3
  reports (224.0.0.22, router alert) for each group and its sources (SSM: Source 1/2 per stream),
  and mux_sync, which checks "assigned but not streaming". Per stream: UDP or RTP
  (`UDP://a.b.c.d:port` / `RTP://…`), one group, two optional IGMPv3 sources.
  Multicast 224/4 is routed to eth0 in rc. [I] eth2/eth3 or the FPGA MACs are the GigE ports. Which
  Linux interface maps to which port is not settled.
* **EAS:** `eas_soft_trigger` listens on a TCP/UDP port (EAS Trigger Port / Eth Sel) and switches to
  the EAS 1/2 multicast streams. There is a test and force on/off from eas.cgi.

## 6. Feature evidence

* **Modes:** SPTS (64 single-program streams, 4 per QAM) or MPTS (16 MPTS, 1 per QAM). `tsMode`,
  "Auto Re-PID" (PID remapping [V string]).
* **PSIP:** VCT type **CVCT/TVCT**, major/minor channel, 7-char short name, path select, band
  select, cable flag, "Modify VCT/MGT Parameters". PMT program numbers are per stream. There is
  no EIT/ETT/EPG, no SAP and no STT in the strings [V negative grep].
* **QAM:** modes `64B 256B` (Annex B) and `16A 32A 64A 128A 256A` (Annex A). Interleaver `128-1`,
  `12-17`. Roll-off strings `18% 15% 12%`. Output level in dBmV. Per-channel On/Off/**CW**.
  Channel plans **STD/IRC/HRC**, with a CEA-542 channel table in qamconfig.cgi JS and
  `qamchart.html`. Each RF module has 4 **adjacent** channels (B, C, D follow A).
* **Bitrate:** "Peak Bitrates / Average Bitrates" and per-stream bps from the FPGA2A/2B registers.
  Null-packet stuffing is not visible in the software. [I] It is done in FPGA.
* **EAS:** yes (2 EAS streams, a trigger port, a test duration).
* **Encryption:** **Pro:Idiom** (LG/Zenith hospitality DRM) cores in the FPGA. There are a license
  key, `epcs -k/-p` and "P:I Enable" per stream (max 2 per GigE in the UI), and a DTCP msgq.

## 7. Ghidra / disassembly setup

**Tools built (outside the repo):**
`work/tools-build/inst/bin/nios2-elf-objdump` = GNU binutils 2.42 `--target=nios2-elf`. To rebuild,
download the binutils 2.42 tarball and run
`configure --target=nios2-elf --disable-gdb --disable-sim --disable-nls --disable-werror` inside
`nix-shell -p gcc gnumake texinfo bison flex zlib`. Full disassembly of every userland binary is in
`work/firmware/epcs1/disasm/*.S`.

**Userland binaries:**
`python3 -I tools/bflt.py --base 0x10000000 OUTDIR files…` writes, for each binary:
* `.img`: the raw relocated text+data image
* `.elf`: an ELF32 with e_machine 113 (EM_ALTERA_NIOS2), .text/.data/.bss at 0x10000000, and the
  real entry point
* `.json`: the header and relocs

These outputs are already in `work/firmware/epcs1/flat/`. In each one, `_start` sets **gp** with
`movhi gp,…; addi gp,…` at entry+8. For example, in update_config, gp = 0x1001BEF0. Read the gp
value from those two instructions and give it to the decompiler. Strings are in .data. The libc
is statically linked and has no symbols. FLIRT/BSim on uClibc 0.9.29-era nios2 is not available.

**Kernel:** `work/firmware/epcs1/vmlinux.bin`. Load it raw at **0x00000000**, entry at 0x0. The
platform data is at 0x1FFB20–0x1FFE40. There are no symbols, but `__func__`-style strings and
`printk` formats are plentiful.

**zImage head:** `rec0_00d00000.bin`. Load it raw at 0x00D00000.

**Ghidra (works) [V]:** there is a custom SLEIGH module in `tools/ghidra-nios2/`, language ID
`Nios2:LE:32:default`. The ELF opinion selects it automatically for e_machine 113. It covers the
full R1 integer ISA, the uClinux `trap` syscall model, the Nios II ABI cspec, and gp tracking.
Checked against `nios2-elf-objdump`, it has **0 mismatches** on update_config, mux_sync, the whole
vmlinux, and a synthetic sweep of all opcodes. Every function in update_config and mux_sync
decompiles. Commands (run from `work/ghidra`):
```sh
nix develop ~/re-shell -c ../../tools/ghidra-nios2/build.sh        # build .sla, install into ~/.config/ghidra/ghidra_12.0.2_NIX/Extensions/Nios2
nix develop ~/re-shell -c ../../tools/ghidra-nios2/nios2-import.sh -d out/<prog>/decomp ../firmware/epcs1/flat/<prog>.elf
nix develop ~/re-shell -c ../../tools/ghidra-nios2/nios2-import.sh -r 0x0 -n kernel ../firmware/epcs1/vmlinux.bin
```
The import script finds gp from the `_start` stub and sets it (override with `-g`). Exported C for
update_config, mux_sync and the kernel is under `work/ghidra/out/`. In update_config,
`FUN_10001c6c` is the config-file parser, `FUN_100018f4` is the config-file writer, and
`FUN_100000ec` is main. Limitations: R1 only, trap args r8/r9 are not shown, and a few blocks
reached only through pointers stay undisassembled.

## 8. What I could NOT determine

* The exact FPGA devices and families (only the header style is known). Whether the EPCS_2 images
  are factory/production copies or two designs.
* The JFFS2 config partition contents (not in the dump). Whether mtdblock0 is really the EPCS_1
  partition at 0xE00000 (the `epcs` text says "mounting EPCS_1 PROM file system: /mnt", which
  supports it). The live `/proc/mtd` would settle it.
* The EEPROM part and bus (I²C behind FPGA logic? a DDQ MCU?). What the mailbox command numbers
  mean (6, 12, 0x11, …).
* The meaning of EEPROM 0x1128–0x112B (not in the descriptor table).
* The `/etc/config/config` DES hash (not in rockyou).
* The root cause of the >48 problem (see §4 leads).
