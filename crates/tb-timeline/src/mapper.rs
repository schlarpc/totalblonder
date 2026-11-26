//! Mapping input timestamps to output times (SPEC §5.3).
//!
//! One [`Mapper`] per input. It holds at most one *session*, a run of input with consistent
//! timestamps, described by the affine map `out = offset + in · (1 + skew)`. The map is kept as a
//! piecewise-linear function that is continuous where the skew changes: when the filtered skew
//! changes, the anchor moves to the current frame, and the anchor's output time is held as an
//! exact rational (units of 10^-9 tick), so changing the skew a million times adds no rounding.
//!
//! Choices where the SPEC is open or ambiguous (each marked `[choice]` in the code):
//! - The "timestamp step against arrival step" rule measures both steps from the last frame that
//!   was consistent (the *reference*), not from the previous frame: a real jump of the input
//!   timestamps shows on one step only, so a per-frame comparison could never last "more than a
//!   few packets". "A few" is [`MapperConfig::jump_packets`] (default 3): the session ends on the
//!   fourth consecutive inconsistent frame.
//! - Only the direction "timestamps ahead of arrivals" counts for that rule. The other direction
//!   (arrivals late against timestamps) is delivery delay, which the jitter buffer exists to
//!   absorb; it is the depth rule's business. Counting it would end a session on every network
//!   stall longer than 500 ms although a 1 s buffer covers it.
//! - "A timestamp goes backwards" compares with the reference: an isolated wild timestamp far in
//!   the future (counted as inconsistent) does not make the next normal frame "backwards". Equal
//!   timestamps are not backwards.
//! - Buffer depth of a frame is `out − arrival` (how long it waits before it is due); the range
//!   is `[0, 2·J]`, checked on consistent frames.
//! - `live_timeout` "beyond the buffer": the session stalls when `now` passes the output time of
//!   the last consistent frame (clamped to `[arrival, arrival + 2·J]` of that frame) plus
//!   `live_timeout`. With a frame that arrived at depth `J`, that is `J + 2 s` after it arrived.
//! - Arrival times that go backwards (a caller error) are taken as equal to the latest one.

use crate::skew::SkewEstimator;
use crate::{SessionEnd, Ticks27, sat};

const PPB: i128 = 1_000_000_000;

/// Parameters of a [`Mapper`] (SPEC §5.3 defaults in [`Default`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MapperConfig {
    /// Jitter-buffer target J (default 1 s).
    pub jitter: Ticks27,
    /// Sliding window of the skew estimate (default 30 s).
    pub skew_window: Ticks27,
    /// Clamp of the skew, parts per million (default 500).
    pub skew_max_ppm: f64,
    /// Difference between timestamp step and arrival step that ends a session (default 500 ms).
    pub reset_threshold: Ticks27,
    /// How long the depth may stay outside `[0, 2J]` (default 5 s).
    pub resync_after: Ticks27,
    /// How long nothing may arrive, beyond the buffer (default 2 s).
    pub live_timeout: Ticks27,
    /// "More than a few packets": the session ends when more than this many consecutive frames
    /// exceed `reset_threshold` (default 3). [choice: the SPEC gives no number]
    pub jump_packets: u32,
    /// Time constant of the depth term (default 10 minutes; zero: no depth term). The skew
    /// estimate corrects the slope of the buffer depth, not the depth: its estimation noise
    /// integrates into a random walk. The depth term adds `(J − depth) / depth_tau` to the
    /// skew, depth being measured without network delay on the fastest frame of the newest
    /// skew bucket, so the depth returns to `J` with this time constant instead of wandering
    /// (the video analog of the audio clock's "small proportional correction on the audio
    /// buffer depth", SPEC §5.4). [choice: the SPEC leaves it open]
    pub depth_tau: Ticks27,
    /// Clamp of the depth term alone, parts per million (default 50): it stays weak beside
    /// the estimate, and the sum is clamped to `skew_max_ppm`.
    pub depth_max_ppm: f64,
}

