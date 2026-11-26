//! One TS input over UDP: datagrams in, [`TsEvent`]s out, with sessions.
//!
//! **Transport detection** ([`Transport::Auto`]): a datagram that starts with the sync byte
//! 0x47 and is a whole number of 188-byte packets is raw TS; one that parses as RTP version 2
//! (its first byte is 0x80–0xBF, so the two cannot be confused) with the configured payload
//! type is RTP. The first datagram decides; later datagrams of the other kind are counted and
//! dropped.
//!
//! **Sessions.** A session starts when the demuxer emits `Streams` and ends with exactly one
//! `End`:
//! - `End(Transport("no data for ..."))` after [`TsInputConfig::idle_timeout`] without a
//!   datagram (checked in [`TsInput::on_tick`] and on arrival);
//! - `End(Transport("RTP source changed"))` when the RTP receiver starts a new session (a new
//!   SSRC, or a confirmed sequence restart): the demuxer is reset and waits for a PAT and PMT;
//! - `End(Closed)` from [`TsInput::finish`] (the driver stops the input).
//!
//! Partial PES packets are released before `End`.

use std::time::Duration;

use tb_media::{EndReason, InputEvent};

use crate::demux::{DemuxConfig, DemuxStats, Demuxer, TsEvent};
use crate::packet::{PACKET_SIZE, SYNC};
use crate::rtp::{RtpConfig, RtpOutput, RtpReceiver, RtpStats};

/// How datagrams carry the transport stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Transport {
    /// Decide from the first datagram.
    #[default]
    Auto,
    /// Raw TS, whole packets per datagram.
    RawUdp,
    /// RTP (RFC 2250).
    Rtp,
}

/// Settings of a TS input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TsInputConfig {
    /// Demuxer settings.
    pub demux: DemuxConfig,
    /// RTP receiver settings.
    pub rtp: RtpConfig,
    /// Transport.
    pub transport: Transport,
    /// A session ends after this long with no datagram.
    pub idle_timeout: Duration,
}

impl Default for TsInputConfig {
    fn default() -> Self {
        Self {
            demux: DemuxConfig::default(),
            rtp: RtpConfig::default(),
            transport: Transport::Auto,
            idle_timeout: Duration::from_secs(5),
        }
    }
}

/// Counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TsInputStats {
    /// Datagrams received.
    pub datagrams: u64,
    /// Datagrams of neither (or the other) transport.
    pub rejected: u64,
    /// The demuxer's counters.
    pub demux: DemuxStats,
    /// The RTP receiver's counters.
    pub rtp: RtpStats,
    /// Sessions ended.
    pub sessions_ended: u64,
}

/// A TS input over UDP.
#[derive(Debug, Clone)]
pub struct TsInput {
    config: TsInputConfig,
    transport: Transport,
    rtp: RtpReceiver,
    demux: Demuxer,
    last_rx: Option<Duration>,
    datagrams: u64,
    rejected: u64,
    ended: u64,
}

fn is_raw_ts(d: &[u8]) -> bool {
    !d.is_empty() && d[0] == SYNC && d.len().is_multiple_of(PACKET_SIZE)
}

impl TsInput {
    /// A new input.
    pub fn new(config: TsInputConfig) -> Self {
        Self {
            config,
            transport: config.transport,
            rtp: RtpReceiver::new(config.rtp),
            demux: Demuxer::new(config.demux),
            last_rx: None,
            datagrams: 0,
            rejected: 0,
            ended: 0,
        }
    }

    /// The transport in use (decided by the first datagram when configured `Auto`).
    pub fn transport(&self) -> Transport {
        self.transport
    }

    /// Counters.
    pub fn stats(&self) -> TsInputStats {
        TsInputStats {
            datagrams: self.datagrams,
            rejected: self.rejected,
            demux: self.demux.stats(),
            rtp: self.rtp.stats(),
            sessions_ended: self.ended,
        }
    }

    /// A datagram arrived at `now`.
    pub fn on_datagram(&mut self, now: Duration, d: &[u8]) -> Vec<TsEvent> {
        let mut ev = Vec::new();
        self.check_idle(now, &mut ev);
        self.datagrams += 1;
        self.last_rx = Some(now);
        if self.transport == Transport::Auto {
            if is_raw_ts(d) {
                self.transport = Transport::RawUdp;
            } else if crate::rtp::RtpPacket::parse(d)
                .is_ok_and(|p| p.payload_type == self.config.rtp.payload_type)
            {
                self.transport = Transport::Rtp;
            }
        }
        match self.transport {
            Transport::RawUdp if is_raw_ts(d) => self.demux.push_packets(d),
            Transport::Rtp => {
                let mut out = Vec::new();
                self.rtp.on_datagram(now, d, &mut out);
                self.rtp_output(out, &mut ev);
            }
            _ => self.rejected += 1,
        }
        ev.extend(self.demux.take_events());
        ev
    }

    /// Time passes: releases RTP packets held too long and ends an idle session.
    pub fn on_tick(&mut self, now: Duration) -> Vec<TsEvent> {
        let mut ev = Vec::new();
        if self.transport == Transport::Rtp {
            let mut out = Vec::new();
            self.rtp.on_tick(now, &mut out);
            self.rtp_output(out, &mut ev);
            ev.extend(self.demux.take_events());
        }
        self.check_idle(now, &mut ev);
        ev
    }

    /// The input is stopped: release what is held and end the session.
    pub fn finish(&mut self, now: Duration) -> Vec<TsEvent> {
        let mut ev = Vec::new();
        if self.transport == Transport::Rtp {
            // Everything still held is released: nothing more will fill the gaps.
            let mut out = Vec::new();
            self.rtp.on_tick(now + self.config.rtp.max_delay, &mut out);
            self.rtp_output(out, &mut ev);
        }
        self.end(EndReason::Closed, &mut ev);
        ev
    }

    fn rtp_output(&mut self, out: Vec<RtpOutput>, ev: &mut Vec<TsEvent>) {
        for o in out {
            match o {
                RtpOutput::NewSession { ssrc } => {
                    if self.rtp.stats().sessions > 1 {
                        let why = format!("RTP source changed (SSRC {ssrc:#010x})");
                        self.end(EndReason::Transport(why), ev);
                    }
                }
                RtpOutput::Payload { data, .. } => {
                    // Loss shows up as continuity errors in the demuxer, per PID.
                    self.demux.push_packets(&data);
                    ev.extend(self.demux.take_events());
                }
            }
        }
    }

    fn check_idle(&mut self, now: Duration, ev: &mut Vec<TsEvent>) {
        if let Some(t) = self.last_rx
            && now.saturating_sub(t) >= self.config.idle_timeout
        {
            self.last_rx = None;
            let why = format!("no data for {:?}", self.config.idle_timeout);
            self.end(EndReason::Transport(why), ev);
        }
    }

    /// Flush, emit `End` if a session is open, and reset the demuxer.
    fn end(&mut self, reason: EndReason, ev: &mut Vec<TsEvent>) {
        self.demux.flush();
        ev.extend(self.demux.take_events());
        if self.demux.started() {
            ev.push(TsEvent {
                event: InputEvent::End(reason),
                after_loss: false,
            });
            self.ended += 1;
        }
        self.demux.reset();
    }
}
