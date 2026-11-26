//! The control channel: a `SOCK_SEQPACKET` socket pair (message boundaries kept, reliable,
//! ordered, connection-oriented: the peer's exit is an end of file), with file descriptors
//! passed as `SCM_RIGHTS`.

use std::mem::MaybeUninit;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};

use rustix::io::Errno;
use rustix::net::{
    AddressFamily, RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, ReturnFlags,
    SendAncillaryBuffer, SendAncillaryMessage, SendFlags, SocketFlags, SocketType,
};

/// A connected pair, both ends close-on-exec.
pub fn pair() -> Result<(OwnedFd, OwnedFd), Errno> {
    rustix::net::socketpair(
        AddressFamily::UNIX,
        SocketType::SEQPACKET,
        SocketFlags::CLOEXEC,
        None,
    )
}

/// Sends one message, with at most one descriptor. With `nonblocking`, a full socket is
/// `Err(Errno::AGAIN)` and nothing is sent.
pub fn send(
    sock: BorrowedFd<'_>,
    msg: &[u8],
    pass: Option<BorrowedFd<'_>>,
    nonblocking: bool,
) -> Result<(), Errno> {
    let mut flags = SendFlags::NOSIGNAL;
    if nonblocking {
        flags |= SendFlags::DONTWAIT;
    }
    let fds;
    let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
    let mut anc = SendAncillaryBuffer::new(&mut space);
    if let Some(fd) = pass {
        fds = [fd];
        if !anc.push(SendAncillaryMessage::ScmRights(&fds)) {
            return Err(Errno::NOBUFS);
        }
    }
    loop {
        match rustix::net::sendmsg(sock, &[std::io::IoSlice::new(msg)], &mut anc, flags) {
            Ok(n) if n == msg.len() => return Ok(()),
            Ok(_) => return Err(Errno::MSGSIZE),
            Err(Errno::INTR) => {}
            Err(e) => return Err(e),
        }
    }
}

/// One received message.
#[derive(Debug)]
pub struct Received {
    /// Bytes of the message in the buffer.
    pub len: usize,
    /// The first descriptor that came with it.
    pub fd: Option<OwnedFd>,
    /// More than one descriptor came (the extra ones are closed).
    pub extra_fds: bool,
    /// The message was longer than the buffer (it is cut).
    pub truncated: bool,
}

/// Receives one message into `buf`. `Ok(None)`: the peer has closed. With `nonblocking`, no
/// message is `Err(Errno::AGAIN)`.
pub fn recv(
    sock: BorrowedFd<'_>,
    buf: &mut [u8],
    nonblocking: bool,
) -> Result<Option<Received>, Errno> {
    let mut flags = RecvFlags::CMSG_CLOEXEC;
    if nonblocking {
        flags |= RecvFlags::DONTWAIT;
    }
    let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(4))];
    loop {
        let mut anc = RecvAncillaryBuffer::new(&mut space);
        let r = rustix::net::recvmsg(sock, &mut [std::io::IoSliceMut::new(buf)], &mut anc, flags);
        let m = match r {
            Ok(m) => m,
            Err(Errno::INTR) => continue,
            Err(e) => return Err(e),
        };
        let mut fd = None;
        let mut extra_fds = false;
        for a in anc.drain() {
            if let RecvAncillaryMessage::ScmRights(fds) = a {
                for f in fds {
                    if fd.is_none() {
                        fd = Some(f);
                    } else {
                        extra_fds = true;
                    }
                }
            }
        }
        if m.bytes == 0 && !m.flags.contains(ReturnFlags::TRUNC) && fd.is_none() {
            // SEQPACKET: a zero-length read is the peer's close (empty messages are not sent)
            return Ok(None);
        }
        return Ok(Some(Received {
            len: m.bytes.min(buf.len()),
            fd,
            extra_fds,
            truncated: m.flags.contains(ReturnFlags::TRUNC)
                || m.flags.contains(ReturnFlags::CTRUNC),
        }));
    }
}

/// Puts a descriptor in non-blocking mode.
pub fn set_nonblocking(fd: impl AsFd) -> Result<(), Errno> {
    rustix::io::ioctl_fionbio(fd, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_keep_their_boundaries_and_carry_descriptors() {
        let (a, b) = pair().unwrap();
        let (r, _w) = rustix::pipe::pipe().unwrap();
        send(a.as_fd(), b"one", None, false).unwrap();
        send(a.as_fd(), b"two!", Some(r.as_fd()), false).unwrap();
        let mut buf = [0u8; 16];
        let m = recv(b.as_fd(), &mut buf, false).unwrap().unwrap();
        assert_eq!(&buf[..m.len], b"one");
        assert!(m.fd.is_none());
        let m = recv(b.as_fd(), &mut buf, false).unwrap().unwrap();
        assert_eq!(&buf[..m.len], b"two!");
        assert!(m.fd.is_some());
        // a message longer than the buffer is reported as truncated
        send(a.as_fd(), &[7u8; 32], None, false).unwrap();
        let m = recv(b.as_fd(), &mut buf, false).unwrap().unwrap();
        assert!(m.truncated);
        assert_eq!(recv(b.as_fd(), &mut buf, true).unwrap_err(), Errno::AGAIN);
        drop(a);
        assert!(recv(b.as_fd(), &mut buf, false).unwrap().is_none());
    }

    #[test]
    fn a_full_socket_does_not_block_a_nonblocking_sender() {
        let (a, _b) = pair().unwrap();
        let msg = [0u8; 1024];
        let mut sent = 0;
        loop {
            match send(a.as_fd(), &msg, None, true) {
                Ok(()) => sent += 1,
                Err(Errno::AGAIN) => break,
                Err(e) => panic!("{e}"),
            }
            assert!(sent < 1_000_000);
        }
        assert!(sent > 0);
    }
}
