//! What confinement took effect: the vocabulary, and the fixed-size records in which the
//! sandboxed side reports it. Pure: no I/O.
//!
//! Two pipes carry records from the child to the parent:
//!
//! - the **report pipe** (fd 5 in the child): one [`LayerReport`] per layer as each is put in
//!   place, written by the child between `clone` and `execve`, then by [`crate::child::enter`]
//!   and [`crate::child::Entered::lock_down`], which closes it. Everything on it is written
//!   before the sandboxed program has read any untrusted input, so it is believed; it is still
//!   parsed defensively (fixed size, known values, bounded count);
//! - the **status pipe** (fd 4): a set-up failure before `execve` ([`Status::SetupFailed`]),
//!   or, with a PID namespace, the exit of the program as the namespace's init saw it
//!   ([`Status::Exited`]). The program itself closes its copy, so it cannot forge one.

use std::fmt;

/// Bytes of one record.
pub const RECORD: usize = 16;
const MAGIC: u8 = 0xb5;
/// Most records the parent reads from one child.
pub const MAX_RECORDS: usize = 64;

/// One confinement layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Layer {
    /// A user namespace mapping only our own uid and gid.
    UserNs,
    /// A mount namespace whose root is a read-only tmpfs with only the listed paths bound in.
    MountNs,
    /// A PID namespace: the program sees no process outside, and dies with its init.
    PidNs,
    /// A network namespace with no interfaces but a loopback that is down.
    NetNs,
    /// An IPC namespace (System V IPC, POSIX message queues).
    IpcNs,
    /// A UTS namespace (own host name).
    UtsNs,
    /// `PR_SET_NO_NEW_PRIVS`.
    NoNewPrivs,
    /// `PR_SET_PDEATHSIG(SIGKILL)` in the program (and its init).
    DeathSignal,
    /// Every inherited file descriptor closed but the IPC ones.
    CloseFds,
    /// Resource limits (`RLIMIT_NOFILE`, `RLIMIT_CORE` = 0, optional `AS`, `CPU`).
    Rlimits,
    /// `PR_SET_DUMPABLE` 0: no core dumps, no `ptrace` or `/proc` access by our own uid.
    NotDumpable,
    /// Landlock: file system access only to read and execute the listed paths; no TCP; no
    /// abstract sockets or signals outside (by ABI level).
    Landlock,
    /// A seccomp-bpf allow-list of system calls.
    Seccomp,
}

impl Layer {
    /// Every layer, in the order they are put in place.
    pub const ALL: [Layer; 13] = [
        Layer::UserNs,
        Layer::MountNs,
        Layer::PidNs,
        Layer::NetNs,
        Layer::IpcNs,
        Layer::UtsNs,
        Layer::NoNewPrivs,
        Layer::DeathSignal,
        Layer::CloseFds,
        Layer::Rlimits,
        Layer::NotDumpable,
        Layer::Landlock,
        Layer::Seccomp,
    ];

    fn code(self) -> u8 {
        Self::ALL.iter().position(|l| *l == self).unwrap_or(0) as u8
    }

    fn from_code(c: u8) -> Option<Self> {
        Self::ALL.get(usize::from(c)).copied()
    }

    /// A short name.
    pub fn name(self) -> &'static str {
        match self {
            Layer::UserNs => "user-ns",
            Layer::MountNs => "mount-ns",
            Layer::PidNs => "pid-ns",
            Layer::NetNs => "net-ns",
            Layer::IpcNs => "ipc-ns",
            Layer::UtsNs => "uts-ns",
            Layer::NoNewPrivs => "no-new-privs",
            Layer::DeathSignal => "death-signal",
            Layer::CloseFds => "close-fds",
            Layer::Rlimits => "rlimits",
            Layer::NotDumpable => "not-dumpable",
            Layer::Landlock => "landlock",
            Layer::Seccomp => "seccomp",
        }
    }
}

/// How a layer ended up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LayerState {
    /// In effect.
    Applied,
    /// In effect in part (Landlock at a lower ABI than asked for; a step of the mount set-up
    /// after the root was changed).
    Partial,
    /// The kernel or the system does not offer it (`errno` says why).
    Unavailable,
    /// The policy turned it off.
    Off,
    /// It was tried and failed (`errno` says why).
    Failed,
}

impl LayerState {
    fn from_code(c: u8) -> Option<Self> {
        Some(match c {
            0 => Self::Applied,
            1 => Self::Partial,
            2 => Self::Unavailable,
            3 => Self::Off,
            4 => Self::Failed,
            _ => return None,
        })
    }
    /// Whether the layer is (at least partly) in effect.
    pub fn in_effect(self) -> bool {
        matches!(self, Self::Applied | Self::Partial)
    }
}

/// The outcome of one layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LayerReport {
    /// The layer.
    pub layer: Layer,
    /// Its state.
    pub state: LayerState,
    /// The error number when unavailable or failed, else 0.
    pub errno: i32,
    /// Layer-specific detail: the Landlock ABI in effect; the seccomp action (1 errno, 2 kill);
    /// for a mount failure, the step that failed.
    pub detail: u32,
}

