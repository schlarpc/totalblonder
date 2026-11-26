# Design inputs

Decisions, owner-supplied facts and feature candidates collected during the lay-of-the-land
pass (2026-11-24). This is input for a future `docs/SPEC.md`, not the spec.

Labels: [O] the owner said it; [V] verified by us; [K] from general knowledge, not tested
here.

## Facts from the owner

- [O] The owner's friend runs the same model in MPTS mode with EPG working. Guide data is
  generated upstream and passed through.
- [O] The same friend reported that the model corrupts its configuration "when channels over
  48 are used".
- [O] The friend probably did the initial setup of this unit. The flash files dated
  2026-09-07 (`config_file.dat` with 48 lines, `results.dat`, `nas/`) are likely from that.
- [O] The friend has written a CUDA MPEG-2 encoder (AI-assisted). Not yet seen by us: source
  availability, rate-control and VBV behavior, interlace support and quality are unknown.
- [O] An HDHomeRun and attenuators have been bought to monitor the RF output.
- [O] The owner may change anything on the device except network and login settings.

## Host

- [O] The server will run on `nixark`, which holds an NVIDIA RTX 2000 Ada (16 GB).
- [V] 2026-11-24: `nixark` reports 8 CPUs (AMD EPYC 9124, a 16-core part, so likely a VM or
  a restricted CPU set), 31 GB RAM, NVIDIA driver 595.71.05.
- [O][V] `nixark` is VM 113 on the Proxmox host `proxmox`: 8 vCPUs (threads, `cpu: host`),
  32 GB, no NUMA setting, one SR-IOV network function plus the GPU passed through. The host
  has two EPYC 9124 (2 × 16 cores, 64 threads) and 755 GB RAM, about 160 GB free on
  2026-11-24. Other large VMs: `vllm` 32 vCPUs / 256 GB, `ci` 16 / 96 GB, a search
  appliance 16 / 64 GB. vCPU allocations already exceed 64, so more vCPUs for `nixark` are
  possible but contend with those VMs when they are busy.
- The CPU figures in `software-survey.md` were measured on `bagel` (Ryzen 7 5800X), not on
  `nixark`. Per-core speed differs; re-measure there before sizing.

## Decisions so far

- Run the box in MPTS mode. The server builds all tables.
- Re-encode every input by default. Add pass-through later for inputs that already meet the
  constraints (rate-capped, sane GOP, AC-3 audio).
- Keep audio as AC-3: the box parses audio frames even in MPTS mode (420A changelog).
- 4K is out of scope: the box documents only MPEG-2 and H.264, and TV tuner paths are not
  expected to decode HEVC [K].
- Before the first configuration write: set Configuration Mode to Manual so that a reboot
  cannot re-apply the 48-line file (`corruption-48.md`, defect B).

- [O] 2026-11-25: assume the GPU in `nixark` is shared with Plex and Jellyfin; do not plan
  on a dedicated card. Capacity planning uses the measured figures divided by about 1.6
  (`cuda-mpeg2-optlog.md` §32; SPEC §14.x).
- [O] 2026-11-25: media-server load on that GPU is 1–3 simultaneous transcodes, some from
  2160p sources, with HDR tone mapping occasional (expected under 10 % of traffic).
- We built our own CUDA MPEG-2 encoder (the C++ spike, `cuda-mpeg2-spike.md`; ported to Rust,
  `docs/ARCHITECTURE.md`); it is the leading candidate for MPEG-2 channels. The friend's encoder remains unseen.

- [O] 2026-11-25: the encoder's host side is being ported to Rust as a workspace of sans-io
  crates with adapters, built by breakbulk (`docs/ARCHITECTURE.md`). The C++ spike is the
  oracle for the port only: C++ parity is dropped soon after the Rust reaches it, and the
  C++ is not carried afterwards. Consequences: bugs found in the C++ during the port are
  ported as they are and fixed in Rust once parity is shown; the golden vectors then become
  Rust-generated regression snapshots; the gate (decoders, buffer-model checks) must run
  against the Rust binary; the kernel sources move out of the spike.
- [D] 2026-11-25: done (package X1, `docs/port/X1-notes.md`): the C++ is deleted, the kernels
  are `kernels/`, the gate is `gate/` with the conformance verifier on every stream, the golden
  vectors are written by the Rust recorder; the encoder's output may change from now on.

- [O] 2026-11-26: coding should flex fairly dynamically between back ends per channel:
  decode on NVDEC or CPU; MPEG-2 encode on CUDA or CPU; H.264 encode on NVENC, CPU or CUDA.
  H.264 output is in scope eventually. Implication: placement is a run-time decision made
  from measured costs and budgets, and a channel must be able to move between back ends
  without a visible break.

