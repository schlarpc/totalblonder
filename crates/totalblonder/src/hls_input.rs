//! An HLS / Low-Latency HLS pull input (SPEC §9.2): the HTTP side of `tb-hls`.
//!
//! The playlist URL may be a multivariant playlist: the variant with the highest `BANDWIDTH`
//! (within `max_bandwidth`, when given) is taken, with every audio rendition of its `AUDIO`
//! group that has a playlist of its own. Each playlist (the variant's and the renditions')
//! has a fetch thread with its own [`tb_hls::planner::Planner`] and demuxer: fragmented MP4
//! when the playlist has an `EXT-X-MAP` ([`tb_hls::fmp4`]), MPEG-TS otherwise (`tb-tsin`).
//! Stream `k` of playlist `r` is index `r · 0x2000 + k` (`k` the track_ID or the PID), so
//! indices never collide or change meaning in a session. A rendition's `LANGUAGE` labels its
//! audio streams that carry none of their own, so the program's tracks take them by language
//! (`crate::audio_routes`).
//!
//! The merger (the input's own thread) announces the union of the playlists' streams once
//! every playlist has given its own (or after five seconds), then hands the packets on paced by
//! their time stamps, like a file input: a segment arrives in one burst but leaves over its
//! duration, so the timeline mapper sees a live source. A playlist that fails, ends or meets a
//! discontinuity ends the session (`End`); the input starts again from the top after two
//! seconds.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use tb_hls::fmp4;
use tb_hls::planner::{Fetch, Planner};
use tb_hls::playlist::{Playlist, RenditionKind, parse};
use tb_media::{
    AudioHint, EndReason, InputEvent, Packet, Rational, StreamIndex, StreamInfo, VideoHint,
};
use url::Url;

use crate::ingest::{EventSink, InputStats};
use crate::time::Clock;

/// Streams of playlist `r` start at this index times `r`.
const STRIDE: u16 = 0x2000;

/// A media playlist to fetch, and the language its audio is in.
type MediaList = (Url, Option<[u8; 3]>);

/// What a fetch thread tells the merger.
enum FromFetch {
    /// Playlist `.0`'s streams (again, when they change).
    Streams(usize, Vec<StreamInfo>),
    /// A packet and its time in seconds.
    Packet(Packet, f64),
    /// The playlist cannot go on (the reason).
    Stop(String),
}

/// ISO 639-1 (two letters, as RFC 5646 language tags usually start) to ISO 639-2 for the
/// common ones; three-letter tags pass through.
fn language(tag: &str) -> Option<[u8; 3]> {
    let primary = tag.split(['-', '_']).next()?.to_ascii_lowercase();
    let three = match primary.as_str() {
        "en" => "eng",
        "es" => "spa",
        "fr" => "fra",
        "de" => "deu",
        "it" => "ita",
        "pt" => "por",
        "ja" => "jpn",
        "ko" => "kor",
        "zh" => "zho",
        "ru" => "rus",
        "ar" => "ara",
        "hi" => "hin",
        "vi" => "vie",
        "tl" => "tgl",
        "pl" => "pol",
        "nl" => "nld",
        s if s.len() == 3 => s,
        _ => return None,
    };
    let b = three.as_bytes();
    Some([b[0], b[1], b[2]])
}

/// The HTTP client: blocking, rustls on ring (no other TLS stack in the process).
fn client() -> Result<reqwest::blocking::Client, reqwest::Error> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .user_agent(concat!("totalblonder/", env!("CARGO_PKG_VERSION")))
        .build()
}

fn get(
    c: &reqwest::blocking::Client,
    url: &Url,
    range: Option<(u64, Option<u64>)>,
    timeout: Duration,
) -> Result<Vec<u8>, String> {
    let mut rq = c.get(url.clone()).timeout(timeout);
    if let Some((start, len)) = range {
        let v = match len {
            Some(n) if n > 0 => format!("bytes={start}-{}", start + n - 1),
            _ => format!("bytes={start}-"),
        };
        rq = rq.header(reqwest::header::RANGE, v);
    }
    let r = rq.send().map_err(|e| format!("{url}: {e}"))?;
    if !r.status().is_success() {
        return Err(format!("{url}: HTTP {}", r.status()));
    }
    r.bytes()
        .map(|b| b.to_vec())
        .map_err(|e| format!("{url}: {e}"))
}

