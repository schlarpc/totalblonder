//! Assembly: threads, queues and clocks from a [`Plant`] (SPEC §3 runtime model).
//!
//! ```text
//!  tables thread ──┐
//!  video hub ──────┼──► carrier queue ──► mux thread ──► Feeder ══► pacer thread ──► socket
//!  audio thread ───┘        (one per carrier)                    (one per interface)
//! ```
//!
//! Offline mode replaces the pacers and mux threads by one driver that advances a
//! [`VirtualClock`] and writes every carrier to a file: the same workers, the same mux, a
//! deterministic stream produced as fast as the encoder allows.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use tb_device_gpu::{Cuda, CudaDevice, Hip, HipDevice, Runtime};
use tb_engine::{Engine, Guarded, TickDevice};
use tb_mux::Mux;
use tb_output::frame::Framing;
use tb_output::pacer::{self, PacerTiming};
use tb_output::sink::{FileSink, Sink, UdpSink};
use tb_ts::PCR_MODULUS;

use crate::carrier::CarrierGauges;
use crate::carrier::{self, CarrierMsg, CarrierState};
use crate::config::{Destination, Plant, VideoDevice};
use crate::control::{Registry, Value};
use crate::metrics::Counter;
use crate::slate::{Slate, SlateProvider};
use crate::tables::{self, TableTarget};
use crate::time::{Clock, HZ, RealClock, VirtualClock};
use crate::video::{self, HubSession, HubTiming};

/// How the server runs.
#[derive(Debug, Clone)]
pub enum Mode {
    /// On the wall clock, to the configured destinations, until stopped or for a duration.
    Realtime {
        /// Stop after this long.
        duration: Option<Duration>,
    },
    /// As fast as possible on a virtual clock, every carrier into a file in `dir`.
    Offline {
        /// Stream time to produce.
        duration: Duration,
        /// Where the files go (`carrier-<id>.ts`).
        dir: PathBuf,
    },
}

struct Built {
    carriers: Vec<CarrierState>,
    senders: Vec<std::sync::mpsc::SyncSender<CarrierMsg>>,
    /// Per carrier, its auto tracks' rates (empty without the statmux: nothing flexes).
    flex: Vec<std::sync::Arc<crate::audio::CarrierFlex>>,
}

/// The auto tracks of each carrier, starting at their stereo rate (the slate is stereo).
fn carrier_flex(plant: &Plant) -> Vec<std::sync::Arc<crate::audio::CarrierFlex>> {
    plant
        .carriers
        .iter()
        .map(|c| {
            let tracks = c
                .programs
                .iter()
                .flat_map(|p| p.audio.iter().enumerate().map(move |(k, a)| (p, k, a)))
                .filter(|(_, _, a)| a.auto && plant.statmux.is_some())
                .map(|(p, k, a)| {
                    crate::audio::FlexTrack::new(
                        p.index,
                        k,
                        crate::config::ac3_ts_bps(a.kbps_for(6)),
                        crate::config::ac3_ts_bps(a.kbps_for(2)),
                    )
                })
                .collect();
            std::sync::Arc::new(crate::audio::CarrierFlex { tracks })
        })
        .collect()
}

/// A track's share of its carrier's flexible budget, if it has one.
fn flex_of(
    built: &Built,
    carrier: usize,
    program: usize,
    track: usize,
) -> Option<(std::sync::Arc<crate::audio::CarrierFlex>, usize)> {
    let f = built.flex.get(carrier)?;
    let k = f
        .tracks
        .iter()
        .position(|t| t.program == program && t.track == track)?;
    Some((f.clone(), k))
}

fn stc0_for(id: u8, unix_nanos: i128) -> u64 {
    // An arbitrary start (SPEC §5.1); taken from the wall clock so restarts do not repeat it.
    let t = (unix_nanos.max(0) as u128 * 27 / 1000) as u64;
    (t + u64::from(id) * 1_000_003 * 300) % PCR_MODULUS
}

fn build_carriers(plant: &Plant, unix0: i128) -> anyhow::Result<Built> {
    let mut carriers = Vec::new();
    let mut senders = Vec::new();
    for c in &plant.carriers {
        let stc0 = stc0_for(c.id, unix0);
        let cfg = carrier::mux_config(
            c,
            &plant.output,
            stc0,
            plant.leap,
            plant.dst,
            plant.statmux.is_some(),
        );
        let mux = Mux::new(cfg).with_context(|| format!("carrier {}", c.id))?;
        let (state, tx) = CarrierState::new(c.id, mux, stc0);
        carriers.push(state);
        senders.push(tx);
    }
    Ok(Built {
        carriers,
        senders,
        flex: carrier_flex(plant),
    })
}