impl LayerReport {
    /// A layer in effect.
    pub const fn applied(layer: Layer, detail: u32) -> Self {
        Self {
            layer,
            state: LayerState::Applied,
            errno: 0,
            detail,
        }
    }

    /// A layer in another state.
    pub const fn new(layer: Layer, state: LayerState, errno: i32, detail: u32) -> Self {
        Self {
            layer,
            state,
            errno,
            detail,
        }
    }

    /// The record's bytes (no allocation: written by the child before `execve`).
    pub const fn encode(&self) -> [u8; RECORD] {
        let e = self.errno.to_le_bytes();
        let d = self.detail.to_le_bytes();
        let mut r = [0u8; RECORD];
        r[0] = MAGIC;
        r[1] = KIND_LAYER;
        r[2] = layer_code_const(self.layer);
        r[3] = state_code_const(self.state);
        r[4] = e[0];
        r[5] = e[1];
        r[6] = e[2];
        r[7] = e[3];
        r[8] = d[0];
        r[9] = d[1];
        r[10] = d[2];
        r[11] = d[3];
        r
    }
}

const KIND_LAYER: u8 = 1;
const KIND_SETUP: u8 = 2;
const KIND_EXIT: u8 = 3;

const fn layer_code_const(l: Layer) -> u8 {
    match l {
        Layer::UserNs => 0,
        Layer::MountNs => 1,
        Layer::PidNs => 2,
        Layer::NetNs => 3,
        Layer::IpcNs => 4,
        Layer::UtsNs => 5,
        Layer::NoNewPrivs => 6,
        Layer::DeathSignal => 7,
        Layer::CloseFds => 8,
        Layer::Rlimits => 9,
        Layer::NotDumpable => 10,
        Layer::Landlock => 11,
        Layer::Seccomp => 12,
    }
}

const fn state_code_const(s: LayerState) -> u8 {
    match s {
        LayerState::Applied => 0,
        LayerState::Partial => 1,
        LayerState::Unavailable => 2,
        LayerState::Off => 3,
        LayerState::Failed => 4,
    }
}

/// Steps of the child's set-up, named in a [`Status::SetupFailed`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Stage {
    /// Writing `uid_map`, `gid_map`, `setgroups`.
    IdMaps,
    /// The mount namespace's set-up (when it is required).
    Mounts,
    /// The host name.
    Hostname,
    /// A resource limit.
    Rlimits,
    /// `PR_SET_NO_NEW_PRIVS`.
    NoNewPrivs,
    /// Moving file descriptors into place.
    Fds,
    /// `execve`.
    Exec,
    /// The program's own entry ([`crate::child::enter`]): a required layer failed there.
    Enter,
}

impl Stage {
    const ALL: [Stage; 8] = [
        Stage::IdMaps,
        Stage::Mounts,
        Stage::Hostname,
        Stage::Rlimits,
        Stage::NoNewPrivs,
        Stage::Fds,
        Stage::Exec,
        Stage::Enter,
    ];
}

const fn stage_code_const(s: Stage) -> u8 {
    match s {
        Stage::IdMaps => 0,
        Stage::Mounts => 1,
        Stage::Hostname => 2,
        Stage::Rlimits => 3,
        Stage::NoNewPrivs => 4,
        Stage::Fds => 5,
        Stage::Exec => 6,
        Stage::Enter => 7,
    }
}

/// A record of the status pipe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Status {
    /// The child's set-up failed at this stage with this error number; it exited.
    SetupFailed {
        /// The stage.
        stage: Stage,
        /// The error number.
        errno: i32,
    },
    /// The sandboxed program ended (reported by the PID namespace's init): an exit code ...
    Exited(i32),
    /// ... or a signal.
    Signaled(i32),
}

impl Status {
    /// The record's bytes.
    pub const fn encode(&self) -> [u8; RECORD] {
        let mut r = [0u8; RECORD];
        r[0] = MAGIC;
        let (kind, a, v) = match *self {
            Status::SetupFailed { stage, errno } => (KIND_SETUP, stage_code_const(stage), errno),
            Status::Exited(c) => (KIND_EXIT, 0, c),
            Status::Signaled(s) => (KIND_EXIT, 1, s),
        };
        r[1] = kind;
        r[2] = a;
        let b = v.to_le_bytes();
        r[4] = b[0];
        r[5] = b[1];
        r[6] = b[2];
        r[7] = b[3];
        r
    }
}

/// A record that does not parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("malformed sandbox record")]
pub struct BadRecord;

fn split(r: &[u8; RECORD]) -> Result<(u8, u8, u8, i32, u32), BadRecord> {
    if r[0] != MAGIC {
        return Err(BadRecord);
    }
    let v = i32::from_le_bytes([r[4], r[5], r[6], r[7]]);
    let d = u32::from_le_bytes([r[8], r[9], r[10], r[11]]);
    Ok((r[1], r[2], r[3], v, d))
}

