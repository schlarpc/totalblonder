//! A carrier: its mux, the queue that feeds it, and the loop that keeps the pacer supplied
//! (SPEC §6.1). The mux core is pure (`tb-mux`); this is the thread around it.

use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::time::Duration;

use tb_mux::{
    AudioAu, AudioConfig, Mux, MuxConfig, Periods, ProgramConfig, SttConfig, VideoAu, VideoConfig,
    WallClock,
};
use tb_psip::build::TableSet;
use tb_psip::gps::{DstRule, LeapSeconds};
use tb_ts::{Packet, Stc};

use crate::config::{CarrierPlan, OutputPlan};
use crate::metrics::{Counter, Gauge};
use crate::time::{Clock, HZ};

/// What reaches a carrier's mux.
pub enum CarrierMsg {
    /// A video access unit of program `.0`.
    Video(usize, VideoAu),
    /// An audio frame of program `.0`, track `.1`.
    Audio(usize, usize, AudioAu),
    /// A new table set.
    Tables(TableSet),
    /// Program `.0`'s video delivery rate (transport bit/s), from the statmux.
    VideoRate(usize, u64),
}

/// The mux configuration of a carrier.
pub fn mux_config(
    c: &CarrierPlan,
    o: &OutputPlan,
    stc0: u64,
    leap: LeapSeconds,
    dst: DstRule,
    statmux: bool,
) -> MuxConfig {
    let ms = HZ / 1000;
    MuxConfig {
        rate: o.rate,
        stc0: Stc(stc0),
        programs: c
            .programs
            .iter()
            .map(|p| ProgramConfig {
                number: p.psip.number.get(),
                video: VideoConfig {
                    pid: p.psip.video.pid.pid(),
                    rx_bps: p.format.rx_bps(),
                    // With the statmux every program starts at its floor; the controller raises
                    // them from the pool at start.
                    rate_bps: if statmux {
                        p.ts_bps_for(p.min_bps, o.pcr_interval_ms)
                    } else {
                        p.video_ts_bps(o.pcr_interval_ms)
                    },
                    // D = vbv / max (SPEC §8.2), in 27 MHz ticks.
                    buffer_delay: p.vbv_bits * HZ / p.max_bps,
                    es_priority_on_key: false,
                },
                audio: p
                    .psip
                    .audio
                    .iter()
                    .map(|a| AudioConfig::ac3(a.pid.pid()))
                    .collect(),
                pcr_interval: o.pcr_interval_ms * ms,
            })
            .collect(),
        periods: Periods::default(),
        stt: Some(SttConfig { leap, dst }),
        queue_limit: 256,
        token_depth: 4,
    }
}

/// A carrier's mux and its input queue.
pub struct CarrierState {
    /// The carrier's id.
    pub id: u8,
    /// The mux.
    pub mux: Mux,
    /// The carrier's STC offset from process time.
    pub stc0: u64,
    rx: Receiver<CarrierMsg>,
    /// Access units the mux refused.
    pub refused: Counter,
    last_wall: Option<u64>,
}

impl CarrierState {
    /// A carrier with its queue; the sender goes to the producers.
    pub fn new(id: u8, mux: Mux, stc0: u64) -> (Self, SyncSender<CarrierMsg>) {
        let (tx, rx) = sync_channel(4096);
        (
            CarrierState {
                id,
                mux,
                stc0,
                rx,
                refused: Counter::default(),
                last_wall: None,
            },
            tx,
        )
    }

    /// Moves everything queued into the mux.
    pub fn drain(&mut self) {
        while let Ok(m) = self.rx.try_recv() {
            let r = match m {
                CarrierMsg::Video(i, au) => self.mux.push_video(i, au),
                CarrierMsg::Audio(i, t, au) => self.mux.push_audio(i, t, au),
                CarrierMsg::Tables(t) => {
                    self.mux.set_tables(&t);
                    Ok(())
                }
                CarrierMsg::VideoRate(i, bps) => {
                    self.mux.set_video_rate(i, bps);
                    Ok(())
                }
            };
            if let Err(e) = r {
                self.refused.inc();
                tracing::debug!(carrier = self.id, "mux refused: {e}");
            }
        }
    }