/// A program's live wiring: what its input's decoder thread sends it.
struct LiveProg {
    /// Live (index 0) and standby (index 1).
    video: [Option<std::sync::mpsc::Receiver<crate::source::LiveEvent>>; 2],
    /// Per role, one queue per audio track.
    audio: [Vec<Option<std::sync::mpsc::Receiver<crate::audio::LiveAudio>>>; 2],
    stats: crate::source::SourceStats,
}

type LiveMap = std::collections::HashMap<(usize, usize), LiveProg>;

/// The decoder worker binary: `TB_AVWORKER`, else `tb-avworker` beside this program.
fn worker_path() -> anyhow::Result<PathBuf> {
    if let Some(p) = std::env::var_os("TB_AVWORKER") {
        return Ok(PathBuf::from(p));
    }
    let me = std::env::current_exe()?;
    let p = me.with_file_name("tb-avworker");
    if p.exists() {
        Ok(p)
    } else {
        bail!("no decoder worker: set TB_AVWORKER to the tb-avworker binary")
    }
}

/// Starts every live input that a program uses: its socket thread and its decoder thread,
/// and the queues to the program (SPEC §9; `live.rs`).
fn start_live(
    plant: &Plant,
    p0: u64,
    delay: u64,
    epoch: Instant,
    stop: &Arc<AtomicBool>,
    reg: &mut Registry,
) -> anyhow::Result<(LiveMap, Vec<std::thread::JoinHandle<()>>)> {
    let mut map = LiveMap::new();
    let mut threads = Vec::new();
    // (carrier, program, role 0 live / 1 standby, input id)
    let users: Vec<(usize, &crate::config::ProgramPlan, usize, &str)> = plant
        .carriers
        .iter()
        .enumerate()
        .flat_map(|(ci, c)| c.programs.iter().map(move |p| (ci, p)))
        .flat_map(|(ci, p)| {
            [p.live.as_deref(), p.standby.as_deref()]
                .into_iter()
                .enumerate()
                .filter_map(move |(role, id)| id.map(|id| (ci, p, role, id)))
        })
        .collect();
    if users.is_empty() {
        return Ok((map, threads));
    }
    let pool = Arc::new(
        tb_decoder_client::DecoderPool::new(tb_decoder_client::PoolConfig::new(worker_path()?))
            .map_err(|e| anyhow::anyhow!("decoder pool: {e}"))?,
    );
    let _ = p0;
    for (ci, p, role, id) in users {
        let Some(input) = plant.inputs.iter().find(|i| i.id() == id) else {
            continue;
        };
        let (vtx, vrx) = std::sync::mpsc::sync_channel(256);
        let (atx, arx): (Vec<_>, Vec<_>) = p
            .audio
            .iter()
            .map(|_| std::sync::mpsc::sync_channel(1024))
            .unzip();
        let entry = map.entry((ci, p.index)).or_insert_with(|| LiveProg {
            video: [None, None],
            audio: [Vec::new(), Vec::new()],
            stats: crate::source::SourceStats::default(),
        });
        entry.video[role] = Some(vrx);
        entry.audio[role] = arx.into_iter().map(Some).collect();
        if role == 0 || p.live.is_none() {
            reg.add(
                "program_source",
                vec![("service", p.service.clone())],
                Value::Gauge(entry.stats.source.clone()),
            );
            reg.add(
                "source_switches_total",
                vec![("service", p.service.clone())],
                Value::Counter(entry.stats.switches.clone()),
            );
        }
        let l = vec![("input", id.to_owned())];
        let target = crate::live::LiveTarget {
            video: vtx,
            audio: atx,
            audio_tracks: crate::live::audio_tracks(p),
            video_spec: crate::live::specs(p),
            specs: p
                .ladder
                .iter()
                .map(|&f| crate::live::specs(&p.at(f)))
                .collect(),
            interlaced: p.ladder.iter().map(|f| f.interlaced()).collect(),
            rung: p.rung.clone(),
            delay,
        };
        let lstats = crate::live::LiveStats::default();
        reg.add(
            "decoded_pictures_total",
            l.clone(),
            Value::Counter(lstats.pictures.clone()),
        );
        reg.add(
            "decoder_worker_exits_total",
            l.clone(),
            Value::Counter(lstats.worker_exits.clone()),
        );
        reg.add(
            "timeline_sessions_total",
            l.clone(),
            Value::Counter(lstats.sessions.clone()),
        );
        let (sink, rx) = crate::live::input_queue();
        use crate::ingest::SessionReporter;
        reg.add(
            "input_events_dropped_total",
            l.clone(),
            Value::Counter(sink.dropped.clone()),
        );
        let ist = crate::ingest::InputStats::default();
        reg.add(
            "input_connections_total",
            l.clone(),
            Value::Counter(ist.connections.clone()),
        );
        reg.add(
            "input_bytes_total",
            l.clone(),
            Value::Counter(ist.bytes.clone()),
        );
        reg.add("input_errors_total", l, Value::Counter(ist.errors.clone()));
        let clock = RealClock::new(epoch, stop.clone());
        let input = input.clone();
        let (service, role_name) = (p.service.clone(), ["live", "standby"][role]);
        let (service2, input_id) = (service.clone(), id.to_owned());
        threads.push(
            std::thread::Builder::new()
                .name(format!("in-{id}"))
                .spawn(move || {
                    let _ctx = tracing::info_span!(
                        "input",
                        input = %input_id,
                        "input.kind" = input.kind(),
                        service = %service,
                        role = role_name
                    )
                    .entered();
                    match input {
                        crate::config::InputPlan::Rtmp { bind, app, key, .. } => {
                            crate::ingest::run_rtmp(
                                bind,
                                app,
                                key,
                                Box::new(SessionReporter::new(sink)),
                                ist,
                                &clock,
                            )
                        }
                        crate::config::InputPlan::TsUdp { bind, .. } => crate::ingest::run_ts_udp(
                            bind,
                            Box::new(SessionReporter::new(sink)),
                            ist,
                            &clock,
                        ),
                        crate::config::InputPlan::File { path, .. } => crate::ingest::run_ts_file(
                            path,
                            Box::new(SessionReporter::new(sink)),
                            ist,
                            &clock,
                        ),
                    }
                })?,
        );
        let clock = RealClock::new(epoch, stop.clone());
        let pool2 = pool.clone();
        let name = id.to_owned();
        threads.push(
            std::thread::Builder::new()
                .name(format!("dec-{id}"))
                .spawn(move || {
                    let _ctx = tracing::info_span!(
                        "decoder",
                        input = %name,
                        service = %service2,
                        role = role_name
                    )
                    .entered();
                    crate::live::run_decoder(&pool2, &name, rx, target, lstats, &clock)
                })?,
        );
    }
    Ok((map, threads))
}

