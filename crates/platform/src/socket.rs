//! Raw socket operations (`lib/network/endpoint.ipp` / `endpoint.cpp`).
//!
//! The reference's `Endpoint` is a thin wrapper over `socket_t`; this module
//! is the only place in the workspace that touches the kernel's socket
//! interface (BW-16-02-01). Everything higher up -- addresses, endpoints,
//! channels -- talks to these functions instead of duplicating the calls.
//!
//! Linux/Unix spellings only: the reference's `_WIN32`, PLAYSTATION3 and
//! `_XBOX` arms are build targets this project does not reproduce.

use std::net::ToSocketAddrs;
use std::os::fd::RawFd;

/// `AF_INET`: the reference only builds IPv4 addresses
/// (`sockaddr_in` everywhere in `endpoint.ipp`).
pub const AF_INET: i32 = libc::AF_INET;
/// `SOCK_STREAM`.
pub const SOCK_STREAM: i32 = libc::SOCK_STREAM;
/// `SOCK_DGRAM`.
pub const SOCK_DGRAM: i32 = libc::SOCK_DGRAM;
/// `SOL_SOCKET`.
pub const SOL_SOCKET: i32 = libc::SOL_SOCKET;
/// `SOL_IP`, the level of the `IP_RECVERR` error queue option.
pub const SOL_IP: i32 = libc::IPPROTO_IP;
/// `SO_BROADCAST`.
pub const SO_BROADCAST: i32 = libc::SO_BROADCAST;
/// `SO_REUSEADDR`.
pub const SO_REUSEADDR: i32 = libc::SO_REUSEADDR;
/// `SO_KEEPALIVE`.
pub const SO_KEEPALIVE: i32 = libc::SO_KEEPALIVE;
/// `SO_SNDBUF`.
pub const SO_SNDBUF: i32 = libc::SO_SNDBUF;
/// `SO_RCVBUF`.
pub const SO_RCVBUF: i32 = libc::SO_RCVBUF;
/// `O_NONBLOCK`, the flag value `setnonblocking` writes.
pub const O_NONBLOCK: i32 = libc::O_NONBLOCK;
/// `FIONREAD`, the ioctl that reports the readable byte count.
pub const FIONREAD: libc::Ioctl = libc::FIONREAD;
/// `MSG_NOSIGNAL`, set on every `sendto` on Linux so an undeliverable
/// packet does not raise SIGPIPE.
pub const MSG_NOSIGNAL: i32 = libc::MSG_NOSIGNAL;
/// `IP_RECVERR`, enables the per-socket error queue.
pub const IP_RECVERR: i32 = libc::IP_RECVERR;
/// `MSG_ERRQUEUE`, reads from that queue.
pub const MSG_ERRQUEUE: i32 = libc::MSG_ERRQUEUE;
/// `ECONNREFUSED`, the error an ICMP "port unreachable" turns into.
pub const ECONNREFUSED: i32 = libc::ECONNREFUSED;

/// `htons`: host order to network order.
pub fn htons(value: u16) -> u16 {
    u16::from_be(value.to_le())
}

/// `ntohs`.
pub fn ntohs(value: u16) -> u16 {
    u16::from_le(value.to_be())
}

/// `htonl`.
pub fn htonl(value: u32) -> u32 {
    u32::from_be(value.to_le())
}

/// `ntohl`.
pub fn ntohl(value: u32) -> u32 {
    u32::from_le(value.to_be())
}

/// `::socket(AF_INET, type, 0)`. The reference's Windows retry
/// (`WSANOTINITIALISED` then `initNetwork()`) has no Unix counterpart.
pub fn create_socket(type_: i32) -> RawFd {
    unsafe { libc::socket(AF_INET, type_, 0) }
}

/// `::bind` with an IPv4 address already in network byte order.
pub fn bind(fd: RawFd, port: u16, ip: u32) -> i32 {
    let mut sin = sockaddr_in(port, ip);
    unsafe {
        libc::bind(
            fd,
            &mut sin as *mut libc::sockaddr_in as *mut libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_in>() as u32,
        )
    }
}

