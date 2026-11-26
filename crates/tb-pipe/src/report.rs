//! What a run reports: the lines and the JSON record of the C++ harness,
//! with the same keys, which the gate and the line-up scripts read.

use std::collections::BTreeMap;
use std::io::Write;

use crate::run::PERIOD;

/// The figures of one run.
#[derive(Clone, Debug, Default)]
#[allow(
    missing_docs,
    reason = "the fields are the report's own vocabulary; see `print`"
)]
pub struct Report {
    pub channels: usize,
    pub inputs: usize,
    pub shared_ctx: bool,
    pub paced: bool,
    pub offline: bool,
    pub ticks: u64,
    pub counted: u64,
    pub late: u64,
    pub sum_tick: f64,
    pub worst: f64,
    /// Tick times in seconds, ascending (after the warm-up).
    pub tick_times: Vec<f64>,
    pub wall: f64,
    pub cpu: f64,
    pub main_cpu: f64,
    pub dec_cpu: f64,
    pub frames: i64,
    pub fresh: i64,
    pub psf: i64,
    pub weaves: i64,
    pub again: i64,
    pub slates: i64,
    pub drops: i64,
    pub pictures: u64,
    pub repeats: u64,
    pub shed: u64,
    pub violations: u64,
    pub mbs: f64,
    pub dec_frames: u64,
    pub dec_drop: u64,
    pub dec_err: u64,
    pub dec_resync: u64,
    pub dec_skip: u64,
    pub dec_degr: u64,
    pub in_skipped: u64,
    pub all_on_device: bool,
    /// Lateness of decoded frames per input file name, seconds.
    pub lateness: BTreeMap<String, Vec<f32>>,
    pub lead: f64,
    pub conv_classes: usize,
    /// Classes whose tap tables were made before the first tick.
    pub classes_before_start: usize,
    /// Classes made inside a tick (first used there).
    pub classes_in_ticks: usize,
    /// Classes that came with an input opened while running, made on its side thread.
    pub classes_adopted: usize,
    /// Caption entries lost in the decoders' queues.
    pub cc_lost: u64,
    /// Milliseconds from the last tick of the run to the end of the flush and the settle.
    pub flush_ms: f64,
    pub conv_launches: u64,
    pub conv_host_ms: f64,
    pub enc_mb: f64,
    pub conv_mb: f64,
    pub dec_mb: f64,
    pub ctx_mb: f64,
    pub up_bytes: f64,
    pub down_bytes: f64,
    pub device_errors: u64,
    /// Inputs the decoder budget sent to software decoding, and inputs it refused.
    pub routed_sw: u64,
    pub refused: u64,
    /// Caption triplets taken from the inputs and put into coded pictures.
    pub cc_in: u64,
    pub cc_out: u64,
    /// Channels in film mode, and their counters summed (`tb_timeline::film::FilmStats`):
    /// pictures by display duration, cadence breaks by what was seen at them.
    pub film_channels: u64,
    pub film: tb_timeline::film::FilmStats,
    /// Inputs opened while running: index, at seconds, decoder ready after ms, longest tick
    /// in the 1.5 s after (ms).
    pub late_inputs: Vec<(usize, f64, f64, f64)>,
}

impl Report {
    fn pct(&self, p: f64) -> f64 {
        let n = self.tick_times.len();
        if n == 0 {
            return 0.0;
        }
        self.tick_times[((n as f64 * p) as usize).min(n - 1)]
    }

    /// Share of ticks whose output existed before the next tick was due.
    pub fn ontime(&self) -> f64 {
        if self.counted == 0 {
            0.0
        } else {
            1.0 - self.late as f64 / self.counted as f64
        }
    }

    /// Decoded frames that came later than the lead: (late, all, the worst in seconds).
    pub fn dec_late(&self) -> (u64, u64, f64) {
        let (mut late, mut n, mut max) = (0u64, 0u64, 0.0f64);
        for &l in self.lateness.values().flatten() {
            n += 1;
            late += u64::from(f64::from(l) > self.lead);
            max = max.max(f64::from(l));
        }
        (late, n, max)
    }