    /// Refreshes the STC-to-UTC relation once a second of process time.
    pub fn wall(&mut self, t: u64, unix_nanos: i128) {
        if self.last_wall.is_none_or(|l| t >= l + HZ) {
            self.mux.set_wall_clock(WallClock {
                stc: self.stc0 + t,
                unix_nanos,
            });
            self.last_wall = Some(t);
        }
    }

    /// Produces every slot that starts before process time `t` (offline mode).
    pub fn produce_until(&mut self, t: u64, out: &mut dyn FnMut(&Packet)) {
        loop {
            let at = self.mux.clock().stc(self.mux.slot()).0 - self.stc0;
            if at >= t {
                break;
            }
            let (_, p) = self.mux.next_slot();
            out(&p);
        }
    }
}

/// Gauges a carrier publishes for the control API (SPEC §11.3), refreshed by its thread.
pub struct CarrierGauges {
    /// Slots produced.
    pub slots: Gauge,
    /// Null packets.
    pub nulls: Gauge,
    /// Slots skipped because the pacer went ahead.
    pub skipped: Gauge,
    /// Table packets.
    pub tables: Gauge,
    /// Table instances that overran their period.
    pub table_overruns: Gauge,
    /// Datagrams the pacer sent as nulls because the mux was late.
    pub underruns: Gauge,
    /// Send errors.
    pub send_errors: Gauge,
    /// Worst pacer lateness, µs.
    pub late_max_us: Gauge,
    /// Per program: video AUs, video dropped, audio AUs, audio dropped, max PCR gap µs.
    pub programs: Vec<[Gauge; 5]>,
    /// The pacer's counters.
    pub tx: Option<std::sync::Arc<tb_output::pacer::TxStats>>,
    /// Self-check findings by severity S1–S4 (SPEC §11.2).
    pub findings: [Gauge; 4],
}

impl CarrierGauges {
    /// Registers a carrier's gauges.
    pub fn register(reg: &mut crate::control::Registry, id: u8, programs: usize) -> Self {
        let l = || vec![("carrier", id.to_string())];
        let pl = |p: usize| {
            vec![
                ("carrier", id.to_string()),
                ("program", (p + 1).to_string()),
            ]
        };
        CarrierGauges {
            slots: reg.gauge("mux_slots_total", l()),
            nulls: reg.gauge("mux_null_packets_total", l()),
            skipped: reg.gauge("mux_underrun_slots_total", l()),
            tables: reg.gauge("mux_table_packets_total", l()),
            table_overruns: reg.gauge("mux_table_overruns_total", l()),
            underruns: reg.gauge("null_datagrams_total", l()),
            send_errors: reg.gauge("send_errors_total", l()),
            late_max_us: reg.gauge("send_lateness_max_microseconds", l()),
            programs: (0..programs)
                .map(|p| {
                    [
                        reg.gauge("video_access_units_total", pl(p)),
                        reg.gauge("late_au_total", pl(p)),
                        reg.gauge("audio_frames_total", pl(p)),
                        reg.gauge("late_audio_frames_total", pl(p)),
                        reg.gauge("pcr_interval_max_microseconds", pl(p)),
                    ]
                })
                .collect(),
            tx: None,
            findings: ["S1", "S2", "S3", "S4"].map(|sev| {
                reg.gauge(
                    "selfcheck_findings_total",
                    vec![("carrier", id.to_string()), ("severity", sev.into())],
                )
            }),
        }
    }

    /// Copies the current values in.
    pub fn update(&self, c: &CarrierState) {
        use std::sync::atomic::Ordering::Relaxed;
        let s = c.mux.stats();
        self.slots.set(s.slots);
        self.nulls.set(s.null_packets);
        self.skipped.set(s.skipped);
        self.tables.set(s.table_packets);
        self.table_overruns.set(c.mux.table_overruns());
        if let Some(tx) = &self.tx {
            self.underruns.set(tx.underruns.load(Relaxed));
            self.send_errors.set(tx.send_errors.load(Relaxed));
            self.late_max_us.set(tx.late_max_ns.load(Relaxed) / 1000);
        }
        for (i, g) in self.programs.iter().enumerate() {
            if let Some(p) = c.mux.program_stats(i) {
                g[0].set(p.video_aus);
                g[1].set(p.video_dropped);
                g[2].set(p.audio_aus);
                g[3].set(p.audio_dropped);
                g[4].set(p.max_pcr_gap / 27);
            }
        }
    }
}