impl Default for MapperConfig {
    fn default() -> Self {
        Self {
            jitter: Ticks27::from_secs(1),
            skew_window: Ticks27::from_secs(30),
            skew_max_ppm: 500.0,
            reset_threshold: Ticks27::from_ms(500),
            resync_after: Ticks27::from_secs(5),
            live_timeout: Ticks27::from_secs(2),
            jump_packets: 3,
            depth_tau: Ticks27::from_secs(600),
            depth_max_ppm: 50.0,
        }
    }
}

/// What the mapper made of one frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mapped {
    /// The frame belongs to the current session and is due at this output time.
    At(Ticks27),
    /// The frame (a key frame) starts a new session and is due at `at`; `ended` is why the
    /// previous session ended, if one was running.
    Started {
        /// Output time of the frame (`J` after its arrival).
        at: Ticks27,
        /// Why the previous session ended at this frame, if one was running.
        ended: Option<SessionEnd>,
    },
    /// The session ended at this frame; the frame is not a key frame and is discarded.
    Ended(SessionEnd),
    /// No session and the frame is not a key frame: discarded until the next key frame.
    Waiting,
}

impl Mapped {
    /// The output time, if the frame is to be shown.
    pub fn at(&self) -> Option<Ticks27> {
        match *self {
            Mapped::At(t) | Mapped::Started { at: t, .. } => Some(t),
            Mapped::Ended(_) | Mapped::Waiting => None,
        }
    }
    /// Why a session ended at this frame, if one did.
    pub fn ended(&self) -> Option<SessionEnd> {
        match *self {
            Mapped::Started { ended, .. } => ended,
            Mapped::Ended(e) => Some(e),
            Mapped::At(_) | Mapped::Waiting => None,
        }
    }
}

#[derive(Clone, Debug)]
struct Session {
    anchor_in: i64,
    /// Output time of `anchor_in`, in units of 10^-9 tick.
    anchor_out_n: i128,
    skew_ppb: i64,
    /// The last consistent frame: input time, arrival, output time.
    ref_in: i64,
    ref_arrival: i64,
    ref_out: i64,
    /// Consecutive frames whose timestamp ran ahead of their arrival by the threshold.
    suspects: u32,
    /// Since when the depth has been out of range.
    depth_bad_since: Option<i64>,
    skew: SkewEstimator,
}

impl Session {
    fn start(cfg: &MapperConfig, pts: i64, arrival: i64) -> (Self, i64) {
        let out = arrival.saturating_add(cfg.jitter.0);
        let mut skew = SkewEstimator::new(cfg.skew_window.0, cfg.skew_max_ppm);
        skew.sample(pts, arrival);
        (
            Self {
                anchor_in: pts,
                anchor_out_n: i128::from(out) * PPB,
                skew_ppb: 0,
                ref_in: pts,
                ref_arrival: arrival,
                ref_out: out,
                suspects: 0,
                depth_bad_since: None,
                skew,
            },
            out,
        )
    }

    fn map_n(&self, pts: i64) -> i128 {
        let d = i128::from(pts) - i128::from(self.anchor_in);
        // |d| < 2^65, factor < 2^31: the product fits i128.
        self.anchor_out_n + d * (PPB + i128::from(self.skew_ppb))
    }

    fn map(&self, pts: i64) -> i64 {
        sat(self.map_n(pts).div_euclid(PPB))
    }

    /// The depth term of the skew in parts per billion: `(J − depth) / depth_tau`, clamped to
    /// `depth_max_ppm`. `depth` is how long the fastest frame of the newest closed skew bucket
    /// waits before it is due (`out − arrival` at the transit floor: network delay only ever
    /// makes a frame later, so this is the depth the jitter buffer was set up with, `J` at the
    /// session start). A buffer shallower than `J` raises the skew (output times move later),
    /// a deeper one lowers it. Integer arithmetic: the term is exact and has no state.
    fn depth_term_ppb(&self, cfg: &MapperConfig) -> i64 {
        let tau = i128::from(cfg.depth_tau.0);
        let Some((x, d)) = self.skew.floor() else {
            return 0;
        };
        if tau <= 0 || cfg.depth_max_ppm.is_nan() || cfg.depth_max_ppm <= 0.0 {
            return 0;
        }
        let depth = i128::from(self.map(x)) - (i128::from(x) + i128::from(d));
        let err = i128::from(cfg.jitter.0) - depth;
        let max = (cfg.depth_max_ppm.min(100_000.0) * 1000.0) as i128;
        // |err| < 2^65 and PPB < 2^30: the product fits i128
        sat((err * PPB / tau).clamp(-max, max))
    }

