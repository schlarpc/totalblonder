//! The pacer (SPEC §4.3): one thread per output interface sends every carrier's datagrams at
//! their slot times, and owns time.
//!
//! - Datagram `k` of a carrier holds slots `k·p … k·p + p − 1` (`p` packets per datagram) and is
//!   due at `epoch + clock.nanos(k·p)`, computed from the index, never accumulated.
//! - The mux thread produces [`Block`]s ahead of time into a bounded queue. When a datagram is
//!   due and its block has not arrived, the pacer sends `p` null packets in its place and moves
//!   on; the mux thread reads the pacer's position ([`Position`]) and skips the slots it
//!   missed. Every PCR therefore stays exact even when the mux is late; only payload is
//!   delayed, and the event is counted.
//! - The pacer never waits for the mux and the mux never waits for the network. A send error
//!   is counted and the datagram is lost; time is never made up by bursting.
//! - Sleeping: until `wake_early` before the earliest deadline, then everything due within
//!   `batch` is sent. Lateness is measured and kept as a histogram.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, TrySendError, sync_channel};
use std::time::{Duration, Instant};

use tb_mux::{Mux, SlotClock};
use tb_ts::{PACKET_LEN, Packet};

use crate::frame::{Framing, MAX_DATAGRAM, PerDatagram, datagram};
use crate::sink::Sink;

/// `p` consecutive packets of one carrier, starting at slot `first`.
#[derive(Clone)]
pub struct Block {
    /// The first slot.
    pub first: u64,
    /// The packets (only the first `p` are used).
    pub packets: [[u8; PACKET_LEN]; 7],
}

/// The pacer's position on a carrier: the next slot it will send. Shared with the mux thread.
#[derive(Debug, Default)]
pub struct Position(AtomicU64);

impl Position {
    /// The next slot the pacer will send.
    pub fn get(&self) -> u64 {
        self.0.load(Ordering::Acquire)
    }
    fn set(&self, v: u64) {
        self.0.store(v, Ordering::Release);
    }
}

/// Counters of one carrier's output, shared for metrics.
#[derive(Debug, Default)]
pub struct TxStats {
    /// Datagrams sent with the mux's content.
    pub sent: AtomicU64,
    /// Datagrams sent as nulls because the mux was late.
    pub underruns: AtomicU64,
    /// Blocks that arrived after their time and were discarded.
    pub stale: AtomicU64,
    /// Send errors.
    pub send_errors: AtomicU64,
    /// Lateness histogram: < 50 µs, < 100, < 250, < 500, < 1000, < 5000, ≥ 5000.
    pub late_hist: [AtomicU64; 7],
    /// Largest lateness seen, ns.
    pub late_max_ns: AtomicU64,
}

const LATE_EDGES_US: [u64; 6] = [50, 100, 250, 500, 1000, 5000];

impl TxStats {
    fn late(&self, ns: u64) {
        let us = ns / 1000;
        let i = LATE_EDGES_US.iter().position(|&e| us < e).unwrap_or(6);
        self.late_hist[i].fetch_add(1, Ordering::Relaxed);
        self.late_max_ns.fetch_max(ns, Ordering::Relaxed);
    }
}

/// The mux side of one carrier's output: produces blocks ahead of the pacer.
pub struct Feeder {
    tx: SyncSender<Block>,
    pos: Arc<Position>,
    per: usize,
    lead_slots: u64,
}

/// What a carrier's output needs on the pacer side.
pub struct CarrierTx {
    rx: Receiver<Block>,
    pending: Option<Block>,
    sink: Box<dyn Sink>,
    framing: Framing,
    per: usize,
    clock: SlotClock,
    next: u64,
    seq: u16,
    pos: Arc<Position>,
    /// Counters.
    pub stats: Arc<TxStats>,
    null: [[u8; PACKET_LEN]; 7],
}

/// Makes the two halves of one carrier's output. `lead` is how far ahead of the pacer the mux
/// keeps the queue filled (SPEC §6.1: 20 ms).
pub fn carrier_output(
    clock: SlotClock,
    framing: Framing,
    per: PerDatagram,
    lead: Duration,
    sink: Box<dyn Sink>,
    start: u64,
) -> (Feeder, CarrierTx) {
    let per = per.get();
    let slots_per_sec = clock.rate().bps() / (PACKET_LEN as u64 * 8);
    let lead_slots = (u128::from(slots_per_sec) * lead.as_nanos() / 1_000_000_000) as u64;
    let lead_slots = lead_slots.max(per as u64 * 4);
    let cap = (lead_slots / per as u64 + 64) as usize;
    let (tx, rx) = sync_channel(cap);
    // The first datagram: `start` rounded down to a datagram boundary.
    let start = start - start % per as u64;
    let pos = Arc::new(Position::default());
    pos.set(start);
    let stats = Arc::new(TxStats::default());
    (
        Feeder {
            tx,
            pos: pos.clone(),
            per,
            lead_slots,
        },
        CarrierTx {
            rx,
            pending: None,
            sink,
            framing,
            per,
            clock,
            next: start,
            seq: 0,
            pos,
            stats,
            null: [Packet::null().0; 7],
        },
    )
}

