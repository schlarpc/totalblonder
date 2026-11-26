//! The normal form of a plan: what the golden `op` records say about a phase, and nothing
//! more. Each operation becomes `(stream, operation, kernel, grid, block, bytes)`; buffer
//! placement and kernel arguments are left out. Two plans with the same normal form issue the
//! same operations, of the same sizes and launch dimensions, in the same order.
//!
//! The golden records name buffers by C++ expression (`d_par`, `h_list[L_CODE]`); the core names
//! them by layout field. That mapping is a table the tick's tests fill; the comparison here is by
//! size and order only.

use crate::plan::{Op, Plan, StreamId};

/// The kind of an operation, as the `op` records name it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OpKind {
    /// [`Op::Fill`] (`fill`).
    Fill,
    /// [`Op::Upload`] (`upload`).
    Upload,
    /// [`Op::Upload2d`] (`upload2d`).
    Upload2d,
    /// [`Op::Download`] (`download`).
    Download,
    /// [`Op::Copy`] (`copy`, device to device).
    Copy,
    /// [`Op::Launch`] (`launch`).
    Launch,
    /// [`Op::Record`] (`record`).
    Record,
    /// [`Op::WaitFor`] (`waitfor`).
    WaitFor,
}

impl OpKind {
    /// The name the `op` records use.
    pub fn name(self) -> &'static str {
        match self {
            OpKind::Fill => "fill",
            OpKind::Upload => "upload",
            OpKind::Upload2d => "upload2d",
            OpKind::Download => "download",
            OpKind::Copy => "copy",
            OpKind::Launch => "launch",
            OpKind::Record => "record",
            OpKind::WaitFor => "waitfor",
        }
    }

    /// The kind of this record name, or `None` (`wait` ends a plan and is not an operation).
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "fill" => OpKind::Fill,
            "upload" => OpKind::Upload,
            "upload2d" => OpKind::Upload2d,
            "download" => OpKind::Download,
            "copy" => OpKind::Copy,
            "launch" => OpKind::Launch,
            "record" => OpKind::Record,
            "waitfor" => OpKind::WaitFor,
            _ => return None,
        })
    }
}

/// One operation in normal form.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NormalOp {
    /// The stream it is queued on.
    pub stream: StreamId,
    /// What it does.
    pub op: OpKind,
    /// The kernel's name, for a launch.
    pub kernel: Option<&'static str>,
    /// Blocks of a launch (0 otherwise).
    pub grid: u32,
    /// Threads per block of a launch (0 otherwise).
    pub block: u32,
    /// Bytes moved or set: the length of a fill, upload or download, `width * rows` of a 2-D
    /// upload, 0 for a launch or an event.
    pub bytes: u64,
}

impl NormalOp {
    /// An operation that is not a launch.
    pub fn transfer(stream: StreamId, op: OpKind, bytes: u64) -> Self {
        Self {
            stream,
            op,
            kernel: None,
            grid: 0,
            block: 0,
            bytes,
        }
    }
}

/// The normal form of every step of a plan, in queue order.
pub fn normal_form(plan: &Plan) -> Vec<NormalOp> {
    plan.steps
        .iter()
        .map(|s| {
            let t = |op, bytes| NormalOp::transfer(s.stream, op, bytes);
            match &s.op {
                Op::Fill { dst, .. } => t(OpKind::Fill, dst.len),
                // To the records a handle upload is a copy like any other.
                Op::Upload { src, .. } | Op::UploadRefs { src, .. } => t(OpKind::Upload, src.len),
                Op::Upload2d { width, rows, .. } => {
                    t(OpKind::Upload2d, u64::from(*width) * u64::from(*rows))
                }
                Op::Download { dst, .. } => t(OpKind::Download, dst.len),
                Op::Copy { dst, .. } => t(OpKind::Copy, dst.len),
                Op::Launch(l) => NormalOp {
                    stream: s.stream,
                    op: OpKind::Launch,
                    kernel: Some(l.kernel.name()),
                    grid: l.grid,
                    block: l.block,
                    bytes: 0,
                },
                Op::Record(_) => t(OpKind::Record, 0),
                Op::WaitFor(_) => t(OpKind::WaitFor, 0),
            }
        })
        .collect()
}

