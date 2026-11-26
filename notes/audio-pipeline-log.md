# Audio pipeline (`tb-audio`): running log and design record

Everything between the decoder's `f32` planar output and the AC-3 encoder's 1536-sample frames:
resampling to 48 kHz, channel mapping, loudness measurement and normalization, true-peak
limiting, and the sample clock (SPEC §5.2–§5.4, §8.3, §9.5). Written 2026-11-26, unattended;
every decision that was mine is marked **[choice]** with its reason.

## How things are run

- Worktree branch `worktree-agent-ac7963b6558405994`, started from `60ca408`.
- `nix develop -c cargo nextest run -p tb-audio`: the sandbox set (62 tests, about 45 s
  unoptimized; the longest is the one-minute M9 test).
- `nix develop -c cargo nextest run --release -p tb-audio --run-ignored ignored-only
  --no-capture`: the two ignored tests, the ten-minute M9 run and the comparison with ffmpeg
  (needs `ffmpeg` on `PATH`; it passes with a note when ffmpeg is absent).
- Reports, release build: `cargo run --release -p tb-audio --example resampler_report`,
  `--example normalizer_report`, `--example cost`.

## State

Built and tested. Commits on the branch after `60ca408`:

1. `b80aade` resampler, channel mapping, BS.1770-4 meter, EBU Tech 3341 tests;
2. `e5eb8a2` normalizer and true-peak limiter;
3. `67c4088` sample clock, trim controller, property tests;
4. `7bc346e` `TrackPipeline`, limiter fast path, cost example;
5. `6380759` pipeline tests (M9, timing, trim loop, layouts);
6. `9f6c608` the meter against ffmpeg;
7. `a099358` resampler report, crate documentation;
8. `af20b6d` normalizer: loud-to-quiet program changes, restart per session;
9. `d7f8f47` saturating time arithmetic in the clock; then this log.

Not done: see "Open items" at the end.

## Where it sits

```text
 decoder (tb-media AudioFrameDesc: f32 planar, any rate, any channel count)
   │  planes + output time of the first sample (the mapper of tb-timeline, SPEC §5.3)
   ▼
 TrackPipeline::push(source, at, planes)          one per output audio track
   live    ─► resample ─► map ─► normalize ─► limit ─┐
   standby ─► resample ─► map ─► normalize ─► limit ─┴─► SampleClock ─► AudioFrame (1536 × ch)
                                                         slate = silence     │
 TrackPipeline::next_frame(&mut frame) -> FrameInfo { index, pts, source }   ▼
                                     tb-ac3 encode(&frame.planes())  (A/52 channel order)
```

The crate is pure (`#![forbid(unsafe_code)]`, no clock, file, thread or global state); time is
`tb_timeline::Ticks27` and 48 kHz sample indices. It depends on `tb-timeline` (for `Ticks27`,
`Source`, `to_pts90`) and on nothing else but `thiserror`.

**[choice]** The SPEC and ARCHITECTURE §9 put the sample clock in `tb-timeline` and made
`tb-audio` the libav AC-3 adapter. With an encoder of our own (`tb-ac3`, another agent) the
audio work is all pure DSP, and the sample clock is entangled with it (fades and crossfades act
on samples, the drift feeds the resampler), so it lives here next to the resampler. ARCHITECTURE
§9's row "§8.3 audio" should now read: pure crates `tb-audio` (this) and `tb-ac3`; no adapter.

## Modules and API