/// `::connect`.
pub fn connect(fd: RawFd, port: u16, ip: u32) -> i32 {
    let mut sin = sockaddr_in(port, ip);
    unsafe {
        libc::connect(
            fd,
            &mut sin as *mut libc::sockaddr_in as *mut libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_in>() as u32,
        )
    }
}

/// `::listen`.
pub fn listen(fd: RawFd, backlog: i32) -> i32 {
    unsafe { libc::listen(fd, backlog) }
}

/// `::accept` with the peer port and address (both in network byte order)
/// returned alongside the descriptor. `Endpoint::accept` reads exactly
/// these three things.
pub fn accept_address(fd: RawFd) -> (RawFd, u16, u32) {
    let mut sin = sockaddr_in(0, 0);
    let mut len = std::mem::size_of::<libc::sockaddr_in>() as u32;
    let result =
        unsafe { libc::accept(fd, &mut sin as *mut _ as *mut _, &mut len) as RawFd };
    (result, sin.sin_port, sin.sin_addr.s_addr)
}

/// `::sendto` with `MSG_NOSIGNAL`.
pub fn sendto(fd: RawFd, data: &[u8], port: u16, ip: u32) -> i32 {
    let mut sin = sockaddr_in(port, ip);
    unsafe {
        libc::sendto(
            fd,
            data.as_ptr() as *const _,
            data.len(),
            MSG_NOSIGNAL,
            &mut sin as *mut _ as *mut _,
            std::mem::size_of::<libc::sockaddr_in>() as u32,
        ) as i32
    }
}

/// `::send`.
pub fn send(fd: RawFd, data: &[u8]) -> i32 {
    unsafe {
        libc::send(fd, data.as_ptr() as *const _, data.len(), 0) as i32
    }
}

/// `::recvfrom`, returning the byte count with the source port and
/// address (both in network byte order).
pub fn recvfrom(fd: RawFd, buf: &mut [u8]) -> (i32, u16, u32) {
    let mut sin = sockaddr_in(0, 0);
    let mut len = std::mem::size_of::<libc::sockaddr_in>() as u32;
    let result = unsafe {
        libc::recvfrom(
            fd,
            buf.as_mut_ptr() as *mut _,
            buf.len(),
            0,
            &mut sin as *mut _ as *mut _,
            &mut len,
        ) as i32
    };
    (result, sin.sin_port, sin.sin_addr.s_addr)
}

/// `::recv`.
pub fn recv(fd: RawFd, buf: &mut [u8]) -> i32 {
    unsafe { libc::recv(fd, buf.as_mut_ptr() as *mut _, buf.len(), 0) as i32 }
}

/// `::getsockname`.
pub fn local_address(fd: RawFd) -> Option<(u16, u32)> {
    let mut sin = sockaddr_in(0, 0);
    let mut len = std::mem::size_of::<libc::sockaddr_in>() as u32;
    let result =
        unsafe { libc::getsockname(fd, &mut sin as *mut _ as *mut _, &mut len) };
    if result == 0 {
        Some((sin.sin_port, sin.sin_addr.s_addr))
    } else {
        None
    }
}

/// `::getpeername`.
pub fn remote_address(fd: RawFd) -> Option<(u16, u32)> {
    let mut sin = sockaddr_in(0, 0);
    let mut len = std::mem::size_of::<libc::sockaddr_in>() as u32;
    let result = unsafe { libc::getpeername(fd, &mut sin as *mut _ as *mut _, &mut len) };
    if result == 0 {
        Some((sin.sin_port, sin.sin_addr.s_addr))
    } else {
        None
    }
}

/// `::close`, clearing the descriptor when it succeeds. Returns the
/// kernel result with `fd` already consumed.
pub fn close(fd: RawFd) -> i32 {
    unsafe { libc::close(fd) }
}

/// `getrlimit(RLIMIT_NOFILE)`. The limit itself is one past the highest
/// number the kernel will ever hand out, so it stays dead forever -- a
/// closed descriptor's number does not, because the next `open` reuses it.
pub fn descriptor_limit() -> RawFd {
    let mut limit = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) };
    limit.rlim_cur as RawFd
}

