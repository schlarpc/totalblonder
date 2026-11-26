//! Helpers shared by the integration tests: a synthetic publisher built from the crate's own
//! client-side pieces, and a driver that runs a session over byte chunks.
#![allow(dead_code, reason = "each test binary uses a subset")]
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test support")]

use std::time::Duration;

use tb_media::{InputEvent, StreamIndex};
use tb_rtmp::amf0::Amf0Value;
use tb_rtmp::chunk::ChunkEncoder;
use tb_rtmp::handshake::{ClientHandshake, PACKET_SIZE};
use tb_rtmp::message::{self, Command};
use tb_rtmp::{
    ChunkStreamId, Message, MessageStreamId, MessageType, PublishPolicy, RtmpTime, ServerConfig,
    ServerSession,
};

/// Builds the bytes a publisher sends, without waiting for replies (a server never needs the
/// client to wait).
pub struct Publisher {
    pub bytes: Vec<u8>,
    enc: ChunkEncoder,
    tx: f64,
}

pub fn cs(id: u32) -> ChunkStreamId {
    ChunkStreamId::new(id).unwrap()
}

impl Publisher {
    pub fn new() -> Self {
        let (_, mut bytes) = ClientHandshake::start(1, 0);
        bytes.extend_from_slice(&[0x5A; PACKET_SIZE]); // C2
        Self {
            bytes,
            enc: ChunkEncoder::new(),
            tx: 1.0,
        }
    }

    pub fn msg(&mut self, m: Message) -> &mut Self {
        self.enc.write(&m, &mut self.bytes).unwrap();
        self
    }

    pub fn chunk_size(&mut self, n: u32) -> &mut Self {
        self.msg(message::set_chunk_size(n));
        self.enc.set_chunk_size(n).unwrap();
        self
    }

    pub fn command(
        &mut self,
        stream: u32,
        name: &str,
        object: Amf0Value,
        args: Vec<Amf0Value>,
    ) -> &mut Self {
        let c = Command {
            name: name.into(),
            transaction: self.tx,
            object,
            args,
        };
        self.tx += 1.0;
        let csid = if stream == 0 { 3 } else { 8 };
        self.msg(c.to_message(cs(csid), MessageStreamId(stream)))
    }

    pub fn connect(&mut self, app: &str) -> &mut Self {
        self.command(
            0,
            "connect",
            Amf0Value::object([
                ("app", Amf0Value::string(app)),
                ("type", Amf0Value::string("nonprivate")),
                (
                    "tcUrl",
                    Amf0Value::string(&format!("rtmp://127.0.0.1/{app}")),
                ),
            ]),
            vec![],
        )
    }

    /// connect, releaseStream, FCPublish, createStream, publish: ffmpeg's sequence.
    pub fn start(&mut self, app: &str, key: &str) -> &mut Self {
        self.chunk_size(4096).connect(app);
        let k = Amf0Value::string(key);
        self.command(0, "releaseStream", Amf0Value::Null, vec![k.clone()]);
        self.command(0, "FCPublish", Amf0Value::Null, vec![k.clone()]);
        self.command(0, "createStream", Amf0Value::Null, vec![]);
        self.command(
            1,
            "publish",
            Amf0Value::Null,
            vec![k, Amf0Value::string("live")],
        )
    }

    pub fn metadata(&mut self, props: Vec<(&str, Amf0Value)>) -> &mut Self {
        let mut p = Vec::new();
        tb_rtmp::amf0::encode(&Amf0Value::string("@setDataFrame"), &mut p);
        tb_rtmp::amf0::encode(&Amf0Value::string("onMetaData"), &mut p);
        let obj = Amf0Value::EcmaArray(props.into_iter().map(|(k, v)| (k.into(), v)).collect());
        tb_rtmp::amf0::encode(&obj, &mut p);
        self.msg(Message {
            csid: cs(4),
            timestamp: RtmpTime(0),
            type_id: MessageType::DATA_AMF0,
            stream: MessageStreamId(1),
            payload: p,
        })
    }

    pub fn media(&mut self, video: bool, ts: u32, payload: Vec<u8>) -> &mut Self {
        self.msg(Message {
            csid: cs(if video { 6 } else { 4 }),
            timestamp: RtmpTime(ts),
            type_id: if video {
                MessageType::VIDEO
            } else {
                MessageType::AUDIO
            },
            stream: MessageStreamId(1),
            payload,
        })
    }

