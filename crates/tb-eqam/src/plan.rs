//! Desired state: the RF plan (four modules) and the stream table (16 slots).

use std::collections::{BTreeMap, BTreeSet};

use crate::pages::streaminfo::DetectedProgram;
use crate::types::{
    Channel, FirstChannel, OutputLevel, QamMap, QamMode, RfModule, Slot, StreamName, StreamUrl,
};

/// A plan that cannot be applied.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlanError {
    /// Two carriers on the same channel (the page's `check_duplicates()` refuses this).
    #[error("channel {channel} used by modules {a} and {b}")]
    DuplicateChannel {
        /// The channel.
        channel: u8,
        /// One module.
        a: u8,
        /// The other.
        b: u8,
    },
    /// Two slots with the same stream URL (manual §4.1: no duplicates).
    #[error("stream URL {url} used by slots {a} and {b}")]
    DuplicateUrl {
        /// The URL.
        url: String,
        /// One slot.
        a: u8,
        /// The other.
        b: u8,
    },
}

/// One RF module's settings (one `qamconfig.cgi` page).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ModulePlan {
    /// Channel of carrier A; B–D follow from the device's table.
    pub first_channel: FirstChannel,
    /// QAM mode.
    pub qam: QamMode,
    /// Channel plan.
    pub map: QamMap,
    /// Carriers A–D on or off.
    pub carriers_on: [bool; 4],
}

impl ModulePlan {
    /// The four carrier channels.
    pub fn channels(&self) -> [Channel; 4] {
        self.first_channel.carriers()
    }
}

/// The whole RF plan. It is only ever written as the four-module sequence of SPEC §10.3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RfPlan {
    level: OutputLevel,
    modules: [ModulePlan; 4],
}

impl RfPlan {
    /// A plan with no channel used twice.
    pub fn new(level: OutputLevel, modules: [ModulePlan; 4]) -> Result<Self, PlanError> {
        check_unique_channels(&modules.map(|m| Some(m.channels())))?;
        Ok(RfPlan { level, modules })
    }

    /// The global output level (only module 1's page sets it).
    pub fn level(&self) -> OutputLevel {
        self.level
    }

    /// Module `m`'s settings.
    pub fn module(&self, m: RfModule) -> &ModulePlan {
        &self.modules[m.index()]
    }

    /// All four.
    pub fn modules(&self) -> &[ModulePlan; 4] {
        &self.modules
    }

    /// The channel of `slot`'s carrier.
    pub fn channel_of(&self, slot: Slot) -> Channel {
        self.module(slot.module()).channels()[(usize::from(slot.get()) - 1) % 4]
    }

    /// Whether `slot`'s carrier is on.
    pub fn carrier_on(&self, slot: Slot) -> bool {
        self.module(slot.module()).carriers_on[(usize::from(slot.get()) - 1) % 4]
    }
}

/// Check that no channel appears on two modules. `None` entries are unknown and skipped.
pub(crate) fn check_unique_channels(mods: &[Option<[Channel; 4]>; 4]) -> Result<(), PlanError> {
    let mut seen: BTreeMap<u8, u8> = BTreeMap::new();
    for (i, chans) in mods.iter().enumerate() {
        let Some(chans) = chans else { continue };
        let m = u8::try_from(i + 1).unwrap_or(0);
        for c in chans {
            if let Some(&other) = seen.get(&c.get()) {
                return Err(PlanError::DuplicateChannel {
                    channel: c.get(),
                    a: other,
                    b: m,
                });
            }
            seen.insert(c.get(), m);
        }
    }
    Ok(())
}

/// One input slot's desired content.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SlotConfig {
    /// No stream: the URL and name fields are posted empty.
    Empty,
    /// A stream and its name.
    Assigned {
        /// Stream URL.
        url: StreamUrl,
        /// Name.
        name: StreamName,
    },
}

impl SlotConfig {
    /// The `streamGroupAddress` value to post.
    pub fn url_text(&self) -> String {
        match self {
            SlotConfig::Empty => String::new(),
            SlotConfig::Assigned { url, .. } => url.to_string(),
        }
    }

    /// The `streamName` value to post.
    pub fn name_text(&self) -> String {
        match self {
            SlotConfig::Empty => String::new(),
            SlotConfig::Assigned { name, .. } => name.to_string(),
        }
    }
}

