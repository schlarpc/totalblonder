//! The driver on the reference adapter beyond the golden cases: input paths, output files,
//! groups at two frame rates, run-time sessions, and what happens when the device fails.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers outside #[test] functions"
)]

mod common;

use common::{Scratch, encode, encode_args, parse, shape};
use tb_device::{ArenaSizes, Completion, Device, DeviceError, Nanos, Plan, Ticket};
use tb_device_ref::RefDevice;
use tb_engine::device::{Guarded, TickDevice};
use tb_m2venc::opts::Opts;
use tb_m2venc::{Outcome, Status, arena_sizes, run};

const TINY: &str = "--size 64x48 --gop 6 --bframes 2";

/// What a faulty device does to the plan with a given number (counted from 0 over the run).
#[derive(Clone, Debug)]
enum Trouble {
    /// `submit` refuses it with this error.
    Refuse(usize, DeviceError),
    /// It runs, and its completion carries this error.
    Complete(usize, DeviceError),
    /// From this plan on, no wait ever finishes.
    Hang(usize),
}

/// The reference device with one fault.
struct Faulty {
    inner: RefDevice,
    trouble: Trouble,
    n: usize,
    bad: Option<(Ticket, DeviceError)>,
    hung: bool,
}

impl Device for Faulty {
    fn arenas(&self) -> ArenaSizes {
        self.inner.arenas()
    }
    fn host(&self) -> &[u8] {
        self.inner.host()
    }
    fn host_mut(&mut self) -> &mut [u8] {
        self.inner.host_mut()
    }
    fn now(&self) -> Nanos {
        self.inner.now()
    }
    fn submit(&mut self, plan: &Plan) -> Result<Ticket, DeviceError> {
        let n = self.n;
        self.n += 1;
        match &self.trouble {
            Trouble::Refuse(at, e) if *at == n => return Err(e.clone()),
            Trouble::Refuse(at, e @ DeviceError::Lost { .. }) if *at < n => {
                return Err(e.clone());
            }
            Trouble::Complete(at, e @ DeviceError::Lost { .. }) if *at < n => {
                return Err(e.clone());
            }
            Trouble::Hang(at) if *at <= n => self.hung = true,
            _ => {}
        }
        let t = self.inner.submit(plan)?;
        if let Trouble::Complete(at, e) = &self.trouble
            && *at == n
        {
            self.bad = Some((t, e.clone()));
        }
        Ok(t)
    }
    fn wait(&mut self, ticket: Ticket, timeout: Option<Nanos>) -> Completion {
        if self.hung {
            let t = self.now();
            return Completion {
                ticket,
                status: Err(DeviceError::Timeout),
                submitted: t,
                finished: t,
            };
        }
        let mut c = self.inner.wait(ticket, timeout);
        if let Some((t, e)) = &self.bad
            && *t == ticket
        {
            c.status = Err(e.clone());
            self.bad = None;
        }
        c
    }
}

impl TickDevice for Faulty {
    fn submit_class(&mut self, plan: &Plan, _class: u8) -> Result<Ticket, DeviceError> {
        self.submit(plan)
    }
    fn real_time(&self) -> bool {
        false
    }
    fn synchronous(&self) -> bool {
        true
    }
    fn describe(&self) -> String {
        "a faulty device".to_owned()
    }
}

fn encode_faulty(o: &Opts, trouble: Trouble) -> Outcome {
    let dev = Faulty {
        inner: RefDevice::new(arena_sizes(o).unwrap()).unwrap(),
        trouble,
        n: 0,
        bad: None,
        hung: false,
    };
    run(o, Guarded::new(dev), &mut Vec::new()).unwrap()
}

