//! Elementary-stream framing: just enough of MPEG-2 video, AVC and AC-3 to find access units
//! and the parameters the buffer model and the descriptor checks need.

/// What a scanner found, with offsets in the elementary stream (payload bytes of the PID's
/// PES packets, counted from where scanning began).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum EsEvent {
    /// An access unit begins at this offset (MPEG-2: the first of a sequence header, GOP
    /// header and picture start code before a picture; AVC: an access unit delimiter; AC-3:
    /// a syncframe).
    AuStart { off: u64 },
    /// The access unit begun last ends at this offset (AC-3 only: its size is in its header).
    AuEnd { off: u64 },
    /// The last MPEG-2 picture is a field picture (picture_coding_extension).
    FieldPicture,
    /// A sequence header with its extension was parsed (MPEG-2).
    Sequence,
    /// An AVC sequence parameter set was found.
    AvcSps,
    /// AC-3 framing was lost at this offset (no syncword where one was due, or a bad header).
    Ac3SyncLost { off: u64, why: &'static str },
}

/// Parameters of an MPEG-2 video sequence.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct M2vSeq {
    pub(crate) bit_rate: f64,
    pub(crate) vbv_bits: f64,
    pub(crate) frame_rate: f64,
    pub(crate) profile_level: Option<u8>,
    pub(crate) low_delay: bool,
}

/// Scans for start codes 0x000001xx across chunk boundaries.
#[derive(Debug, Clone, Default)]
struct StartCodes {
    zeros: u8,
    after_prefix: bool,
}

impl StartCodes {
    /// Calls `f(index_of_code_byte, code)` for every start code whose code byte is in
    /// `data`. `f` returns how many bytes after the code byte to skip (captures are taken by
    /// the caller from `data`).
    fn scan(&mut self, data: &[u8], mut f: impl FnMut(usize, u8)) {
        let n = data.len();
        let mut i = 0;
        while i < n {
            if self.after_prefix {
                self.after_prefix = false;
                f(i, data[i]);
                self.zeros = u8::from(data[i] == 0);
                i += 1;
                continue;
            }
            match data[i..].iter().position(|&b| b == 1) {
                None => {
                    let tail = data[i..].iter().rev().take_while(|&&b| b == 0).count();
                    if tail == n - i {
                        self.zeros = (usize::from(self.zeros) + tail).min(2) as u8;
                    } else {
                        self.zeros = tail.min(2) as u8;
                    }
                    return;
                }
                Some(k) => {
                    let j = i + k;
                    let run = data[i..j].iter().rev().take_while(|&&b| b == 0).count();
                    let run = if run == k {
                        run + usize::from(self.zeros)
                    } else {
                        run
                    };
                    self.zeros = 0;
                    if run >= 2 {
                        self.after_prefix = true;
                    }
                    i = j + 1;
                }
            }
        }
    }
}

/// MPEG-2 video scanner.
#[derive(Debug, Clone, Default)]
pub(crate) struct M2v {
    sc: StartCodes,
    /// Code being captured and the bytes so far.
    cap_code: u8,
    cap: Vec<u8>,
    cap_need: usize,
    header_group_start: Option<u64>,
    pub(crate) seq: M2vSeq,
    seq_value: Option<(u32, u32, u8)>,
    pub(crate) pictures: u64,
    hits: Vec<(usize, u8)>,
}

fn frame_rate(code: u8) -> f64 {
    match code {
        1 => 24000.0 / 1001.0,
        2 => 24.0,
        3 => 25.0,
        4 => 30000.0 / 1001.0,
        5 => 30.0,
        6 => 50.0,
        7 => 60000.0 / 1001.0,
        8 => 60.0,
        _ => 0.0,
    }
}

impl M2v {
    /// Forgets the scanning state (after lost packets), keeping the sequence parameters.
    pub(crate) fn reset_scan(&mut self) {
        *self = M2v {
            seq: self.seq,
            seq_value: self.seq_value,
            ..M2v::default()
        };
    }

    /// Scans `data`, whose first byte is at elementary-stream offset `base`.
    pub(crate) fn push(&mut self, data: &[u8], base: u64, ev: &mut Vec<EsEvent>) {
        if self.cap_need > 0 {
            let take = self.cap_need.min(data.len());
            self.cap.extend_from_slice(&data[..take]);
            self.cap_need -= take;
            if self.cap_need == 0 {
                self.finish_capture(ev);
            }
        }
        let mut hits = std::mem::take(&mut self.hits);
        hits.clear();
        self.sc.scan(data, |i, c| hits.push((i, c)));
        for &(idx, code) in &hits {
            self.code(code, (base + idx as u64).saturating_sub(3), ev);
            if self.cap_need > 0 && self.cap.is_empty() {
                let from = idx + 1;
                let take = self.cap_need.min(data.len() - from);
                self.cap.extend_from_slice(&data[from..from + take]);
                self.cap_need -= take;
                if self.cap_need == 0 {
                    self.finish_capture(ev);
                }
            }
        }
        self.hits = hits;
    }