    /// The report's lines.
    pub fn print(&self, w: &mut dyn Write) -> std::io::Result<()> {
        for (name, v) in &self.lateness {
            if v.is_empty() {
                continue;
            }
            let mut v = v.clone();
            v.sort_by(f32::total_cmp);
            let nl = v.iter().filter(|l| f64::from(**l) > self.lead).count();
            writeln!(
                w,
                "  input {name:<26} frames {:7}  after its nominal time: median {:6.1} ms, p99 {:6.1} ms, max {:6.1} ms; late for the frame clock (> {:.0} ms) {:.2}%",
                v.len(),
                1e3 * v[v.len() / 2],
                1e3 * v[(v.len() as f64 * 0.99) as usize],
                1e3 * v[v.len() - 1],
                1e3 * self.lead,
                100.0 * nl as f64 / v.len() as f64
            )?;
        }
        let (dec_late, dec_n, late_max) = self.dec_late();
        let wall = self.wall.max(1e-9);
        writeln!(
            w,
            "channels {}, inputs {} ({}), {}, {} ticks of {:.3} ms",
            self.channels,
            self.inputs,
            if self.shared_ctx {
                "decoders in the encoder's context"
            } else {
                "decoders in a context of their own"
            },
            if self.offline {
                "offline"
            } else if self.paced {
                "paced"
            } else {
                "free-run"
            },
            self.ticks,
            1e3 * PERIOD
        )?;
        writeln!(
            w,
            "ticks on time {:.2}% ({} late of {}); tick ms: mean {:.3} p50 {:.3} p99 {:.3} p99.9 {:.3} max {:.3}",
            100.0 * self.ontime(),
            self.late,
            self.counted,
            1e3 * self.sum_tick / self.counted.max(1) as f64,
            1e3 * self.pct(0.5),
            1e3 * self.pct(0.99),
            1e3 * self.pct(0.999),
            1e3 * self.worst
        )?;
        writeln!(
            w,
            "output frames {}: from new input frames {} (progressive in an interlaced frame {}, two instants woven {}), repeated {}, slate {}; input frames dropped by the frame clock {}",
            self.frames, self.fresh, self.psf, self.weaves, self.again, self.slates, self.drops
        )?;
        writeln!(
            w,
            "pictures coded {}, repeats made on the host {} (deadline policy {}); buffer violations {}; {:.0} macroblocks/s",
            self.pictures,
            self.repeats,
            self.shed,
            self.violations,
            self.mbs / wall
        )?;
        writeln!(
            w,
            "inputs: decoded {} frames ({:.1} fps), {}; late for the frame clock {} of {} ({:.2}%), worst {:.0} ms after its time; dropped in a full queue {}; decode errors {}; resynchronized {} times ({} packets skipped); decoding of non-reference pictures suspended {} times",
            self.dec_frames,
            self.dec_frames as f64 / wall,
            if self.all_on_device {
                "all on the device"
            } else {
                "NOT all on the device"
            },
            dec_late,
            dec_n,
            100.0 * dec_late as f64 / dec_n.max(1) as f64,
            1e3 * late_max,
            self.dec_drop,
            self.dec_err,
            self.dec_resync,
            self.dec_skip,
            self.dec_degr
        )?;
        writeln!(
            w,
            "host cpu {:.3} cores total = {:.4} per channel: tick thread {:.3}, decode threads {:.3} ({:.4} per input), other {:.3}",
            self.cpu / wall,
            self.cpu / wall / self.channels.max(1) as f64,
            self.main_cpu / wall,
            self.dec_cpu / wall,
            self.dec_cpu / wall / self.inputs.max(1) as f64,
            (self.cpu - self.main_cpu - self.dec_cpu) / wall
        )?;
        writeln!(
            w,
            "conversion: {} classes ({} made before the first tick, {} on the side threads of late inputs, {} inside a tick), {:.1} kernel launches per tick, {:.3} ms of host time per tick; device memory: encoder {:.0} MB, converter {:.1} MB, decoders and frames {:.0} MB, context total {:.0} MB",
            self.conv_classes,
            self.classes_before_start,
            self.classes_adopted,
            self.classes_in_ticks,
            self.conv_launches as f64 / self.ticks.max(1) as f64,
            self.conv_host_ms / self.ticks.max(1) as f64,
            self.enc_mb,
            self.conv_mb,
            self.dec_mb,
            self.ctx_mb
        )?;
        writeln!(
            w,
            "bus: frames uploaded from host memory {:.1} MB/s, coded pictures downloaded {:.2} MB/s",
            self.up_bytes / wall / 1e6,
            self.down_bytes / wall / 1e6
        )?;
        if self.routed_sw + self.refused > 0 {
            writeln!(
                w,
                "admission: {} inputs decoded in software, {} refused (the decoder budget)",
                self.routed_sw, self.refused
            )?;
        }
        if self.cc_in > 0 {
            writeln!(
                w,
                "captions: {} triplets from the inputs, {} in coded pictures, {} frames' captions lost in a full decoder queue",
                self.cc_in, self.cc_out, self.cc_lost
            )?;
        }
        if self.film_channels > 0 {
            let f = &self.film;
            writeln!(
                w,
                "film mode on {} channels: pictures shown for 1 / 2 / 3 display units {} / {} / {} ({} of one instant, {} the picture before again), {} display units in, {} out; ticks with nothing due {}; cadence breaks: {}",
                self.film_channels,
                f.pictures[1],
                f.pictures[2],
                f.pictures[3],
                f.progressive,
                f.again,
                f.units_in,
                f.units_out,
                f.idle,
                tb_timeline::film::Break::ALL
                    .iter()
                    .map(|b| format!("{} {}", b.name(), f.breaks[*b as usize]))
                    .collect::<Vec<_>>()
                    .join(", "),
            )?;
        }
        writeln!(
            w,
            "after the run: {:.1} ms to flush the sessions and settle",
            self.flush_ms
        )?;
        if self.device_errors > 0 {
            writeln!(w, "device errors: {}", self.device_errors)?;
        }
        for (i, at, ms, max) in &self.late_inputs {
            writeln!(
                w,
                "input {i} opened at {at:.1} s while running: decoder ready after {ms:.1} ms (side thread); longest tick in the 1.5 s after: {max:.2} ms (p99 of the run {:.2} ms)",
                1e3 * self.pct(0.99)
            )?;
        }
        Ok(())
    }

