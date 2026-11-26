//! The server session driven by a synthetic publisher: the command flow, the events, replies,
//! acceptance and rejection, timeouts, bounds, and split and corruption invariance.
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

mod common;

use std::time::Duration;

use common::*;
use tb_media::{CodecId, EndReason, InputEvent, StreamIndex};
use tb_rtmp::amf0::{self, Amf0Value};
use tb_rtmp::chunk::{ChunkDecoder, Limits};
use tb_rtmp::message::{Command, UserControl};
use tb_rtmp::{
    AllowAll, AllowList, CloseReason, Message, MessageType, Request, ServerConfig, ServerSession,
};

/// Decode what the server sent (after S0/S1/S2) into messages.
fn server_messages(out: &[u8]) -> Vec<Message> {
    let mut d = ChunkDecoder::new(Limits::default());
    let mut m = Vec::new();
    d.push(&out[1 + 2 * 1536..], &mut m).unwrap();
    m
}

fn commands(msgs: &[Message]) -> Vec<Command> {
    msgs.iter()
        .filter(|m| m.type_id == MessageType::COMMAND_AMF0)
        .map(|m| Command::parse(&m.payload, false, amf0::Limits::default()).unwrap())
        .collect()
}

/// Runs and keeps everything the server sent.
fn run_keep_output<P: tb_rtmp::PublishPolicy>(
    policy: P,
    bytes: &[u8],
) -> (Vec<InputEvent>, Vec<u8>, ServerSession<P>) {
    let mut s = ServerSession::new(ServerConfig::default(), policy, Duration::ZERO);
    let ev = s.on_bytes(Duration::ZERO, bytes);
    let out = s.pending_output().to_vec();
    (ev, out, s)
}

#[test]
fn publish_flow_events_and_replies() {
    let bytes = synthetic_session(0);
    let (ev, out, s) = run_keep_output(AllowAll, &bytes);
    assert_eq!(s.close_reason(), Some(&CloseReason::Finished));

    // Replies: connect's _result, createStream's _result with stream 1, Publish.Start.
    let msgs = server_messages(&out);
    let types: Vec<u8> = msgs.iter().map(|m| m.type_id.0).collect();
    assert_eq!(&types[..3], &[5, 6, 1], "WAS, SPB, SCS before _result");
    let cmds = commands(&msgs);
    let names: Vec<&str> = cmds.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(
        names,
        ["_result", "_result", "_result", "_result", "onStatus"]
    );
    assert_eq!(
        cmds[0].args[0].get("code").and_then(Amf0Value::as_str),
        Some("NetConnection.Connect.Success")
    );
    assert_eq!(cmds[3].args[0], Amf0Value::Number(1.0));
    assert_eq!(
        cmds[4].args[0].get("code").and_then(Amf0Value::as_str),
        Some("NetStream.Publish.Start")
    );
    assert!(msgs.iter().any(|m| m.type_id == MessageType::USER_CONTROL
        && UserControl::parse(&m.payload)
            == Some(UserControl::StreamBegin(tb_rtmp::MessageStreamId(1)))));

    // Events.
    let InputEvent::Streams(streams) = &ev[0] else {
        panic!("first event {:?}", ev[0])
    };
    assert_eq!(streams.len(), 2);
    assert_eq!(streams[0].codec, CodecId::H264);
    assert_eq!(streams[0].video.width, Some(640));
    assert_eq!(streams[1].codec, CodecId::Aac);
    assert_eq!(streams[1].extradata, &AAC_SEQ[2..]);
    assert_eq!(streams[1].audio.sample_rate, Some(44100));
    assert_eq!(ev.last(), Some(&InputEvent::End(EndReason::Closed)));
    let per = check_shape(&ev);
    let video = &per[&StreamIndex(0)];
    assert_eq!(video.len(), 90);
    let keys: Vec<usize> = (0..90).filter(|&i| video[i].keyframe).collect();
    assert_eq!(keys, [0, 30, 60]);
    for (i, p) in video.iter().enumerate() {
        let dts = i as i64 * 33;
        assert_eq!(p.dts, Some(dts));
        assert_eq!(p.pts, Some(dts + if i % 3 == 1 { 66 } else { 0 }));
    }
    let audio = &per[&StreamIndex(1)];
    assert!(audio.len() > 120);
    assert!(audio.windows(2).all(|w| w[0].dts < w[1].dts));
}