    fn code(&mut self, code: u8, off: u64, ev: &mut Vec<EsEvent>) {
        match code {
            0xB3 => {
                self.header_group_start.get_or_insert(off);
                self.cap_code = code;
                self.cap.clear();
                self.cap_need = 8;
            }
            0xB8 => {
                self.header_group_start.get_or_insert(off);
            }
            0xB5 => {
                self.cap_code = code;
                self.cap.clear();
                self.cap_need = 6;
            }
            0x00 => {
                let start = self.header_group_start.take().unwrap_or(off);
                self.pictures += 1;
                ev.push(EsEvent::AuStart { off: start });
            }
            _ => {}
        }
    }

    fn finish_capture(&mut self, ev: &mut Vec<EsEvent>) {
        let c = &self.cap;
        match self.cap_code {
            0xB3 if c.len() >= 8 => {
                let rate_value =
                    (u32::from(c[4]) << 10) | (u32::from(c[5]) << 2) | u32::from(c[6] >> 6);
                let vbv_value = (u32::from(c[6] & 0x1F) << 5) | u32::from(c[7] >> 3);
                self.seq_value = Some((rate_value, vbv_value, c[3] & 0x0F));
            }
            0xB5 if c.len() >= 6 => match c[0] >> 4 {
                1 => {
                    let pl = ((c[0] & 0x0F) << 4) | (c[1] >> 4);
                    let rate_ext = (u32::from(c[2] & 0x1F) << 7) | u32::from(c[3] >> 1);
                    let vbv_ext = u32::from(c[4]);
                    let low_delay = c[5] & 0x80 != 0;
                    let frn = f64::from((c[5] >> 5) & 3);
                    let frd = f64::from(c[5] & 0x1F);
                    if let Some((rv, vv, frc)) = self.seq_value {
                        let seq = M2vSeq {
                            bit_rate: f64::from((rate_ext << 18) | rv) * 400.0,
                            vbv_bits: f64::from((vbv_ext << 10) | vv) * 16384.0,
                            frame_rate: frame_rate(frc) * (frn + 1.0) / (frd + 1.0),
                            profile_level: Some(pl),
                            low_delay,
                        };
                        if seq != self.seq {
                            self.seq = seq;
                            ev.push(EsEvent::Sequence);
                        }
                    }
                }
                8 if c[2] & 3 != 3 => {
                    ev.push(EsEvent::FieldPicture);
                }
                _ => {}
            },
            _ => {}
        }
    }
}

/// AVC scanner: access unit delimiters and the level of the sequence parameter set.
#[derive(Debug, Clone, Default)]
pub(crate) struct Avc {
    sc: StartCodes,
    cap: Vec<u8>,
    cap_need: usize,
    pub(crate) profile_idc: Option<u8>,
    pub(crate) level_idc: Option<u8>,
    pub(crate) constraint3: bool,
    pub(crate) auds: u64,
}

impl Avc {
    pub(crate) fn push(&mut self, data: &[u8], base: u64, ev: &mut Vec<EsEvent>) {
        let mut hits: Vec<(usize, u8)> = Vec::new();
        let mut i = 0;
        if self.cap_need > 0 {
            let take = self.cap_need.min(data.len());
            self.cap.extend_from_slice(&data[..take]);
            self.cap_need -= take;
            if self.cap_need == 0 {
                self.sps(ev);
            }
            i = 0;
        }
        self.sc.scan(&data[i..], |idx, code| hits.push((idx, code)));
        for (idx, code) in hits {
            match code & 0x1F {
                9 if code & 0x80 == 0 => {
                    self.auds += 1;
                    ev.push(EsEvent::AuStart {
                        off: base + idx as u64 - 3,
                    });
                }
                7 if code & 0x80 == 0 => {
                    self.cap.clear();
                    let have = data.len() - (idx + 1);
                    let take = have.min(4);
                    self.cap.extend_from_slice(&data[idx + 1..idx + 1 + take]);
                    self.cap_need = 4 - take;
                    if self.cap_need == 0 {
                        self.sps(ev);
                    }
                }
                _ => {}
            }
        }
    }