| Module | Main types | Standard |
|---|---|---|
| `resample` | `InputRate` (8–192 kHz, validated), `Trim` (±500 ppm in ppb, validated), `Resampler::{new, process, set_trim, next_output_position}`, `Design` | — |
| `layout` | `Speaker`, `InputLayout` (distinct speakers, 1–16; `default_for(n)`), `OutputLayout::{Stereo, FivePointOne}` (`acmod`, `lfeon`, A/52 order), `Mixer`, `DownmixScaling` | A/52 Table 5.8, §7.8 |
| `loudness` | `LoudnessMeter` (M, S, I, true peak), `BlockMeter`, `GatingHistogram`, `KWeighting`, `Oversampler`, `TruePeak`, `Loudness`, `Dbtp` | BS.1770-4, A/85 Annex A |
| `normalize` | `Normalizer`, `NormalizerConfig` (validated) | A/85 §6, SPEC §8.3 |
| `limiter` | `Limiter`, `LimiterConfig` (validated) | A/85 §6, BS.1770-4 Annex 2 |
| `clock` | `SampleClock`, `ClockConfig`, `AudioFrame`, `Planes`, `FrameInfo`, `SourceCounters`, `ClockCounters`, `TrimController`, `sample_at` | SPEC §5.2, §5.4, §9.5 |
| `pipeline` | `TrackPipeline`, `TrackConfig`, `InputFormat`, `PipelineError` | — |
| `signal` | test signals: `sine`, `program`, `fit_sine`, `Rng` | — |

Use by the server, per output track:

```rust
let mut t = TrackPipeline::new(TrackConfig::new(p0, OutputLayout::Stereo))?;
t.open(Source::Live, InputFormat { rate: InputRate::new(44_100)?, layout })?;
t.switch(Source::Live, at);                 // with the video cut (Fallback, SPEC §9.5)
t.set_skew(Source::Live, mapper.skew_ppb().unwrap_or(0));
t.push(Source::Live, mapper_time_of_first_sample, &planes)?;  // as blocks are decoded
let info = t.next_frame(&mut frame);        // every 32 ms of STC, at PTS − Δ
encoder.encode(&frame.planes());            // info.pts90() = P0 + 2880·j (mod 2^33)
```

`end_session(source)` when the mapper ends a session; `close(source)` when an input goes away.
Several tracks of a program (SPEC §8.3: up to three) are separate `TrackPipeline`s with the
same `p0`; their frames then have identical PTS by construction.

## 1. Resampler

**Design.** Kaiser-windowed sinc evaluated at a continuously variable position (bandlimited
interpolation). The kernel is tabulated at 512 phases per input sample, and coefficients between
two phases are interpolated linearly with a 24-bit weight. The position is a 64.64 fixed-point
number advanced by an exact step `fs_in/48000 · (1 + trim)` computed in 128-bit arithmetic, so
the ratio does not drift and a trim change only changes the step: the waveform stays continuous
(no click; test `trim_change_is_click_free` puts a 500 ppm step in a tone and finds the output
within 1e-5 of the ideal tone at the exact output positions). Each tabulated row is normalized
to unit sum (no phase-dependent DC gain). The dot product runs in eight lanes, which the
compiler vectorizes. History is per channel; after construction nothing is allocated except the
growth of the caller's output vectors.

**[choice] Filter edges.** For input rates of 44.1 kHz and above the passband is 20 kHz and
the stopband starts at `min(fs_in, 48 kHz) − 20 kHz` (24.1 kHz for 44.1 kHz, 28 kHz for 48 kHz
and above), not at the Nyquist frequency: anything in the transition band lands above 20 kHz in
the output after folding, so nothing audible aliases, and the transition band is twice as wide
(half the taps). Below 44.1 kHz (32 kHz broadcast, speech rates) the region above the input's
Nyquist is audible in the output, so the classic design: passband `0.45 fs`, stopband `0.5 fs`.
Stopband attenuation 120 dB.