/// Frames copied into the sessions' frame buffers (`--test-input-copy`, `FrameSource::Host`) give
/// the stream of frames uploaded from the clip (`FrameSource::HostAt`), with and without the
/// overlapped upload; and the overlapped upload changes no byte.
#[test]
fn the_two_input_paths_and_the_overlapped_upload_code_the_same_stream() {
    let dir = Scratch::new("input");
    let clip = dir.clip("tiny-i");
    let mut streams = Vec::new();
    for (i, extra) in [
        "",
        "--test-input-copy",
        "--overlap",
        "--overlap --test-input-copy",
    ]
    .iter()
    .enumerate()
    {
        let out = encode_args(&format!(
            "--input {clip} {TINY} --interlaced --bitrate 300000 --sessions 3 --stagger --cc \
             --write 3 --frames 20 --output {} {extra}",
            dir.path(&format!("o{i}"))
        ));
        assert_eq!(out.status, Status::Complete);
        assert_eq!(out.report.pictures, 60);
        let s: Vec<Vec<u8>> = (0..3).map(|s| dir.read(&format!("o{i}.{s}.m2v"))).collect();
        for x in &s {
            let sh = shape(x);
            assert!(sh.ended);
            assert_eq!(sh.types.len(), 20);
        }
        // staggered sessions read the clip at different frames: three different streams
        assert_ne!(s[0], s[1]);
        streams.push(s);
    }
    assert_eq!(streams[0], streams[1]);
    assert_eq!(streams[0], streams[2]);
    assert_eq!(streams[0], streams[3]);
}

/// Every output file of a run, and what is in it.
#[test]
fn the_files_of_a_run() {
    let dir = Scratch::new("files");
    let clip = dir.clip("tiny");
    let mut o = parse(&format!(
        "--input {clip} {TINY} --bitrate 300000 --frames 18 --output {} --recon {} --stats {} \
         --json {} --ticklog {} --golden {} --log --rate-plan 1:200000 --force-key 8",
        dir.path("s"),
        dir.path("recon.yuv"),
        dir.path("stats.jsonl"),
        dir.path("run.json"),
        dir.path("ticks.txt"),
        dir.path("au.jsonl"),
    ));
    o.profile = true;
    let dev = RefDevice::new(arena_sizes(&o).unwrap()).unwrap();
    let mut console = Vec::new();
    let out = run(&o, Guarded::new(dev), &mut console).unwrap();
    assert_eq!(out.status, Status::Complete);
    let r = &out.report;
    assert_eq!((r.pictures, r.repeats, r.failed_ticks), (18, 0, 0));
    assert_eq!(r.mbs, 18 * 4 * 3);

    let sh = shape(&dir.read("s.m2v"));
    assert!(sh.ended);
    assert_eq!(sh.types.len(), 18);
    // key frames at 0, 6, the forced one at 8, then 14 (the grid moves with a forced key)
    assert_eq!(sh.sequence_headers, 4);
    assert_eq!(sh.gop_headers, 4);

    // the reconstruction: every display frame, at the clip's frame size
    assert_eq!(dir.read("recon.yuv").len(), 18 * 64 * 48 * 3 / 2);

    // one stats line per picture, in coded order, with the rate plan applied at key frame 1
    let stats = String::from_utf8(dir.read("stats.jsonl")).unwrap();
    let lines: Vec<&str> = stats.lines().collect();
    assert_eq!(lines.len(), 18);
    assert!(lines[0].starts_with("{\"n\":0,\"disp\":0,\"type\":\"I\",\"key\":1,"));
    assert!(lines[0].contains("\"rate\":300000,"));
    assert!(lines[17].contains("\"rate\":200000,"));
    assert!(stats.contains("\"ticket\":1,"));
    let disp: Vec<u64> = lines
        .iter()
        .map(|l| {
            let at = l.find("\"disp\":").unwrap() + 7;
            l[at..].split(',').next().unwrap().parse().unwrap()
        })
        .collect();
    let mut sorted = disp.clone();
    sorted.sort_unstable();
    assert_eq!(sorted, (0..18).collect::<Vec<u64>>());
    assert_ne!(
        disp, sorted,
        "coded order is not display order with B pictures"
    );

    // the tick log: one line of 19 fields per tick, the macroblocks of the tick by type
    let ticks = String::from_utf8(dir.read("ticks.txt")).unwrap();
    let rows: Vec<Vec<&str>> = ticks.lines().map(|l| l.split(' ').collect()).collect();
    assert!(rows.len() >= 18);
    assert!(rows.iter().all(|f| f.len() == 19));
    assert_eq!(rows[0][0], "0");
    assert_eq!(&rows[0][2..5], ["12", "0", "0"]);
    let mb: u64 = rows
        .iter()
        .flat_map(|f| f[2..5].iter().map(|x| x.parse::<u64>().unwrap()))
        .sum();
    assert_eq!(mb, r.mbs);

    // the JSON record has the keys the gate and the benchmark read
    // (the binary writes it to the file of --json)
    let json = out.report.json();
    for key in [
        "underflows",
        "retries_per_picture",
        "ladder_pictures",
        "skipped_pictures",
        "i_replaced",
        "room_min_frac",
        "rt",
        "mb_per_s",
        "tick_mean_ms",
        "tick_p99_ms",
        "tick_p999_ms",
        "period_ms",
        "ontime_frac",
        "cpu_cores",
        "stages_ms",
        "late_deadline",
        "mbps_per_session",
        "dev_mb",
    ] {
        assert!(json.contains(&format!("\"{key}\":")), "{key}");
    }
    assert!(json.contains("\"pictures\":18,"));
    assert!(json.contains("\"begin: search+transform\":"));
    let one_line = json.replace('\n', " ").replacen('{', "{\"k\":\"json\",", 1);
    let rec = &tb_golden::parse("json", &one_line).unwrap()[0];
    assert_eq!(rec.int("underflows").unwrap(), 0);
    assert_eq!(rec.int("sessions").unwrap(), 1);

    // --log: a line per picture of session 0
    let console = String::from_utf8(console).unwrap();
    assert_eq!(
        console.lines().filter(|l| l.starts_with("pic ")).count(),
        18
    );
    assert!(out.report.text().contains("wall "));
}