#[test]
fn timestamps_unwrap_across_2_pow_32_with_extended_fields() {
    let base = 0xFFFF_FC00u32;
    let (ev, _, _) = run_keep_output(AllowAll, &synthetic_session(base));
    let per = check_shape(&ev);
    let video = &per[&StreamIndex(0)];
    assert_eq!(video.len(), 90);
    for (i, p) in video.iter().enumerate() {
        assert_eq!(p.dts, Some(i64::from(base) + i as i64 * 33));
    }
    assert!(video[89].dts.unwrap() > 1 << 32);
}

/// A small session, so that every split point can be tried.
fn small_session() -> Vec<u8> {
    let mut p = Publisher::new();
    p.start("live", "key");
    p.metadata(vec![("videocodecid", Amf0Value::Number(7.0))]);
    p.media(true, 0, avc_seq(0));
    for i in 0..12u32 {
        p.media(
            true,
            0x00FF_FFF0 + i * 33,
            avc_frame(i % 6 == 0, 33, 20 + i as usize * 30),
        );
    }
    p.media(true, 0x00FF_FFF0 + 12 * 33, avc_seq(1));
    p.media(true, 0x00FF_FFF0 + 13 * 33, avc_frame(true, 0, 10));
    p.finish();
    std::mem::take(&mut p.bytes)
}

#[test]
fn every_split_point_gives_the_same_events() {
    let bytes = small_session();
    let (whole, _) = run(ServerConfig::default(), AllowAll, [bytes.clone()]);
    // Two Streams (the sequence header changes), 13 packets, End.
    assert_eq!(
        whole
            .iter()
            .filter(|e| matches!(e, InputEvent::Streams(_)))
            .count(),
        2
    );
    assert_eq!(whole.len(), 2 + 13 + 1);
    for k in 0..=bytes.len() {
        let (ev, _) = run(
            ServerConfig::default(),
            AllowAll,
            [bytes[..k].to_vec(), bytes[k..].to_vec()],
        );
        assert_eq!(ev, whole, "split at {k}");
    }
    // Byte by byte.
    let (ev, _) = run(
        ServerConfig::default(),
        AllowAll,
        bytes.iter().map(|b| vec![*b]),
    );
    assert_eq!(ev, whole);
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

#[test]
fn random_partitions_give_the_same_events() {
    let bytes = synthetic_session(7);
    let (whole, _) = run(ServerConfig::default(), AllowAll, [bytes.clone()]);
    let mut rng = Rng(0x1234_5678);
    for _ in 0..50 {
        let mut chunks = Vec::new();
        let mut at = 0;
        while at < bytes.len() {
            let n = 1 + rng.below(3000).min(bytes.len() - at - 1);
            chunks.push(bytes[at..at + n].to_vec());
            at += n;
        }
        let (ev, _) = run(ServerConfig::default(), AllowAll, chunks);
        assert_eq!(ev, whole);
    }
}

#[test]
fn rejection_by_allow_list_and_closure() {
    let bytes = synthetic_session(0);
    // Wrong key.
    let list = AllowList::new().allow("live", "secret");
    let (ev, out, s) = run_keep_output(list, &bytes);
    assert!(ev.is_empty());
    assert!(matches!(s.close_reason(), Some(CloseReason::Rejected(_))));
    let cmds = commands(&server_messages(&out));
    let last = cmds.last().unwrap();
    assert_eq!(last.name, "onStatus");
    assert_eq!(
        last.args[0].get("level").and_then(Amf0Value::as_str),
        Some("error")
    );
    assert_eq!(
        last.args[0].get("code").and_then(Amf0Value::as_str),
        Some("NetStream.Publish.BadName")
    );
    // Wrong application.
    let list = AllowList::new().allow_app("other");
    let (ev, out, s) = run_keep_output(list, &bytes);
    assert!(ev.is_empty());
    assert!(matches!(s.close_reason(), Some(CloseReason::Rejected(_))));
    let cmds = commands(&server_messages(&out));
    assert_eq!(cmds.len(), 1);
    assert_eq!(cmds[0].name, "_error");
    assert_eq!(
        cmds[0].args[0].get("code").and_then(Amf0Value::as_str),
        Some("NetConnection.Connect.Rejected")
    );
    // The right key, and a closure that sees the query string.
    let (ev, _, _) = run_keep_output(AllowList::new().allow("live", "key"), &bytes);
    assert!(matches!(ev[0], InputEvent::Streams(_)));
    let mut seen = Vec::new();
    let mut p = Publisher::new();
    p.start("live", "key?token=abc");
    let policy = |r: &Request<'_>| {
        if let Request::Publish { key, query, .. } = r {
            seen.push((key.to_string(), query.map(str::to_string)));
        }
        true
    };
    let (_, _, s) = run_keep_output(policy, &p.bytes);
    assert_eq!(s.published(), Some(("live", "key")));
    drop(s);
    assert_eq!(seen, [("key".to_string(), Some("token=abc".to_string()))]);
}