/// Where two normal forms first differ.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("operation {index}{}: expected {expected:?}, got {got:?}", stream.map_or(String::new(), |s| format!(" of stream {}", s.0)))]
pub struct Mismatch {
    /// The stream compared, when the comparison is per stream.
    pub stream: Option<StreamId>,
    /// Index of the first differing operation (in the whole list, or in the stream's).
    pub index: usize,
    /// The expected operation (`None`: the expected list ended).
    pub expected: Option<NormalOp>,
    /// The operation found (`None`: the list ended).
    pub got: Option<NormalOp>,
}

fn first_difference(
    stream: Option<StreamId>,
    expected: impl Iterator<Item = NormalOp>,
    got: impl Iterator<Item = NormalOp>,
) -> Result<(), Mismatch> {
    let (mut e, mut g) = (expected.fuse(), got.fuse());
    let mut index = 0;
    loop {
        match (e.next(), g.next()) {
            (None, None) => return Ok(()),
            (a, b) if a == b => index += 1,
            (expected, got) => {
                return Err(Mismatch {
                    stream,
                    index,
                    expected,
                    got,
                });
            }
        }
    }
}

/// Compare two normal forms operation for operation, in total queue order.
pub fn compare(expected: &[NormalOp], got: &[NormalOp]) -> Result<(), Mismatch> {
    first_difference(None, expected.iter().copied(), got.iter().copied())
}

/// Compare two normal forms stream by stream: the same operations in the same order on each
/// stream, however the streams are interleaved in the queue. (The device runs each stream in
/// order; an event wait still sees the record issued last before it, so a test that relies on
/// the interleaving of `Record` and `WaitFor` uses [`compare`].)
pub fn compare_per_stream(expected: &[NormalOp], got: &[NormalOp]) -> Result<(), Mismatch> {
    for s in 0..StreamId::COUNT {
        let s = StreamId(s as u8);
        let of = |l: &[NormalOp]| {
            l.iter()
                .copied()
                .filter(move |o| o.stream == s)
                .collect::<Vec<_>>()
        };
        first_difference(Some(s), of(expected).into_iter(), of(got).into_iter())?;
    }
    // Operations on a stream out of range are compared in total order.
    let out = |l: &[NormalOp]| {
        l.iter()
            .copied()
            .filter(|o| usize::from(o.stream.0) >= StreamId::COUNT)
            .collect::<Vec<_>>()
    };
    first_difference(None, out(expected).into_iter(), out(got).into_iter())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::selftest;

    #[test]
    fn the_selftest_plan_in_normal_form() {
        let nf = normal_form(&selftest::plan());
        let ops: Vec<_> = nf
            .iter()
            .map(|o| (o.stream.0, o.op.name(), o.bytes))
            .collect();
        assert_eq!(
            ops,
            [
                (0, "fill", selftest::ARENAS.device),
                (0, "upload", 8),
                (0, "upload", 40),
                (0, "record", 0),
                (1, "waitfor", 0),
                (1, "upload", 8192),
                (1, "record", 0),
                (0, "waitfor", 0),
                (0, "launch", 0),
                (0, "download", 1024),
                (0, "download", 256),
            ]
        );
        let l = nf[8];
        assert_eq!((l.kernel, l.grid, l.block), (Some("k_pyr"), 1, 256));
        for o in &nf {
            assert_eq!(OpKind::from_name(o.op.name()), Some(o.op));
        }
    }

    #[test]
    fn comparisons_find_the_first_difference() {
        let a = normal_form(&selftest::plan());
        assert_eq!(compare(&a, &a), Ok(()));
        let mut b = a.clone();
        b[9].bytes += 1;
        assert_eq!(
            compare(&a, &b),
            Err(Mismatch {
                stream: None,
                index: 9,
                expected: Some(a[9]),
                got: Some(b[9]),
            })
        );
        let short = &a[..10];
        let e = compare(&a, short).unwrap_err();
        assert_eq!((e.index, e.got), (10, None));
        // Moving the upload stream's operations elsewhere in the queue keeps each stream's order.
        let mut c: Vec<_> = a
            .iter()
            .copied()
            .filter(|o| o.stream == StreamId::MAIN)
            .collect();
        c.extend(a.iter().copied().filter(|o| o.stream == StreamId::UPLOAD));
        assert!(compare(&a, &c).is_err());
        assert_eq!(compare_per_stream(&a, &c), Ok(()));
        let e = compare_per_stream(&a, &b).unwrap_err();
        assert_eq!((e.stream, e.index), (Some(StreamId::MAIN), 6));
    }
}
