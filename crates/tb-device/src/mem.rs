//! Buffer handles and the arena allocator.
//!
//! An adapter owns two blocks of memory, taken once when it is made: the device arena and the
//! host arena (pinned, for the CUDA adapter). A buffer is an offset and a length in one of
//! them. The core lays batches out with [`ArenaAlloc`], which is arithmetic on offsets: making
//! or freeing a batch needs no call into a driver (the C++ `Arena`).

/// A region of the device arena.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct DevBuf {
    /// Offset of the first byte from the start of the arena.
    pub off: u64,
    /// Length in bytes.
    pub len: u64,
}

/// A region of the host arena.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct HostBuf {
    /// Offset of the first byte from the start of the arena.
    pub off: u64,
    /// Length in bytes.
    pub len: u64,
}

macro_rules! buf_impl {
    ($t:ident) => {
        impl $t {
            /// A region of `len` bytes at `off`.
            pub const fn new(off: u64, len: u64) -> Self {
                Self { off, len }
            }
            /// The part of this region that starts `off` bytes in and is `len` bytes long, or
            /// `None` when it does not lie inside.
            pub fn slice(self, off: u64, len: u64) -> Option<Self> {
                let end = off.checked_add(len)?;
                (end <= self.len).then(|| Self {
                    off: self.off + off,
                    len,
                })
            }
            /// One past the last byte.
            pub const fn end(self) -> u64 {
                self.off + self.len
            }
            /// Whether the region lies inside an arena of `arena` bytes.
            pub fn fits(self, arena: u64) -> bool {
                self.off.checked_add(self.len).is_some_and(|e| e <= arena)
            }
        }
    };
}
buf_impl!(DevBuf);
buf_impl!(HostBuf);

impl HostBuf {
    /// The bytes of this region in the host arena, or `None` when it does not lie inside.
    pub fn of(self, arena: &[u8]) -> Option<&[u8]> {
        arena.get(usize::try_from(self.off).ok()?..usize::try_from(self.end()).ok()?)
    }
    /// The bytes of this region in the host arena, mutable.
    pub fn of_mut(self, arena: &mut [u8]) -> Option<&mut [u8]> {
        arena.get_mut(usize::try_from(self.off).ok()?..usize::try_from(self.end()).ok()?)
    }
}

/// First-fit allocator over one arena: a list of free ranges, neighbors merged on return.
/// Port of the C++ `Arena` without the mutex (the core is single-threaded).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArenaAlloc {
    size: u64,
    /// Free ranges as (offset, length), ascending, never adjacent.
    free: Vec<(u64, u64)>,
}

impl ArenaAlloc {
    /// An allocator over `size` bytes, all free.
    pub fn new(size: u64) -> Self {
        Self {
            size,
            free: if size > 0 {
                vec![(0, size)]
            } else {
                Vec::new()
            },
        }
    }

    /// Size of the arena.
    pub fn size(&self) -> u64 {
        self.size
    }

    /// Take `len` bytes from the first free range that has them. Returns the offset.
    pub fn take(&mut self, len: u64) -> Option<u64> {
        if len == 0 {
            return None;
        }
        let i = self.free.iter().position(|&(_, n)| n >= len)?;
        let (off, n) = self.free[i];
        if n == len {
            self.free.remove(i);
        } else {
            self.free[i] = (off + len, n - len);
        }
        Some(off)
    }

    /// Give back `len` bytes at `off` (a range obtained from [`ArenaAlloc::take`]).
    pub fn give(&mut self, off: u64, len: u64) {
        let i = self.free.partition_point(|&(o, _)| o < off);
        self.free.insert(i, (off, len));
        if i + 1 < self.free.len() && self.free[i].0 + self.free[i].1 == self.free[i + 1].0 {
            self.free[i].1 += self.free[i + 1].1;
            self.free.remove(i + 1);
        }
        if i > 0 && self.free[i - 1].0 + self.free[i - 1].1 == self.free[i].0 {
            self.free[i - 1].1 += self.free[i].1;
            self.free.remove(i);
        }
    }

    /// Bytes not taken.
    pub fn free_bytes(&self) -> u64 {
        self.free.iter().map(|&(_, n)| n).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn take_give_merges() {
        let mut a = ArenaAlloc::new(1000);
        let x = a.take(100).unwrap();
        let y = a.take(200).unwrap();
        let z = a.take(300).unwrap();
        assert_eq!((x, y, z), (0, 100, 300));
        assert_eq!(a.free_bytes(), 400);
        a.give(y, 200);
        assert_eq!(a.take(250), Some(600));
        a.give(x, 100);
        a.give(z, 300);
        a.give(600, 250);
        assert_eq!(a, ArenaAlloc::new(1000));
        assert_eq!(a.take(1001), None);
    }

    #[test]
    fn slices_stay_inside() {
        let b = DevBuf::new(64, 100);
        assert_eq!(b.slice(10, 90), Some(DevBuf::new(74, 90)));
        assert_eq!(b.slice(10, 91), None);
        assert!(b.fits(164) && !b.fits(163));
    }
}
