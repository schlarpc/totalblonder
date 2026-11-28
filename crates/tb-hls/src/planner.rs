//! What to request next from one media playlist (RFC 8216bis §6.2.5 for Low-Latency HLS).
//!
//! - **Start.** At the live edge less `PART-HOLD-BACK` (with parts) or `HOLD-BACK` (three
//!   target durations when absent), moved back to a part (or segment) a decoder can start
//!   from: an `INDEPENDENT=YES` part, or a segment boundary.
//! - **Parts.** The next listed part; when the playlist does not list it yet, the
//!   `EXT-X-PRELOAD-HINT` if it is that part, else a blocking playlist reload for it
//!   (`_HLS_msn` / `_HLS_part`, when `CAN-BLOCK-RELOAD=YES`), else a plain reload after half a
//!   part target. When a segment is complete the next one starts at part 0.
//! - **Fallen behind.** A segment whose parts are no longer listed is fetched whole; one no
//!   longer listed at all restarts at the live edge.
//! - **Segments** (no parts): the next segment, or a reload after half a target duration.
//! - **Init.** An `EXT-X-MAP` not yet fetched comes first.
//!
//! The caller fetches what [`Planner::next_fetch`] says and reports back with [`Planner::fetched`]
//! (or [`Planner::failed`]); playlists it receives go to [`Planner::playlist`].

use std::time::Duration;

use url::Url;

use crate::playlist::MediaPlaylist;

/// A request to make.
#[derive(Debug, Clone, PartialEq)]
pub enum Fetch {
    /// The playlist, after `wait`. `url` carries the blocking-reload query when the server can
    /// hold the request.
    Playlist {
        /// What to request.
        url: Url,
        /// How long to wait first.
        wait: Duration,
    },
    /// An init segment.
    Map(Url),
    /// Media: a part, a hinted part or a whole segment.
    Media(Media),
    /// The playlist has ended and everything in it was fetched.
    Ended,
}

/// A media request.
#[derive(Debug, Clone, PartialEq)]
pub struct Media {
    /// The resource.
    pub url: Url,
    /// `(start, length)`; length `None` up to the end (an open-ended preload hint).
    pub range: Option<(u64, Option<u64>)>,
    /// Its segment.
    pub msn: u64,
    /// Its part within the segment; `None` for a whole segment.
    pub part: Option<usize>,
    /// A whole segment fetched after some of its parts were: samples already delivered come
    /// again and must be skipped.
    pub overlaps: bool,
}

/// Where the next request is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Position {
    msn: u64,
    /// The next part of `msn` (0 at a segment start).
    part: usize,
}

/// The planner of one media playlist.
#[derive(Debug, Clone)]
pub struct Planner {
    url: Url,
    pl: Option<MediaPlaylist>,
    at: Option<Position>,
    map_have: Option<Url>,
    /// End of the last byte range per resource, for ranges without an offset.
    range_end: std::collections::HashMap<Url, u64>,
    /// The last request was a reload that came back without what it waited for.
    stalled: bool,
}

fn with_query(url: &Url, msn: u64, part: Option<usize>) -> Url {
    let mut u = url.clone();
    {
        let mut q = u.query_pairs_mut();
        q.append_pair("_HLS_msn", &msn.to_string());
        if let Some(p) = part {
            q.append_pair("_HLS_part", &p.to_string());
        }
    }
    u
}

impl Planner {
    /// A planner for the media playlist at `url`.
    pub fn new(url: Url) -> Self {
        Planner {
            url,
            pl: None,
            at: None,
            map_have: None,
            range_end: Default::default(),
            stalled: false,
        }
    }

    /// A new copy of the playlist arrived.
    pub fn playlist(&mut self, pl: MediaPlaylist) {
        self.pl = Some(pl);
        self.stalled = false;
    }

    /// The last playlist.
    pub fn current(&self) -> Option<&MediaPlaylist> {
        self.pl.as_ref()
    }

    /// Start over at the live edge (after an error, or a discontinuity the caller handled).
    pub fn restart(&mut self) {
        self.at = None;
    }