    /// Stall deadline: output time of the last consistent frame clamped to its buffer, plus the
    /// live timeout. [choice, see the module documentation]
    fn deadline(&self, cfg: &MapperConfig) -> i64 {
        let hi = self
            .ref_arrival
            .saturating_add(cfg.jitter.0.saturating_mul(2));
        let base = self
            .ref_out
            .max(self.ref_arrival)
            .min(hi.max(self.ref_arrival));
        base.saturating_add(cfg.live_timeout.0)
    }

    fn place(&mut self, cfg: &MapperConfig, pts: i64, arrival: i64) -> Result<i64, SessionEnd> {
        let d_in = i128::from(pts) - i128::from(self.ref_in);
        // SPEC §5.3 Discontinuity: "a timestamp goes backwards".
        if d_in < 0 {
            return Err(SessionEnd::Backwards);
        }
        let d_arr = i128::from(arrival) - i128::from(self.ref_arrival);
        let out = self.map(pts);
        // SPEC §5.3 Discontinuity: timestamp step against arrival step, for more than a few
        // packets. [choice: one-sided, from the reference; see the module documentation]
        if d_in - d_arr > i128::from(cfg.reset_threshold.0) {
            self.suspects = self.suspects.saturating_add(1);
            if self.suspects > cfg.jump_packets {
                return Err(SessionEnd::Jump);
            }
            return Ok(out);
        }
        self.suspects = 0;
        self.ref_in = pts;
        self.ref_arrival = arrival;
        self.ref_out = out;
        // SPEC §5.3 Discontinuity: depth outside [0, 2J] for longer than resync_after.
        let depth = i128::from(out) - i128::from(arrival);
        let j = i128::from(cfg.jitter.0);
        if depth < 0 || depth > 2 * j {
            let since = *self.depth_bad_since.get_or_insert(arrival);
            if i128::from(arrival) - i128::from(since) > i128::from(cfg.resync_after.0) {
                return Err(SessionEnd::Depth);
            }
        } else {
            self.depth_bad_since = None;
        }
        // SPEC §5.3 Skew estimate. Re-anchor at this frame so the map stays continuous.
        if self.skew.sample(pts, arrival) {
            let ppb = self.skew.ppb().saturating_add(self.depth_term_ppb(cfg));
            let max = (cfg.skew_max_ppm.clamp(0.0, 100_000.0) * 1000.0) as i64;
            let ppb = ppb.clamp(-max, max);
            if ppb != self.skew_ppb {
                self.anchor_out_n = self.map_n(pts);
                self.anchor_in = pts;
                self.skew_ppb = ppb;
            }
        }
        Ok(out)
    }
}

/// Maps one input's timestamps to output times (SPEC §5.3).
///
/// Every time is an argument: `arrival` and `now` are readings of the carrier's STC that the
/// caller took; the mapper never reads a clock.
#[derive(Clone, Debug)]
pub struct Mapper {
    cfg: MapperConfig,
    session: Option<Session>,
    last_arrival: Option<i64>,
}

impl Mapper {
    /// A mapper with no session.
    pub fn new(cfg: MapperConfig) -> Self {
        Self {
            cfg,
            session: None,
            last_arrival: None,
        }
    }

