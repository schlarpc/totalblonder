//! Where datagrams go: a UDP socket (unicast or multicast, with an explicit outgoing interface
//! and TTL; SPEC §4.2), a file (for tests and recordings), or nowhere.

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::net::{Ipv4Addr, SocketAddrV4};

use socket2::{Domain, Protocol, SockAddr, Socket, Type};

/// A destination for datagrams. Sending never blocks for long: a UDP socket drops when its
/// buffer is full and reports an error, which the pacer counts.
pub trait Sink: Send {
    /// Sends one datagram.
    fn send(&mut self, datagram: &[u8]) -> io::Result<()>;
    /// Flushes buffered output (files).
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A UDP socket bound to an optional local interface address.
pub struct UdpSink {
    sock: Socket,
    dest: SockAddr,
}

impl UdpSink {
    /// A socket sending to `dest`. For a multicast destination, `iface` selects the outgoing
    /// interface by address (`IP_MULTICAST_IF`) and `ttl` the multicast TTL; loopback of our
    /// own multicast is turned off. For unicast, `iface` binds the source address.
    pub fn new(dest: SocketAddrV4, iface: Option<Ipv4Addr>, ttl: u32) -> io::Result<Self> {
        let sock = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
        // Room for a few milliseconds of a carrier at 38.8 Mbit/s without drops.
        let _ = sock.set_send_buffer_size(1 << 20);
        if dest.ip().is_multicast() {
            if let Some(a) = iface {
                sock.set_multicast_if_v4(&a)?;
            }
            sock.set_multicast_ttl_v4(ttl)?;
            sock.set_multicast_loop_v4(false)?;
        } else if let Some(a) = iface {
            sock.bind(&SockAddr::from(SocketAddrV4::new(a, 0)))?;
        }
        Ok(UdpSink {
            sock,
            dest: SockAddr::from(dest),
        })
    }
}

impl Sink for UdpSink {
    fn send(&mut self, datagram: &[u8]) -> io::Result<()> {
        self.sock.send_to(datagram, &self.dest).map(|_| ())
    }
}

/// A file of raw TS packets (the RTP header, if any, is left out so the file is plain TS).
pub struct FileSink {
    w: BufWriter<File>,
    skip: usize,
}

impl FileSink {
    /// Writes to `file`, dropping `skip` header bytes of each datagram (12 for RTP).
    pub fn new(file: File, skip: usize) -> Self {
        FileSink {
            w: BufWriter::with_capacity(1 << 20, file),
            skip,
        }
    }
}

impl Sink for FileSink {
    fn send(&mut self, datagram: &[u8]) -> io::Result<()> {
        self.w.write_all(datagram.get(self.skip..).unwrap_or(&[]))
    }
    fn flush(&mut self) -> io::Result<()> {
        self.w.flush()
    }
}

/// Discards everything.
pub struct NullSink;

impl Sink for NullSink {
    fn send(&mut self, _: &[u8]) -> io::Result<()> {
        Ok(())
    }
}
