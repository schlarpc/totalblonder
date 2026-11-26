//! Shared memory between a parent and its sandboxed child: sealed memfds and their mappings.
//!
//! The parent makes every memfd ([`create`]), gives it its final size, and seals it against
//! shrinking and growing ([`seal`]) before the child sees it: a child that could shrink a
//! memfd would make the parent's next access of the lost pages a `SIGBUS`, which would take
//! the parent down. A region only the parent writes is also sealed with `F_SEAL_FUTURE_WRITE`
//! after the parent has mapped it writable: the child can then map it read-only and nothing
//! else.
//!
//! [`Mapping`] is a `MAP_SHARED` mapping. Bytes in it may change at any time if the other
//! process writes them: the parent reads frames the child wrote and handed over, and a
//! misbehaving child could keep writing. Such changes can only change byte values (never the
//! mapping's size, which is sealed, or anything the parent validated, which it copied out);
//! consumers treat the bytes as pixel or sample values and take no decision on them that
//! memory safety depends on. Reads go through raw pointers or [`Mapping::copy_out`].

use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::ptr::NonNull;

use rustix::fs::{MemfdFlags, SealFlags};
use rustix::io::Errno;
use rustix::mm::{MapFlags, ProtFlags};

/// Makes a memfd of `len` bytes (zeroed), close-on-exec, sealable.
pub fn create(name: &std::ffi::CStr, len: u64) -> Result<OwnedFd, Errno> {
    let fd = rustix::fs::memfd_create(name, MemfdFlags::CLOEXEC | MemfdFlags::ALLOW_SEALING)?;
    rustix::fs::ftruncate(&fd, len)?;
    Ok(fd)
}

/// Seals the size (`F_SEAL_SHRINK | F_SEAL_GROW`), optionally future writes, and the seals
/// themselves (`F_SEAL_SEAL`). Checks that the seals are in place.
pub fn seal(fd: BorrowedFd<'_>, future_write: bool) -> Result<(), Errno> {
    let mut s = SealFlags::SHRINK | SealFlags::GROW;
    if future_write {
        s |= SealFlags::FUTURE_WRITE;
    }
    rustix::fs::fcntl_add_seals(fd, s | SealFlags::SEAL)?;
    let got = rustix::fs::fcntl_get_seals(fd)?;
    if !got.contains(s | SealFlags::SEAL) {
        return Err(Errno::PERM);
    }
    Ok(())
}

/// Whether a received memfd is sealed against resizing and has at least `len` bytes (the
/// child checks what the parent sent before mapping it).
pub fn check(fd: BorrowedFd<'_>, len: u64) -> Result<(), Errno> {
    let st = rustix::fs::fstat(fd)?;
    if (st.st_size as u64) < len {
        return Err(Errno::INVAL);
    }
    Ok(())
}

/// A shared mapping of a memfd.
#[derive(Debug)]
pub struct Mapping {
    ptr: NonNull<u8>,
    len: usize,
    writable: bool,
}

// SAFETY: the mapping is plain memory owned by this object; access goes through raw pointers
// or methods that take `&mut self` for writing; it may be used and unmapped from any thread.
unsafe impl Send for Mapping {}
// SAFETY: shared access only reads through raw pointers (`copy_out`, `as_ptr`); writes need
// `&mut self`.
unsafe impl Sync for Mapping {}

impl Mapping {
    /// Maps `len` bytes of `fd` from offset 0, shared, read-only or read-write.
    pub fn map(fd: impl AsFd, len: usize, writable: bool) -> Result<Self, Errno> {
        if len == 0 {
            return Err(Errno::INVAL);
        }
        let prot = if writable {
            ProtFlags::READ | ProtFlags::WRITE
        } else {
            ProtFlags::READ
        };
        // SAFETY: a new mapping at an address the kernel chooses; nothing else refers to it.
        // The memfd is sealed against shrinking (by the parent, which made it), so every page
        // of the mapping stays backed.
        let p = unsafe {
            rustix::mm::mmap(
                std::ptr::null_mut(),
                len,
                prot,
                MapFlags::SHARED,
                fd.as_fd(),
                0,
            )?
        };
        let ptr = NonNull::new(p.cast::<u8>()).ok_or(Errno::NOMEM)?;
        Ok(Self { ptr, len, writable })
    }