fn audio_tracks(
    plant: &Plant,
    built: &Built,
    p0: u64,
    live: &mut LiveMap,
) -> anyhow::Result<Vec<crate::audio::Track>> {
    let mut v = Vec::new();
    for (ci, c) in plant.carriers.iter().enumerate() {
        for p in &c.programs {
            for (k, a) in p.audio.iter().enumerate() {
                // Every track follows the program's source; one its input does not feed stays
                // on the slate's audio.
                let (rx, follow) = match live.get_mut(&(ci, p.index)) {
                    Some(l) => {
                        let mut take =
                            |role: usize| l.audio[role].get_mut(k).and_then(Option::take);
                        ([take(0), take(1)], Some(l.stats.source.clone()))
                    }
                    None => ([None, None], None),
                };
                v.push(
                    crate::audio::Track::new(
                        c.id,
                        a,
                        p.index,
                        k,
                        built.carriers[ci].stc0,
                        p0,
                        built.senders[ci].clone(),
                        rx,
                        follow,
                        flex_of(built, ci, p.index, k),
                    )
                    .with_context(|| {
                        format!("carrier {} program {} audio {k}", c.id, p.index + 1)
                    })?,
                );
            }
        }
    }
    Ok(v)
}

fn hub_sessions(
    plant: &Plant,
    built: &Built,
    live: &mut LiveMap,
    p0: u64,
    delay: u64,
) -> Vec<HubSession> {
    let plans: Vec<&crate::config::ProgramPlan> = plant
        .carriers
        .iter()
        .flat_map(|c| c.programs.iter())
        .collect();
    let cfgs = video::session_configs(&plans);
    let mut v = Vec::new();
    let mut n = 0;
    for (ci, c) in plant.carriers.iter().enumerate() {
        for p in &c.programs {
            let (w, h) = p.format.size();
            let short: String = p.service.to_uppercase();
            let text = format!("{} {}", p.psip.channel.number, short);
            let slate = Slate::new(w, h, &text);
            let phase = cfgs.get(n).map_or(0, |c| c.gop_phase);
            n += 1;
            let ctx = program_span(c.id, p);
            let provider: Box<dyn video::FrameProvider> = match live.get_mut(&(ci, p.index)) {
                Some(l) => match (l.video[0].take(), l.video[1].take()) {
                    (None, None) => Box::new(SlateProvider(slate)),
                    (live_rx, standby_rx) => {
                        let (fn_, fd) = p.format.fps();
                        let cadence = crate::source::Cadence {
                            p0,
                            period: HZ * u64::from(fd) / u64::from(fn_),
                            gop: p.format.gop(),
                            phase,
                            delay,
                            units: if p.film && p.format.interlaced() {
                                2
                            } else {
                                1
                            },
                        };
                        Box::new(
                            crate::source::LiveProvider::new(
                                live_rx,
                                standby_rx,
                                slate,
                                cadence,
                                crate::source::FallbackTimes::default(),
                                l.stats.clone(),
                            )
                            .with_context(ctx.clone()),
                        )
                    }
                },
                None => Box::new(SlateProvider(slate)),
            };
            v.push(HubSession {
                carrier: ci,
                plan: p.clone(),
                stc0: built.carriers[ci].stc0,
                tx: built.senders[ci].clone(),
                provider,
                dropped: Counter::default(),
                delivered: Counter::default(),
                captioner: video::captioner(p),
                ladder: p.ladder.iter().map(|&f| p.at(f)).collect(),
                rung_gauge: crate::metrics::Gauge::default(),
                switches: Counter::default(),
                old_ids: Vec::new(),
                encoder: video::EncoderStats::default(),
                ctx,
            });
        }
    }
    v
}

