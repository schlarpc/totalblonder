//! Version numbers that change exactly when content changes (Rec. §2.4.4.5, p.49; A/65
//! §4.1; `notes/standards-digest.md` §1.3: "+1 mod 32 when the table changes").
//!
//! A table is identified by a [`Key`]. Its content is compared through a digest of the
//! sections built with version 0, so the version field itself never makes the content look
//! changed. The state can be exported and imported so versions keep incrementing across
//! restarts (SPEC §7.3).

use std::collections::HashMap;

use tb_ts::section::{Section, Version};

use crate::Error;

/// Which table instance a version belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Key {
    /// The PID it is sent on.
    pub pid: u16,
    /// Its table_id.
    pub table_id: u8,
    /// Its table_id_extension, or `None` when the version covers every extension on the PID
    /// (EIT-k and ETT-k instances share their MGT entry's version).
    pub extension: Option<u16>,
}

/// One stored version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stored {
    /// The version last used.
    pub version: Version,
    /// Digest of the content it was used for.
    pub digest: u64,
}

/// The versions of every table of a carrier.
#[derive(Debug, Clone, Default)]
pub struct Versions {
    map: HashMap<Key, Stored>,
    /// Bumped since the last export (the caller persists when this is non-zero).
    pub dirty: u32,
}

/// FNV-1a, 64 bits: stable across builds and platforms, enough to tell versions apart.
pub fn digest(sections: &[Section]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for s in sections {
        for &b in s.bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        // Section boundary marker, so [ab][c] and [a][bc] differ.
        h ^= 0xFF;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

impl Versions {
    /// Empty state: every table starts at version 0.
    pub fn new() -> Self {
        Self::default()
    }

    /// State restored from an export.
    pub fn import(entries: impl IntoIterator<Item = (Key, Stored)>) -> Self {
        Versions {
            map: entries.into_iter().collect(),
            dirty: 0,
        }
    }

    /// Every stored entry, for persistence.
    pub fn export(&mut self) -> Vec<(Key, Stored)> {
        self.dirty = 0;
        let mut v: Vec<_> = self.map.iter().map(|(k, s)| (*k, *s)).collect();
        v.sort_by_key(|(k, _)| *k);
        v
    }

    /// Builds the sections of `key` with the right version: unchanged content keeps its
    /// version, changed content gets the next one, a new table starts at 0.
    pub fn stamp(
        &mut self,
        key: Key,
        build: impl Fn(Version) -> Result<Vec<Section>, Error>,
    ) -> Result<(Version, Vec<Section>), Error> {
        let probe = build(Version::new(0))?;
        let d = digest(&probe);
        let v = match self.map.get(&key) {
            Some(s) if s.digest == d => s.version,
            Some(s) => {
                self.dirty += 1;
                s.version.next()
            }
            None => {
                self.dirty += 1;
                Version::new(0)
            }
        };
        self.map.insert(
            key,
            Stored {
                version: v,
                digest: d,
            },
        );
        let sections = if v.get() == 0 { probe } else { build(v)? };
        Ok((v, sections))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tb_ts::section::{Kind, Long, TableId};

    fn build(content: u8) -> impl Fn(Version) -> Result<Vec<Section>, Error> {
        move |v| {
            let h = Long {
                table_id: TableId::PAT,
                extension: 1,
                version: v,
                current: true,
                number: 0,
                last: 0,
            };
            Ok(vec![Section::long(Kind::Psi, h, &[content])?])
        }
    }

    #[test]
    fn bumps_only_on_change_and_survives_restart() {
        let k = Key {
            pid: 0,
            table_id: 0,
            extension: Some(1),
        };
        let mut v = Versions::new();
        assert_eq!(v.stamp(k, build(1)).unwrap().0.get(), 0);
        assert_eq!(v.stamp(k, build(1)).unwrap().0.get(), 0);
        let (ver, s) = v.stamp(k, build(2)).unwrap();
        assert_eq!(ver.get(), 1);
        assert_eq!(
            tb_ts::section::Parsed::parse(s[0].bytes())
                .unwrap()
                .header
                .version
                .get(),
            1
        );
        let mut w = Versions::import(v.export());
        assert_eq!(w.stamp(k, build(2)).unwrap().0.get(), 1);
        assert_eq!(w.stamp(k, build(3)).unwrap().0.get(), 2);
    }

    #[test]
    fn wraps_at_32() {
        let k = Key {
            pid: 0,
            table_id: 0,
            extension: Some(1),
        };
        let mut v = Versions::new();
        for i in 0..33u8 {
            assert_eq!(v.stamp(k, build(i)).unwrap().0.get(), i % 32);
        }
    }
}
