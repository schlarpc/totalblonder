# First air: putting a carrier on the modulator

A runbook for the first time a stream from `totalblonder` reaches the EQAM-420B. It replaces
SPEC M0's ffmpeg-plus-`tsp` spike: the server now produces the stream itself
(`docs/SERVER.md`). Every step that changes the modulator is marked **(writes)** and needs the
owner.

## 0. Where things stand (2026-11-26, 03:30)

- The control port 10.42.0.184 answers from bagel and nixark; the supervisor has run against it
  in report mode (every page parsed, nothing written).
- GIGE1 (10.42.0.183, MAC 00:14:39:00:35:C6 per `docs/notes/device.md` §1) does not answer ARP
  from either host: `ip neigh` shows it INCOMPLETE. Either the port is not cabled to this LAN,
  or the FPGA's data port does not answer ARP at all (the Linux side of eth2 saw 0 packets in
  the survey). Ping is therefore not a test of the data path; the box's own status page is.
- The box is in MPTS mode, 16 carriers on and locked, no inputs configured, Configuration Mode
  Automatic (defect B active at the next boot), uptime 8 days.

## 1. Decisions needed first

1. **The data path.** Cable GIGE1 to the switch nixark is on (the same L2 as 10.42.0.0/22), or
   directly to a NIC of nixark's. GIGE1 is configured inside the control port's subnet, which
   the manual forbids but which also means no re-addressing is needed on our side. Network
   settings of the box are not to be changed (owner's rule).
2. **Multicast or unicast.** Multicast (`239.10.0.1:5000` for carrier 1) needs no ARP: the box
   joins the group with IGMPv2 on GIGE1 and the switch forwards it (the switch must not drop
   unknown multicast, or must snoop IGMP correctly). Unicast to 10.42.0.183 needs ARP to
   resolve; if the FPGA does not answer ARP, add a static entry on nixark:
   `ip neigh replace 10.42.0.183 lladdr 00:14:39:00:35:c6 dev ens16 nud permanent`.
   Multicast is the default of the design (SPEC §4.2).
3. **Configuration Mode Manual** before the first configuration write (defect B, SPEC §10.3),
   set by hand on `admin.cgi` by the owner. The supervisor refuses that page.

## 2. The server

On nixark, a configuration with one carrier and the supervisor in report mode:

```toml
[output]
framing = "udp"

[[output.interface]]
name = "lan"
address = "10.42.0.15"          # nixark's address: the multicast interface
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