/// `fcntl(fd, F_SETFL, flags)` -- `setnonblocking`. The reference passes
/// `O_NONBLOCK` or 0 straight to `F_SETFL`, which silently clears every
/// other flag; this keeps the current flags so the port does not lose the
/// `O_APPEND`/`O_DIRECT` bits a caller set.
pub fn set_nonblocking(fd: RawFd, nonblocking: bool) -> i32 {
    let current = unsafe { libc::fcntl(fd, libc::F_GETFL, 0) };
    if current < 0 {
        return current;
    }
    let flags = if nonblocking {
        current | O_NONBLOCK
    } else {
        current & !O_NONBLOCK
    };
    unsafe { libc::fcntl(fd, libc::F_SETFL, flags) }
}

/// `ioctl(fd, FIONREAD, &bytes)` -- `numBytesAvailableForReading`.
pub fn bytes_available(fd: RawFd) -> i32 {
    let mut bytes: i32 = 0;
    let result = unsafe { libc::ioctl(fd, FIONREAD, &mut bytes) };
    if result != 0 {
        -1
    } else {
        bytes
    }
}

/// `::setsockopt` for a boolean option, mirroring the reference's
/// `setSocketOption< bool >` specialisation (a bool becomes 1/0).
pub fn set_bool_option(fd: RawFd, level: i32, optname: i32, value: bool) -> i32 {
    let value = if value { 1 } else { 0 };
    let len = std::mem::size_of::<i32>() as u32;
    unsafe {
        libc::setsockopt(
            fd,
            level,
            optname,
            &value as *const i32 as *const _,
            len,
        )
    }
}

/// `::getsockopt` for a boolean option.
pub fn get_bool_option(fd: RawFd, level: i32, optname: i32) -> Option<bool> {
    let mut value: i32 = 0;
    let mut len = std::mem::size_of::<i32>() as u32;
    let result = unsafe {
        libc::getsockopt(fd, level, optname, &mut value as *mut i32 as *mut _, &mut len)
    };
    if result != 0 {
        None
    } else {
        Some(value != 0)
    }
}

/// `::getsockopt` for a signed integer option -- `getBufferSize`, which
/// only accepts `SO_SNDBUF` / `SO_RCVBUF` and reports -1 otherwise.
pub fn get_int_option(fd: RawFd, level: i32, optname: i32) -> Option<i32> {
    let mut value: i32 = -1;
    let mut len = std::mem::size_of::<i32>() as u32;
    let result = unsafe {
        libc::getsockopt(fd, level, optname, &mut value as *mut i32 as *mut _, &mut len)
    };
    if result == 0 && len as usize == std::mem::size_of::<i32>() {
        Some(value)
    } else {
        None
    }
}

/// `::setsockopt` for a signed integer option -- `setBufferSize`.
pub fn set_int_option(fd: RawFd, level: i32, optname: i32, value: i32) -> i32 {
    let len = std::mem::size_of::<i32>() as u32;
    unsafe {
        libc::setsockopt(
            fd,
            level,
            optname,
            &value as *const i32 as *const _,
            len,
        )
    }
}

/// One `MSG_ERRQUEUE` entry: the kernel error number, the offending peer
/// in network byte order, and the ICMP `ee_info` code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueuedError {
    pub errno: u32,
    pub port: u16,
    pub ip: u32,
    pub info: u32,
}