#[test]
fn ping_is_answered_and_acks_follow_the_window() {
    let mut p = Publisher::new();
    p.connect("live");
    p.msg(UserControl::PingRequest(1234).to_message());
    p.msg(tb_rtmp::message::window_ack_size(1000));
    for _ in 0..10 {
        p.msg(UserControl::PingRequest(1).to_message());
    }
    let (_, out, s) = run_keep_output(AllowAll, &p.bytes);
    let msgs = server_messages(&out);
    assert!(
        msgs.iter()
            .any(|m| UserControl::parse(&m.payload) == Some(UserControl::PingResponse(1234)))
    );
    assert_eq!(s.stats().pings, 11);
    assert!(s.stats().acks_sent >= 1);
    assert!(
        msgs.iter()
            .any(|m| m.type_id == MessageType::ACKNOWLEDGMENT)
    );
}

#[test]
fn timeouts() {
    let cfg = ServerConfig::default();
    // Handshake.
    let mut s = ServerSession::new(cfg, AllowAll, Duration::ZERO);
    assert!(s.on_tick(Duration::from_secs(9)).is_empty());
    assert!(!s.wants_close());
    s.on_tick(Duration::from_secs(10));
    assert_eq!(s.close_reason(), Some(&CloseReason::Timeout("handshake")));
    // Publish: connected but never publishing, while still sending pings.
    let mut s = ServerSession::new(cfg, AllowAll, Duration::ZERO);
    let mut p = Publisher::new();
    p.connect("live");
    s.on_bytes(Duration::ZERO, &p.bytes);
    for t in 1..=15 {
        let ping = {
            let mut b = Vec::new();
            let mut e = tb_rtmp::chunk::ChunkEncoder::new();
            e.write(&UserControl::PingRequest(t).to_message(), &mut b)
                .unwrap();
            b
        };
        s.on_bytes(Duration::from_secs(u64::from(t)), &ping);
    }
    assert_eq!(s.close_reason(), Some(&CloseReason::Timeout("publish")));
    // Idle while publishing: End(Transport).
    let mut s = ServerSession::new(cfg, AllowAll, Duration::ZERO);
    let mut p = Publisher::new();
    p.start("live", "key");
    p.metadata(vec![("videocodecid", Amf0Value::Number(7.0))]);
    p.media(true, 0, avc_seq(0));
    p.media(true, 0, avc_frame(true, 0, 10));
    let ev = s.on_bytes(Duration::ZERO, &p.bytes);
    assert_eq!(ev.len(), 2);
    let ev = s.on_tick(Duration::from_secs(10));
    assert_eq!(
        ev,
        [InputEvent::End(EndReason::Transport("idle timeout".into()))]
    );
    assert!(s.wants_close());
    assert!(s.on_tick(Duration::from_secs(20)).is_empty());
}