**[choice] 48 → 48 kHz.** At 44.1 and 48 kHz input the cutoff (middle of the transition band)
falls exactly on the input's Nyquist frequency, so the phase-0 row is an exact unit impulse
(the kernel code returns exact zeros at the sinc's zero crossings). With zero trim since
construction the position stays on whole samples and the output is bit-identical to the input
(test `same_rate_zero_trim_is_bit_transparent`, report item 3). There is no separate bypass
mode, so a trim moving away from zero and back never switches paths (a bypass would have to
cut between a delayed copy and a filter, at a fractional position). After any non-zero trim the
position is fractional and the output is a fractional delay, −137 dB THD+N or better below
10 kHz.

**Measurements** (`--example resampler_report`, release build):

Designed kernels (continuous response; ripple to the passband edge, worst stopband level from
the stopband edge to 4× the input rate):

| Input | Taps | Pass (Hz) | Stop (Hz) | Ripple (dB) | Stopband (dB) |
|---|---|---|---|---|---|
| 8 / 16 / 22.05 / 32 kHz | 160 | 0.45 fs | 0.5 fs | 0.000008 | −120.2 |
| 44.1 kHz | 88 | 20 000 | 24 100 | 0.000008 | −121.5 |
| 48 kHz | 48 | 20 000 | 28 000 | 0.000008 | −119.8 |
| 88.2 kHz | 88 | 20 000 | 28 000 | 0.000008 | −120.5 |
| 96 kHz | 96 | 20 000 | 28 000 | 0.000008 | −119.8 |
| 192 kHz | 192 | 20 000 | 28 000 | 0.000008 | −119.8 |

End to end, through the tabulated, interpolated `f32` implementation: passband gain over
20 Hz–20 kHz within −0.000022 / +0.000009 dB (44.1 kHz, 48 kHz at ±500 ppm, 96 kHz).

THD+N of a −6 dBFS sine (least-squares fit of the tone at its exact output frequency;
everything else counted), dB relative to the tone:

| Input, trim | 997 Hz | 6 kHz | 12 kHz | 19 kHz |
|---|---|---|---|---|
| 44.1 kHz, 0 | −141.9 | −136.7 | −126.0 | −117.8 |
| 44.1 kHz, +250 ppm | −142.0 | −137.5 | −127.5 | −119.0 |
| 48 kHz, 0 (identity) | −153.7 | −167.1 | −235.5 | −154.1 |
| 48 kHz, +1 ppb | −144.9 | −144.8 | −146.5 | −141.5 |
| 48 kHz, ±500 ppm | −137.0 | −138.5 | −127.1 | −118.7 |
| 32 kHz, 0 | −136.2 | −131.4 | −118.9 | — |
| 96 kHz, 0 | −146.4 | −149.9 | −235.5 | −145.8 |
| 192 kHz, −123 ppm | −145.1 | −145.1 | −144.7 | −142.0 |

(The "identity" row measures the input's own `f32` rounding against the fit.) The rise toward
19 kHz at fractional positions is the linear interpolation between table phases (its error grows
with frequency squared); at −118 dB it is 28 dB better than the 90 dB asked for. Rejection of
tones that must be removed when downsampling: 96 kHz input, 28 kHz tone −120.4 dB, 30 kHz −141.2,
40 kHz −145.7; 192 kHz input, 30 kHz −135.6, 70 kHz −142.1; 88.2 kHz input, 30 kHz −137.5.

Timing: output sample `n` is the input at position `p(n)`, exactly
(`next_output_position`); the processing latency is `K` input samples (half the taps). The
pipeline turns positions into output times (below); `timing_through_the_stages` finds a tone
burst within 0.5 samples of where its input time says, at 44.1, 48 and 96 kHz.

## 2. Channel mapping

Output order is A/52's: Table 5.8 (A/52:2012 p.39) gives "L, R" for `acmod` 2 and
"L, C, R, SL, SR" for `acmod` 7; the LFE channel is coded after the full-bandwidth channels.
`OutputLayout::channels()` returns exactly that, and `acmod()`/`lfe()` the codes.

Mapping, as a constant matrix in two steps:

1. Into a 5.1 bed: front channels and LFE to their own; side and back pairs into the surrounds
   (both present, as in 7.1: −3 dB each, power-preserving; one pair: unity); a back center into
   both surrounds at −3 dB (A/52 §7.8.1 routes a mono surround "into left and right with −3
   dB"); front left/right-of-center into center and side at −3 dB each.
2. To stereo, the LoRo downmix of A/52 §7.8.2 (p.95): `Lo = L + clev·C + slev·Ls`,
   `Ro = R + clev·C + slev·Rs`, `clev = slev = 0.707` (Tables 5.9, 5.10, code '00'; also the
   ITU-R BS.775 coefficients). LFE dropped (§7.8: optional). Mono → `0.707·C` in each of L, R.

**[choice] No upmix.** Stereo into a 5.1 track is L and R only; C, surrounds and LFE silent.
Synthesizing a center or surrounds changes the mix and is audible as such; a stereo source
should normally get a 2/0 track anyway. Mono into 5.1 is the center.

**[choice] Downmix scaling.** A/52 §7.8.2 scales LoRo coefficients by 0.414 (−7.65 dB) so a
fixed-point decoder cannot overflow. We work in `f32`, and the normalizer sets the level and the
limiter catches peaks after the mix, so the default is unscaled (it keeps the level of a
downmix near the source's); `DownmixScaling::A52WorstCase` gives the A/52 factor
(test checks Table 7.31: 0.414 and 0.293).

**[choice] Default layouts by count.** `InputLayout::default_for(n)` uses the WAVE / SMPTE
channel order that libav also uses: 1 C; 2 L R; 3 L R C; 4 L R BL BR; 5 L R C BL BR; 6 5.1
(L R C LFE BL BR); 7 6.1 (L R C LFE BC SL SR); 8 7.1 (L R C LFE BL BR SL SR). Counts 3–5 are
ambiguous (2.1 vs 3.0, quad vs 4.0); the decoder adapter should pass the layout libav reports
(`tb_media::AudioFrameDesc` carries only a count today; see "Open items").

**Order of resample and map.** Both are linear and time-invariant per channel, so they commute.
The pipeline maps first when the source has more channels than the track (5.1 → stereo:
resample 2 channels, not 6) and resamples first otherwise.

## 3. Loudness meter (BS.1770-4)

The A/85 PDF in `docs/external/standards` (A/85:2013 with Corrigendum 1) describes BS.1770 in Annex A
(pp.38–43: K-weighting as pre-filter + RLB, channel weights, gating at −70 LKFS absolute and
−10 LU relative, 4× oversampled true peak) but does not print the constants. They are from
ITU-R BS.1770-4 (10/2015): Annex 1 Table 1 (pre-filter at 48 kHz: b = 1.53512485958697,
−2.69169618940638, 1.19839281085285; a = −1.69065929318241, 0.73248077421585), Table 2 (RLB:
b = 1, −2, 1; a = −1.99004745483398, 0.99007225036621), Table 3 (weights 1.0 for L, R, C; 1.41
for Ls, Rs; LFE excluded), eq. (2) (`−0.691 + 10 log10 Σ G z`), eqs. (3)–(7) (gating). The meter
works at 48 kHz only (the type has no rate parameter), which is where it is used.

Integrated loudness is kept in bounded memory: a histogram of 400 ms block powers in 0.01 LU
bins from −70 to +10 LUFS, holding per bin the count and the exact sum of powers; only the
relative gate's threshold is resolved to 0.01 LU. Momentary (400 ms) and short-term (3 s) are
means of the last 4 and 30 100-ms steps.

**[choice] True-peak interpolator.** BS.1770-4 Annex 2 shows a 48-tap (4 × 12) interpolating
filter as an example. Ours is a Kaiser-windowed sinc, 4 phases × 24 taps, passband 20 kHz,
stopband 28 kHz (≈65 dB): its passband is flatter (within 0.005 dB to 20 kHz, test
`oversampler_interpolates_to_20_khz`) and the code is shared with the resampler. The readings
agree with ffmpeg to 0.01 dB (below).

**EBU Tech 3341 (v4) cases, synthesized** (`tests/ebu3341.rs`; the EBU's WAV files are not in
the repository, so the cases defined by a formula): case 1 (1 kHz stereo −23 dBFS: M, S, I),
2 (−33), 3 (−36/−23/−36: relative gate), 4 (−72/−36/−23/−36/−72: both gates), 5 (−26/−20/−26),
6 (5.0: L R −28, C −24, Ls Rs −30; with a full-scale LFE that must not count), 9 (short-term at
every step: 1.34 s −20 / 1.66 s −30), 12 (momentary at every step: 0.18 s −20 / 0.22 s −30); all
within the required ±0.1 LU. True peak, built like cases 15–19 (sines at fs/4, fs/6, fs/8 with
45°, 60°, 67.5° phase so every sample is below the peak, and a 19 997 Hz sine): all within the
required +0.2/−0.4 dB. (A hard onset is a step whose band-limited waveform does overshoot; the
test tones start and end with a 10 ms ramp.)

**Against ffmpeg** (`tests/ffmpeg.rs`, ffmpeg 6.1.6 from the user profile on bagel; the same files through
`ebur128=peak=true` and `loudnorm`'s measurement pass):

| Case | I ours | ebur128 | loudnorm | TP ours | ebur128 | loudnorm |
|---|---|---|---|---|---|---|
| 3341-1 −23 dBFS | −22.99 | −23.0 | −23.05 | −23.00 | −23.0 | −23.00 |
| 3341-2 −33 dBFS | −32.99 | −33.0 | −33.05 | −33.00 | −33.0 | −33.00 |
| 3341-3 −36/−23/−36 | −23.01 | −23.0 | −23.07 | −23.00 | −23.0 | −23.00 |
| 3341-4 −72/…/−72 | −23.01 | −23.0 | −23.07 | −23.00 | −23.0 | −23.00 |
| 3341-5 −26/−20/−26 | −22.98 | −23.0 | −23.14 | −20.00 | −20.0 | −20.00 |
| 3341-6 5.0 | −23.02 | −23.0 | −23.05 | −24.00 | −24.0 | −24.00 |
| TP 12 kHz 45° −6 dBTP | −2.65 | −2.7 | −2.66 | −6.00 | −6.0 | −6.00 |
| TP 8 kHz 60° −6 dBTP | −2.65 | −2.7 | −2.66 | −6.00 | −6.0 | −6.00 |
| TP 6 kHz 67.5° −6 dBTP | −2.66 | −2.7 | −2.66 | −6.00 | −6.0 | −6.00 |
| TP 19 997 Hz 10° −3 dBTP | 0.35 | 0.3 | 0.34 | −3.00 | −3.0 | −3.00 |
| program noise, stereo | −21.03 | −21.0 | −21.07 | −5.39 | −5.4 | −5.39 |
| program noise, 5.1 | −16.33 | −16.3 | −16.38 | −5.55 | −5.5 | −5.55 |
| pipeline output, input −10 | −23.44 | −23.4 | −23.54 | −2.20 | −2.2 | −2.19 |
| pipeline output, input −24 | −24.08 | −24.1 | −24.14 | −9.27 | −9.3 | −9.27 |
| pipeline output, input −35 | −24.17 | −24.2 | −24.22 | −9.28 | −9.3 | −9.28 |

ebur128 prints one decimal; we agree with it to its rounding everywhere. `loudnorm` reads
0.05–0.15 LU low on the EBU cases, whose true value is known to be −23.0, so the test bounds it
at 0.2 LU. True peak agrees to 0.01 dB with loudnorm's two decimals. (The "pipeline output"
rows are 59 s including the first seconds of convergence, which is why the −10 input reads
−23.4 here and −24.0 in the M9 test, measured after 20–30 s.)

## 4. Normalizer

A feed-forward gain driven by a BS.1770 measurement of its own input, no look-ahead (SPEC §8.3:
not `loudnorm`, which adds up to 3 s). The measurement is a gated loudness (absolute −70,
relative −10 LU) over the last 100 *active* 400 ms blocks (10 s of program); blocks at or below
−50 LUFS do not enter, so the gain holds through pauses and silence of any length instead of
creeping up on room tone. The wanted gain `−24 − measured` is clamped to [−30, +15] dB and
followed by a first-order filter in dB, updated every 100 ms and interpolated per sample.

**[choice] Time constants and limits:**

| Situation | τ | Reason |
|---|---|---|
| gain going down | 2 s | loud material comes down promptly without pumping on transients |
| gain going up | 8 s | slow rise: speech and music dynamics are not flattened |
| gain going up by more than 3 dB | 2 s | a quiet program after a loud one is not left far down |
| first 3 s of active input of a source / session | 0.5 s | a new source is at the target in a second or two |
| output momentary more than 8 LU over the target | 0.3 s | a loud program after a quiet one is pulled down at once (the limiter holds peaks meanwhile) |

Max boost +15 dB (an M9 input at −35 needs +11; 15 dB covers −39 LKFS without lifting
near-silence much further); max cut 30 dB (inputs up to +6 LKFS).

**[choice] Program change.** The relative gate keeps a previous loud program's blocks in
the measurement and gates the new quiet ones out, so a loud-to-quiet change would otherwise not
register until the window had emptied. When the last 3 s of active blocks are all more than
15 LU below the measurement, the older blocks are dropped. 15 LU is high so that the quiet
scenes of one program (6–12 LU under its dialogue) are not taken for a new program. A new
session (`end_session`) restarts the measurement with the fast acquisition.

**Results.** SPEC M9 ("for test inputs at −10, −24 and −35 LKFS, output measured over 10 minutes
is −24 ± 2 LKFS"), program-like noise (pink-ish noise with a syllabic envelope and pauses of
0.2–1 s every few seconds), stereo 48 kHz, producer clock 40 ppm fast with the skew given:

| Input | Output, 10 min (from 30 s) | Output, 1 min (from 20 s) | True peak | Final gain |
|---|---|---|---|---|
| −10.00 LUFS | −24.03 | −24.05 | −2.20 dBTP | −13.91 dB |
| −24.00 LUFS | −24.04 | −24.04 | −7.97 dBTP | +0.05 dB |
| −35.00 LUFS | −24.04 | −24.07 | −8.12 dBTP | +10.98 dB |

The one-minute version runs in the sandbox (`m9_loudness_one_minute`, asserting ±2 and, for our
own standard, ±0.5); the ten-minute one is `#[ignore]`d (release build, 19 s). Behavior over
time (`--example normalizer_report`; short-term loudness once a second):

| Scenario | Gain within 2 dB / 1 dB of final | Notes |
|---|---|---|
| start at −10, −24, −35 | 0–2 s | the first short-term reading (3 s) is −17.5 for the −10 start |
| −35 then −10 (quiet to loud) | 3 s / 4 s | output −17, −16, −16 LUFS short-term for 3 s, then −21, −22, −23; true peak held at −2.20 |
| −10 then −35 (loud to quiet) | 11 s / 17 s | the change is detected after 3 s; output short-term −48 for about 3 s, −31 at 7 s, −26 at 11 s |
| −24, 60 s of digital silence, −24 | 0 s | the gain does not move during the silence (+0.02 dB at the end) |

The loud-to-quiet case is the weak one: about 10 s below −26 LUFS. Faster is possible
(lower `change_s`, higher `recover` speed) at the price of pulling up quiet scenes; it is a
configuration change, not a design change.

## 5. True-peak limiter

Per sample, the true peak of the interval it starts is estimated with the meter's 4×
oversampler, over all channels (linked: the image does not move); the gain needed is
`r = min(1, threshold / peak)`. The gain curve: running minimum of `r` over `L + 1` samples
(hold), instant attack / exponential release (80 ms), then a moving average over `L` (the
look-ahead, 3 ms = 144 samples). With the audio delayed by `L − 1` plus the oversampler's lag,
the sample that needed `r`, and the next one, are multiplied by an average of values all ≤ `r`:
the output cannot exceed the threshold at the oversampler's points, by construction. The
threshold is the ceiling −2.0 dBTP minus a 0.2 dB guard (what a 4× estimate can miss between its
points: at most 0.12 dB at 20 kHz). Latency 155 samples (3.2 ms), compensated in the
pipeline's time stamps.

**[choice]** Look-ahead 3 ms (a gain reduction is a 3 ms ramp, not a click), release 80 ms (no
ducking after a peak; slower than a 40 Hz period). Below the threshold the limiter is an exact
delay (test `quiet_input_is_delayed_exactly`), and it returns to exactly unity after limiting
(the release runs in `f64`: in `f32` a first-order rise toward 1.0 stalls about 100 ppm short,
where its step is below half an ulp — found by the test, fixed). A fast path skips the
oversampler for 64-sample chunks whose samples are all below `threshold / Σ|h|` (a bound the
interpolator cannot exceed) while no gain reduction is pending: this took the limiter from 0.22 %
to 0.04 % of a core.

Measured: program noise driven 10 dB into the ceiling, a +12 dBFS 997 Hz tone and a +3 dBFS
19 kHz tone all come out at or below −2.0 dBTP by the meter (`loud_input_is_held_below_the_ceiling`);
the M9 runs and the ffmpeg comparison read −2.20 dBTP for the −10 LUFS input (−2.19 by
loudnorm). A transient's gain is a ramp over the look-ahead (a 0.31 reduction in equal steps of about
0.0022 per sample over 144 samples; the test bounds any step at 0.03) and recovers with the
80 ms release, then returns to exactly 1.

## 6. Sample clock

Frame `j` holds output samples `1536 j .. 1536 (j+1)` and has `pts = P0 + 864 000 j` ticks of
27 MHz (`2880 j` at 90 kHz, A/52 Table 5.18: 1536 samples per syncframe = 32 ms). A block stamped
`t` starts at sample `round((t − P0) · 2/1125)`; all integer, nothing accumulated, so frame
81 million (30 days) is as exact as frame 0 (test).

**[choice] Continuity first, timestamps as reference.** Decoded blocks carry jittery stamps
(RTMP: whole milliseconds, ±48 samples); placing each block at its own rounded stamp would cut a
gap or an overlap into every block boundary. Each source keeps the position its next sample is
*expected* at. A block within 20 ms (`tolerance`) of that is placed there, contiguously, and
`expected − stamp` is the source's **drift**. Beyond the tolerance a new segment starts at the
block's own stamp (forward: silence between; backward: the newer block replaces what was
buffered from its start on). Late samples (before the output position) and early ones (beyond
the 2.5 s buffer) are dropped; overlapping ones replace; all counted per source
(`SourceCounters`).

**Edges.** Each source has an envelope: 0 on empty samples, rising by 1/480 per sample (10 ms)
after a gap or at a segment start, and falling to reach 0 at the next gap or segment start,
seen 480 samples ahead in the buffer. If a gap fills in the meantime (a late block), the
envelope just rises again, continuously. So "silence when dry, with 10 ms fades at both edges"
needs no "dry" state. Older buffered audio right after a newly placed block is marked as a
segment start, so a restart landing inside buffered audio fades instead of jumping.

**Sources.** Buffers for `Live` and `Standby`; `Slate` is silence. `switch(to, at)` is a 20 ms
raised-cosine crossfade starting at `at` (the video cut's time, SPEC §9.5); gains sum to one at
every sample, so two signals under the true-peak ceiling stay under it. A second change while
one is pending follows it.

**Drift and trim.** `depth(source)` is how far the expected next sample is past the output
position (the jitter buffer as audio sees it; the mapper holds it near `J`). `drift(source)` is
the smoothed `expected − stamp`; it is what the resampler's trim must remove.
`TrimController::trim(skew, drift) = (1/(1 + skew) − 1) + drift / (τ · 48 kHz)`, τ 10 s,
clamped to ±500 ppm. **[choice]** The SPEC's correction is "on the buffer depth"; the depth is
already governed by the mapper (shared with video), and correcting audio by it would fight the
mapper, so the audio correction acts on the drift, which is zero exactly when the sample count
follows the stamps. **[choice]** The SPEC calls the correction "small": with the skew fed
forward it is (a few ppm). Its clamp is nevertheless the full 500 ppm, so a source whose skew is
unknown is still held: `trim_loop_follows_a_fast_producer` runs a producer 300 ppm fast for
60 s. Without the skew the drift rests at 144.9 samples (3 ms; the prediction is
300 ppm × 10 s × 48 kHz = 144), with no discontinuity and the trim at +300 ppm; with the skew
the drift is 1.1 samples.

**Property tests** (`tests/clock_properties.rs`, seeded SplitMix64 as in `tb-timeline`): random
block sizes 1–3000, ±48 samples of stamp jitter, gaps, backward restarts, stalls, source changes
including the slate, P0 next to the 33-bit PTS wrap. Every frame: index `j`, PTS exactly
`P0 + 864 000 j`, 90 kHz PTS exactly 2880 apart modulo 2^33, 1536 samples per channel, all
finite, bounded by the input's magnitude. With restarts kept out of the play head, no output
step exceeds a fade step plus a crossfade step (no clicks). A second set lets restarts and stamps
land anywhere (including the past and at the play head) and puts NaN and ±inf in the input: the
same invariants except the click bound. The tests assert that discontinuities, replacements,
late drops, switches and underruns all occurred. `contiguous_input_is_reproduced_exactly`: 20 s
of noise in random blocks with jittered stamps comes out sample for sample after the start fade.

## 7. Cost

Release build on bagel, one thread, 60 s of program noise through `TrackPipeline` in
1024-sample blocks with the frames pulled as the server would (`--example cost`):

| Track | Share of one core |
|---|---|
| 44.1 kHz stereo → stereo | 0.32 % |
| 48 kHz stereo → stereo (trim +37 ppm) | 0.26 % |
| 48 kHz 5.1 → stereo | 0.26 % |
| 48 kHz 5.1 → 5.1 | 0.52 % |
| 96 kHz stereo → stereo | 0.30 % |
| 32 kHz mono → stereo | 0.31 % |

Stages alone (stereo): resampler 44.1 → 48 0.135 %, 48 → 48 with trim 0.079 %, normalizer
0.031 %, limiter 0.043 %, mixer 5.1 → 2 0.007 %; the rest of a track (about 0.1 %) is the clock
and copying. A meter with true peak (not in the pipeline) is 0.20 %. So a stereo track is a
third of one percent of a core; fifty stereo tracks are a sixth of a core.

## Open items

- **Decoder layout.** `tb_media::AudioFrameDesc` has a channel count but no layout; for 3–5
  channels the count is ambiguous. The decoder worker should report libav's channel order and the
  adapter build an `InputLayout` from it.
- **EBU files.** Tech 3341 cases 7, 8 (authentic program), 10, 11, 13 and the remaining
  true-peak files were not run (no WAVs here). Tech 3342 (loudness range) is not implemented;
  nothing in the SPEC needs it.
- **After the encoder.** A/85's −2 dBTP is meant to leave room for coding overshoot; the true
  peak after AC-3 encode and decode is untested until `tb-ac3` exists (the gate could decode with
  ffmpeg and run `ebur128=peak=true`).
- **Within-session program changes** loud-to-quiet take about 10 s to come within 2 dB (§4);
  a session change restarts the measurement and is fast.
- **A/V sync over 24 h** (SPEC M9) depends on the mapper's skew and on the stamps the server
  passes; the audio side is tested to place a burst within 0.5 samples of its stamp and to keep
  the drift bounded with a 300 ppm producer, not over 24 h.
- **AAC priming.** Decoders drop the encoder delay of the first frame and libav adjusts the
  first PTS; if a producer's stamps do not account for it, audio would sit 21–46 ms off its
  video. That is the decoder adapter's and mapper's concern; it shows up as a constant offset,
  not as drift.
- **ARCHITECTURE §9** still says the sample clock lives in `tb-timeline` and `tb-audio` is a libav
  adapter; not edited here to avoid conflicting with the other agents' changes to the same table.