/// Three groups at two frame rates on one tick (the mixed line-up in small): each stream has
/// its frames, staggered key frames, a rate walk; the run is deterministic.
#[test]
fn groups_at_two_frame_rates() {
    let dir = Scratch::new("mixed");
    let (a, b) = (dir.clip("tiny"), dir.clip("tiny-i"));
    let cmd = |tag: &str| {
        format!(
            "--input {b} {TINY} --interlaced --bitrate 300000 --sessions 2 --frames 12 --stagger \
             --rate-walk 150000:300000:1 \
             --group --input {a} {TINY} --fps 60000/1001 --bitrate 500000 --sessions 2 \
             --frames 24 --stagger --rate-walk 250000:500000:2 \
             --group --input {a} --size 64x48 --gop 15 --q 8 --sessions 3 --frames 12 \
             --output {} --write 4 --overlap",
            dir.path(tag)
        )
    };
    let out = encode_args(&cmd("x"));
    assert_eq!(out.status, Status::Complete);
    assert_eq!(out.report.pictures, 2 * 12 + 2 * 24 + 3 * 12);
    assert_eq!(out.report.sessions, 7);
    assert!((out.report.period_ms - 1001.0 / 60.0).abs() < 1e-9);
    for (g, n, frames) in [(0, 2, 12), (1, 2, 24), (2, 3, 12)] {
        for s in 0..n {
            let sh = shape(&dir.read(&format!("x.g{g}.{s}.m2v")));
            assert!(sh.ended, "group {g} session {s}");
            assert_eq!(sh.types.len(), frames);
            assert_eq!(sh.types[0], 1);
        }
    }
    // session 1 of a staggered pair has its second key frame earlier
    assert_ne!(dir.read("x.g1.0.m2v"), dir.read("x.g1.1.m2v"));
    let again = encode_args(&cmd("y"));
    assert_eq!(again.status, Status::Complete);
    for name in ["g0.0", "g0.1", "g1.0", "g1.1", "g2.0", "g2.1", "g2.2"] {
        assert_eq!(
            dir.read(&format!("x.{name}.m2v")),
            dir.read(&format!("y.{name}.m2v")),
            "{name}"
        );
    }
}