    /// Bytes mapped.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether the mapping is empty (never).
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The first byte. Reading through it is reading memory another process may write.
    pub fn as_ptr(&self) -> *const u8 {
        self.ptr.as_ptr()
    }

    fn in_bounds(&self, off: usize, n: usize) -> bool {
        off.checked_add(n).is_some_and(|e| e <= self.len)
    }

    /// Copies `dst.len()` bytes from `off` (false if out of bounds).
    pub fn copy_out(&self, off: usize, dst: &mut [u8]) -> bool {
        if !self.in_bounds(off, dst.len()) {
            return false;
        }
        // SAFETY: the source range lies in the mapping (checked), which is readable; the
        // destination is a distinct slice of this process.
        unsafe {
            std::ptr::copy_nonoverlapping(self.ptr.as_ptr().add(off), dst.as_mut_ptr(), dst.len())
        };
        true
    }

    /// Copies `src` to `off` (false if out of bounds or read-only).
    pub fn copy_in(&mut self, off: usize, src: &[u8]) -> bool {
        if !self.writable || !self.in_bounds(off, src.len()) {
            return false;
        }
        // SAFETY: the destination range lies in the writable mapping (checked); the source is
        // a distinct slice of this process.
        unsafe {
            std::ptr::copy_nonoverlapping(src.as_ptr(), self.ptr.as_ptr().add(off), src.len())
        };
        true
    }

    /// A mutable view of `n` bytes at `off`, for a writer that owns that range by protocol
    /// (the worker writing a slot it holds).
    pub fn slice_mut(&mut self, off: usize, n: usize) -> Option<&mut [u8]> {
        if !self.writable || !self.in_bounds(off, n) {
            return None;
        }
        // SAFETY: the range lies in the writable mapping; `&mut self` makes it the only view
        // of it in this process for the borrow's life.
        Some(unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr().add(off), n) })
    }

    /// A view of `n` bytes at `off`, for a reader the protocol gives the range to (the worker
    /// reading a packet the parent wrote and will not touch until it is consumed; the parent
    /// reading a frame the worker handed over). Another process could still write the range
    /// if it broke the protocol; see the module's documentation.
    pub fn slice(&self, off: usize, n: usize) -> Option<&[u8]> {
        if !self.in_bounds(off, n) {
            return None;
        }
        // SAFETY: the range lies in the readable mapping, which lives as long as `self`.
        Some(unsafe { std::slice::from_raw_parts(self.ptr.as_ptr().add(off), n) })
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: the mapping this object made; no reference into it outlives `self`.
        let _ = unsafe { rustix::mm::munmap(self.ptr.as_ptr().cast(), self.len) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sealed_memfd_cannot_be_resized_and_maps_both_ways() {
        let fd = create(c"t", 8192).unwrap();
        let mut w = Mapping::map(&fd, 8192, true).unwrap();
        seal(fd.as_fd(), true).unwrap();
        assert_eq!(rustix::fs::ftruncate(&fd, 4096), Err(Errno::PERM));
        assert_eq!(rustix::fs::ftruncate(&fd, 1 << 20), Err(Errno::PERM));
        // future writes sealed: no new writable mapping, a read-only one is fine
        assert!(Mapping::map(&fd, 8192, true).is_err());
        let r = Mapping::map(&fd, 8192, false).unwrap();
        assert!(w.copy_in(100, b"hello"));
        assert!(!w.copy_in(8190, b"hello"));
        let mut b = [0u8; 5];
        assert!(r.copy_out(100, &mut b));
        assert_eq!(&b, b"hello");
        assert_eq!(r.slice(100, 5), Some(&b"hello"[..]));
        assert!(r.slice(8190, 5).is_none());
        check(fd.as_fd(), 8192).unwrap();
        assert!(check(fd.as_fd(), 8193).is_err());
    }
}