/// The playlists of a session: the media playlist URLs and the language of each.
fn playlists(
    c: &reqwest::blocking::Client,
    url: &Url,
    max_bandwidth: Option<u64>,
) -> Result<Vec<MediaList>, String> {
    let text = get(c, url, None, Duration::from_secs(10))?;
    let text = String::from_utf8_lossy(&text);
    match parse(&text, url).map_err(|e| format!("{url}: {e}"))? {
        Playlist::Media(_) => Ok(vec![(url.clone(), None)]),
        Playlist::Multivariant(m) => {
            let v = m
                .variants
                .iter()
                .filter(|v| max_bandwidth.is_none_or(|b| v.bandwidth <= b))
                .max_by_key(|v| v.bandwidth)
                .or_else(|| m.variants.iter().min_by_key(|v| v.bandwidth))
                .ok_or_else(|| format!("{url}: no variants"))?;
            let mut out = vec![(v.uri.clone(), None)];
            if let Some(g) = &v.audio {
                for r in &m.renditions {
                    if r.kind == RenditionKind::Audio
                        && &r.group == g
                        && let Some(u) = &r.uri
                    {
                        out.push((u.clone(), r.language.as_deref().and_then(language)));
                    }
                }
            }
            tracing::info!(
                %url,
                variant = %v.uri,
                bandwidth = v.bandwidth,
                audio_renditions = out.len() - 1,
                "HLS input: variant chosen"
            );
            Ok(out)
        }
    }
}

/// Sleeps `d` unless `stop` is set first.
fn nap(d: Duration, stop: &AtomicBool) -> bool {
    let end = Instant::now() + d;
    while Instant::now() < end {
        if stop.load(Ordering::Relaxed) {
            return false;
        }
        std::thread::sleep((end - Instant::now()).min(Duration::from_millis(50)));
    }
    !stop.load(Ordering::Relaxed)
}