    /// A decoded frame with input timestamp `pts` (in 27 MHz, made continuous by the caller, for
    /// example with [`crate::Unwrapper`]) arrived at `arrival`; `key` says whether it can start
    /// a session (the first decodable frame after a key frame).
    ///
    /// Evaluates the stall timeout at `arrival` first, then the discontinuity rules. A session
    /// that ends here is replaced by a new one if `key` is set.
    pub fn frame(&mut self, pts: Ticks27, arrival: Ticks27, key: bool) -> Mapped {
        let arrival = match self.last_arrival {
            Some(l) => arrival.0.max(l),
            None => arrival.0,
        };
        self.last_arrival = Some(arrival);
        let mut ended = self.poll(Ticks27(arrival)).err();
        if ended.is_none()
            && let Some(s) = &mut self.session
        {
            match s.place(&self.cfg, pts.0, arrival) {
                Ok(at) => return Mapped::At(Ticks27(at)),
                Err(e) => {
                    self.session = None;
                    ended = Some(e);
                }
            }
        }
        if key {
            // SPEC §5.3 Session start: due J after its arrival, skew 0.
            let (s, at) = Session::start(&self.cfg, pts.0, arrival);
            self.session = Some(s);
            Mapped::Started {
                at: Ticks27(at),
                ended,
            }
        } else {
            match ended {
                Some(e) => Mapped::Ended(e),
                None => Mapped::Waiting,
            }
        }
    }

    /// The connection closed: ends the session (SPEC §5.3 Discontinuity). Returns
    /// [`SessionEnd::Closed`] if a session was running.
    pub fn closed(&mut self) -> Option<SessionEnd> {
        self.session.take().map(|_| SessionEnd::Closed)
    }

    /// Evaluate the timeouts at `now` (SPEC §5.3 Gap): ends the session with
    /// [`SessionEnd::Stalled`] once `live_timeout` has passed beyond the buffer with nothing
    /// arriving. A shorter gap is reported by the frame clock as repeats, not here.
    pub fn poll(&mut self, now: Ticks27) -> Result<(), SessionEnd> {
        if let Some(s) = &self.session
            && now.0 > s.deadline(&self.cfg)
        {
            self.session = None;
            return Err(SessionEnd::Stalled);
        }
        Ok(())
    }

    /// Whether a session is running.
    pub fn in_session(&self) -> bool {
        self.session.is_some()
    }

    /// The session's current skew in parts per million (`None` without a session). Positive
    /// when the producer's clock is slow: input time is stretched to match ours.
    pub fn skew_ppm(&self) -> Option<f64> {
        self.session.as_ref().map(|s| s.skew.skew_ppm())
    }

    /// The skew the map applies, in parts per billion (the filtered skew quantized).
    pub fn skew_ppb(&self) -> Option<i64> {
        self.session.as_ref().map(|s| s.skew_ppb)
    }

    /// The latest unfiltered skew estimate in parts per million, once there is one.
    pub fn skew_estimate_ppm(&self) -> Option<f64> {
        self.session.as_ref().and_then(|s| s.skew.estimate_ppm())
    }

    /// The output time the current session gives input time `pts`, without changing anything.
    pub fn map(&self, pts: Ticks27) -> Option<Ticks27> {
        self.session.as_ref().map(|s| Ticks27(s.map(pts.0)))
    }