## Codec choice per channel

- [K] Every cable-ready TV decodes MPEG-2. H.264 over QAM works on most sets from roughly
  the last decade but must be tested per TV model.
- H.264 through this unit is documented in the manual but not yet tested by us.
- Working ceiling: 1080i/720p MPEG-2 for universal reach, 1080p H.264 where TVs accept it.

## Feature candidates

Requested by the owner: EPG, SAP, MPTS, dynamic bitrate allocation, network ingest from
producers, Rust implementation, high robustness.

Visible on the TV [K]:
- System time table (TVs set their clock and place guide entries from it).
- Closed captions: CEA-608/708 in the video plus the caption service descriptor.
- Content advisory descriptor, so parental locks work.
- Extended text (program descriptions) alongside guide titles.
- Audio: language tags per track, described-video track type, loudness normalization.
- Aspect-ratio signaling (AFD / bar data).
- Audio-only channels, most reliably as a still slate with audio.

Robustness:
- A QAM tuner as a monitoring probe: verify each carrier is on air, decodable and labeled.
- Continuous self-checks: table repetition rates, PCR jitter, continuity, bitrate headroom.
- Device supervision: poll status, detect configuration drift, re-apply known-good settings.
- Per-channel fallback chain: live source, standby source, slate; switch at keyframes.
- Priority-aware bitrate sharing within a carrier.

Worth considering:
- A scheduler/playout layer and a "now/next" API for live producers; the guide needs a
  schedule to describe.
- A generated scrolling guide channel.
- EAS override as an "all channels show this" feature. The trigger packet format on UDP 5050
  is not decoded.
- Interactivity out of band: a web or phone remote that changes what a channel shows.

Not worth pursuing [K]:
- Channel logos, on-demand, pay-per-view: no clear-QAM TV supports them.
- Pro:Idiom encryption: unlicensed on this unit and needs hospitality TVs.
- Directed channel change and data broadcast: ignored by real TVs.

## Interactivity

- [K] US clear-QAM TVs are one-way. tru2way (OCAP) shipped in a few Panasonic sets around
  2008–2009 and needed a CableCARD and the operator's control channel. EBIF ran only on
  set-top boxes.
- [K] HbbTV works over DVB-C (QAM Annex A) on European TVs. The box lists Annex A modes.
  Unchecked: whether its Annex A symbol rates suit a real DVB-C TV. It would need DVB tables
  in place of the North American ones on those carriers. A side experiment at most.

## Monitoring probe

- HDHomeRun with clear-QAM support (Annex B). It relays the raw transport stream without
  decoding, reports signal strength, signal quality and symbol quality per tuner, and tunes
  by frequency and modulation from a script [K].
- The box outputs about +40 dBmV per carrier; a tuner wants roughly 0 to +10 dBmV. Start
  with about 30 dB of 75-ohm attenuation and adjust from the tuner's readings. With 16
  carriers on, total power is about 12 dB above one carrier.
- Once TVs are attached, a 20–27 dB directional tap is the better way to feed the tuner.

## Questions for the friend

1. What was used to set this unit up on 2026-09-07: web UI only, a config upload, a script,
   or a firmware change?
2. Does she recognize a lineup file with lines like `0,0,2-1`, or `results.dat` and `nas/`?
3. Did anyone then save QAM Config for modules 1 and 4 and clear the stream addresses?
4. Did the corruption appear at once on adding channels, or after the box ran for a while?
5. What did "corrupted" look like: lost address, wrong channels, or a reset to defaults?
6. What generates her stream and guide data (tsduck, ffmpeg, a commercial mux, custom)?

## Research not yet done

- Measured input tolerances of the box: packets per datagram, jitter, unicast, bitrate
  limits, behavior on overflow. Needs a first test stream.
- An implementer's digest of the standards in `docs/external/standards/` (table syntax, PIDs,
  repetition rates, descriptors) for PSIP, captions and AC-3 signaling.
- A survey of existing software: tsduck, ffmpeg's mpegts muxer limits, Rust transport-stream
  crates, AC-3 and MPEG-2 encoders, statmux approaches.
- Encode capacity of the host: how many simultaneous re-encodes it can sustain, in software
  and with hardware encoders.
- Which codecs the actual TVs on the plant accept.
- HDHomeRun control API and what it reports, once the unit arrives.
- EAS trigger packet format; unexamined firmware programs (see `corruption-48.md`).