#[test]
fn output_queue_is_bounded() {
    let mut p = Publisher::new();
    p.connect("live");
    for _ in 0..60_000 {
        p.msg(UserControl::PingRequest(1).to_message());
    }
    let (ev, out, s) = run_keep_output(AllowAll, &p.bytes);
    assert!(ev.is_empty());
    assert!(matches!(s.close_reason(), Some(CloseReason::Local(_))));
    assert!(out.len() <= ServerConfig::default().max_output + 64);
}

#[test]
fn protocol_violations_end_cleanly() {
    // A declared message longer than the limit, while publishing.
    let mut p = Publisher::new();
    p.start("live", "key");
    p.chunk_size(0x7FFF_FFFF);
    p.bytes
        .extend_from_slice(&[0x06, 0, 0, 0, 0xFF, 0xFF, 0xFF, 9, 1, 0, 0, 0, 1, 2, 3]);
    let (ev, _, s) = run_keep_output(AllowAll, &p.bytes);
    assert!(matches!(
        ev.last(),
        Some(InputEvent::End(EndReason::Protocol(_)))
    ));
    assert!(matches!(s.close_reason(), Some(CloseReason::Protocol(_))));
    // An oversized command.
    let mut p = Publisher::new();
    p.chunk_size(65_536);
    p.command(0, "connect", Amf0Value::String("x".repeat(70_000)), vec![]);
    let (_, _, s) = run_keep_output(AllowAll, &p.bytes);
    assert!(matches!(s.close_reason(), Some(CloseReason::Protocol(_))));
    // Not RTMP.
    let (_, out, s) = run_keep_output(AllowAll, b"GET / HTTP/1.1\r\n\r\n");
    assert!(out.is_empty());
    assert!(matches!(s.close_reason(), Some(CloseReason::Protocol(_))));
    // publish before createStream.
    let mut p = Publisher::new();
    p.connect("live");
    p.command(1, "publish", Amf0Value::Null, vec![Amf0Value::string("k")]);
    let (_, _, s) = run_keep_output(AllowAll, &p.bytes);
    assert!(matches!(s.close_reason(), Some(CloseReason::Protocol(_))));
}

#[test]
fn amf3_command_wrapper_is_accepted() {
    let mut p = Publisher::new();
    let c = Command {
        name: "connect".into(),
        transaction: 1.0,
        object: Amf0Value::object([("app", Amf0Value::string("live"))]),
        args: vec![],
    };
    let mut m = c.to_message(cs(3), tb_rtmp::MessageStreamId(0));
    m.payload.insert(0, 0);
    m.type_id = MessageType::COMMAND_AMF3;
    p.msg(m);
    let (_, out, s) = run_keep_output(AllowAll, &p.bytes);
    assert!(!s.wants_close());
    assert_eq!(commands(&server_messages(&out))[0].name, "_result");
}

/// Random bytes after a valid handshake, and random corruption of a valid session: no panic,
/// and the event list keeps its shape.
#[test]
fn corruption_never_panics() {
    let mut rng = Rng(0xDEAD_BEEF);
    let good = synthetic_session(0);
    let hs = Publisher::new().bytes;
    for round in 0..300 {
        let bytes = if round % 3 == 0 {
            let mut b = hs.clone();
            let n = rng.below(20_000);
            b.extend((0..n).map(|_| rng.next() as u8));
            b
        } else {
            let mut b = good.clone();
            for _ in 0..1 + rng.below(20) {
                let i = 1 + rng.below(b.len() - 1);
                b[i] = rng.next() as u8;
            }
            if round % 3 == 2 {
                b.truncate(rng.below(b.len()));
            }
            b
        };
        let mut chunks = Vec::new();
        let mut at = 0;
        while at < bytes.len() {
            let n = (1 + rng.below(5000)).min(bytes.len() - at);
            chunks.push(bytes[at..at + n].to_vec());
            at += n;
        }
        let (ev, s) = run(ServerConfig::default(), AllowAll, chunks);
        assert!(s.wants_close());
        if !ev.is_empty() {
            assert!(matches!(ev.last(), Some(InputEvent::End(_))));
            assert_eq!(
                ev.iter()
                    .filter(|e| matches!(e, InputEvent::End(_)))
                    .count(),
                1
            );
        }
    }
}
