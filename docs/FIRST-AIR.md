# First air: putting a carrier on the modulator

A runbook for the first time a stream from `totalblonder` reaches the EQAM-420B. It replaces
SPEC M0's ffmpeg-plus-`tsp` spike: the server now produces the stream itself
(`docs/SERVER.md`). Every step that changes the modulator is marked **(writes)** and needs the
owner.

## 0. Where things stand (2026-11-26, evening)

- The control port 10.42.0.184 is on the router's ether7 and answers from bagel and nixark. The
  supervisor has run against it in report mode (every page parsed, nothing written).
- GIGE1 is cabled to the rack switch's sfp-sfpplus14 and GIGE2 to sfp-sfpplus8. Both links
  needed the switch-side speed forced (schlarpc-network-terraform `config/interfaces.nix`).
  With the speed forced the switch reports link even with no copper link, so use the box's
  LEDs, or where its MAC is learned, as the test. GIGE1 (10.42.0.183, `00:14:39:00:35:C6`)
  answers ARP and ping. GIGE2 still has its old address (192.168.253.4, corrupted mask) and
  does not answer.
- The box is in MPTS mode, 16 carriers on and locked, no inputs configured, Configuration Mode
  Automatic (defect B active at the next boot).

## 1. Before the first stream

1. **Data VLANs (network repo, Proxmox, flake).** VLAN 20 `10.42.20.0/24` on sfp-sfpplus14 and
   VLAN 21 `10.42.21.0/24` on sfp-sfpplus8, untagged, tagged on the Proxmox trunk, with no
   router interface. Give `nixark` one VF in each at `.1` (SPEC §4.2).
2. **One `admin.cgi` save (writes; owner present).** Take an EEPROM dump first
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