    /// The start position at the live edge.
    fn start(pl: &MediaPlaylist) -> Option<Position> {
        if pl.low_latency() && pl.part_target.is_some() {
            let hold = pl
                .part_hold_back
                .unwrap_or(3.0 * pl.part_target.unwrap_or(1.0));
            // Parts from the newest back, with their segment and index.
            let mut all: Vec<(u64, usize, f64, bool)> = Vec::new();
            for s in &pl.segments {
                for (i, p) in s.parts.iter().enumerate() {
                    all.push((s.msn, i, p.duration, p.independent || i == 0));
                }
            }
            for (i, p) in pl.open_parts.iter().enumerate() {
                all.push((pl.next_msn(), i, p.duration, p.independent || i == 0));
            }
            if !all.is_empty() {
                let mut acc = 0.0;
                let mut k = all.len();
                while k > 0 && acc < hold {
                    k -= 1;
                    acc += all[k].2;
                }
                while k > 0 && !all[k].3 {
                    k -= 1;
                }
                return Some(Position {
                    msn: all[k].0,
                    part: all[k].1,
                });
            }
        }
        let hold = pl.hold_back.unwrap_or(3.0 * pl.target_duration);
        let mut acc = 0.0;
        let mut k = pl.segments.len();
        while k > 0 && acc < hold {
            k -= 1;
            acc += pl.segments[k].duration;
        }
        pl.segments.get(k).map(|s| Position {
            msn: s.msn,
            part: 0,
        })
    }

    fn reload(&self, at: Option<Position>, pl: &MediaPlaylist) -> Fetch {
        let half = |s: f64| Duration::from_secs_f64((s / 2.0).clamp(0.05, 5.0));
        match at {
            Some(p) if pl.can_block_reload => Fetch::Playlist {
                url: with_query(&self.url, p.msn, pl.low_latency().then_some(p.part)),
                // a held request that came back early is not repeated at once
                wait: if self.stalled {
                    half(pl.part_target.unwrap_or(pl.target_duration))
                } else {
                    Duration::ZERO
                },
            },
            _ => Fetch::Playlist {
                url: self.url.clone(),
                wait: half(pl.part_target.unwrap_or(pl.target_duration)),
            },
        }
    }

    /// What to request now.
    pub fn next_fetch(&mut self) -> Fetch {
        let Some(pl) = self.pl.as_ref() else {
            return Fetch::Playlist {
                url: self.url.clone(),
                wait: Duration::ZERO,
            };
        };
        if self.at.is_none() {
            self.at = Self::start(pl);
        }
        let Some(mut at) = self.at else {
            return self.reload(None, pl);
        };
        // A segment no longer listed: start again at the live edge.
        if at.msn < pl.media_sequence {
            self.at = Self::start(pl);
            match self.at {
                Some(p) => at = p,
                None => return self.reload(None, pl),
            }
        }
        // Its init segment first.
        let map = pl
            .segment(at.msn)
            .map_or(pl.map.as_ref(), |s| s.map.as_ref());
        if let Some(m) = map
            && self.map_have.as_ref() != Some(m)
        {
            return Fetch::Map(m.clone());
        }
        if pl.low_latency() {
            loop {
                match pl.parts_of(at.msn) {
                    Some((parts, complete)) => {
                        if let Some(p) = parts.get(at.part) {
                            let range = p.range.map(|r| {
                                let start = r.offset.unwrap_or_else(|| {
                                    self.range_end.get(&p.uri).copied().unwrap_or(0)
                                });
                                (start, Some(r.len))
                            });
                            self.at = Some(at);
                            return Fetch::Media(Media {
                                url: p.uri.clone(),
                                range,
                                msn: at.msn,
                                part: Some(at.part),
                                overlaps: false,
                            });
                        }
                        if complete
                            && parts.is_empty()
                            && let Some(s) = pl.segment(at.msn)
                        {
                            // the parts are gone: the whole segment
                            self.at = Some(at);
                            return Fetch::Media(Media {
                                url: s.uri.clone(),
                                range: s.range.map(|r| (r.offset.unwrap_or(0), Some(r.len))),
                                msn: at.msn,
                                part: None,
                                overlaps: at.part > 0,
                            });
                        }
                        if complete {
                            at = Position {
                                msn: at.msn + 1,
                                part: 0,
                            };
                            continue;
                        }
                        // The part is not listed yet: the hint, if it is this part.
                        self.at = Some(at);
                        if at.part == parts.len()
                            && let Some(h) = &pl.preload_hint
                        {
                            return Fetch::Media(Media {
                                url: h.uri.clone(),
                                range: h.start.map(|s| (s, h.len)),
                                msn: at.msn,
                                part: Some(at.part),
                                overlaps: false,
                            });
                        }
                        return self.reload(Some(at), pl);
                    }
                    None => {
                        self.at = Some(at);
                        if pl.ended {
                            return Fetch::Ended;
                        }
                        return self.reload(Some(at), pl);
                    }
                }
            }
        }
        self.at = Some(at);
        match pl.segment(at.msn) {
            Some(s) => Fetch::Media(Media {
                url: s.uri.clone(),
                range: s.range.map(|r| (r.offset.unwrap_or(0), Some(r.len))),
                msn: at.msn,
                part: None,
                overlaps: false,
            }),
            None if pl.ended => Fetch::Ended,
            None => self.reload(Some(at), pl),
        }
    }