    fn sps(&mut self, ev: &mut Vec<EsEvent>) {
        if self.cap.len() >= 3 {
            self.profile_idc = Some(self.cap[0]);
            self.constraint3 = self.cap[1] & 0x10 != 0;
            self.level_idc = Some(self.cap[2]);
            ev.push(EsEvent::AvcSps);
        }
    }
}

/// One AC-3 syncframe header.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Ac3Header {
    pub(crate) fscod: u8,
    pub(crate) frmsizecod: u8,
    pub(crate) bsid: u8,
    pub(crate) bsmod: u8,
    pub(crate) acmod: u8,
    /// Frame size in bytes.
    pub(crate) size: usize,
}

/// Words per syncframe at 48 kHz, 44.1 kHz and 32 kHz by frmsizecod / 2 (A/52 Table 5.18).
const AC3_WORDS: [[u16; 3]; 19] = [
    [64, 69, 96],
    [80, 87, 120],
    [96, 104, 144],
    [112, 121, 168],
    [128, 139, 192],
    [160, 174, 240],
    [192, 208, 288],
    [224, 243, 336],
    [256, 278, 384],
    [320, 348, 480],
    [384, 417, 576],
    [448, 487, 672],
    [512, 557, 768],
    [640, 696, 960],
    [768, 835, 1152],
    [896, 975, 1344],
    [1024, 1114, 1536],
    [1152, 1253, 1728],
    [1280, 1393, 1920],
];

/// Nominal bit rate in kbit/s by frmsizecod / 2 (A/52 Table 5.18).
pub(crate) const AC3_KBPS: [u16; 19] = [
    32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384, 448, 512, 576, 640,
];

fn ac3_header(h: &[u8]) -> Result<Ac3Header, &'static str> {
    if h[0] != 0x0B || h[1] != 0x77 {
        return Err("no syncword 0x0B77 where the previous syncframe ends");
    }
    let fscod = h[4] >> 6;
    let frmsizecod = h[4] & 0x3F;
    if fscod == 3 {
        return Err("fscod 11 (reserved)");
    }
    if frmsizecod > 37 {
        return Err("frmsizecod above 37");
    }
    let mut words = usize::from(AC3_WORDS[usize::from(frmsizecod / 2)][usize::from(fscod)]);
    if fscod == 1 && frmsizecod & 1 == 1 {
        words += 1;
    }
    Ok(Ac3Header {
        fscod,
        frmsizecod,
        bsid: h[5] >> 3,
        bsmod: h[5] & 7,
        acmod: h[6] >> 5,
        size: words * 2,
    })
}

/// AC-3 framing.
#[derive(Debug, Clone, Default)]
pub(crate) struct Ac3 {
    /// Bytes of the current header being collected.
    hdr: Vec<u8>,
    hdr_off: u64,
    /// Bytes left of the current frame after its header.
    left: usize,
    in_frame: bool,
    hunting: bool,
    prev_0b: bool,
    pub(crate) last: Option<Ac3Header>,
    pub(crate) frames: u64,
    started: bool,
}

impl Ac3 {
    /// True when framing is established and the next byte is not where a syncframe is due.
    pub(crate) fn misaligned(&self) -> bool {
        !self.hunting && (self.in_frame || !self.hdr.is_empty())
    }

    /// Restarts by hunting for a syncword (after lost packets).
    pub(crate) fn resync(&mut self) {
        self.hunting = true;
        self.hdr.clear();
        self.in_frame = false;
        self.left = 0;
        self.prev_0b = false;
    }

