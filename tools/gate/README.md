# tools/gate/

The tools that check the encoder and the pipeline on their own merits, and measure them. The
encoder under test is `tb-m2venc` (`M2VENC`), the pipeline `tb-pipe` (`M2VPIPE`); the flake's
apps set both, the conformance verifier (`M2V_VERIFY`) and the corpus (`M2V_MEDIA`). Run them
from the repository root, on the GPU host inside the memory cap (`tools/gate/capped.sh`). Where things go: `tools/gate/env.sh` (shell) and `tools/gate/formats.py` (Python);
scratch and results under `work/gate/` unless `M2V_WORK` / `M2V_OUT` / `M2V_RESULTS` say
otherwise.

| Tool | App | What |
|---|---|---|
| `check.py` | `gate-check` | The correctness gate on the corpus: strict ffmpeg and libmpeg2 decode, structure, both buffer models (`vbv_check.py`), reconstruction against decode, rate, quality against `baselines/` (54 cases; `--quick` 15) |
| `pipe_check.py` | `gate-pipe` | The pipeline's gate: device input, shedding and holds, sessions at run time, conversions against CPU references and analytic patterns (46 cases) |
| `film_gate.py` | `gate-film` | The film-mode gate on the card: `tb-m2venc --film` on film frames at broadcast sizes (3:2 on 1080i and 480i, frame repeats on 720p59.94, mixed cadences, captions, holds, shedding by chance and by a real deadline, forced key frames, rate changes) and `tb-pipe film=auto` (23.976, 24, 25, 29.97 Hz and interlaced sources, a stalling source, captions): cadence on both decoders and the displayed fields (`film_check.py`), both buffer models, reconstruction and converted frames against decode, the verifier (49 cases). `--measure`: rate and quality of film mode against the coded pulldown |
| `gate_small.py` | `gate-small`, check `gate/ref-small` | The golden cases (`crates/tb-golden/vectors/`) through the binary, each stream on its own merits, and their digests |
| `pipe_small.py` | check `gate/pipe-ref-small` | The pipeline at small size on the reference adapter, film mode included (23.976, 25 and 29.97 Hz and interlaced sources into 480i, 23.976 Hz into 720p59.94) |
| `film_check.py` | (the gates' film cases) | The cadence a stream with repeat_first_field asks for, on the decoders: the flags (an independent reader, `crates/tb-m2v-verify/tools/esflags.py`), ffprobe's and libmpeg2's reports of fields per picture, and the displayed sequence (ffmpeg `repeatfields`, or its constant-rate output for a progressive sequence) sample for sample against the fields of the decoded pictures |
| `conform.py` | (all of the above) | The conformance verifier on every stream a case writes; S3 findings allowed only by `crates/tb-m2v-verify/tools/gate-allow.txt` |
| `formats.py`, `vbv_check.py` | | The formats and command lines; the two buffer-model simulations (Annex C as coded, and the operating model with the encoder's rate schedule), both with the removal times of H.262 Annex C.9 / C.11 read from each picture's repeat_first_field and top_field_first |
| `bench_capacity.py`, `bench.py`, `gpumon.py` | `gate-bench` | Capacity: free-running macroblocks per second and paced on-time fractions, interleaved builds, runs disturbed by other users of the card repeated (`bench.py`: line-up definitions, the card sampler, the spike's older benchmark run) |
| `pipe_lineup.py` | `gate-lineup` | Paced line-ups of the pipeline (decode, convert, encode) |
| `contend.sh`, `contend_report.py`, `contend_late.py`, `contend_shed.py`, `contend_table.py`, `gpucomp.cu`, `fmt.sh` | `gate-contend` | The encoder next to other users of the card (decoders, transcodes, `gpucomp`: a trivial-kernel context built by Nix) |
| `prep_media.sh`, `derive_media.py`, `prep_inputs.sh`, `prep_film.sh` | `gate-prep` | The corpus: raw clips from the Xiph collection, derived clips, compressed pipeline inputs; `prep_film.sh` (`gate-prep film`): the film material of the film gate, derived from the corpus into `$M2V_FILM` (film frames at 704x480 and 1280x720, 23.976 / 24 / 25 / 29.97 Hz inputs, a 23.976 Hz input with captions) |
| `qdiff.py` | | Quality of a `check.py` run against the baseline |
| `nsys_profile.sh`, `nsys_pipe.sh`, `nsys_tick.py`, `contend_nsys.sh`, `ncu_profile.sh`, `ncu_summary.py` | | Nsight Systems and Compute around any binary (`nix build .#nsys`, `.#ncu`; `ncu` needs root for the counters) |
| `baselines/` | | `baseline.json` (stored quality per case of `check.py`; `--update-baseline`), `ref.json` (cached ffmpeg reference encodes) |