impl Feeder {
    /// Fills the queue up to `lead` ahead of the pacer, skipping the mux over slots the pacer
    /// has already sent as nulls. Returns the number of blocks produced.
    pub fn pump(&mut self, mux: &mut Mux) -> usize {
        self.pump_tap(mux, &mut |_| {})
    }

    /// As [`Feeder::pump`], showing every packet produced to `tap` (the self-check of SPEC
    /// §11.2 sits here: after the mux, before the pacer).
    pub fn pump_tap(&mut self, mux: &mut Mux, tap: &mut dyn FnMut(&[u8; PACKET_LEN])) -> usize {
        let at = self.pos.get();
        // Keep the mux aligned on datagram boundaries.
        if mux.stats().slots == 0 {
            mux.start_at(at);
        } else if mux.slot() < at {
            mux.skip_to(at);
        }
        let mut made = 0;
        while mux.slot() < at + self.lead_slots {
            let mut b = Block {
                first: mux.slot(),
                packets: [[0u8; PACKET_LEN]; 7],
            };
            for p in b.packets.iter_mut().take(self.per) {
                *p = mux.next_slot().1.0;
                tap(p);
            }
            match self.tx.try_send(b) {
                Ok(()) => made += 1,
                // The pacer is not draining (stopped or far behind): drop, it will send nulls.
                Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => break,
            }
        }
        made
    }

    /// The pacer's position.
    pub fn position(&self) -> u64 {
        self.pos.get()
    }
}

impl CarrierTx {
    fn deadline(&self, epoch: Instant) -> Instant {
        let ns = self.clock.nanos(self.next);
        epoch + Duration::from_nanos(u64::try_from(ns).unwrap_or(u64::MAX))
    }

    fn send_one(&mut self, epoch: Instant, now: Instant) {
        let due = self.deadline(epoch);
        self.stats
            .late(now.saturating_duration_since(due).as_nanos() as u64);
        // Discard blocks for slots already gone; find this datagram's block.
        loop {
            if self.pending.is_none() {
                match self.rx.try_recv() {
                    Ok(b) => self.pending = Some(b),
                    Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
                }
            }
            match &self.pending {
                Some(b) if b.first < self.next => {
                    self.pending = None;
                    self.stats.stale.fetch_add(1, Ordering::Relaxed);
                }
                _ => break,
            }
        }
        let use_block = self.pending.as_ref().is_some_and(|b| b.first == self.next);
        let ts90k = (self.clock.stc(self.next).0 / 300) as u32;
        let mut out = [0u8; MAX_DATAGRAM];
        let n = if use_block {
            let b = self.pending.take();
            let packets = b.as_ref().map_or(&self.null, |b| &b.packets);
            datagram(
                self.framing,
                self.seq,
                ts90k,
                &packets[..self.per],
                &mut out,
            )
        } else {
            self.stats.underruns.fetch_add(1, Ordering::Relaxed);
            datagram(
                self.framing,
                self.seq,
                ts90k,
                &self.null[..self.per],
                &mut out,
            )
        };
        match self.sink.send(&out[..n]) {
            Ok(()) => {
                if use_block {
                    self.stats.sent.fetch_add(1, Ordering::Relaxed);
                }
            }
            Err(_) => {
                self.stats.send_errors.fetch_add(1, Ordering::Relaxed);
            }
        }
        self.seq = self.seq.wrapping_add(1);
        self.next += self.per as u64;
        self.pos.set(self.next);
    }
}

/// Timing of the pacer loop.
#[derive(Debug, Clone, Copy)]
pub struct PacerTiming {
    /// Wake this long before the earliest deadline.
    pub wake_early: Duration,
    /// Send everything due within this window of now.
    pub batch: Duration,
}

impl Default for PacerTiming {
    fn default() -> Self {
        PacerTiming {
            wake_early: Duration::from_micros(60),
            batch: Duration::from_micros(100),
        }
    }
}