/// The independent checker on a carrier's own output (SPEC §11.2): a different implementation
/// from the mux (`tb-ts-verify`), so one misreading cannot hide in both.
pub fn self_checker(rate: u64, tsid: u16) -> tb_ts_verify::Checker {
    tb_ts_verify::Checker::new(tb_ts_verify::Options {
        rate: Some(rate as f64),
        tsid: Some(tsid),
        events_capacity: 256,
        ..tb_ts_verify::Options::default()
    })
}

/// The real-time loop of a carrier: keep the pacer's queue filled until the clock stops,
/// checking every packet on the way when `checker` is given.
pub fn run_realtime(
    mut c: CarrierState,
    mut feeder: tb_output::pacer::Feeder,
    clock: &dyn Clock,
    gauges: CarrierGauges,
    mut checker: Option<tb_ts_verify::Checker>,
    producers_stopped: &std::sync::atomic::AtomicBool,
    drained: &std::sync::atomic::AtomicBool,
) -> CarrierState {
    let mut last = 0u64;
    let mut logged_in_window = 0u32;
    let mut window = 0u64;
    while !clock.stopping() {
        c.drain();
        let t = clock.now();
        c.wall(t, clock.unix_nanos_at(t));
        match checker.as_mut() {
            Some(k) => feeder.pump_tap(&mut c.mux, &mut |p| k.push(p)),
            None => feeder.pump(&mut c.mux),
        };
        if t >= last + HZ / 2 {
            gauges.update(&c);
            if let Some(k) = checker.as_mut() {
                let mut by = [0u64; 4];
                for (rule, n) in k.counts() {
                    by[rule.info().severity as usize] += n;
                }
                for (g, n) in gauges.findings.iter().zip(by) {
                    g.set(n);
                }
                // At most 20 lines a minute, so a broken stream cannot flood the log.
                if t >= window + 60 * HZ {
                    window = t;
                    logged_in_window = 0;
                }
                for f in k.drain_events() {
                    if logged_in_window < 20 {
                        tracing::warn!(carrier = c.id, "self-check: {}", f.line());
                        logged_in_window += 1;
                    }
                }
            }
            last = t;
        }
        // At a stop: once the producers have finished and everything queued has been sent,
        // say so (the output then stops; the end of the stream is whole).
        if producers_stopped.load(std::sync::atomic::Ordering::Relaxed) && c.mux.idle() {
            drained.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    c
}

/// Logs a carrier's counters.
pub fn log_stats(c: &CarrierState, programs: usize) {
    let s = c.mux.stats();
    tracing::info!(
        carrier = c.id,
        slots = s.slots,
        skipped = s.skipped,
        nulls = s.null_packets,
        tables = s.table_packets,
        table_overruns = c.mux.table_overruns(),
        refused = c.refused.get(),
        "carrier"
    );
    for i in 0..programs {
        if let Some(p) = c.mux.program_stats(i) {
            tracing::info!(
                carrier = c.id,
                program = i + 1,
                video_aus = p.video_aus,
                video_dropped = p.video_dropped,
                audio_aus = p.audio_aus,
                audio_dropped = p.audio_dropped,
                pcr_only = p.pcr_only_packets,
                max_pcr_gap_ms = p.max_pcr_gap as f64 / 27_000.0,
                min_dts_margin_ms = p.min_dts_margin.map(|m| m as f64 / 27_000.0),
                "program"
            );
        }
    }
}

/// An audio frame for the queue (helper for producers).
pub fn audio_msg(program: usize, track: usize, au: AudioAu) -> CarrierMsg {
    CarrierMsg::Audio(program, track, au)
}