/// Sessions opened, closed and reconfigured while others run (`--churn`, `--pool`): every
/// stream is whole, the stable sessions' streams are those of a run without churn, and the
/// batches of closed sessions give their memory back.
#[test]
fn sessions_at_run_time() {
    let dir = Scratch::new("churn");
    let (a, b) = (dir.clip("tiny"), dir.clip("tiny-i"));
    let cmd = |tag: &str, churn: &str| {
        format!(
            "--input {a} {TINY} --bitrate 300000 --sessions 2 --frames 60 --stagger \
             --group --input {b} {TINY} --interlaced --bitrate 300000 --sessions 3 --pool \
             --group --input {a} --size 64x48 --fps 30000/1001 --range 4 --q 7 --sessions 2 \
             --pool --churn {churn} --churn-chunk 2 --reap-after 3 --write 9 --output {} \
             --oplog {}",
            dir.path(tag),
            dir.path(&format!("{tag}.ops"))
        )
    };
    let quiet = encode_args(&cmd("q", "1:0"));
    assert_eq!(quiet.status, Status::Complete);
    assert_eq!(quiet.report.pool_opens, 0);
    assert_eq!(quiet.report.pictures, 120);

    let busy = encode_args(&cmd("c", "4:12"));
    assert_eq!(busy.status, Status::Complete);
    let r = &busy.report;
    assert!(r.pool_opens >= 4, "opens {}", r.pool_opens);
    assert_eq!(
        r.pool_opens, r.pool_closes,
        "every pool session is closed at the end"
    );
    assert!(r.pictures > 120);
    assert!(r.op_ticks > 0);
    // the stable sessions do not notice
    for s in 0..2 {
        assert_eq!(
            dir.read(&format!("c.g0.{s}.m2v")),
            dir.read(&format!("q.g0.{s}.m2v"))
        );
    }
    // every incarnation of every pool slot left a whole stream
    let mut pool_streams = 0;
    let mut pool_pictures = 0;
    for g in 1..3 {
        for s in 0..3 {
            for c in 0..40 {
                let name = format!("c.g{g}.{s}.c{c}.m2v");
                if !dir.exists(&name) {
                    break;
                }
                let sh = shape(&dir.read(&name));
                assert!(sh.ended, "{name}");
                if !sh.types.is_empty() {
                    assert_eq!(sh.types[0], 1, "{name}");
                    assert!(sh.sequence_headers >= 1);
                }
                pool_streams += 1;
                pool_pictures += sh.types.len();
            }
        }
    }
    assert_eq!(pool_streams as u64, r.pool_opens);
    assert_eq!(pool_pictures as u64, r.pictures - 120);
    // one line per tick with an operation
    let ops = String::from_utf8(dir.read("c.ops")).unwrap();
    assert_eq!(ops.lines().count() as u64, r.op_ticks);
    // deterministic
    let again = encode_args(&cmd("d", "4:12"));
    assert_eq!(again.report.pool_opens, r.pool_opens);
    assert_eq!(again.report.pictures, r.pictures);
    assert_eq!(dir.read("d.g1.0.c0.m2v"), dir.read("c.g1.0.c0.m2v"));
}

fn fault_line(dir: &Scratch, tag: &str) -> Opts {
    let clip = dir.clip("tiny");
    parse(&format!(
        "--input {clip} {TINY} --bitrate 300000 --sessions 2 --stagger --frames 24 --write 2 \
         --output {} --stats {} --device-timeout 0.05",
        dir.path(tag),
        dir.path(&format!("{tag}.jsonl"))
    ))
}

fn repeats_in(dir: &Scratch, tag: &str) -> Vec<bool> {
    String::from_utf8(dir.read(&format!("{tag}.jsonl")))
        .unwrap()
        .lines()
        .map(|l| l.contains("\"repeat\":1"))
        .collect()
}

/// A plan that fails (in each phase, refused at the submit or failed at the completion): the
/// pictures of that tick are repeats, every frame still has its picture, the streams are
/// whole, and the run reports the failure.
#[test]
fn a_failed_plan_costs_repeats_and_is_reported() {
    let dir = Scratch::new("failed");
    let clean = encode(&fault_line(&dir, "clean"));
    assert_eq!(clean.status, Status::Complete);
    assert_eq!(clean.report.pictures, 48);
    // plans 0 and 1 are the settle before the first tick; a tick is three plans or more
    let failed = DeviceError::Failed { step: 3, code: 701 };
    let bad_plan = DeviceError::BadPlan {
        step: 0,
        what: "test",
    };
    for (i, trouble) in [
        Trouble::Complete(1 + 3 * 5, failed.clone()),
        Trouble::Complete(1 + 3 * 5 + 1, failed.clone()),
        Trouble::Complete(1 + 3 * 5 + 2, failed),
        Trouble::Refuse(1 + 3 * 7, bad_plan),
    ]
    .into_iter()
    .enumerate()
    {
        let tag = format!("f{i}");
        let out = encode_faulty(&fault_line(&dir, &tag), trouble.clone());
        assert_eq!(out.status, Status::FailedPlans(1), "{trouble:?}");
        let r = &out.report;
        assert_eq!((r.failed_ticks, r.device_errors), (1, 1), "{trouble:?}");
        assert!(!r.device_lost);
        assert_eq!(r.pictures, 48, "{trouble:?}: every frame has its picture");
        assert!((1..=8).contains(&r.repeats), "{trouble:?}: {}", r.repeats);
        for s in 0..2 {
            let sh = shape(&dir.read(&format!("{tag}.{s}.m2v")));
            assert!(sh.ended, "{trouble:?}");
            assert_eq!(sh.types.len(), 24, "{trouble:?}");
        }
        // the repeats are a short run, and the session codes again afterwards
        let rep = repeats_in(&dir, &tag);
        assert!(rep.iter().any(|x| *x), "{trouble:?}");
        assert!(!rep[rep.len() - 1], "{trouble:?}");
    }
}