/// One playlist's fetch loop.
fn fetch_playlist(
    r: usize,
    url: Url,
    lang: Option<[u8; 3]>,
    c: reqwest::blocking::Client,
    tx: Sender<FromFetch>,
    stop: Arc<AtomicBool>,
    st: InputStats,
) {
    let base = STRIDE * r as u16;
    let mut planner = Planner::new(url.clone());
    let mut mp4 = fmp4::Demuxer::new();
    let mut ts: Option<tb_tsin::Demuxer> = None;
    let mut last_dts: HashMap<StreamIndex, i64> = HashMap::new();
    let mut fetched_media = false;
    let label = |mut s: StreamInfo| {
        s.index = StreamIndex(base + s.index.0);
        if s.codec.kind() == tb_media::MediaKind::Audio && s.language.is_none() {
            s.language = lang;
        }
        s
    };
    let fail = |why: String| {
        let _ = tx.send(FromFetch::Stop(why));
    };
    while !stop.load(Ordering::Relaxed) {
        let f = planner.next_fetch();
        let target = planner
            .current()
            .map_or(6.0, |p| p.target_duration.max(1.0));
        let timeout = Duration::from_secs_f64(target * 3.0 + 5.0);
        match &f {
            Fetch::Playlist { url: u, wait } => {
                if !nap(*wait, &stop) {
                    return;
                }
                let waited_for = u.query().is_some_and(|q| q.contains("_HLS_msn"));
                let text = match get(&c, u, None, timeout) {
                    Ok(t) => t,
                    Err(e) => {
                        tracing::warn!("HLS playlist: {e}");
                        st.errors.inc();
                        planner.failed(&f);
                        if !nap(Duration::from_secs(1), &stop) {
                            return;
                        }
                        continue;
                    }
                };
                match parse(&String::from_utf8_lossy(&text), &url) {
                    Ok(Playlist::Media(p)) => {
                        let before = planner
                            .current()
                            .map(|q| (q.next_msn(), q.open_parts.len()));
                        planner.playlist(p);
                        let after = planner
                            .current()
                            .map(|q| (q.next_msn(), q.open_parts.len()));
                        if waited_for && before == after {
                            planner.stalled();
                        }
                    }
                    Ok(Playlist::Multivariant(_)) => {
                        return fail(format!(
                            "{url}: a multivariant playlist where media was expected"
                        ));
                    }
                    Err(e) => {
                        tracing::warn!("HLS playlist {url}: {e}");
                        st.errors.inc();
                        planner.failed(&f);
                    }
                }
            }
            Fetch::Map(u) => match get(&c, u, None, timeout) {
                Ok(b) => {
                    st.bytes.add(b.len() as u64);
                    match mp4.init(&b) {
                        Ok(tracks) => {
                            let streams: Vec<StreamInfo> = tracks
                                .iter()
                                .filter_map(|t| {
                                    let Some(codec) = t.codec else {
                                        tracing::warn!(
                                            track = t.id,
                                            "HLS input: track not carried: {}",
                                            t.unsupported.as_deref().unwrap_or("?")
                                        );
                                        return None;
                                    };
                                    Some(label(StreamInfo {
                                        index: StreamIndex(t.id as u16),
                                        codec,
                                        time_base: Rational::new(1, t.timescale).ok()?,
                                        extradata: t.extradata.clone(),
                                        video: VideoHint {
                                            width: t.size.map(|s| s.0),
                                            height: t.size.map(|s| s.1),
                                            frame_rate: None,
                                        },
                                        audio: AudioHint {
                                            sample_rate: t.audio.map(|a| a.1),
                                            channels: t.audio.map(|a| a.0),
                                        },
                                        language: t.language,
                                    }))
                                })
                                .collect();
                            let _ = tx.send(FromFetch::Streams(r, streams));
                            planner.fetched(&f, b.len() as u64);
                        }
                        Err(e) => return fail(format!("{u}: {e}")),
                    }
                }
                Err(e) => {
                    tracing::warn!("HLS init: {e}");
                    st.errors.inc();
                    planner.failed(&f);
                    if !nap(Duration::from_secs(1), &stop) {
                        return;
                    }
                }
            },
            Fetch::Media(m) => {
                // A discontinuity after the start: the time stamps start over.
                let starts_segment = m.part.is_none_or(|p| p == 0);
                if fetched_media
                    && starts_segment
                    && planner
                        .current()
                        .and_then(|p| p.segment(m.msn))
                        .is_some_and(|s| s.discontinuity)
                {
                    return fail(format!("{url}: discontinuity at segment {}", m.msn));
                }
                let bytes = match get(&c, &m.url, m.range, timeout) {
                    Ok(b) => b,
                    Err(e) => {
                        tracing::debug!("HLS media: {e}");
                        st.errors.inc();
                        planner.failed(&f);
                        continue;
                    }
                };
                st.bytes.add(bytes.len() as u64);
                fetched_media = true;
                let mut out: Vec<(Packet, f64)> = Vec::new();
                if mp4.tracks().next().is_some() {
                    let samples = match mp4.fragment(&bytes) {
                        Ok(s) => s,
                        Err(e) => {
                            tracing::warn!("HLS {}: {e}", m.url);
                            st.errors.inc();
                            planner.fetched(&f, bytes.len() as u64);
                            continue;
                        }
                    };
                    let scale: HashMap<u32, u32> =
                        mp4.tracks().map(|t| (t.id, t.timescale)).collect();
                    for s in samples {
                        let Some(&ts) = scale.get(&s.track) else {
                            continue;
                        };
                        out.push((
                            Packet {
                                stream: StreamIndex(base + s.track as u16),
                                pts: Some(s.pts),
                                dts: Some(s.dts),
                                duration: None,
                                keyframe: s.key,
                                data: s.data,
                            },
                            s.dts as f64 / f64::from(ts),
                        ));
                    }
                } else {
                    let d = ts.get_or_insert_with(|| {
                        tb_tsin::Demuxer::new(tb_tsin::DemuxConfig::default())
                    });
                    d.push_bytes(&bytes);
                    for e in d.take_events() {
                        match e.event {
                            InputEvent::Streams(list) => {
                                let streams = list.into_iter().map(label).collect();
                                let _ = tx.send(FromFetch::Streams(r, streams));
                            }
                            InputEvent::Packet(mut p) => {
                                p.stream = StreamIndex(base + p.stream.0);
                                let t = p.dts.or(p.pts).unwrap_or(0) as f64 / 90_000.0;
                                out.push((p, t));
                            }
                            InputEvent::End(_) => {}
                        }
                    }
                }
                for (p, t) in out {
                    let dts = p.dts.or(p.pts).unwrap_or(i64::MIN);
                    // a whole segment after some of its parts: what came already is skipped
                    if m.overlaps && last_dts.get(&p.stream).is_some_and(|&l| dts <= l) {
                        continue;
                    }
                    last_dts.insert(p.stream, dts);
                    if tx.send(FromFetch::Packet(p, t)).is_err() {
                        return;
                    }
                }
                planner.fetched(&f, bytes.len() as u64);
            }
            Fetch::Ended => return fail(format!("{url}: playlist ended")),
        }
    }
}

/// A packet waiting for its time.
struct Held {
    due: Instant,
    seq: u64,
    packet: Packet,
}