    /// The JSON record (the keys of the C++ harness, then the port's own).
    pub fn json(&self) -> String {
        let (dec_late, dec_n, late_max) = self.dec_late();
        let wall = self.wall.max(1e-9);
        let ticks = self.ticks.max(1) as f64;
        format!(
            "{{\"channels\":{},\"inputs\":{},\"shared_ctx\":{},\"paced\":{},\"ticks\":{},\"ontime_frac\":{:.6},\"late\":{},\"tick_mean_ms\":{:.4},\"tick_p50_ms\":{:.4},\"tick_p99_ms\":{:.4},\"tick_max_ms\":{:.4},\
             \"frames\":{},\"fresh\":{},\"psf\":{},\"weaves\":{},\"again\":{},\"slates\":{},\"drops\":{},\"pictures\":{},\"repeats\":{},\"shed\":{},\"violations\":{},\"mb_per_s\":{:.0},\
             \"dec_frames\":{},\"dec_fps\":{:.2},\"all_on_device\":{},\"dec_late\":{},\"dec_n\":{},\"dec_late_max_ms\":{:.1},\"dec_qdrop\":{},\"dec_errors\":{},\
             \"cpu_cores\":{:.4},\"cpu_tick\":{:.4},\"cpu_decode\":{:.4},\"conv_classes\":{},\"conv_launches_per_tick\":{:.2},\"conv_host_ms\":{:.4},\
             \"enc_mb\":{:.1},\"conv_mb\":{:.1},\"ctx_mb\":{:.1},\"up_mb_s\":{:.2},\"down_mb_s\":{:.3},\
             \"dec_resync\":{},\"dec_degrade\":{},\"classes_before_start\":{},\"classes_in_ticks\":{},\"device_errors\":{},\"routed_sw\":{},\"refused\":{},\"cc_in\":{},\"cc_out\":{},\
             \"film_channels\":{},\"film_pictures\":[{},{},{}],\"film_units_in\":{},\"film_units_out\":{},\"film_again\":{},\"film_idle\":{},\"film_breaks\":{{{}}}}}\n",
            self.channels,
            self.inputs,
            u8::from(self.shared_ctx),
            u8::from(self.paced),
            self.ticks,
            self.ontime(),
            self.late,
            1e3 * self.sum_tick / self.counted.max(1) as f64,
            1e3 * self.pct(0.5),
            1e3 * self.pct(0.99),
            1e3 * self.worst,
            self.frames,
            self.fresh,
            self.psf,
            self.weaves,
            self.again,
            self.slates,
            self.drops,
            self.pictures,
            self.repeats,
            self.shed,
            self.violations,
            self.mbs / wall,
            self.dec_frames,
            self.dec_frames as f64 / wall,
            u8::from(self.all_on_device),
            dec_late,
            dec_n,
            1e3 * late_max,
            self.dec_drop,
            self.dec_err,
            self.cpu / wall,
            self.main_cpu / wall,
            self.dec_cpu / wall,
            self.conv_classes,
            self.conv_launches as f64 / ticks,
            self.conv_host_ms / ticks,
            self.enc_mb,
            self.conv_mb,
            self.ctx_mb,
            self.up_bytes / wall / 1e6,
            self.down_bytes / wall / 1e6,
            self.dec_resync,
            self.dec_degr,
            self.classes_before_start,
            self.classes_in_ticks,
            self.device_errors,
            self.routed_sw,
            self.refused,
            self.cc_in,
            self.cc_out,
            self.film_channels,
            self.film.pictures[1],
            self.film.pictures[2],
            self.film.pictures[3],
            self.film.units_in,
            self.film.units_out,
            self.film.again,
            self.film.idle,
            tb_timeline::film::Break::ALL
                .iter()
                .map(|b| format!("\"{}\":{}", b.name(), self.film.breaks[*b as usize]))
                .collect::<Vec<_>>()
                .join(","),
        )
    }
}