/// The device is lost in the middle of a run: the sessions go on hold as the core defines
/// (repeats made on the host), the input stops, every frame already taken gets its picture,
/// the streams end properly, and the run says so.
#[test]
fn a_lost_device_ends_the_run_with_whole_streams() {
    let dir = Scratch::new("lost");
    let lost = DeviceError::Lost { code: 700 };
    for (i, trouble) in [
        Trouble::Complete(1 + 3 * 6 + 1, lost.clone()),
        Trouble::Refuse(1 + 3 * 9, lost),
    ]
    .into_iter()
    .enumerate()
    {
        let tag = format!("l{i}");
        let out = encode_faulty(&fault_line(&dir, &tag), trouble.clone());
        assert_eq!(out.status, Status::DeviceLost, "{trouble:?}");
        let r = &out.report;
        assert!(r.device_lost);
        assert!(r.repeats >= 2, "{trouble:?}");
        // the input stopped: fewer pictures than frames asked for, but more than were coded
        // before the loss, and as many as frames were submitted (two per tick)
        assert!(
            r.pictures < 48 && r.pictures >= 12,
            "{trouble:?}: {}",
            r.pictures
        );
        assert_eq!(r.pictures % 2, 0);
        let mut total = 0;
        for s in 0..2 {
            let sh = shape(&dir.read(&format!("{tag}.{s}.m2v")));
            assert!(sh.ended, "{trouble:?}");
            assert_eq!(sh.types[0], 1);
            total += sh.types.len();
        }
        assert_eq!(total as u64, r.pictures);
        // from the loss on, nothing but repeats
        let rep = repeats_in(&dir, &tag);
        let first = rep.iter().position(|x| *x).unwrap();
        assert!(first >= 4 && rep[first..].iter().all(|x| *x), "{trouble:?}");
        assert!(out.report.text().contains("THE DEVICE WAS LOST"));
    }
}

/// A device that stops answering: the run ends after the limit with the streams closed at the
/// last complete picture.
#[test]
fn a_device_that_hangs_ends_the_run() {
    let dir = Scratch::new("hung");
    let out = encode_faulty(&fault_line(&dir, "h"), Trouble::Hang(1 + 3 * 8 + 1));
    let Status::Hung(why) = &out.status else {
        panic!("{:?}", out.status);
    };
    assert!(why.contains("did not finish a plan of phase"), "{why}");
    assert!(out.report.pictures >= 8 && out.report.pictures < 48);
    let mut total = 0;
    for s in 0..2 {
        let sh = shape(&dir.read(&format!("h.{s}.m2v")));
        assert!(sh.ended);
        total += sh.types.len();
    }
    assert_eq!(total as u64, out.report.pictures);
}

/// A configuration the encoder refuses is an error before anything runs.
#[test]
fn a_refused_session_is_an_error() {
    let dir = Scratch::new("refused");
    let clip = dir.clip("tiny");
    // a rate below what the buffer guarantee needs
    let o = parse(&format!("--input {clip} {TINY} --bitrate 2000 --vbv 16384"));
    assert!(matches!(
        arena_sizes(&o),
        Err(tb_m2venc::RunError::Encoder(_))
    ));
    // frame rates that are not multiples of each other
    let o = parse(&format!(
        "--input {clip} {TINY} --q 6 --group --input {clip} {TINY} --fps 25/1 --q 6"
    ));
    let dev = RefDevice::new(arena_sizes(&o).unwrap()).unwrap();
    assert!(matches!(
        run(&o, dev, &mut Vec::new()),
        Err(tb_m2venc::RunError::FrameRates)
    ));
    // a clip shorter than one frame
    let o = parse(&format!("--input {clip} --size 1920x1080 --q 6"));
    assert!(matches!(
        arena_sizes(&o),
        Err(tb_m2venc::RunError::InputTooSmall(_))
    ));
}