    /// The parameters.
    pub fn config(&self) -> &MapperConfig {
        &self.cfg
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const F30: i64 = 900_000; // 30.000 fps frame period in ticks

    fn ms(v: i64) -> Ticks27 {
        Ticks27::from_ms(v)
    }

    fn mapper() -> Mapper {
        Mapper::new(MapperConfig::default())
    }

    /// Feeds `n` frames at 30 fps from frame index `k0`: pts = pts0 + k·F30, arrival =
    /// arr0 + k·F30. Asserts every frame maps in-session.
    fn run(m: &mut Mapper, k0: i64, n: i64, pts0: i64, arr0: i64) {
        for k in k0..k0 + n {
            let r = m.frame(Ticks27(pts0 + k * F30), Ticks27(arr0 + k * F30), false);
            assert!(matches!(r, Mapped::At(_)), "frame {k}: {r:?}");
        }
    }

    /// SPEC §5.3 Session start: the first key frame is due J after its arrival; non-key frames
    /// before it are discarded; skew starts at 0.
    #[test]
    fn spec_5_3_session_start_places_first_key_frame_j_after_arrival() {
        let mut m = mapper();
        assert_eq!(m.frame(ms(0), ms(100), false), Mapped::Waiting);
        assert!(!m.in_session());
        let r = m.frame(ms(5000), ms(200), true);
        assert_eq!(
            r,
            Mapped::Started {
                at: ms(1200),
                ended: None
            }
        );
        assert_eq!(m.skew_ppm(), Some(0.0));
        // later frames keep the timestamp spacing
        assert_eq!(m.frame(ms(5040), ms(230), false), Mapped::At(ms(1240)));
        assert_eq!(m.map(ms(6000)), Some(ms(2200)));
    }

    /// Result of [`skewed`].
    struct Skewed {
        m: Mapper,
        /// Worst |depth − J| in the last 10 minutes, ticks.
        depth_err: i64,
        /// Depth max − min in the last 10 minutes (drift), ticks.
        depth_spread: i64,
        /// Worst |skew − expected| in the last 10 minutes, ppm.
        skew_err: f64,
        end: Option<SessionEnd>,
    }

    /// Simulates a producer whose clock runs at `(1 + ppm·1e-6)` of ours, 30 fps of its own
    /// timestamps, with deterministic arrival jitter uniform in `[0, jitter)` on a 50 ms transit.
    fn skewed(ppm: i64, secs: i64, jitter: u64) -> Skewed {
        let mut m = mapper();
        let (mut depth_err, mut skew_err) = (0i64, 0f64);
        let (mut dmin, mut dmax) = (i64::MAX, i64::MIN);
        let expected = -(ppm as f64) / (1.0 + ppm as f64 * 1e-6);
        let n = secs * 30;
        let mut seed = 0x1234_5678u64;
        for k in 0..n {
            // our time of frame k: k frame periods of the producer's clock
            let real = (i128::from(k * F30) * 1_000_000 / i128::from(1_000_000 + ppm)) as i64;
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let jit = if jitter > 0 { (seed >> 33) % jitter } else { 0 } as i64;
            let arrival = real + 1_350_000 + jit;
            let r = m.frame(Ticks27(k * F30), Ticks27(arrival), k == 0);
            if let Some(e) = r.ended() {
                let depth_spread = dmax.saturating_sub(dmin);
                return Skewed {
                    m,
                    depth_err,
                    depth_spread,
                    skew_err,
                    end: Some(e),
                };
            }
            if let Some(at) = r.at()
                && k > n - 30 * 600
            {
                // depth against the jitter-free arrival
                let depth = at.0 - (real + 1_350_000);
                depth_err = depth_err.max((depth - MapperConfig::default().jitter.0).abs());
                dmin = dmin.min(depth);
                dmax = dmax.max(depth);
                skew_err = skew_err.max((m.skew_ppm().unwrap_or(0.0) - expected).abs());
            }
        }
        let depth_spread = dmax.saturating_sub(dmin);
        Skewed {
            m,
            depth_err,
            depth_spread,
            skew_err,
            end: None,
        }
    }

    /// SPEC §5.3 Skew estimate: a producer 100 ppm fast is followed (skew −100 ppm), and the
    /// buffer depth stays put (without the skew it would drift 0.36 s an hour; the skew corrects
    /// the slope, and the depth term the few ms the depth moved while the first window filled).
    /// With 0–20 ms of uniform arrival jitter the estimate of a 30 s window wanders by up to
    /// 10 ppm, which moves the depth by well under a millisecond.
    #[test]
    fn spec_5_3_skew_follows_producer_100ppm_fast() {
        let r = skewed(100, 1800, 0);
        assert_eq!(r.end, None);
        assert!(r.skew_err < 0.05, "skew error {}", r.skew_err);
        // the depth term brings back the few ms the depth moved while the first window filled
        // (time constant 10 minutes): in the last 10 of the 30 minutes it is within 1 ms of J
        // and still closing in
        assert!(
            r.depth_spread < Ticks27::from_ms(1).0,
            "depth drift {}",
            r.depth_spread
        );
        assert!(
            r.depth_err < Ticks27::from_ms(1).0,
            "depth error {}",
            r.depth_err
        );
        let r = skewed(100, 3600, 540_000);
        assert_eq!(r.end, None);
        assert!(r.skew_err < 10.0, "skew error {}", r.skew_err);
        assert!(
            r.depth_spread < Ticks27::from_ms(1).0,
            "depth drift {}",
            r.depth_spread
        );
        // the first frame arrived with up to 20 ms of jitter
        assert!(
            r.depth_err < Ticks27::from_ms(25).0,
            "depth error {}",
            r.depth_err
        );
    }

    /// SPEC §5.3 Skew estimate, the other sign: a producer 100 ppm slow.
    #[test]
    fn spec_5_3_skew_follows_producer_100ppm_slow() {
        let r = skewed(-100, 1800, 0);
        assert_eq!(r.end, None);
        assert!(r.skew_err < 0.05, "skew error {}", r.skew_err);
        // the depth term brings back the few ms the depth moved while the first window filled
        // (time constant 10 minutes): in the last 10 of the 30 minutes it is within 1 ms of J
        // and still closing in
        assert!(
            r.depth_spread < Ticks27::from_ms(1).0,
            "depth drift {}",
            r.depth_spread
        );
        assert!(
            r.depth_err < Ticks27::from_ms(1).0,
            "depth error {}",
            r.depth_err
        );
        let r = skewed(-100, 1800, 540_000);
        assert_eq!(r.end, None);
        assert!(r.skew_err < 10.0, "skew error {}", r.skew_err);
        assert!(
            r.depth_spread < Ticks27::from_ms(1).0,
            "depth drift {}",
            r.depth_spread
        );
        assert!(
            r.depth_err < Ticks27::from_ms(25).0,
            "depth error {}",
            r.depth_err
        );
    }

    /// SPEC §5.3 Skew estimate: clamped to ±500 ppm. A producer 2000 ppm fast saturates the
    /// clamp; the remaining 1500 ppm fill the buffer until the depth rule ends the session
    /// (depth > 2J after about 667 s, plus resync_after).
    #[test]
    fn spec_5_3_skew_clamped_at_500ppm() {
        let r = skewed(2000, 600, 0);
        assert_eq!(r.end, None);
        assert_eq!(r.m.skew_ppb(), Some(-500_000));
        assert!(r.m.skew_ppm().unwrap() >= -500.0);
        assert_eq!(r.m.skew_estimate_ppm(), Some(-500.0));
        let r = skewed(2000, 800, 0);
        assert_eq!(r.end, Some(SessionEnd::Depth));
        // slow producer: the buffer drains at 2.5 ms/s, so look before the depth rule fires
        let r = skewed(-3000, 300, 0);
        assert_eq!(r.end, None);
        assert_eq!(r.m.skew_estimate_ppm(), Some(500.0));
        let s = r.m.skew_ppm().unwrap();
        assert!((499.0..=500.0).contains(&s), "{s}");
    }

    /// SPEC §5.3 Discontinuity: the transport connection closed.
    #[test]
    fn spec_5_3_discontinuity_connection_closed() {
        let mut m = mapper();
        assert_eq!(m.closed(), None);
        m.frame(ms(0), ms(0), true);
        assert_eq!(m.closed(), Some(SessionEnd::Closed));
        assert!(!m.in_session());
        assert_eq!(m.closed(), None);
        // The producer reconnects with timestamps from zero, at a key frame
        assert_eq!(m.frame(ms(0), ms(500), false), Mapped::Waiting);
        assert_eq!(
            m.frame(ms(0), ms(510), true),
            Mapped::Started {
                at: ms(1510),
                ended: None
            }
        );
    }

    /// SPEC §5.3 Discontinuity: a timestamp goes backwards (the producer reconnecting without our
    /// noticing the close: timestamps reset to zero). Equal timestamps are not backwards.
    #[test]
    fn spec_5_3_discontinuity_timestamp_backwards() {
        let mut m = mapper();
        m.frame(ms(100_000), ms(0), true);
        run(&mut m, 1, 30, ms(100_000).0, 0);
        // duplicate timestamp: not backwards
        assert!(matches!(
            m.frame(ms(100_000 + 1000), ms(1001), false),
            Mapped::At(_)
        ));
        assert_eq!(
            m.frame(ms(0), ms(1100), false),
            Mapped::Ended(SessionEnd::Backwards)
        );
        assert_eq!(m.frame(ms(33), ms(1133), false), Mapped::Waiting);
        assert_eq!(
            m.frame(ms(66), ms(1166), true),
            Mapped::Started {
                at: ms(2166),
                ended: None
            }
        );
        // a key frame that goes backwards ends and restarts at once
        assert_eq!(
            m.frame(ms(10), ms(1200), true),
            Mapped::Started {
                at: ms(2200),
                ended: Some(SessionEnd::Backwards)
            }
        );
    }

    /// SPEC §5.3 Discontinuity: the timestamp step exceeds the arrival step by more than
    /// reset_threshold for more than a few (3) packets: the fourth ends the session.
    #[test]
    fn spec_5_3_discontinuity_timestamp_jump_for_more_than_a_few_packets() {
        let mut m = mapper();
        m.frame(Ticks27(0), Ticks27(0), true);
        run(&mut m, 1, 59, 0, 0);
        // timestamps jump 10 s ahead while arrivals continue
        let jump = Ticks27::from_secs(10).0;
        for k in 60..63 {
            let r = m.frame(Ticks27(jump + k * F30), Ticks27(k * F30), false);
            assert!(matches!(r, Mapped::At(_)), "{k}: {r:?}");
        }
        assert_eq!(
            m.frame(Ticks27(jump + 63 * F30), Ticks27(63 * F30), false),
            Mapped::Ended(SessionEnd::Jump)
        );
        // with a key frame the fourth frame starts the next session
        let mut m = mapper();
        m.frame(Ticks27(0), Ticks27(0), true);
        run(&mut m, 1, 59, 0, 0);
        for k in 60..63 {
            m.frame(Ticks27(jump + k * F30), Ticks27(k * F30), false);
        }
        assert_eq!(
            m.frame(Ticks27(jump + 63 * F30), Ticks27(63 * F30), true),
            Mapped::Started {
                at: Ticks27(63 * F30 + Ticks27::from_secs(1).0),
                ended: Some(SessionEnd::Jump)
            }
        );
    }

    /// SPEC §5.3 Discontinuity, "for more than a few packets": one wild timestamp (or three) does
    /// not end the session, nor does a jump below reset_threshold.
    #[test]
    fn spec_5_3_discontinuity_jump_needs_more_than_a_few_packets_and_the_threshold() {
        let mut m = mapper();
        m.frame(Ticks27(0), Ticks27(0), true);
        run(&mut m, 1, 59, 0, 0);
        let wild = Ticks27::from_secs(3600).0;
        for k in 60..63 {
            assert!(matches!(
                m.frame(Ticks27(wild), Ticks27(k * F30), false),
                Mapped::At(_)
            ));
        }
        // normal timestamps again: neither backwards nor a jump
        run(&mut m, 63, 60, 0, 0);
        // a forward jump of 400 ms (< 500 ms) is a consistent step
        run(&mut m, 123, 60, Ticks27::from_ms(400).0, 0);
        assert!(m.in_session());
    }

    /// SPEC §5.3 and the one-sided reading of the step rule: late arrivals (a network stall of
    /// 800 ms, then a burst) are absorbed by the buffer and do not end the session.
    #[test]
    fn spec_5_3_network_stall_with_burst_does_not_end_session() {
        let mut m = mapper();
        m.frame(Ticks27(0), Ticks27(0), true);
        run(&mut m, 1, 59, 0, 0);
        let stall_end = 60 * F30 + Ticks27::from_ms(800).0;
        for k in 60..90 {
            // frames 60..84 arrive together at the end of the stall
            let arrival = (k * F30).max(stall_end);
            let r = m.frame(Ticks27(k * F30), Ticks27(arrival), false);
            assert!(matches!(r, Mapped::At(_)), "{k}: {r:?}");
        }
        assert!(m.in_session());
    }

    /// SPEC §5.3 Discontinuity: the depth leaves [0, 2J] for longer than resync_after.
    #[test]
    fn spec_5_3_discontinuity_depth_out_of_range_for_resync_after() {
        // the transit grows by 1.5 s for good: depth J − 1.5 s < 0
        let mut m = mapper();
        m.frame(Ticks27(0), Ticks27(0), true);
        run(&mut m, 1, 59, 0, 0);
        let late = Ticks27::from_ms(1500).0;
        let mut ended_at = None;
        for k in 60..600 {
            let r = m.frame(Ticks27(k * F30), Ticks27(k * F30 + late), false);
            if let Some(e) = r.ended() {
                assert_eq!(e, SessionEnd::Depth);
                ended_at = Some(k);
                break;
            }
        }
        // 5 s at 30 fps after frame 60, exclusive
        assert_eq!(ended_at, Some(60 + 151));
        // out of range for less than resync_after: no end
        let mut m = mapper();
        m.frame(Ticks27(0), Ticks27(0), true);
        run(&mut m, 1, 59, 0, 0);
        for k in 60..180 {
            let r = m.frame(Ticks27(k * F30), Ticks27(k * F30 + late), false);
            assert!(matches!(r, Mapped::At(_)));
        }
        run(&mut m, 180, 300, 0, 0);
        assert!(m.in_session());
    }

    /// SPEC §5.3 Gap: a gap shorter than the live timeout (beyond the buffer) does not end the
    /// session; a longer one ends it with Stalled, reported once.
    #[test]
    fn spec_5_3_gap_shorter_than_live_timeout_keeps_session() {
        let mut m = mapper();
        m.frame(Ticks27(0), Ticks27(0), true);
        run(&mut m, 1, 59, 0, 0);
        let last = 59 * F30;
        // buffer J (1 s) + live_timeout (2 s) after the last arrival
        for dt in [100, 1000, 2000, 2900, 3000] {
            assert_eq!(m.poll(Ticks27(last + Ticks27::from_ms(dt).0)), Ok(()));
        }
        // input resumes after a 2.9 s idle period with consistent timestamps
        let gap = Ticks27::from_ms(2900).0;
        run(&mut m, 60, 30, gap, gap);
        let last = 89 * F30 + gap;
        assert_eq!(
            m.poll(Ticks27(last + Ticks27::from_ms(3001).0)),
            Err(SessionEnd::Stalled)
        );
        assert_eq!(m.poll(Ticks27(last + Ticks27::from_ms(4000).0)), Ok(()));
        assert!(!m.in_session());
    }

    /// SPEC §5.3 Gap: a frame after the live timeout, without a poll in between, ends the old
    /// session with Stalled (the producer wakes after an idle period with timestamps jumped ahead).
    #[test]
    fn spec_5_3_gap_longer_than_live_timeout_ends_session_at_next_frame() {
        let mut m = mapper();
        m.frame(Ticks27(0), Ticks27(0), true);
        run(&mut m, 1, 59, 0, 0);
        let idle = Ticks27::from_secs(60).0;
        assert_eq!(
            m.frame(
                Ticks27(60 * F30 + idle + 7),
                Ticks27(60 * F30 + idle),
                false
            ),
            Mapped::Ended(SessionEnd::Stalled)
        );
        let r = m.frame(Ticks27(61 * F30 + idle + 7), Ticks27(61 * F30 + idle), true);
        assert_eq!(
            r,
            Mapped::Started {
                at: Ticks27(61 * F30 + idle + Ticks27::from_secs(1).0),
                ended: None
            }
        );
    }

    /// Arrival times that go backwards are taken as the latest one; no panic, no end.
    #[test]
    fn arrival_going_backwards_is_clamped() {
        let mut m = mapper();
        m.frame(Ticks27(0), Ticks27(1_000_000), true);
        let r = m.frame(Ticks27(F30), Ticks27(0), false);
        assert!(matches!(r, Mapped::At(_)));
    }

    /// No function takes a clock: the same inputs give the same outputs.
    #[test]
    fn no_clock_same_inputs_same_outputs() {
        let go = || {
            let r = skewed(37, 200, 540_000);
            (r.m.skew_ppb(), r.depth_err, r.end)
        };
        assert_eq!(go(), go());
    }
}
