# First air: putting a carrier on the modulator

A runbook for the first time a stream from `totalblonder` reaches the EQAM-420B. It replaces
SPEC M0's ffmpeg-plus-`tsp` spike: the server now produces the stream itself
(`docs/SERVER.md`). Every step that changes the modulator is marked **(writes)** and needs the
owner.

## 0. Where things stand (2026-11-27)

- The control port 10.42.0.184 is on the router's ether7. The supervisor has run against it in
  report mode (every page parsed, nothing written).
- The data network is up. GIGE1 is on the rack switch's sfp-sfpplus14, untagged in VLAN 20, and
  GIGE2 on sfp-sfpplus8 in VLAN 21. Both VLANs go to the Proxmox trunk and nowhere else.
  `nixark` has a VF in each: `eqam-gige1` 10.42.20.1 and `eqam-gige2` 10.42.21.1. Both switch
  ports have their speed forced, so the switch reports link even with no copper link; the
  box's LEDs, or ARP, are the test.
- The `admin.cgi` save in §1 item 2 was made on 2026-11-27 at 08:38 UTC. The box rebooted into
  Manual mode, and GIGE1 (10.42.20.2) and GIGE2 (10.42.21.2) answer ARP and ping from nixark. The EEPROM
  dumps before and after differ only at 0x40, 0x1136, 0x113A, 0x115A–0x115F and
  0xF30–0xF37, as predicted (firmware §4.1). The boot logged no "Difference detected". (The
  "Password file not found" line appears at every boot.)
- First stream, 2026-11-27 08:52 UTC: the §2 configuration ran on nixark as a transient unit
  (`systemd-run --unit=tb-trial -p DynamicUser=yes -p LoadCredential=eqam:...`; the config
  sits in `/run`, because `DynamicUser` gives the unit a private `/tmp`). Carrier 1 leaves
  `eqam-gige1` at 40.1 Mbit/s on the wire, 3,692 datagrams/s, with no nulls missing, send errors or late
  access units; the worst pacer lateness was 3.6 ms. One run with `reconcile = "apply"` wrote
  slot 1 (`Verified`, drift 0). The box shows row 1's IP cell green, carrier 2 locked, and
  Stream Info lists both programs (PMT 48 and 64; MPEG-2 video 49 and 65; AC-3 52 and 68).
  The Status bitrate (1.5 Mbit/s average, 4.1 max) counts programs without nulls: two slates
  leave the carrier about 93 % null. Not yet checked: a receiver on cable channel 2.
- The box is in MPTS mode, with 16 carriers on and slot 1 configured.

## 1. Before the first stream

1. **Data VLANs (network repo, Proxmox, flake).** VLAN 20 `10.42.20.0/24` on sfp-sfpplus14 and
   VLAN 21 `10.42.21.0/24` on sfp-sfpplus8, untagged, tagged on the Proxmox trunk, with no
   router interface. Give `nixark` one VF in each at `.1` (SPEC §4.2).
2. **One `admin.cgi` save (writes). Done 2026-11-27 (§0).** Take an EEPROM dump first
   (`tools/eedump`). Then, logged in as Admin, POST exactly these fields and nothing else
   (firmware §4.1):

   ```
   configMode=1
   ipAddressGigE_2a=10.42.20.2  subnetMaskGigE_2a=255.255.255.0  defaultGatewayGigE_2a=10.42.20.1
   ipAddressGigE_2b=10.42.21.2  subnetMaskGigE_2b=255.255.255.0  defaultGatewayGigE_2b=10.42.21.1
   ```

   The box writes the EEPROM and reboots (`epcs -c`) into Manual mode. Afterwards: dump again,
   check that only 0x40, 0x1134–0x113B, 0x1158–0x115F and 0xF30–0xF37 changed, check that the
   event log has no "Difference detected in Config File", and ARP for `.2` from `nixark` on each
   VLAN.
3. **Multicast** (`239.10.0.1:5000` for carrier 1), sent out the VLAN 20 VF with TTL 1. The box
   is the only receiver in its VLAN, so the group reaches it whether or not the switch acts on
   the box's join.

## 2. The server

On nixark, a configuration with one carrier and the supervisor in report mode:

```toml
[output]
framing = "udp"

[[output.interface]]
name = "gige1"
address = "10.42.20.1"          # nixark's VF on VLAN 20: the multicast interface
carriers = [1]
multicast_ttl = 1

[video]
device = "cuda"

[device]
base_url = "http://10.42.0.184"
credentials_file = "/run/credentials/totalblonder.service/eqam"
reconcile = "report"
level_dbmv = 40
  [[device.rf_module]]
  module = 1
  first_channel = 2
  [[device.rf_module]]
  module = 2
  first_channel = 10
  [[device.rf_module]]
  module = 3
  first_channel = 6
  [[device.rf_module]]
  module = 4
  first_channel = 140

[[carrier]]
id = 1
destination = "udp://239.10.0.1:5000"
  [[carrier.program]]
  number = 1
  service = "bars"
  video = { format = "720p", max_kbps = 10000 }
  audio = [ { language = "eng" } ]
  [[carrier.program]]
  number = 2
  service = "bars2"
  video = { format = "480i", max_kbps = 4000 }
  audio = [ { language = "eng" } ]

[[service]]
id = "bars"
name = "TB1"
channel = "50.1"
source_id = 1

[[service]]
id = "bars2"
name = "TB2"
channel = "50.2"
source_id = 2
```

Run it (`totalblonder --config FILE`, or the NixOS module `services.totalblonder`). Within a
minute the supervisor logs a drift alarm: slot 1 should be `UDP://239.10.0.1:5000` named
`TB1` and is empty. That is expected in report mode.

## 3. Enter the stream (writes)

Either by hand on the Status Edit page (slot 1: `UDP://239.10.0.1:5000`, name `TB1`), or by
setting `reconcile = "apply"` for one run, which writes `statusedit.cgi` with all 16 rows (the
other 15 empty, as they are now) and reads the page back. The supervisor never touches any
other form unless the RF plan differs from the four modules above, which it does not today.

## 4. What to look at

| Where | Expect |
|---|---|
| `status.cgi` row 1 | IP cell green (input present), bitrate about 38.8 Mbit/s |
| `streaminfo.cgi` | Two programs with PMT PIDs 0x30 and 0x40 |
| `debug.cgi`, `diagnostic.cgi` | No overflow, no TS sync errors, no 100 ms timeouts |
| `/api/metrics` | `null_datagrams_total` 0, `send_errors_total` 0, `late_au_total` 0 |
| A TV or the HDHomeRun on cable channel 2 (57 MHz) | Channels 50.1 TB1 and 50.2 TB2, bars, the clock set from the STT, two guide entries |

Then the questions of SPEC M0 can be answered with the server instead of the spike: UDP or RTP
(`framing`), packets per datagram (`packets_per_datagram`), behavior near the ceiling
(`ts_rate`), and whether EIT and ETT pass (the guide on the TV).