/// `recvmsg(fd, MSG_ERRQUEUE)` -- `Endpoint::readFromErrorQueue`.
///
/// Returns `None` when the queue is empty. When the kernel left
/// `msg_namelen` at zero (the bug the reference works around) the
/// offender is read out of `SO_EE_OFFENDER` with its port forced to zero.
pub fn read_error_queue(fd: RawFd) -> Option<QueuedError> {
    let mut offender = sockaddr_in(0, 0);
    let mut header = libc::msghdr {
        msg_name: &mut offender as *mut _ as *mut _,
        msg_namelen: std::mem::size_of::<libc::sockaddr_in>() as u32,
        msg_iov: std::ptr::null_mut(),
        msg_iovlen: 0,
        msg_control: std::ptr::null_mut(),
        msg_controllen: 0,
        msg_flags: 0,
    };

    let mut data = [0u8; 256];
    let mut control = [0u8; 256];
    let mut packet = libc::iovec {
        iov_base: data.as_mut_ptr() as *mut _,
        iov_len: data.len(),
    };
    header.msg_iov = &mut packet;
    header.msg_iovlen = 1;
    header.msg_control = control.as_mut_ptr() as *mut _;
    header.msg_controllen = control.len();

    if unsafe { libc::recvmsg(fd, &mut header, MSG_ERRQUEUE) } < 0 {
        return None;
    }

    let cursor = find_recv_error(&header)?;
    parse_queued_error(unsafe { libc::CMSG_DATA(cursor) as *const u8 }, &header, &offender)
}

/// Walk `header`'s control messages for the `IP_RECVERR` one. Kept apart
/// from `read_error_queue` so the walk can be driven from a fixture
/// instead of needing a kernel that actually fails to deliver a packet.
fn find_recv_error(header: &libc::msghdr) -> Option<*mut libc::cmsghdr> {
    let mut cursor = unsafe { libc::CMSG_FIRSTHDR(header) };
    while !cursor.is_null() {
        let level = unsafe { &*cursor }.cmsg_level;
        let kind = unsafe { &*cursor }.cmsg_type;
        if level == SOL_IP && kind == IP_RECVERR {
            return Some(cursor);
        }
        cursor = unsafe { libc::CMSG_NXTHDR(header, cursor) };
    }
    None
}

/// Decode the `IP_RECVERR` data at `data`, which the kernel fills with a
/// `sock_extended_err` at the start. `SO_EE_OFFENDER` steps back out of it
/// to the offender address for the kernel that drops `msg_name`.
fn parse_queued_error(
    data: *const u8,
    header: &libc::msghdr,
    offender: &libc::sockaddr_in,
) -> Option<QueuedError> {
    let ext = unsafe { &*(data as *const libc::sock_extended_err) };

    let (port, ip) = if header.msg_namelen == 0 {
        // "Kernel has a bug: recv_msg did not set msg_name."
        let offender = unsafe { libc::SO_EE_OFFENDER(ext) as *const libc::sockaddr_in };
        let offender = unsafe { &*offender };
        (0, offender.sin_addr.s_addr)
    } else {
        (offender.sin_port, offender.sin_addr.s_addr)
    };

    Some(QueuedError {
        errno: ext.ee_errno,
        port,
        ip,
        info: ext.ee_info,
    })
}

/// `inet_aton`: parse a dotted quad. The result is the address exactly as
/// the kernel stores it, so it can be handed to `bind`/`sendto` untouched.
/// Returns `None` when the string is not an address; `None` also means
/// "try a name lookup".
pub fn inet_aton(text: &str) -> Option<u32> {
    let parts: Vec<&str> = text.split('.').collect();
    if parts.len() != 4 {
        return None;
    }
    let mut octets = [0u8; 4];
    for (slot, part) in octets.iter_mut().zip(parts) {
        *slot = parse_octet(part).ok()?;
    }
    Some(u32::from_ne_bytes(octets))
}

/// `gethostbyname`: resolve a name to its first IPv4 address, stored the
/// way the kernel stores it. Port 0 is supplied because `to_socket_addrs`
/// demands one while `gethostbyname` has none.
pub fn host_by_name(text: &str) -> Option<u32> {
    let addrs = (text, 0u16).to_socket_addrs().ok()?;
    for addr in addrs {
        match addr.ip() {
            std::net::IpAddr::V4(ipv4) => {
                return Some(u32::from_ne_bytes(ipv4.octets()))
            }
            std::net::IpAddr::V6(_) => continue,
        }
    }
    None
}