/// Runs the pacer for `carriers` until `stop` is set. `epoch` is the instant of slot 0 of every
/// carrier. Flushes the sinks on return.
pub fn run(mut carriers: Vec<CarrierTx>, epoch: Instant, timing: PacerTiming, stop: &AtomicBool) {
    while !stop.load(Ordering::Relaxed) {
        let Some(earliest) = carriers.iter().map(|c| c.deadline(epoch)).min() else {
            return;
        };
        let now = Instant::now();
        if earliest > now + timing.wake_early {
            // Never sleep long, so `stop` is seen promptly.
            let d = (earliest - now - timing.wake_early).min(Duration::from_millis(5));
            std::thread::sleep(d);
            continue;
        }
        let horizon = Instant::now() + timing.batch;
        for c in &mut carriers {
            while c.deadline(epoch) <= horizon {
                c.send_one(epoch, Instant::now());
            }
        }
    }
    for c in &mut carriers {
        let _ = c.sink.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct Collect(Arc<Mutex<Vec<Vec<u8>>>>);
    impl Sink for Collect {
        fn send(&mut self, d: &[u8]) -> std::io::Result<()> {
            if let Ok(mut v) = self.0.lock() {
                v.push(d.to_vec());
            }
            Ok(())
        }
    }

    fn mux() -> Mux {
        Mux::new(tb_mux::MuxConfig {
            rate: tb_mux::TsRate::new(10_000_000).unwrap(),
            stc0: tb_ts::Stc(0),
            programs: vec![],
            periods: tb_mux::Periods::default(),
            stt: None,
            queue_limit: 8,
            token_depth: 2,
        })
        .unwrap()
    }

    #[test]
    fn paced_output_keeps_up_and_counts() {
        let got = Arc::new(Mutex::new(Vec::new()));
        let mut m = mux();
        let (mut feed, tx) = carrier_output(
            *m.clock(),
            Framing::Rtp { ssrc: 1 },
            PerDatagram::SEVEN,
            Duration::from_millis(20),
            Box::new(Collect(got.clone())),
            0,
        );
        let stats = tx.stats.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let epoch = Instant::now() + Duration::from_millis(5);
        let s2 = stop.clone();
        let h = std::thread::spawn(move || run(vec![tx], epoch, PacerTiming::default(), &s2));
        let t_end = Instant::now() + Duration::from_millis(300);
        while Instant::now() < t_end {
            feed.pump(&mut m);
            std::thread::sleep(Duration::from_millis(1));
        }
        stop.store(true, Ordering::Relaxed);
        h.join().unwrap();
        let v = got.lock().unwrap();
        // 10 Mbit/s = 6649 packets/s = 950 datagrams/s; about 0.295 s of them.
        assert!(v.len() > 200 && v.len() < 320, "{}", v.len());
        // Sequence numbers consecutive; RTP timestamps increase.
        for w in v.windows(2) {
            let s0 = u16::from_be_bytes([w[0][2], w[0][3]]);
            let s1 = u16::from_be_bytes([w[1][2], w[1][3]]);
            assert_eq!(s1, s0.wrapping_add(1));
        }
        assert_eq!(stats.send_errors.load(Ordering::Relaxed), 0);
        let sent = stats.sent.load(Ordering::Relaxed);
        assert!(
            sent as usize >= v.len() - 5,
            "only {sent} of {} from the mux",
            v.len()
        );
    }

    #[test]
    fn a_late_mux_is_covered_by_nulls_and_skipped() {
        let got = Arc::new(Mutex::new(Vec::new()));
        let mut m = mux();
        let (mut feed, tx) = carrier_output(
            *m.clock(),
            Framing::Udp,
            PerDatagram::SEVEN,
            Duration::from_millis(5),
            Box::new(Collect(got.clone())),
            0,
        );
        let stats = tx.stats.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let epoch = Instant::now();
        let s2 = stop.clone();
        let h = std::thread::spawn(move || run(vec![tx], epoch, PacerTiming::default(), &s2));
        // The mux thread fills the queue once, then stalls for 50 ms.
        feed.pump(&mut m);
        std::thread::sleep(Duration::from_millis(50));
        // The pump skipped the mux to where the pacer is (the pacer keeps moving, so only the
        // counter is a stable witness).
        feed.pump(&mut m);
        for _ in 0..30 {
            feed.pump(&mut m);
            std::thread::sleep(Duration::from_millis(1));
        }
        stop.store(true, Ordering::Relaxed);
        h.join().unwrap();
        // 50 ms at 950 datagrams/s with a 5 ms lead: tens of datagrams sent as nulls; at least
        // one even on a loaded machine.
        assert!(stats.underruns.load(Ordering::Relaxed) > 0);
        assert!(m.stats().skipped > 0);
    }
}