/// A program's context: its events and wide events carry these fields. Made without a parent,
/// so it does not keep the span it was made in open.
fn program_span(carrier: u8, p: &crate::config::ProgramPlan) -> tracing::Span {
    tracing::info_span!(
        parent: None,
        "program",
        service = %p.service,
        carrier,
        program = p.index + 1,
        channel = %p.psip.channel.number,
    )
}

/// What the summaries need to know of each service.
fn service_infos(plant: &Plant) -> Vec<crate::telemetry::ServiceInfo> {
    plant
        .carriers
        .iter()
        .flat_map(|c| {
            c.programs.iter().map(|p| crate::telemetry::ServiceInfo {
                service: p.service.clone(),
                carrier: c.id,
                program: p.index + 1,
                channel: p.psip.channel.number.to_string(),
                ladder: p.ladder.iter().map(|f| f.name().to_owned()).collect(),
                inputs: [p.live.clone(), p.standby.clone()],
            })
        })
        .collect()
}

#[allow(clippy::too_many_arguments, reason = "the hub thread's whole setup")]
fn hub<D: TickDevice>(
    dev: D,
    sessions: Vec<HubSession>,
    timing: HubTiming,
    statmux: Option<video::StatmuxSetup>,
    overload: Option<video::OverloadSetup>,
    clock: &dyn Clock,
    ready: std::sync::mpsc::Sender<Result<(), String>>,
) {
    let _ = ready.send(Ok(()));
    let _ctx = tracing::info_span!("video").entered();
    let eng = Engine::new(dev, video::encoder_options(), 5.0);
    if let Err(e) = video::run(eng, sessions, timing, statmux, overload, clock) {
        tracing::error!("video hub stopped: {e}");
    }
}

/// Starts the video hub on its own thread. The device is made on that thread (a CUDA context
/// is current on the thread that made it, and the adapter is not `Send`); failure to make it
/// is reported here.
fn start_video(
    plant: &Plant,
    flex: &[std::sync::Arc<crate::audio::CarrierFlex>],
    sessions: Vec<HubSession>,
    timing: HubTiming,
    clock: Box<dyn Clock>,
) -> anyhow::Result<std::thread::JoinHandle<()>> {
    let plans: Vec<_> = sessions.iter().map(|s| &s.plan).collect();
    let mut cfgs = video::session_configs(&plans);
    // Room for the formats programs may step to (SPEC §12.1): one more session for each lower
    // rung, in batches of the size the hub makes; during a switch the old and the new session
    // of a program are open together.
    for (i, s) in sessions.iter().enumerate() {
        for p in s.ladder.iter().skip(1) {
            let n = sessions
                .iter()
                .filter(|o| o.ladder.iter().any(|q| q.format == p.format))
                .count() as u32;
            cfgs.push(video::session_config(p, cfgs[i].gop_phase, n));
        }
    }
    let arenas = video::arena_sizes(&cfgs).map_err(|e| anyhow::anyhow!("sessions: {e}"))?;
    tracing::info!(
        device = arenas.device,
        host = arenas.host,
        sessions = cfgs.len(),
        "video arenas"
    );
    let kind = plant.video;
    let statmux = plant.statmux.map(|plan| video::StatmuxSetup {
        plan,
        pools_ts: plant.carriers.iter().map(|c| c.video_pool_ts).collect(),
        pcr_ms: plant.output.pcr_interval_ms,
        flex: flex.to_vec(),
    });
    let overload = Some(video::OverloadSetup {
        plan: plant.overload,
        pcr_ms: plant.output.pcr_interval_ms,
    });
    let (tx, rx) = std::sync::mpsc::channel();
    let h = std::thread::Builder::new()
        .name("video-hub".into())
        .spawn(move || match kind {
            VideoDevice::Ref => match tb_device_ref::RefDevice::new(arenas) {
                Ok(d) => hub(
                    Guarded::new(d),
                    sessions,
                    timing,
                    statmux,
                    overload,
                    clock.as_ref(),
                    tx,
                ),
                Err(e) => {
                    let _ = tx.send(Err(format!("reference device: {e}")));
                }
            },
            VideoDevice::Cuda(gpu) => match CudaDevice::new(gpu as i32, arenas, &Cuda::IMAGE) {
                Ok(d) => hub(
                    Guarded::new(d),
                    sessions,
                    timing,
                    statmux,
                    overload,
                    clock.as_ref(),
                    tx,
                ),
                Err(e) => {
                    let _ = tx.send(Err(format!("CUDA device {gpu}: {e}")));
                }
            },
            VideoDevice::Hip(gpu) => match HipDevice::new(gpu as i32, arenas, &Hip::IMAGE) {
                Ok(d) => hub(
                    Guarded::new(d),
                    sessions,
                    timing,
                    statmux,
                    overload,
                    clock.as_ref(),
                    tx,
                ),
                Err(e) => {
                    let _ = tx.send(Err(format!("HIP device {gpu}: {e}")));
                }
            },
        })?;
    match rx.recv() {
        Ok(Ok(())) => Ok(h),
        Ok(Err(e)) => bail!(e),
        Err(_) => bail!("the video hub thread ended before starting"),
    }
}