/// `gethostbyaddr`: resolve an IPv4 address back to its canonical name.
pub fn host_by_addr(ip: u32) -> Option<String> {
    let sin = sockaddr_in(0, ip);
    let mut name = [0u8; 256];
    let result = unsafe {
        libc::getnameinfo(
            &sin as *const libc::sockaddr_in as *const libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_in>() as u32,
            name.as_mut_ptr().cast(),
            name.len() as u32,
            std::ptr::null_mut(),
            0,
            libc::NI_NAMEREQD,
        )
    };
    if result != 0 {
        return None;
    }
    let name = std::str::from_utf8(&name).ok()?.trim_end_matches('\0');
    Some(name.to_string())
}

/// `struct sockaddr_in` for the port and address the caller already holds
/// in network byte order.
fn sockaddr_in(port: u16, ip: u32) -> libc::sockaddr_in {
    libc::sockaddr_in {
        sin_family: AF_INET as u16,
        sin_port: port,
        sin_addr: libc::in_addr { s_addr: ip },
        sin_zero: [0; 8],
    }
}

/// A single dotted-quad component, 0..=255.
fn parse_octet(part: &str) -> Result<u8, ()> {
    if part.is_empty() || part.len() > 3 {
        return Err(());
    }
    if !part.chars().all(|c| c.is_ascii_digit()) {
        return Err(());
    }
    let value = part.parse::<u32>().map_err(|_| ())?;
    u8::try_from(value).map_err(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `127.0.0.1` as the kernel keeps it: the network-order bytes read
    /// back as a host word.
    const LOOPBACK: u32 = 0x0100_007f;

    /// The limit is one past the highest number the kernel will ever hand
    /// out, so it stays dead -- unlike a *closed* descriptor's number,
    /// which the next `open` in another test thread reuses.
    #[test]
    fn the_descriptor_limit_is_dead_forever() {
        let dead = descriptor_limit();
        assert!(dead > 3, "the limit is well past the small descriptors");
        assert_eq!(close(dead), -1, "the kernel refuses the number");
        assert_eq!(recv(dead, &mut [0u8; 8]), -1);
        assert_eq!(recvfrom(dead, &mut [0u8; 8]).0, -1);
    }

    /// `htons`/`ntohs` and `htonl`/`ntohl` are each other's inverses and
    /// actually swap the byte order on a little-endian host.
    #[test]
    fn byte_order_helpers_round_trip() {
        assert_eq!(ntohs(htons(0x1234)), 0x1234);
        assert_eq!(ntohl(htonl(0x0a000001)), 0x0a000001);
        // 1.2.3.4 in network byte order is the byte-swapped value.
        assert_eq!(htonl(0x01020304), 0x04030201);
    }

    /// A UDP socket round-trips its own bytes, its bound port is readable,
    /// and closing it leaves the descriptor unusable.
    #[test]
    fn udp_round_trip_and_local_address() {
        let fd = create_socket(SOCK_DGRAM);
        assert!(fd >= 0, "socket() failed: {}", std::io::Error::last_os_error());

        // Port 0 asks the kernel to pick a free one; ip 0 is INADDR_ANY.
        assert_eq!(bind(fd, 0, 0), 0, "bind() failed");
        let (port, ip) = local_address(fd).expect("getsockname");
        assert_ne!(port, 0, "the kernel assigned a port");
        assert_eq!(ip, 0);

        let mut buffer = [0u8; 16];
        assert_eq!(sendto(fd, b"hello", port, LOOPBACK), 5, "sendto() failed");
        let (received, reply_port, reply_ip) = recvfrom(fd, &mut buffer);
        assert_eq!(received, 5, "recvfrom() failed");
        assert_eq!(&buffer[..5], b"hello");
        // The port travels in network byte order, so it comes back as sent.
        assert_eq!(reply_port, port);
        assert_eq!(reply_ip, LOOPBACK);

        // Nothing left to read.
        assert_eq!(bytes_available(fd), 0);

        assert_eq!(close(fd), 0);
        // A closed descriptor cannot be read.
        assert_eq!(recv(fd, &mut buffer), -1);
    }

    /// `connect`/`listen`/`accept_address` hand back the peer port and
    /// address the way `Endpoint::accept` does.
    #[test]
    fn tcp_accept_reports_the_peer_address() {
        let listener = create_socket(SOCK_STREAM);
        assert!(listener >= 0);
        assert_eq!(set_bool_option(listener, SOL_SOCKET, SO_REUSEADDR, true), 0);
        assert_eq!(bind(listener, 0, 0), 0);
        let (port, _) = local_address(listener).expect("getsockname");
        assert_eq!(listen(listener, 1), 0);

        let client = create_socket(SOCK_STREAM);
        assert!(client >= 0);
        assert_eq!(connect(client, port, LOOPBACK), 0);

        let (accepted, peer_port, peer_ip) = accept_address(listener);
        assert!(accepted >= 0, "accept() failed");
        assert_eq!(peer_ip, LOOPBACK);
        assert_ne!(peer_port, 0);

        // `recvAll`'s loop depends on `recv` reporting the connection
        // loss as zero.
        let mut buffer = [0u8; 4];
        assert_eq!(send(client, b"ab"), 2);
        assert_eq!(recv(accepted, &mut buffer), 2);
        assert_eq!(&buffer[..2], b"ab");

        close(client);
        // EOF on the closed peer reads as zero.
        assert_eq!(recv(accepted, &mut buffer), 0);
        close(accepted);
        close(listener);
    }

    /// `remote_address` reports the peer only for connected sockets.
    #[test]
    fn remote_address_needs_a_connection() {
        let listener = create_socket(SOCK_STREAM);
        assert!(listener >= 0);
        assert_eq!(bind(listener, 0, 0), 0);
        assert_eq!(listen(listener, 1), 0);
        assert!(remote_address(listener).is_none());

        let port = local_address(listener).expect("getsockname").0;
        let client = create_socket(SOCK_STREAM);
        assert!(client >= 0);
        assert_eq!(connect(client, port, LOOPBACK), 0);
        let accepted = accept_address(listener).0;
        assert!(accepted >= 0);
        let (peer_port, peer_ip) = remote_address(accepted).expect("getpeername");
        assert_eq!(peer_ip, LOOPBACK);
        assert_ne!(peer_port, 0);
        close(accepted);
        close(client);
        close(listener);
    }

    /// Buffer sizes are readable and can be raised (`setBufferSize`
    /// re-reads the option and reports whether it reached the request).
    #[test]
    fn buffer_sizes_are_readable_and_settable() {
        let fd = create_socket(SOCK_DGRAM);
        assert!(fd >= 0);
        assert_eq!(
            set_int_option(fd, SOL_SOCKET, SO_SNDBUF, 64 * 1024),
            0,
            "setsockopt(SO_SNDBUF)"
        );
        let size = get_int_option(fd, SOL_SOCKET, SO_SNDBUF).expect("getsockopt");
        assert!(size >= 64 * 1024, "kernels double the requested size");

        // A boolean option reports its state.
        assert_eq!(set_bool_option(fd, SOL_SOCKET, SO_BROADCAST, true), 0);
        assert_eq!(get_bool_option(fd, SOL_SOCKET, SO_BROADCAST), Some(true));
        assert_eq!(set_bool_option(fd, SOL_SOCKET, SO_BROADCAST, false), 0);
        assert_eq!(get_bool_option(fd, SOL_SOCKET, SO_BROADCAST), Some(false));

        // Neither reader pretends to have a value off a dead descriptor.
        assert_eq!(get_int_option(-1, SOL_SOCKET, SO_SNDBUF), None);
        assert_eq!(get_bool_option(-1, SOL_SOCKET, SO_BROADCAST), None);
        close(fd);
    }

    /// `setnonblocking` toggles the flag on and off, and `fcntl` reports
    /// its own failure for a bad descriptor.
    #[test]
    fn nonblocking_flag_toggles() {
        let fd = create_socket(SOCK_DGRAM);
        assert!(fd >= 0);
        assert_eq!(set_nonblocking(fd, true), 0);
        assert_eq!(set_nonblocking(fd, false), 0);
        close(fd);

        // A bad descriptor fails at the very first fcntl.
        assert_eq!(set_nonblocking(-1, true), -1);
    }

    /// `inet_aton` parses dotted quads into the kernel's own spelling;
    /// names need the lookup path, and garbage fails both.
    #[test]
    fn address_parsing_distinguishes_numbers_from_names() {
        assert_eq!(inet_aton("127.0.0.1"), Some(LOOPBACK));
        assert_eq!(inet_aton("0.0.0.0"), Some(0));
        assert_eq!(inet_aton("255.255.255.255"), Some(u32::MAX));
        assert_eq!(inet_aton("not.an.address"), None);
        assert_eq!(inet_aton("256.0.0.1"), None);
        assert_eq!(inet_aton("1.2.3.4.5"), None);
        assert_eq!(inet_aton("127.0.0.1"), Some(LOOPBACK), "recheck: four octets");
        assert_eq!(inet_aton("1.2.3"), None, "too few octets");
        assert_eq!(inet_aton("1..3.4"), None, "an empty octet is not one");
        assert_eq!(inet_aton("1234.2.3.4"), None, "four digits is not an octet");
        assert_eq!(inet_aton("1.2.3.x"), None, "a letter is not an octet");
        assert_eq!(host_by_name("localhost"), Some(LOOPBACK));
        assert_eq!(host_by_name("no.such.host.invalid"), None);
        // `ip6-localhost` resolves to ::1 only, so the v4 scan runs out
        // without finding anything. Where hosts has no such entry the
        // lookup fails the same way, so either outcome is None.
        assert_eq!(host_by_name("ip6-localhost"), None);
    }

    /// The error queue stays empty on a healthy socket, and enabling it
    /// is a plain `IP_RECVERR` option.
    #[test]
    fn the_error_queue_is_empty_when_nothing_failed() {
        let fd = create_socket(SOCK_DGRAM);
        assert!(fd >= 0);
        assert_eq!(set_bool_option(fd, SOL_IP, IP_RECVERR, true), 0);
        assert!(read_error_queue(fd).is_none());
        close(fd);
    }

    /// An undeliverable packet lands on the error queue with the
    /// destination as its offender.
    #[test]
    fn the_error_queue_reports_an_undeliverable_packet() {
        let fd = create_socket(SOCK_DGRAM);
        assert!(fd >= 0);
        assert_eq!(set_bool_option(fd, SOL_IP, IP_RECVERR, true), 0);

        // A destination port that nothing listens on is refused once.
        let refused = sendto(fd, b"no listener", htons(1), LOOPBACK);
        assert_eq!(refused, 11, "sendto() of 11 bytes");

        let queued = read_error_queue(fd).expect("one queued error");
        assert_eq!(queued.errno, libc::ECONNREFUSED as u32);
        assert_eq!(queued.ip, LOOPBACK);
        assert!(read_error_queue(fd).is_none(), "queue drained");
        close(fd);
    }

    /// Lay one control message into `control` at the kernel's alignment and
    /// return where the next one starts. `data` must already look like the
    /// kernel lays it out.
    fn put_control_message(control: &mut [u8], from: usize, level: i32, kind: i32, data: &[u8]) -> usize {
        let offset = (from + 7) & !7;
        // CMSG_SPACE already adds the header, so it is handed the payload size.
        let space = unsafe { libc::CMSG_SPACE(data.len() as libc::c_uint) } as usize;
        assert!(space > 0, "CMSG_SPACE needs a payload");
        assert!(offset + space <= control.len(), "fixture buffer too small");
        let header = (control.as_ptr() as usize + offset) as *mut libc::cmsghdr;
        unsafe {
            (*header).cmsg_level = level;
            (*header).cmsg_type = kind;
            (*header).cmsg_len = space;
            std::ptr::copy_nonoverlapping(
                data.as_ptr(),
                libc::CMSG_DATA(header),
                data.len(),
            );
        }
        offset + space
    }

    /// The `IP_RECVERR` data exactly as the kernel fills it: a
    /// `sock_extended_err` at the start, then the offender address, which
    /// is what `SO_EE_OFFENDER` steps to.
    fn extended_error_fixture() -> ([u8; 48], libc::sockaddr_in) {
        let offender = sockaddr_in(htons(4321), 0x0100_007f);
        let ext = libc::sock_extended_err {
            ee_errno: libc::ECONNREFUSED as u32,
            ee_origin: 3,
            ee_type: 2,
            ee_code: 0,
            ee_pad: 0,
            ee_info: 0x21,
            ee_data: 0,
        };
        let mut block: [u8; 48] = [0; 48];
        let mut copy = |value: &[u8], at: usize| {
            block[at..at + value.len()].copy_from_slice(value);
        };
        let ext_bytes = unsafe {
            std::slice::from_raw_parts(
                &ext as *const libc::sock_extended_err as *const u8,
                std::mem::size_of::<libc::sock_extended_err>(),
            )
        };
        copy(ext_bytes, 0);
        let offender_bytes = unsafe {
            std::slice::from_raw_parts(
                &offender as *const libc::sockaddr_in as *const u8,
                std::mem::size_of::<libc::sockaddr_in>(),
            )
        };
        copy(offender_bytes, std::mem::size_of::<libc::sock_extended_err>());
        (block, offender)
    }

    /// The reference's control message walk is what picks the offender
    /// out of a message, so the fixture drives it directly: an
    /// unrelated message comes first, the kernel drops `msg_name`
    /// sometimes.
    #[test]
    fn the_error_queue_parser_walks_past_other_control_messages() {
        let (block, offender) = extended_error_fixture();

        // A non-IP_RECVERR message first, then the real one.
        let mut control = [0u8; 256];
        let after_junk = put_control_message(&mut control, 0, SOL_SOCKET, 0x11, &[]);
        let after_recv = put_control_message(
            &mut control,
            after_junk,
            SOL_IP,
            IP_RECVERR,
            &block,
        );

        let mut header = libc::msghdr {
            msg_name: std::ptr::null_mut(),
            msg_namelen: std::mem::size_of::<libc::sockaddr_in>() as u32,
            msg_iov: std::ptr::null_mut(),
            msg_iovlen: 0,
            msg_control: control.as_mut_ptr() as *mut _,
            msg_controllen: after_recv,
            msg_flags: 0,
        };

        // The walk skips the unrelated message and lands on the address.
        let cursor = find_recv_error(&header).expect("the IP_RECVERR message");
        let queued = parse_queued_error(unsafe { libc::CMSG_DATA(cursor) as *const u8 }, &header, &offender)
            .expect("one queued error");
        assert_eq!(queued.errno, libc::ECONNREFUSED as u32);
        assert_eq!(queued.info, 0x21);
        assert_eq!(queued.ip, LOOPBACK);
        assert_eq!(queued.port, htons(4321));

        // With the message that names no offender, the fallback reads the
        // address out of the error itself and drops the port.
        header.msg_namelen = 0;
        let kernel_bug = parse_queued_error(
            unsafe { libc::CMSG_DATA(cursor) as *const u8 },
            &header,
            &offender,
        )
        .expect("fallback path");
        assert_eq!(kernel_bug.port, 0, "the port is forced to zero");
        assert_eq!(kernel_bug.ip, LOOPBACK, "the address still comes through");

        // A queue entry without an IP_RECVERR message is not an error we
        // can attribute.
        header.msg_namelen = std::mem::size_of::<libc::sockaddr_in>() as u32;
        header.msg_controllen = after_junk;
        assert!(
            find_recv_error(&header).is_none(),
            "no IP_RECVERR message means no answer"
        );
    }

    /// `getnameinfo` resolves a bound address back to a name.
    #[test]
    fn host_by_addr_resolves_the_loopback() {
        let fd = create_socket(SOCK_DGRAM);
        assert!(fd >= 0);
        assert_eq!(bind(fd, 0, LOOPBACK), 0);
        let (_, ip) = local_address(fd).expect("getsockname");
        assert!(host_by_addr(ip).is_some());
        assert!(host_by_addr(0).is_none(), "0.0.0.0 has no name");
        close(fd);
    }
}