    /// `f` (from [`Planner::next_fetch`]) arrived: `len` bytes.
    pub fn fetched(&mut self, f: &Fetch, len: u64) {
        match f {
            Fetch::Map(u) => self.map_have = Some(u.clone()),
            Fetch::Media(m) => {
                if let Some((start, _)) = m.range {
                    self.range_end.insert(m.url.clone(), start + len);
                }
                self.at = Some(match m.part {
                    Some(p) => Position {
                        msn: m.msn,
                        part: p + 1,
                    },
                    None => Position {
                        msn: m.msn + 1,
                        part: 0,
                    },
                });
            }
            Fetch::Playlist { .. } | Fetch::Ended => {}
        }
    }

    /// `f` failed (an HTTP error, a hinted part that never came): reload the playlist before
    /// trying again; an init segment is fetched again.
    pub fn failed(&mut self, f: &Fetch) {
        if let Fetch::Map(_) = f {
            self.map_have = None;
        }
        self.stalled = true;
        if let Some(pl) = self.pl.as_mut() {
            // forget the hint: it is what failed, or it is stale
            pl.preload_hint = None;
        }
    }

    /// A reload came back without the part it waited for (the server held it as long as it
    /// would): the next reload waits a little first.
    pub fn stalled(&mut self) {
        self.stalled = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::playlist::{Playlist, parse};

    fn base() -> Url {
        Url::parse("https://h/live/a.m3u8").unwrap()
    }

    fn media(text: &str) -> MediaPlaylist {
        match parse(text, &base()).unwrap() {
            Playlist::Media(m) => m,
            Playlist::Multivariant(_) => panic!(),
        }
    }

    /// Parts of 0.5 s, four to a segment; segments 10 and 11 complete, 12 open with two parts.
    fn ll(seq: u64, open: usize, hint: bool) -> String {
        let mut s = format!(
            "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-SERVER-CONTROL:CAN-BLOCK-RELOAD=YES,PART-HOLD-BACK=1.5\n#EXT-X-PART-INF:PART-TARGET=0.5\n#EXT-X-MEDIA-SEQUENCE:{seq}\n#EXT-X-MAP:URI=\"init.mp4\"\n"
        );
        for m in seq..seq + 2 {
            for p in 0..4 {
                let ind = if p == 0 { ",INDEPENDENT=YES" } else { "" };
                s += &format!("#EXT-X-PART:DURATION=0.5,URI=\"p{m}.{p}.mp4\"{ind}\n");
            }
            s += &format!("#EXTINF:2.0,\ns{m}.mp4\n");
        }
        for p in 0..open {
            let ind = if p == 0 { ",INDEPENDENT=YES" } else { "" };
            s += &format!(
                "#EXT-X-PART:DURATION=0.5,URI=\"p{}.{p}.mp4\"{ind}\n",
                seq + 2
            );
        }
        if hint {
            s += &format!(
                "#EXT-X-PRELOAD-HINT:TYPE=PART,URI=\"p{}.{open}.mp4\"\n",
                seq + 2
            );
        }
        s
    }

    fn uri(f: &Fetch) -> String {
        match f {
            Fetch::Media(m) => m.url.path().rsplit('/').next().unwrap().to_owned(),
            Fetch::Map(u) => format!("map:{}", u.path().rsplit('/').next().unwrap()),
            Fetch::Playlist { url, .. } => format!("playlist?{}", url.query().unwrap_or("")),
            Fetch::Ended => "ended".into(),
        }
    }

    /// Start 1.5 s (PART-HOLD-BACK) before the edge, moved back to an independent part; then
    /// part after part, the hint, a blocking reload; a new playlist goes on.
    #[test]
    fn low_latency_walk() {
        let mut p = Planner::new(base());
        assert_eq!(uri(&p.next_fetch()), "playlist?");
        p.playlist(media(&ll(10, 2, true)));
        let f = p.next_fetch();
        assert_eq!(uri(&f), "map:init.mp4");
        p.fetched(&f, 100);
        // Edge after p12.1; 1.5 s back is p11.3, back to independent p11.0.
        let mut got = Vec::new();
        for _ in 0..7 {
            let f = p.next_fetch();
            got.push(uri(&f));
            if matches!(f, Fetch::Media(_)) {
                p.fetched(&f, 10);
            } else {
                break;
            }
        }
        assert_eq!(
            got,
            [
                "p11.0.mp4",
                "p11.1.mp4",
                "p11.2.mp4",
                "p11.3.mp4",
                "p12.0.mp4",
                "p12.1.mp4",
                "p12.2.mp4"
            ],
            "the hint is the part after the last listed"
        );
        // Nothing more listed and no hint for p12.3: a blocking reload for it.
        assert_eq!(uri(&p.next_fetch()), "playlist?_HLS_msn=12&_HLS_part=3");
        p.playlist(media(&ll(10, 4, false)));
        assert_eq!(uri(&p.next_fetch()), "p12.3.mp4");
    }

    /// Fallen behind: the playlist has moved on; the position restarts at the live edge.
    #[test]
    fn fallen_behind_restarts_at_the_edge() {
        let mut p = Planner::new(base());
        p.playlist(media(&ll(10, 2, false)));
        let f = p.next_fetch();
        p.fetched(&f, 1);
        let f = p.next_fetch();
        assert_eq!(uri(&f), "p11.0.mp4");
        p.playlist(media(&ll(20, 1, false)));
        let f = p.next_fetch();
        assert_eq!(uri(&f), "p21.0.mp4", "{f:?}");
    }

    /// A complete segment whose parts are no longer listed is fetched whole.
    #[test]
    fn whole_segment_when_parts_are_gone() {
        let text = "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-PART-INF:PART-TARGET=0.5\n#EXT-X-SERVER-CONTROL:PART-HOLD-BACK=10\n#EXT-X-MEDIA-SEQUENCE:5\n#EXTINF:2,\ns5.ts\n#EXTINF:2,\ns6.ts\n#EXT-X-PART:DURATION=0.5,URI=\"p7.0.ts\",INDEPENDENT=YES\n";
        let mut p = Planner::new(base());
        p.playlist(media(text));
        // Hold-back 10 s reaches back past everything: the first part listed is p7.0, the
        // only one; segments 5 and 6 have no parts listed and come whole once reached.
        let f = p.next_fetch();
        assert_eq!(uri(&f), "p7.0.mp4".replace("mp4", "ts"));
        let mut q = Planner::new(base());
        q.playlist(media(text));
        q.at = Some(Position { msn: 5, part: 0 });
        let f = q.next_fetch();
        assert_eq!(uri(&f), "s5.ts");
        assert!(matches!(
            f,
            Fetch::Media(Media {
                part: None,
                overlaps: false,
                ..
            })
        ));
        q.fetched(&f, 1);
        assert_eq!(uri(&q.next_fetch()), "s6.ts");
    }

    /// Plain HLS: three target durations back, segment after segment, then a plain reload.
    #[test]
    fn segments_without_parts() {
        let mut s = "#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXT-X-MEDIA-SEQUENCE:100\n".to_owned();
        for m in 100..106 {
            s += &format!("#EXTINF:6,\n{m}.ts\n");
        }
        let mut p = Planner::new(base());
        p.playlist(media(&s));
        let mut got = Vec::new();
        loop {
            let f = p.next_fetch();
            got.push(uri(&f));
            match f {
                Fetch::Media(_) => p.fetched(&f, 1),
                _ => break,
            }
        }
        assert_eq!(got, ["103.ts", "104.ts", "105.ts", "playlist?"]);
        let Fetch::Playlist { wait, .. } = p.next_fetch() else {
            panic!()
        };
        assert_eq!(wait, Duration::from_secs(3));
        p.playlist(media(&(s + "#EXT-X-ENDLIST\n")));
        assert_eq!(uri(&p.next_fetch()), "ended");
    }

    /// Byte ranges without an offset continue after the previous range of the resource.
    #[test]
    fn byte_ranges_continue() {
        let text = "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-PART-INF:PART-TARGET=1\n#EXT-X-SERVER-CONTROL:PART-HOLD-BACK=3\n#EXT-X-MEDIA-SEQUENCE:1\n#EXT-X-PART:DURATION=1,URI=\"s1.mp4\",BYTERANGE=\"100@0\",INDEPENDENT=YES\n#EXT-X-PART:DURATION=1,URI=\"s1.mp4\",BYTERANGE=\"50\"\n#EXTINF:2,\ns1.mp4\n#EXT-X-PART:DURATION=1,URI=\"s2.mp4\",BYTERANGE=\"70@0\",INDEPENDENT=YES\n";
        let mut p = Planner::new(base());
        p.playlist(media(text));
        let f = p.next_fetch();
        let Fetch::Media(m) = &f else { panic!() };
        assert_eq!(m.range, Some((0, Some(100))));
        p.fetched(&f, 100);
        let Fetch::Media(m) = p.next_fetch() else {
            panic!()
        };
        assert_eq!(m.range, Some((100, Some(50))));
    }
}