/// Runs the plant.
pub fn run(plant: Plant, mode: Mode, tel: &tb_telemetry::Telemetry) -> anyhow::Result<()> {
    let delay = plant.output.delay_ms * (HZ / 1000);
    // Frame 0 is presented one Δ after start plus a little, so its submission is at t ≈ 0.
    let p0 = delay + HZ / 10;
    // The carriers start 0.5 s before frame 0 is presented: the tables, PCRs, the first
    // access units (from DTS − D) and the first audio frames all begin within half a second,
    // so no stream a PMT names is absent for longer (TR 101 290 PID_error, PTS_error).
    let out_start = p0.saturating_sub(HZ / 2);
    match mode {
        Mode::Offline { duration, dir } => {
            let unix0 = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos() as i128);
            let vclock = VirtualClock::new(unix0);
            let mut built = build_carriers(&plant, unix0)?;
            if plant
                .carriers
                .iter()
                .any(|c| c.programs.iter().any(|p| p.live.is_some()))
            {
                tracing::warn!(
                    "offline: live inputs are not opened; their programs show the slate"
                );
            }
            let mut nolive = LiveMap::new();
            let sessions = hub_sessions(&plant, &built, &mut nolive, p0, delay);
            let vworker = vclock.register();
            let tworker = vclock.register();
            let aworker = vclock.register();
            let tracks = audio_tracks(&plant, &built, p0, &mut nolive)?;
            let end = duration.as_nanos() as u64 * 27 / 1000;
            let audio = std::thread::spawn(move || {
                crate::audio::run(tracks, p0, delay, Some(end), &aworker)
            });
            let video = start_video(
                &plant,
                &built.flex,
                sessions,
                HubTiming {
                    p0,
                    delay,
                    epoch: None,
                    end: Some(end),
                },
                Box::new(vworker),
            )?;
            let targets: Vec<TableTarget> = plant
                .carriers
                .iter()
                .zip(&built.senders)
                .map(|(c, tx)| TableTarget {
                    lineup: c.lineup.clone(),
                    tx: tx.clone(),
                    services: c.programs.iter().map(|p| p.service.clone()).collect(),
                    versions: plant
                        .versions
                        .as_ref()
                        .map(|p| tables::VersionFile::new(p.clone()).load(c.lineup.tsid()))
                        .unwrap_or_default(),
                })
                .collect();
            let (guide, leap) = (plant.guide, plant.leap);
            let vfile = plant.versions.clone().map(tables::VersionFile::new);
            let ratings = plant.ratings.clone();
            let sfile = plant
                .schedule
                .clone()
                .map(|p| crate::schedule::ScheduleFile::with_ratings(p, ratings.clone()));
            let tables = std::thread::spawn(move || {
                tables::run(
                    targets,
                    guide,
                    &ratings,
                    leap,
                    10 * HZ,
                    &tworker,
                    vfile,
                    sfile,
                )
            });
            std::fs::create_dir_all(&dir)?;
            let mut files: Vec<FileSink> = Vec::new();
            for c in &built.carriers {
                let f = std::fs::File::create(dir.join(format!("carrier-{}.ts", c.id)))?;
                files.push(FileSink::new(f, 0));
            }
            // Every access unit is presented before `end`; the last ones are delivered by their
            // decoding times, so the file ends a little after.
            let stop_at = end + HZ / 10;
            let step = HZ / 1000;
            let mut t = 0;
            let started = Instant::now();
            while t < stop_at {
                t += step;
                if !vclock.advance_to(t) {
                    break;
                }
                for (c, f) in built.carriers.iter_mut().zip(files.iter_mut()) {
                    if c.mux.stats().slots == 0 {
                        let n = c
                            .mux
                            .clock()
                            .slot_at_or_after(tb_ts::Stc(c.stc0 + out_start));
                        c.mux.start_at(n);
                    }
                    c.drain();
                    c.wall(t, vclock.unix_nanos_at(t));
                    let mut err = None;
                    c.produce_until(t, &mut |p| {
                        if let Err(e) = f.send(&p.0) {
                            err = Some(e);
                        }
                    });
                    if let Some(e) = err {
                        bail!("writing carrier {}: {e}", c.id);
                    }
                }
                if t % HZ < step {
                    tracing::info!(
                        "offline: {} s of stream in {:.1} s",
                        t / HZ,
                        started.elapsed().as_secs_f64()
                    );
                }
            }
            vclock.stop();
            for f in &mut files {
                f.flush()?;
            }
            let _ = video.join();
            let _ = tables.join();
            let _ = audio.join();
            for (c, p) in built.carriers.iter().zip(&plant.carriers) {
                carrier::log_stats(c, p.programs.len());
            }
            Ok(())
        }
        Mode::Realtime { duration } => {
            let stop = Arc::new(AtomicBool::new(false));
            let epoch = Instant::now() + Duration::from_millis(200);
            let unix0 = RealClock::new(epoch, stop.clone()).unix_nanos_at(0);
            let built = build_carriers(&plant, unix0)?;
            let mut reg = Registry::default();
            let (mut live, live_threads) = start_live(&plant, p0, delay, epoch, &stop, &mut reg)?;
            let sessions = hub_sessions(&plant, &built, &mut live, p0, delay);
            for s in &sessions {
                let l = vec![("service", s.plan.service.clone())];
                s.encoder.register(&mut reg, &l);
                reg.add(
                    "encoder_access_units_total",
                    l.clone(),
                    Value::Counter(s.delivered.clone()),
                );
                reg.add(
                    "encoder_queue_full_total",
                    l.clone(),
                    Value::Counter(s.dropped.clone()),
                );
                reg.add(
                    "video_format_rung",
                    l.clone(),
                    Value::Gauge(s.rung_gauge.clone()),
                );
                reg.add(
                    "video_format_switches_total",
                    l,
                    Value::Counter(s.switches.clone()),
                );
            }
            let tracks = audio_tracks(&plant, &built, p0, &mut live)?;
            for t in &tracks {
                let l = vec![
                    ("carrier", t.carrier.to_string()),
                    ("program", (t.program + 1).to_string()),
                    ("track", t.track.to_string()),
                ];
                reg.add(
                    "audio_encoded_frames_total",
                    l.clone(),
                    Value::Counter(t.frames.clone()),
                );
                reg.add(
                    "audio_queue_full_total",
                    l.clone(),
                    Value::Counter(t.dropped.clone()),
                );
                reg.add("audio_rate_bps", l, Value::Gauge(t.rate.clone()));
            }
            let aclock = RealClock::new(epoch, stop.clone());
            let audio = std::thread::Builder::new()
                .name("audio".into())
                .spawn(move || {
                    let _ctx = tracing::info_span!("audio").entered();
                    crate::audio::run(tracks, p0, delay, None, &aclock)
                })?;
            let gauges: Vec<CarrierGauges> = plant
                .carriers
                .iter()
                .map(|c| CarrierGauges::register(&mut reg, c.id, c.programs.len()))
                .collect();
            let video = start_video(
                &plant,
                &built.flex,
                sessions,
                HubTiming {
                    p0,
                    delay,
                    epoch: Some(epoch),
                    end: None,
                },
                Box::new(RealClock::new(epoch, stop.clone())),
            )?;
            let targets: Vec<TableTarget> = plant
                .carriers
                .iter()
                .zip(&built.senders)
                .map(|(c, tx)| TableTarget {
                    lineup: c.lineup.clone(),
                    tx: tx.clone(),
                    services: c.programs.iter().map(|p| p.service.clone()).collect(),
                    versions: plant
                        .versions
                        .as_ref()
                        .map(|p| tables::VersionFile::new(p.clone()).load(c.lineup.tsid()))
                        .unwrap_or_default(),
                })
                .collect();
            let (guide, leap) = (plant.guide, plant.leap);
            let vfile = plant.versions.clone().map(tables::VersionFile::new);
            let ratings = plant.ratings.clone();
            let sfile = plant
                .schedule
                .clone()
                .map(|p| crate::schedule::ScheduleFile::with_ratings(p, ratings.clone()));
            let tclock = RealClock::new(epoch, stop.clone());
            let tables = std::thread::Builder::new()
                .name("tables".into())
                .spawn(move || {
                    let _ctx = tracing::info_span!("tables").entered();
                    tables::run(
                        targets,
                        guide,
                        &ratings,
                        leap,
                        10 * HZ,
                        &tclock,
                        vfile,
                        sfile,
                    )
                })?;

            // Pacers: one per interface, plus one for carriers on none.
            let mut groups: Vec<Vec<pacer::CarrierTx>> = (0..=plant.output.interfaces.len())
                .map(|_| Vec::new())
                .collect();
            let mut muxes = Vec::new();
            let mut tx_stats = Vec::new();
            for (c, plan) in built.carriers.into_iter().zip(&plant.carriers) {
                let iface = plan.interface.and_then(|i| plant.output.interfaces.get(i));
                let (sink, framing): (Box<dyn Sink>, Framing) = match &plan.destination {
                    Destination::Udp(a) => (
                        Box::new(UdpSink::new(
                            *a,
                            iface.and_then(|i| i.address),
                            iface.map_or(1, |i| i.ttl),
                        )?),
                        Framing::Udp,
                    ),
                    Destination::Rtp(a) => (
                        Box::new(UdpSink::new(
                            *a,
                            iface.and_then(|i| i.address),
                            iface.map_or(1, |i| i.ttl),
                        )?),
                        Framing::Rtp {
                            ssrc: 0x7462_0000 | u32::from(plan.id),
                        },
                    ),
                    Destination::File(p) => (
                        Box::new(FileSink::new(std::fs::File::create(p)?, 0)),
                        Framing::Udp,
                    ),
                };
                let start = c
                    .mux
                    .clock()
                    .slot_at_or_after(tb_ts::Stc(c.stc0 + out_start));
                let (feeder, tx) = pacer::carrier_output(
                    *c.mux.clock(),
                    framing,
                    plant.output.per,
                    Duration::from_millis(plant.output.lead_ms),
                    sink,
                    start,
                );
                tx_stats.push((plan.id, tx.stats.clone()));
                groups[plan.interface.unwrap_or(plant.output.interfaces.len())].push(tx);
                muxes.push((c, feeder));
            }
            // The muxes and pacers stop last, on their own flag: after the producers have stopped
            // and every access unit they queued has gone out (SPEC §3: "a mux and its pacer are
            // the last things to stop").
            let stop_out = Arc::new(AtomicBool::new(false));
            let mut drained: Vec<Arc<AtomicBool>> = Vec::new();
            // Set once the video and audio threads have ended: everything they made is queued.
            let producers_done = Arc::new(AtomicBool::new(false));
            let mut threads = Vec::new();
            let mut mux_threads = Vec::new();
            for ((c, feeder), (mut g, (_, txs))) in
                muxes.into_iter().zip(gauges.into_iter().zip(&tx_stats))
            {
                g.tx = Some(txs.clone());
                let clock = RealClock::new(epoch, stop_out.clone());
                let (stop2, drained2) = (producers_done.clone(), Arc::new(AtomicBool::new(false)));
                drained.push(drained2.clone());
                let tsid = plant
                    .carriers
                    .iter()
                    .find(|p| p.id == c.id)
                    .map_or(0, |p| p.lineup.tsid());
                let checker = plant
                    .output
                    .self_check
                    .then(|| carrier::self_checker(plant.output.rate.bps(), tsid));
                mux_threads.push(
                    std::thread::Builder::new()
                        .name(format!("mux-{}", c.id))
                        .spawn(move || {
                            let _ctx =
                                tracing::info_span!("carrier", carrier = c.id, tsid).entered();
                            carrier::run_realtime(c, feeder, &clock, g, checker, &stop2, &drained2)
                        })?,
                );
            }
            if let Some(d) = plant.device.clone() {
                let g = crate::device::DeviceGauges::register(&mut reg, &d.desired);
                let report = d.supervisor.mode == tb_eqam::supervisor::ReconcileMode::Report;
                match read_credentials(&d.credentials_file, report) {
                    Ok(creds) => {
                        let clock = RealClock::new(epoch, stop.clone());
                        threads.push(std::thread::Builder::new().name("device".into()).spawn(
                            move || {
                                let _ctx = tracing::info_span!("device").entered();
                                crate::device::run(
                                    d.base_url,
                                    creds,
                                    d.supervisor,
                                    d.desired,
                                    g,
                                    &clock,
                                )
                            },
                        )?);
                    }
                    Err(e) => tracing::error!("device supervisor not started: {e}"),
                }
            }
            crate::telemetry::register_process(&mut reg);
            let reg = Arc::new(reg);
            let _instruments = tel
                .meter()
                .map(|m| crate::telemetry::export_metrics(&m, &reg))
                .unwrap_or_default();
            if let Some(every) = plant.telemetry.summaries {
                let s = crate::telemetry::Summaries {
                    registry: reg.clone(),
                    every,
                    services: service_infos(&plant),
                };
                let stop = stop_out.clone();
                threads.push(
                    std::thread::Builder::new()
                        .name("summaries".into())
                        .spawn(move || s.run(&stop))?,
                );
            }
            if let Some(addr) = plant.control {
                match crate::control::serve(addr, reg.clone()) {
                    Ok(_) => {}
                    Err(e) => tracing::warn!(%addr, "control API not started: {e}"),
                }
            }
            for (i, g) in groups.into_iter().enumerate() {
                if g.is_empty() {
                    continue;
                }
                let s = stop_out.clone();
                threads.push(
                    std::thread::Builder::new()
                        .name(format!("pacer-{i}"))
                        .spawn(move || {
                            let _ctx = tracing::info_span!("pacer", pacer = i).entered();
                            pacer::run(g, epoch, PacerTiming::default(), &s)
                        })?,
                );
            }
            tracing::info!(carriers = plant.carriers.len(), "running");
            // SIGTERM (systemd) and SIGINT stop the server cleanly: sinks flushed, statistics
            // logged.
            let signaled = Arc::new(AtomicBool::new(false));
            for sig in [signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT] {
                if let Err(e) = signal_hook::flag::register(sig, signaled.clone()) {
                    tracing::warn!("signal {sig} not handled: {e}");
                }
            }
            let end = duration.map(|d| Instant::now() + d);
            while !signaled.load(std::sync::atomic::Ordering::Relaxed)
                && end.is_none_or(|e| Instant::now() < e)
            {
                std::thread::sleep(Duration::from_millis(100));
            }
            let why = if signaled.load(std::sync::atomic::Ordering::Relaxed) {
                "signal"
            } else {
                "duration"
            };
            tb_telemetry::annotate(&tracing::Span::current(), "stop", why);
            tracing::info!(why, "stopping");
            stop.store(true, std::sync::atomic::Ordering::Relaxed);
            // The encoder closes its sessions (the last GOP whole, sequence_end_code); then the
            // queued access units are delivered by their decoding times, at most Δ away.
            let _ = video.join();
            let _ = audio.join();
            producers_done.store(true, std::sync::atomic::Ordering::Relaxed);
            let give_up =
                Instant::now() + Duration::from_nanos(delay * 1000 / 27) + Duration::from_secs(1);
            while Instant::now() < give_up
                && !drained
                    .iter()
                    .all(|d| d.load(std::sync::atomic::Ordering::Relaxed))
            {
                std::thread::sleep(Duration::from_millis(2));
            }
            // The pacer sends what the mux already queued (its lead) before it is told to stop.
            std::thread::sleep(Duration::from_millis(plant.output.lead_ms + 5));
            stop_out.store(true, std::sync::atomic::Ordering::Relaxed);
            for t in threads.into_iter().chain(live_threads) {
                let _ = t.join();
            }
            for (t, p) in mux_threads.into_iter().zip(&plant.carriers) {
                if let Ok(c) = t.join() {
                    carrier::log_stats(&c, p.programs.len());
                }
            }
            for (id, s) in &tx_stats {
                use std::sync::atomic::Ordering::Relaxed;
                let h: Vec<u64> = s.late_hist.iter().map(|x| x.load(Relaxed)).collect();
                tracing::info!(
                    carrier = id,
                    sent = s.sent.load(Relaxed),
                    underruns = s.underruns.load(Relaxed),
                    stale = s.stale.load(Relaxed),
                    send_errors = s.send_errors.load(Relaxed),
                    late_max_us = s.late_max_ns.load(Relaxed) / 1000,
                    "pacer lateness <50/<100/<250/<500/<1000/<5000/>=5000 us: {h:?}"
                );
            }
            let _ = tables.join();
            Ok(())
        }
    }
}