    pub(crate) fn push(&mut self, data: &[u8], base: u64, ev: &mut Vec<EsEvent>) {
        if !self.started {
            self.started = true;
            self.hunting = true;
        }
        let mut i = 0;
        while i < data.len() {
            if self.in_frame {
                let take = self.left.min(data.len() - i);
                self.left -= take;
                i += take;
                if self.left == 0 {
                    self.in_frame = false;
                    ev.push(EsEvent::AuEnd {
                        off: base + i as u64,
                    });
                }
                continue;
            }
            if self.hunting {
                // Find 0x0B 0x77.
                let mut found = None;
                if self.prev_0b && data[i] == 0x77 {
                    found = Some(i as i64 - 1);
                } else {
                    for k in i..data.len().saturating_sub(1) {
                        if data[k] == 0x0B && data[k + 1] == 0x77 {
                            found = Some(k as i64);
                            break;
                        }
                    }
                }
                match found {
                    Some(k) => {
                        self.hunting = false;
                        self.prev_0b = false;
                        self.hdr.clear();
                        if k < i as i64 {
                            self.hdr.push(0x0B);
                            self.hdr_off = base + i as u64 - 1;
                        } else {
                            i = k as usize;
                            self.hdr_off = base + i as u64;
                        }
                    }
                    None => {
                        self.prev_0b = data.last() == Some(&0x0B);
                        return;
                    }
                }
            }
            if self.hdr.is_empty() {
                self.hdr_off = base + i as u64;
            }
            let take = (7 - self.hdr.len()).min(data.len() - i);
            self.hdr.extend_from_slice(&data[i..i + take]);
            i += take;
            if self.hdr.len() < 7 {
                return;
            }
            match ac3_header(&self.hdr) {
                Ok(h) => {
                    self.frames += 1;
                    self.last = Some(h);
                    ev.push(EsEvent::AuStart { off: self.hdr_off });
                    self.left = h.size.saturating_sub(7);
                    self.in_frame = true;
                    self.hdr.clear();
                    if self.left == 0 {
                        self.in_frame = false;
                        ev.push(EsEvent::AuEnd {
                            off: base + i as u64,
                        });
                    }
                }
                Err(why) => {
                    ev.push(EsEvent::Ac3SyncLost {
                        off: self.hdr_off,
                        why,
                    });
                    // Hunt again from the byte after the bad header's first byte.
                    let back = self.hdr.len() - 1;
                    self.hdr.clear();
                    self.hunting = true;
                    self.prev_0b = false;
                    i = i.saturating_sub(back);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_codes_across_chunks() {
        let data = [0u8, 0, 1, 0xB3, 9, 0, 0, 0, 1, 0x00, 7, 0, 0];
        for split in 0..data.len() {
            let mut sc = StartCodes::default();
            let mut got = Vec::new();
            sc.scan(&data[..split], |i, c| got.push((i, c)));
            let mut more = Vec::new();
            sc.scan(&data[split..], |i, c| more.push((i + split, c)));
            got.extend(more);
            assert_eq!(got, vec![(3, 0xB3), (9, 0x00)], "split {split}");
        }
    }

    #[test]
    fn m2v_picture_after_sequence_header() {
        let mut es = vec![
            0u8, 0, 1, 0xB3, 0x50, 0x02, 0xD0, 0x37, 0xFF, 0xFF, 0xE0, 0x18,
        ];
        es.extend_from_slice(&[0, 0, 1, 0xB5, 0x14, 0x8A, 0x00, 0x01, 0x00, 0x00]);
        es.extend_from_slice(&[0, 0, 1, 0xB8, 0, 0, 0, 0]);
        es.extend_from_slice(&[0, 0, 1, 0x00, 0, 0x0F, 0xFF, 0xF8]);
        es.extend_from_slice(&[0, 0, 1, 0x01, 0x12]);
        es.extend_from_slice(&[0, 0, 1, 0x00, 0, 0x0F, 0xFF, 0xF8]);
        for split in [1usize, 5, 13, 20, 30] {
            let mut m = M2v::default();
            let mut ev = Vec::new();
            m.push(&es[..split], 0, &mut ev);
            m.push(&es[split..], split as u64, &mut ev);
            let aus: Vec<u64> = ev
                .iter()
                .filter_map(|e| match e {
                    EsEvent::AuStart { off } => Some(*off),
                    _ => None,
                })
                .collect();
            assert_eq!(aus, vec![0, 43], "split {split}");
            assert_eq!(m.seq.profile_level, Some(0x48));
            assert!(m.seq.bit_rate > 0.0);
        }
    }

    #[test]
    fn ac3_frames() {
        // Two 448 kbit/s frames at 48 kHz: 1792 bytes each.
        let mut es = Vec::new();
        for _ in 0..2 {
            let mut f = vec![0u8; 1792];
            f[0] = 0x0B;
            f[1] = 0x77;
            f[4] = 0x1E; // fscod 0, frmsizecod 30
            f[5] = 0x40; // bsid 8, bsmod 0
            f[6] = 0x40; // acmod 2
            es.extend(f);
        }
        let mut a = Ac3::default();
        let mut ev = Vec::new();
        for (k, c) in es.chunks(184).enumerate() {
            a.push(c, (k * 184) as u64, &mut ev);
        }
        assert_eq!(a.frames, 2);
        assert!(ev.contains(&EsEvent::AuStart { off: 1792 }));
        assert!(ev.contains(&EsEvent::AuEnd { off: 3584 }));
        assert_eq!(
            a.last.map(|h| (h.bsid, h.acmod, h.size)),
            Some((8, 2, 1792))
        );
    }
}