/// All 16 slots. Writes always carry all of them (the CGI zeroes rows missing from a POST).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StreamTable(Box<[SlotConfig; 16]>);

impl StreamTable {
    /// A table with no URL used twice.
    pub fn new(slots: [SlotConfig; 16]) -> Result<Self, PlanError> {
        let mut seen: BTreeMap<StreamUrl, u8> = BTreeMap::new();
        for (i, s) in slots.iter().enumerate() {
            if let SlotConfig::Assigned { url, .. } = s {
                let n = u8::try_from(i + 1).unwrap_or(0);
                if let Some(&a) = seen.get(url) {
                    return Err(PlanError::DuplicateUrl {
                        url: url.to_string(),
                        a,
                        b: n,
                    });
                }
                seen.insert(*url, n);
            }
        }
        Ok(StreamTable(Box::new(slots)))
    }

    /// Every slot empty.
    pub fn empty() -> Self {
        StreamTable(Box::new(std::array::from_fn(|_| SlotConfig::Empty)))
    }

    /// The table with `slot` set to `config` (checked again for duplicates).
    pub fn with(&self, slot: Slot, config: SlotConfig) -> Result<Self, PlanError> {
        let mut s = (*self.0).clone();
        s[slot.index()] = config;
        StreamTable::new(s)
    }

    /// Slot `slot`.
    pub fn get(&self, slot: Slot) -> &SlotConfig {
        &self.0[slot.index()]
    }

    /// All slots in order.
    pub fn slots(&self) -> &[SlotConfig; 16] {
        &self.0
    }

    /// Slots that are assigned.
    pub fn assigned(&self) -> BTreeSet<Slot> {
        Slot::all()
            .filter(|s| matches!(self.get(*s), SlotConfig::Assigned { .. }))
            .collect()
    }
}

/// What the supervisor keeps the device at (SPEC §10.2): MPTS mode (checked, never written),
/// the RF plan, and one URL and name per slot. It also holds what `streaminfo.cgi` should
/// detect in each stream; that is only compared, never written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesiredState {
    /// RF plan.
    pub rf: RfPlan,
    /// Stream table.
    pub streams: StreamTable,
    /// The programs each slot's stream carries, as Stream Info shows them. Slots left out
    /// are not compared.
    pub programs: BTreeMap<Slot, Vec<DetectedProgram>>,
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    /// The plan the live unit has today (device §4).
    pub fn live_plan() -> RfPlan {
        let m = |ch| ModulePlan {
            first_channel: FirstChannel::new(ch).unwrap(),
            qam: QamMode::Qam256B,
            map: QamMap::Std,
            carriers_on: [true; 4],
        };
        RfPlan::new(OutputLevel::new(40).unwrap(), [m(2), m(10), m(6), m(140)]).unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicates_are_refused() {
        let m = |ch| ModulePlan {
            first_channel: FirstChannel::new(ch).unwrap(),
            qam: QamMode::Qam256B,
            map: QamMap::Std,
            carriers_on: [true; 4],
        };
        let lv = OutputLevel::new(40).unwrap();
        // 3 overlaps 2,3,4,5.
        assert!(matches!(
            RfPlan::new(lv, [m(2), m(3), m(6), m(140)]),
            Err(PlanError::DuplicateChannel {
                channel: 3,
                a: 1,
                b: 2
            })
        ));
        // 94 -> 100, 95: 6 -> 95 collides with 94's... 94,100,101,102 vs 6,95,96,97: fine.
        assert!(RfPlan::new(lv, [m(2), m(10), m(6), m(94)]).is_ok());
        let p = testing::live_plan();
        assert_eq!(p.channel_of(Slot::new(10).unwrap()).get(), 95);
        let u: StreamUrl = "UDP://239.1.1.1:5000".parse().unwrap();
        let a = SlotConfig::Assigned {
            url: u,
            name: "A".parse().unwrap(),
        };
        let t = StreamTable::empty()
            .with(Slot::new(1).unwrap(), a.clone())
            .unwrap();
        assert!(matches!(
            t.with(Slot::new(2).unwrap(), a),
            Err(PlanError::DuplicateUrl { a: 1, b: 2, .. })
        ));
    }
}