/// Reads `user\npassword` from a credentials file (SPEC §10.1: never from the TOML).
/// The account the supervisor logs in with. The file holds the Administrator (user, then
/// password) and optionally, on lines 3 and 4, a read-only account (the box's Guest): in
/// report mode, which never writes, that one is used, so the supervisor does not hold the
/// single Administrator session and log people out of the web UI (SPEC §18 item 10).
fn read_credentials(
    p: &std::path::Path,
    report: bool,
) -> anyhow::Result<tb_eqam::types::Credentials> {
    let text = std::fs::read_to_string(p).with_context(|| p.display().to_string())?;
    let l: Vec<&str> = text.lines().map(str::trim).collect();
    let at = |i: usize| l.get(i).copied().unwrap_or("");
    let (u, pw) = if report && !at(2).is_empty() && !at(3).is_empty() {
        tracing::info!(user = at(2), "device: polling with the read-only account");
        (at(2), at(3))
    } else {
        (at(0), at(1))
    };
    tb_eqam::types::Credentials::new(u, pw).map_err(|e| anyhow::anyhow!("{}: {e}", p.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_mode_polls_with_the_read_only_account_when_there_is_one() {
        let dir = std::env::temp_dir().join(format!("tb-creds-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let both = dir.join("both");
        std::fs::write(&both, "Admin\nsecret\nGuest\nother\n").unwrap();
        let admin_only = dir.join("admin");
        std::fs::write(&admin_only, "Admin\nsecret\n").unwrap();
        assert_eq!(read_credentials(&both, true).unwrap().username(), "Guest");
        assert_eq!(read_credentials(&both, false).unwrap().username(), "Admin");
        assert_eq!(
            read_credentials(&admin_only, true).unwrap().username(),
            "Admin"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
