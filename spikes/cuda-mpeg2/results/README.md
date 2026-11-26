# Results of the CUDA MPEG-2 spike: frozen history

The measurements of the C++ spike and of the Rust port while it was compared with the spike
(benchmarks, capacity searches, contention runs, Nsight profiles, gate tables, decoder rates).
`notes/cuda-mpeg2-spike.md`, `notes/cuda-mpeg2-optlog.md`, `docs/ARCHITECTURE.md` and the port's
notes (`docs/port/`) cite them. Nothing writes here any more: the C++ spike was deleted in
package X1 (`docs/port/X1-notes.md`), and the gate's tools (`gate/`) write their results under
`work/gate/` (or `M2V_RESULTS`).

The programs and scripts that made these files are in the git history: the tree of commit
`e79069b` (`git show e79069b:spikes/cuda-mpeg2/Makefile`, `.../scripts/`, `.../src/`). The
scripts still in use moved to `gate/`; the quality baseline and the cached reference encodes
that `check/` held are `gate/baselines/` now.