/// Parses a report-pipe record.
pub fn parse_layer(r: &[u8; RECORD]) -> Result<LayerReport, BadRecord> {
    let (kind, l, s, errno, detail) = split(r)?;
    if kind != KIND_LAYER {
        return Err(BadRecord);
    }
    Ok(LayerReport {
        layer: Layer::from_code(l).ok_or(BadRecord)?,
        state: LayerState::from_code(s).ok_or(BadRecord)?,
        errno,
        detail,
    })
}

/// Parses a status-pipe record.
pub fn parse_status(r: &[u8; RECORD]) -> Result<Status, BadRecord> {
    let (kind, a, _, v, _) = split(r)?;
    match (kind, a) {
        (KIND_SETUP, s) => Ok(Status::SetupFailed {
            stage: *Stage::ALL.get(usize::from(s)).ok_or(BadRecord)?,
            errno: v,
        }),
        (KIND_EXIT, 0) => Ok(Status::Exited(v)),
        (KIND_EXIT, 1) => Ok(Status::Signaled(v)),
        _ => Err(BadRecord),
    }
}

/// Whether a layer must be in effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Requirement {
    /// The child is not started (or is killed) without it.
    Required,
    /// Used if the system offers it; its absence is reported loudly.
    BestEffort,
    /// Not used.
    Off,
}

/// The confinement of one child: the latest report of each layer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Confinement {
    layers: Vec<LayerReport>,
}

impl Confinement {
    /// Records a layer's outcome (replacing an earlier report of the same layer).
    pub fn set(&mut self, r: LayerReport) {
        match self.layers.iter_mut().find(|x| x.layer == r.layer) {
            Some(x) => *x = r,
            None => self.layers.push(r),
        }
        self.layers.sort_by_key(|x| x.layer.code());
    }

    /// The report of a layer, if any.
    pub fn get(&self, layer: Layer) -> Option<LayerReport> {
        self.layers.iter().find(|x| x.layer == layer).copied()
    }

    /// Whether a layer is in effect.
    pub fn in_effect(&self, layer: Layer) -> bool {
        self.get(layer).is_some_and(|r| r.state.in_effect())
    }

    /// Every report, in layer order.
    pub fn layers(&self) -> &[LayerReport] {
        &self.layers
    }

    /// The required layers that are not in effect (or have no report).
    pub fn missing(&self, required: impl IntoIterator<Item = Layer>) -> Vec<Layer> {
        required
            .into_iter()
            .filter(|l| !self.in_effect(*l))
            .collect()
    }
}

impl fmt::Display for Confinement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut first = true;
        for r in &self.layers {
            if !first {
                f.write_str(", ")?;
            }
            first = false;
            let s = match r.state {
                LayerState::Applied => "on",
                LayerState::Partial => "partial",
                LayerState::Unavailable => "UNAVAILABLE",
                LayerState::Off => "off",
                LayerState::Failed => "FAILED",
            };
            write!(f, "{}={s}", r.layer.name())?;
            if r.layer == Layer::Landlock && r.state.in_effect() {
                write!(f, "(abi {})", r.detail)?;
            }
            if r.layer == Layer::Seccomp && r.state.in_effect() {
                f.write_str(if r.detail == 2 { "(kill)" } else { "(errno)" })?;
            }
            if r.errno != 0 {
                write!(f, "(errno {})", r.errno)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_round_trip() {
        for l in Layer::ALL {
            assert_eq!(layer_code_const(l), l.code());
            for s in [
                LayerState::Applied,
                LayerState::Partial,
                LayerState::Unavailable,
                LayerState::Off,
                LayerState::Failed,
            ] {
                assert_eq!(LayerState::from_code(state_code_const(s)), Some(s));
                let r = LayerReport::new(l, s, -3, 77);
                assert_eq!(parse_layer(&r.encode()), Ok(r));
                assert!(parse_status(&r.encode()).is_err());
            }
        }
        for s in [
            Status::SetupFailed {
                stage: Stage::Exec,
                errno: 2,
            },
            Status::Exited(86),
            Status::Signaled(11),
        ] {
            assert_eq!(parse_status(&s.encode()), Ok(s));
            assert!(parse_layer(&s.encode()).is_err());
        }
        let mut bad = Status::Exited(0).encode();
        bad[0] = 0;
        assert!(parse_status(&bad).is_err());
        let mut bad = LayerReport::applied(Layer::Seccomp, 2).encode();
        bad[2] = 200;
        assert!(parse_layer(&bad).is_err());
    }

    #[test]
    fn confinement_keeps_the_latest_and_lists_missing() {
        let mut c = Confinement::default();
        c.set(LayerReport::new(Layer::Landlock, LayerState::Failed, 38, 0));
        c.set(LayerReport::applied(Layer::UserNs, 0));
        c.set(LayerReport::applied(Layer::Landlock, 6));
        assert_eq!(c.layers().len(), 2);
        assert_eq!(c.layers()[0].layer, Layer::UserNs);
        assert_eq!(
            c.missing([Layer::UserNs, Layer::Landlock, Layer::Seccomp]),
            vec![Layer::Seccomp]
        );
        assert_eq!(c.to_string(), "user-ns=on, landlock=on(abi 6)");
    }
}