impl PartialEq for Held {
    fn eq(&self, o: &Self) -> bool {
        (self.due, self.seq) == (o.due, o.seq)
    }
}
impl Eq for Held {}
impl PartialOrd for Held {
    fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for Held {
    fn cmp(&self, o: &Self) -> std::cmp::Ordering {
        (self.due, self.seq).cmp(&(o.due, o.seq))
    }
}

/// One session: fetch threads for every playlist, merged and paced, until one stops or the
/// clock does. Returns whether to go on.
fn session(
    c: &reqwest::blocking::Client,
    lists: Vec<MediaList>,
    sink: &mut dyn EventSink,
    st: &InputStats,
    clock: &dyn Clock,
) -> bool {
    let stop = Arc::new(AtomicBool::new(false));
    let (tx, rx): (Sender<FromFetch>, Receiver<FromFetch>) = channel();
    let n = lists.len();
    for (r, (u, lang)) in lists.into_iter().enumerate() {
        let (c, tx, halt, st) = (c.clone(), tx.clone(), stop.clone(), st.clone());
        let spawned = std::thread::Builder::new()
            .name(format!("hls-{r}"))
            .spawn(move || fetch_playlist(r, u, lang, c, tx, halt, st));
        if let Err(e) = spawned {
            tracing::error!("HLS input: {e}");
            stop.store(true, Ordering::Relaxed);
            return true;
        }
    }
    drop(tx);
    // Packets go out this far ahead of their time (the mapper absorbs it as arrival jitter).
    let lead = Duration::from_millis(300);
    let t0 = Instant::now();
    let mut streams: Vec<Option<Vec<StreamInfo>>> = vec![None; n];
    let mut announced = false;
    let mut origin: Option<(Instant, f64)> = None;
    let mut heap: BinaryHeap<Reverse<Held>> = BinaryHeap::new();
    let mut seq = 0u64;
    let announce = |streams: &[Option<Vec<StreamInfo>>], sink: &mut dyn EventSink| {
        let all: Vec<StreamInfo> = streams.iter().flatten().flatten().cloned().collect();
        if !all.is_empty() {
            sink.event(InputEvent::Streams(all));
        }
    };
    let reason = loop {
        if clock.stopping() {
            break None;
        }
        match rx.recv_timeout(Duration::from_millis(20)) {
            Ok(FromFetch::Streams(r, s)) => {
                if streams[r].as_ref() != Some(&s) {
                    streams[r] = Some(s);
                    if announced {
                        announce(&streams, sink);
                    }
                }
            }
            Ok(FromFetch::Packet(p, t)) => {
                let (w, t_0) =
                    *origin.get_or_insert((Instant::now() + Duration::from_millis(500), t));
                let due = w + Duration::from_secs_f64((t - t_0).max(0.0));
                seq += 1;
                heap.push(Reverse(Held {
                    due,
                    seq,
                    packet: p,
                }));
            }
            Ok(FromFetch::Stop(why)) => break Some(why),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                break Some("every playlist stopped".into());
            }
        }
        if !announced
            && (streams.iter().all(Option::is_some)
                || (t0.elapsed() > Duration::from_secs(5) && streams.iter().any(Option::is_some)))
        {
            announce(&streams, sink);
            announced = true;
        }
        if announced {
            let now = Instant::now();
            while heap.peek().is_some_and(|h| h.0.due <= now + lead) {
                let Some(Reverse(h)) = heap.pop() else { break };
                st.packets.inc();
                sink.event(InputEvent::Packet(h.packet));
            }
        }
    };
    stop.store(true, Ordering::Relaxed);
    if announced {
        sink.event(InputEvent::End(EndReason::Closed));
    }
    match reason {
        Some(why) => {
            tracing::warn!("HLS input: session ended: {why}");
            true
        }
        None => false,
    }
}

/// Pulls `url` (a multivariant or media playlist, HLS or LL-HLS) until the clock stops.
pub fn run_hls(
    url: Url,
    max_bandwidth: Option<u64>,
    mut sink: Box<dyn EventSink>,
    st: InputStats,
    clock: &dyn Clock,
) {
    let c = match client() {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(%url, "HLS input: {e}");
            return;
        }
    };
    while !clock.stopping() {
        match playlists(&c, &url, max_bandwidth) {
            Ok(lists) => {
                st.connections.inc();
                if !session(&c, lists, sink.as_mut(), &st, clock) {
                    return;
                }
            }
            Err(e) => {
                tracing::warn!("HLS input: {e}");
                st.errors.inc();
            }
        }
        let until = Instant::now() + Duration::from_secs(2);
        while Instant::now() < until && !clock.stopping() {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn languages() {
        assert_eq!(language("en"), Some(*b"eng"));
        assert_eq!(language("es-419"), Some(*b"spa"));
        assert_eq!(language("fra"), Some(*b"fra"));
        assert_eq!(language("x-klingon"), None);
    }
}
