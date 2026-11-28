//! Which of a live input's audio streams feeds which of a program's audio tracks (SPEC §8.3,
//! second audio). An input may carry several audio streams (an MPEG-TS input with an English
//! and a Spanish PID, say); each track takes one, chosen in this order:
//!
//! 1. `input_track = N` in the track's configuration: the input's N-th audio stream (1-based,
//!    in the order the input announces them).
//! 2. The first stream not yet taken whose language is the track's.
//! 3. The main track (the first) still without one: the first stream not yet taken.
//! 4. Any other track still without one: the first stream not yet taken that has no language
//!    (or `und`), so two unlabeled streams fill the main and second tracks in order.
//! 5. A track still without one carries the main track's stream, as a station without a second
//!    language does; its own downmix makes the layout fit.
//!
//! A stream can feed several tracks (rules 1 and 5). Streams no track takes are not decoded.

use tb_media::StreamIndex;

/// One audio stream of the input, in announcement order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputAudio {
    /// The stream.
    pub index: StreamIndex,
    /// Its ISO 639-2 language, if the input gives one.
    pub language: Option<[u8; 3]>,
}

/// What one track asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrackWant {
    /// The track's language.
    pub language: [u8; 3],
    /// `input_track`: an explicit 1-based position among the input's audio streams.
    pub input_track: Option<u8>,
}

/// The stream each track takes (`None`: the input has no audio at all, or the explicit
/// position does not exist and nothing else is left).
pub fn assign(streams: &[InputAudio], tracks: &[TrackWant]) -> Vec<Option<StreamIndex>> {
    let mut out: Vec<Option<StreamIndex>> = vec![None; tracks.len()];
    let mut taken = vec![false; streams.len()];
    // 1. explicit positions
    for (k, t) in tracks.iter().enumerate() {
        if let Some(n) = t.input_track
            && let Some(i) = usize::from(n).checked_sub(1)
            && let Some(s) = streams.get(i)
        {
            out[k] = Some(s.index);
            taken[i] = true;
        }
    }
    // 2. language
    for (k, t) in tracks.iter().enumerate() {
        if out[k].is_some() {
            continue;
        }
        if let Some(i) =
            (0..streams.len()).find(|&i| !taken[i] && streams[i].language == Some(t.language))
        {
            out[k] = Some(streams[i].index);
            taken[i] = true;
        }
    }
    // 3. the main track takes what is left first
    if let Some(None) = out.first()
        && let Some(i) = (0..streams.len()).find(|&i| !taken[i])
    {
        out[0] = Some(streams[i].index);
        taken[i] = true;
    }
    // 4. unlabeled streams, in order
    let unlabeled = |l: Option<[u8; 3]>| l.is_none_or(|l| &l == b"und");
    for o in out.iter_mut().skip(1).filter(|o| o.is_none()) {
        if let Some(i) = (0..streams.len()).find(|&i| !taken[i] && unlabeled(streams[i].language)) {
            *o = Some(streams[i].index);
            taken[i] = true;
        }
    }
    // 5. the main track's stream
    let main = out.first().copied().flatten();
    for o in out.iter_mut().skip(1) {
        if o.is_none() {
            *o = main;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(i: u16, lang: Option<&[u8; 3]>) -> InputAudio {
        InputAudio {
            index: StreamIndex(i),
            language: lang.copied(),
        }
    }
    fn t(lang: &[u8; 3]) -> TrackWant {
        TrackWant {
            language: *lang,
            input_track: None,
        }
    }
    fn ix(v: &[Option<u16>]) -> Vec<Option<StreamIndex>> {
        v.iter().map(|o| o.map(StreamIndex)).collect()
    }

    #[test]
    fn languages_pick_their_streams_in_any_order() {
        let streams = [s(0x45, Some(b"spa")), s(0x44, Some(b"eng"))];
        assert_eq!(
            assign(&streams, &[t(b"eng"), t(b"spa")]),
            ix(&[Some(0x44), Some(0x45)])
        );
    }

    #[test]
    fn one_unlabeled_stream_feeds_every_track() {
        // RTMP: one audio stream, no language. The second track carries the main audio.
        let streams = [s(1, None)];
        assert_eq!(
            assign(&streams, &[t(b"eng"), t(b"spa")]),
            ix(&[Some(1), Some(1)])
        );
    }

    #[test]
    fn unlabeled_streams_fill_tracks_in_order() {
        let streams = [s(1, None), s(2, Some(b"und")), s(3, None)];
        assert_eq!(
            assign(&streams, &[t(b"eng"), t(b"spa"), t(b"fra")]),
            ix(&[Some(1), Some(2), Some(3)])
        );
    }

    #[test]
    fn a_missing_language_falls_back_to_the_main_stream_not_another_language() {
        // A French stream is not passed off as Spanish.
        let streams = [s(1, Some(b"eng")), s(2, Some(b"fra"))];
        assert_eq!(
            assign(&streams, &[t(b"eng"), t(b"spa")]),
            ix(&[Some(1), Some(1)])
        );
    }

    #[test]
    fn the_main_track_takes_any_stream_when_its_language_is_absent() {
        let streams = [s(1, Some(b"fra")), s(2, Some(b"spa"))];
        assert_eq!(
            assign(&streams, &[t(b"eng"), t(b"spa")]),
            ix(&[Some(1), Some(2)])
        );
    }

    #[test]
    fn explicit_positions_win_and_may_share() {
        let streams = [
            s(10, Some(b"eng")),
            s(11, Some(b"eng")),
            s(12, Some(b"spa")),
        ];
        let mut dv = t(b"eng");
        dv.input_track = Some(2);
        assert_eq!(
            assign(&streams, &[t(b"eng"), dv, t(b"spa")]),
            ix(&[Some(10), Some(11), Some(12)])
        );
        // A position past the end is ignored.
        let mut far = t(b"spa");
        far.input_track = Some(9);
        assert_eq!(
            assign(&streams, &[t(b"eng"), far]),
            ix(&[Some(10), Some(12)])
        );
    }

    #[test]
    fn no_audio_no_tracks_fed() {
        assert_eq!(assign(&[], &[t(b"eng"), t(b"spa")]), ix(&[None, None]));
        assert_eq!(assign(&[s(1, None)], &[]), ix(&[]));
    }
}