    pub fn finish(&mut self) -> &mut Self {
        let k = Amf0Value::string("key");
        self.command(0, "FCUnpublish", Amf0Value::Null, vec![k]);
        self.command(
            0,
            "deleteStream",
            Amf0Value::Null,
            vec![Amf0Value::Number(1.0)],
        )
    }
}

/// An AVC sequence header tag body (configuration bytes are opaque here).
pub fn avc_seq(extra: u8) -> Vec<u8> {
    vec![0x17, 0, 0, 0, 0, 1, 0x64, 0, 0x1F, extra]
}

/// An AVC NALU tag body.
pub fn avc_frame(key: bool, cts: u32, size: usize) -> Vec<u8> {
    let mut v = vec![if key { 0x17 } else { 0x27 }, 1];
    v.extend_from_slice(&cts.to_be_bytes()[1..]);
    v.extend_from_slice(&(size as u32).to_be_bytes());
    v.extend(std::iter::repeat_n(0x41, size));
    v
}

pub const AAC_SEQ: [u8; 4] = [0xAF, 0, 0x12, 0x10];

pub fn aac_frame(size: usize) -> Vec<u8> {
    let mut v = vec![0xAF, 1];
    v.extend(std::iter::repeat_n(0x21, size));
    v
}

/// A complete synthetic publish: 3 s of 30 fps video with a key frame every 30 and B-frame
/// composition offsets, AAC every 23 ms, metadata, an extended-timestamp region, and the
/// unpublish sequence.
pub fn synthetic_session(base_ts: u32) -> Vec<u8> {
    let mut p = Publisher::new();
    p.start("live", "key");
    p.metadata(vec![
        ("width", Amf0Value::Number(640.0)),
        ("height", Amf0Value::Number(360.0)),
        ("framerate", Amf0Value::Number(30.0)),
        ("videocodecid", Amf0Value::Number(7.0)),
        ("audiocodecid", Amf0Value::Number(10.0)),
    ]);
    p.media(true, base_ts, avc_seq(0));
    p.media(false, base_ts, AAC_SEQ.to_vec());
    let mut a = 0u32;
    for i in 0..90u32 {
        let t = base_ts.wrapping_add(i * 33);
        while a * 23 <= i * 33 {
            p.media(
                false,
                base_ts.wrapping_add(a * 23),
                aac_frame(100 + (a as usize % 7)),
            );
            a += 1;
        }
        p.media(
            true,
            t,
            avc_frame(
                i % 30 == 0,
                if i % 3 == 1 { 66 } else { 0 },
                50 + (i as usize * 37) % 400,
            ),
        );
    }
    p.finish();
    std::mem::take(&mut p.bytes)
}

/// Run a session over `chunks` at a fixed time; returns the events and the session.
pub fn run<P: PublishPolicy>(
    config: ServerConfig,
    policy: P,
    chunks: impl IntoIterator<Item = Vec<u8>>,
) -> (Vec<InputEvent>, ServerSession<P>) {
    let mut s = ServerSession::new(config, policy, Duration::ZERO);
    let mut ev = Vec::new();
    for c in chunks {
        ev.extend(s.on_bytes(Duration::ZERO, &c));
        let n = s.pending_output().len();
        s.consume_output(n);
    }
    ev.extend(s.on_eof(Duration::ZERO));
    (ev, s)
}

/// Checks the shape of an event list: Streams first, packets only of announced streams,
/// exactly one End at the end. Returns the packets per stream index.
pub fn check_shape(
    ev: &[InputEvent],
) -> std::collections::BTreeMap<StreamIndex, Vec<tb_media::Packet>> {
    let mut announced: Option<Vec<StreamIndex>> = None;
    let mut per = std::collections::BTreeMap::new();
    for (i, e) in ev.iter().enumerate() {
        match e {
            InputEvent::Streams(s) => announced = Some(s.iter().map(|s| s.index).collect()),
            InputEvent::Packet(p) => {
                let a = announced.as_ref().expect("packet before Streams");
                assert!(a.contains(&p.stream), "packet of an unannounced stream");
                per.entry(p.stream).or_insert_with(Vec::new).push(p.clone());
            }
            InputEvent::End(_) => assert_eq!(i, ev.len() - 1, "End is last"),
        }
    }
    per
}
