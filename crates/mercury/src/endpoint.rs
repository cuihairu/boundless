//! `Mercury::Address` and `Endpoint` (`basictypes.hpp`, `endpoint.ipp`).
//!
//! `Address` is a network byte order IP and port, the wire identity Mercury
//! hands between processes. `Endpoint` wraps one IPv4 socket; every method
//! here returns the kernel's own result code (0 on success, -1 on error),
//! which is how the reference's callers test them.

use std::collections::BTreeMap;
use std::os::fd::RawFd;

use platform::interface;
use platform::socket;

/// `Endpoint::NO_SOCKET`.
pub const NO_SOCKET: i32 = -1;
/// `INADDR_ANY`: bind every address.
pub const INADDR_ANY: u32 = 0;
/// `BROADCAST` (`basictypes.hpp`): `255.255.255.255`, network byte order.
pub const BROADCAST: u32 = u32::MAX;

/// The empty address (`Address::NONE`).
pub const NONE: Address = Address {
    ip: 0,
    port: 0,
    salt: 0,
};

/// An IP address and port, the unit of identity Mercury talks about.
///
/// Both `ip` and `port` are kept in network byte order, so a value can be
/// handed to the kernel untouched -- the reference does exactly that
/// (`sin_port = networkPort`).
#[derive(Clone, Copy, Debug, Default)]
pub struct Address {
    /// The IP address, network byte order.
    pub ip: u32,
    /// The port, network byte order.
    pub port: u16,
    /// Different each time.
    pub salt: u16,
}

impl PartialEq for Address {
    /// `operator==`: the salt never counts, it is "different each time".
    fn eq(&self, other: &Address) -> bool {
        self.ip == other.ip && self.port == other.port
    }
}

impl Eq for Address {}

impl Address {
    /// `Address(uint32 ipArg, uint16 portArg)`: `salt` starts at zero.
    pub fn new(ip: u32, port: u16) -> Self {
        Address { ip, port, salt: 0 }
    }

    /// `Address::isNone`.
    pub fn is_none(&self) -> bool {
        self.ip == 0
    }

    /// `Address::writeToString`: `%d.%d.%d.%d:%d`, the four octets printed
    /// as unsigned bytes. Returns the number of characters the render
    /// would have written, `snprintf` style, so a short buffer reports the
    /// full length rather than what fit.
    pub fn write_to_string(&self, buf: &mut [u8]) -> usize {
        let text = format!("{}", self);
        let bytes = text.as_bytes();
        let n = bytes.len().min(buf.len());
        buf[..n].copy_from_slice(&bytes[..n]);
        bytes.len()
    }

    /// `Address::c_str`: the address with its port.
    ///
    /// The reference returns a pointer into one of two rotating static
    /// buffers, valid only until the next render on any thread. Rust owns
    /// its strings, so this returns its own copy and the rotation
    /// disappears with it.
    pub fn c_str(&self) -> String {
        format!("{self}")
    }

    /// `Address::ipAsString`: the address without its port.
    pub fn ip_as_string(&self) -> String {
        let (a, b, c, d) = self.octets();
        format!("{a}.{b}.{c}.{d}")
    }

    /// The four octets of `ip` in host order, the way the reference prints
    /// them.
    fn octets(&self) -> (u32, u32, u32, u32) {
        let hip = socket::ntohl(self.ip);
        (
            (hip >> 24) & 0xff,
            (hip >> 16) & 0xff,
            (hip >> 8) & 0xff,
            hip & 0xff,
        )
    }
}

impl std::fmt::Display for Address {
    /// `Address::writeToString`'s `%d.%d.%d.%d:%d`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (a, b, c, d) = self.octets();
        write!(f, "{a}.{b}.{c}.{d}:{}", socket::ntohs(self.port))
    }
}

impl PartialOrd for Address {
    /// `operator<`: the map key order, IP first then port.
    fn partial_cmp(&self, other: &Address) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }

    fn lt(&self, other: &Address) -> bool {
        self.ip < other.ip || (self.ip == other.ip && self.port < other.port)
    }
}

impl std::cmp::Ord for Address {
    fn cmp(&self, other: &Address) -> std::cmp::Ordering {
        self.ip.cmp(&other.ip).then(self.port.cmp(&other.port))
    }
}

impl std::hash::Hash for Address {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.ip.hash(state);
        self.port.hash(state);
    }
}

/// `Endpoint::convertAddress`: a dotted quad first, then a name lookup.
/// `None` means neither worked.
pub fn convert_address(text: &str) -> Option<u32> {
    socket::inet_aton(text).or_else(|| socket::host_by_name(text))
}

/// `atoi`: the leading integer of the text, skipping leading blanks,
/// accepting a sign, and yielding 0 when there are no digits -- the C
/// library's loose parse, which `findIndicatedInterface` relies on for
/// the netmask-bits field.
fn atoi(text: &str) -> i32 {
    let text = text.trim_start();
    let (sign, digits) = match text.strip_prefix('-') {
        Some(rest) => (-1i64, rest),
        None => (1i64, text.strip_prefix('+').unwrap_or(text)),
    };
    let end = digits
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(digits.len());
    digits[..end]
        .parse::<i64>()
        .map(|v| (sign * v) as i32)
        .unwrap_or(0)
}

/// `NetMask` (`netmask.hpp/.cpp`): the prefix matcher `getInterfaces`
/// filters interface addresses with.
///
/// The reference builds the mask with the *last* dotted octet shifted
/// highest and flags its own TODO about endianness; on the little-endian
/// hosts this runs on, comparing the raw network-order words like this is
/// a prefix match on the leading dotted octets, which is what the callers
/// want. The port keeps the reference's arithmetic and its raw-word
/// comparison unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetMask {
    mask: u32,
    bits: i32,
    parsed_ok: bool,
}

impl NetMask {
    /// What a failed parse leaves: zero bits, which matches everything.
    const MATCH_ALL: NetMask = NetMask {
        mask: 0,
        bits: 0,
        parsed_ok: false,
    };

    /// `NetMask::parse` on `"%d.%d.%d.%d/%d"`. A malformed string parses
    /// to the match-everything mask (`bits_ = 0`) -- the caller warns.
    pub fn parse(text: &str) -> NetMask {
        let Some((address_part, bits_part)) = text.split_once('/') else {
            return NetMask::MATCH_ALL;
        };
        // sscanf demands exactly four dot-separated ints and then a bits
        // int; anything else is the match-all mask.
        let pieces: Vec<&str> = address_part.split('.').collect();
        if pieces.len() != 4 {
            return NetMask::MATCH_ALL;
        }
        let mut octets = [0i64; 4];
        for (slot, piece) in octets.iter_mut().zip(pieces) {
            match piece.parse::<i64>() {
                Ok(value) => *slot = value,
                Err(_) => return NetMask::MATCH_ALL,
            }
        }
        let Ok(bits) = bits_part.parse::<i64>() else {
            return NetMask::MATCH_ALL;
        };
        let (a, b, c, d) = (octets[0], octets[1], octets[2], octets[3]);
        NetMask {
            mask: ((d << 24) | (c << 16) | (b << 8) | a) as u32,
            bits: bits as i32,
            parsed_ok: true,
        }
    }

    /// Whether `parse` accepted the text (drives the caller's warning).
    pub fn parsed_ok(&self) -> bool {
        self.parsed_ok
    }

    /// `containsAddress`: a zero-bit mask matches everything (the
    /// reference's `n << 32 == n` guard), otherwise the low `bits` bits of
    /// the raw words must agree. Bits beyond 32 are treated as all of them
    /// and non-positive counts as zero, where the reference's shifts would
    /// run off the end of the word.
    pub fn contains_address(&self, addr: u32) -> bool {
        if self.bits <= 0 {
            return true;
        }
        let bits_of_interest = u32::MAX >> (32 - self.bits.min(32));
        (addr & bits_of_interest) == (self.mask & bits_of_interest)
    }
}

/// A wrapper around one socket (`Endpoint`).
///
/// Not `Clone`: a C++ `Endpoint` is copyable, but Rust refuses to hand one
/// file descriptor to two owners, so ownership here is exclusive. Callers
/// that need both ends of an `accept` keep the listening endpoint and the
/// returned one separately, as the reference does with `new Endpoint()`.
pub struct Endpoint {
    socket_: RawFd,
}

impl Endpoint {
    /// `Endpoint()`: no socket until `socket` is called.
    pub fn new() -> Self {
        Endpoint { socket_: NO_SOCKET }
    }

    /// `fileno`.
    pub fn fileno(&self) -> RawFd {
        self.socket_
    }

    /// `setFileDescriptor`.
    pub fn set_file_descriptor(&mut self, fd: RawFd) {
        self.socket_ = fd;
    }

    /// `good`.
    pub fn good(&self) -> bool {
        self.socket_ != NO_SOCKET
    }

    /// `Endpoint::socket`: create a socket of `type` (`SOCK_STREAM` /
    /// `SOCK_DGRAM`). The reference's Windows `WSANOTINITIALISED` retry has
    /// no Unix counterpart.
    pub fn socket(&mut self, type_: i32) {
        self.socket_ = socket::create_socket(type_);
    }

    /// `setnonblocking`.
    pub fn set_nonblocking(&self, nonblocking: bool) -> i32 {
        socket::set_nonblocking(self.socket_, nonblocking)
    }

    /// `setbroadcast`.
    pub fn set_broadcast(&self, broadcast: bool) -> i32 {
        socket::set_bool_option(
            self.socket_,
            socket::SOL_SOCKET,
            socket::SO_BROADCAST,
            broadcast,
        )
    }

    /// `setreuseaddr`.
    pub fn set_reuseaddr(&self, reuseaddr: bool) -> i32 {
        socket::set_bool_option(
            self.socket_,
            socket::SOL_SOCKET,
            socket::SO_REUSEADDR,
            reuseaddr,
        )
    }

    /// `setkeepalive`.
    pub fn set_keepalive(&self, keepalive: bool) -> i32 {
        socket::set_bool_option(
            self.socket_,
            socket::SOL_SOCKET,
            socket::SO_KEEPALIVE,
            keepalive,
        )
    }

    /// `Endpoint::bind`: port and address in network byte order.
    pub fn bind(&self, port: u16, ip: u32) -> i32 {
        socket::bind(self.socket_, port, ip)
    }

    /// `Endpoint::close`: returns 0 if there is nothing to close, and
    /// clears the descriptor when the close succeeds.
    pub fn close(&mut self) -> i32 {
        if self.socket_ == NO_SOCKET {
            return 0;
        }
        let ret = socket::close(self.socket_);
        if ret == 0 {
            self.socket_ = NO_SOCKET;
        }
        ret
    }

    /// `Endpoint::detach`: hand the descriptor out without closing it.
    pub fn detach(&mut self) -> i32 {
        let ret = self.socket_;
        self.socket_ = NO_SOCKET;
        ret
    }

    /// `getlocaladdress`. Out-parameters are zeroed up front, the way the
    /// reference does it, so a failed call leaves them empty rather than
    /// stale.
    pub fn local_address(&self, mut port: Option<&mut u16>, mut addr: Option<&mut u32>) -> i32 {
        if let Some(out) = port.as_deref_mut() {
            *out = 0;
        }
        if let Some(out) = addr.as_deref_mut() {
            *out = 0;
        }
        let Some((bound_port, bound_ip)) = socket::local_address(self.socket_) else {
            return -1;
        };
        if let Some(out) = port {
            *out = bound_port;
        }
        if let Some(out) = addr {
            *out = bound_ip;
        }
        0
    }

    /// `getremoteaddress`. Unlike `local_address` the reference does not
    /// pre-clear its out-parameters, so a failed call leaves them alone.
    pub fn remote_address(&self, port: Option<&mut u16>, addr: Option<&mut u32>) -> i32 {
        match socket::remote_address(self.socket_) {
            None => -1,
            Some((peer_port, peer_ip)) => {
                if let Some(out) = port {
                    *out = peer_port;
                }
                if let Some(out) = addr {
                    *out = peer_ip;
                }
                0
            }
        }
    }

    /// `getLocalAddress`.
    pub fn local(&self) -> Address {
        let mut addr = Address::new(0, 0);
        if self.local_address(Some(&mut addr.port), Some(&mut addr.ip)) == -1 {
            eprintln!("Endpoint::getLocalAddress: Failed");
        }
        addr
    }

    /// `getRemoteAddress`.
    pub fn remote(&self) -> Address {
        let mut addr = Address::new(0, 0);
        if self.remote_address(Some(&mut addr.port), Some(&mut addr.ip)) == -1 {
            eprintln!("Endpoint::getRemoteAddress: Failed");
        }
        addr
    }

    /// `Endpoint::c_str`: the bound address as a string.
    pub fn c_str(&self) -> String {
        self.local().c_str()
    }

    /// `getremotehostname`: the peer's canonical name, or -1.
    pub fn remote_host_name(&self, host: &mut String) -> i32 {
        match socket::remote_address(self.socket_) {
            Some((_, ip)) => match socket::host_by_addr(ip) {
                Some(name) => {
                    *host = name;
                    0
                }
                None => -1,
            },
            None => -1,
        }
    }

    /// `enableErrorQueue`: `IP_RECVERR` on `SOL_IP`.
    pub fn enable_error_queue(&self, should_enable: bool) {
        socket::set_bool_option(
            self.socket_,
            socket::SOL_IP,
            socket::IP_RECVERR,
            should_enable,
        );
    }

    /// `readFromErrorQueue`: read one queued ICMP error. Fills the kernel
    /// error number, the offending peer and `ee_info`. False when the
    /// queue is empty.
    pub fn read_error_queue(
        &self,
        queued_err_no: &mut i32,
        offender: &mut Address,
        info: &mut u32,
    ) -> bool {
        let Some(queued) = socket::read_error_queue(self.socket_) else {
            return false;
        };
        *queued_err_no = queued.errno as i32;
        offender.ip = queued.ip;
        offender.port = queued.port;
        *info = queued.info;
        true
    }

    /// `sendto(data, size, networkPort, networkAddr = BROADCAST)`.
    pub fn sendto(&self, data: &[u8], port: u16, ip: u32) -> i32 {
        socket::sendto(self.socket_, data, port, ip)
    }

    /// `sendto(data, size, const Mercury::Address &)`.
    pub fn sendto_address(&self, data: &[u8], addr: Address) -> i32 {
        self.sendto(data, addr.port, addr.ip)
    }

    /// `recvfrom(data, size, networkPort, networkAddr)`: the byte count,
    /// or -1 on error.
    pub fn recvfrom(
        &self,
        buf: &mut [u8],
        mut port: Option<&mut u16>,
        mut addr: Option<&mut u32>,
    ) -> i32 {
        if let Some(out) = port.as_deref_mut() {
            *out = 0;
        }
        if let Some(out) = addr.as_deref_mut() {
            *out = 0;
        }
        let (result, source_port, source_ip) = socket::recvfrom(self.socket_, buf);
        if result >= 0 {
            if let Some(out) = port {
                *out = source_port;
            }
            if let Some(out) = addr {
                *out = source_ip;
            }
        }
        result
    }

    /// `recvfrom(data, size, Mercury::Address &)`.
    pub fn recvfrom_address(&self, buf: &mut [u8], addr: &mut Address) -> i32 {
        self.recvfrom(buf, Some(&mut addr.port), Some(&mut addr.ip))
    }

    /// `listen(backlog = 5)`.
    pub fn listen(&self, backlog: i32) -> i32 {
        socket::listen(self.socket_, backlog)
    }

    /// `connect(port, addr = BROADCAST)`.
    pub fn connect(&self, port: u16, ip: u32) -> i32 {
        socket::connect(self.socket_, port, ip)
    }

    /// `accept`: the new endpoint, or `None` when `accept` failed.
    pub fn accept(&self, port: Option<&mut u16>, addr: Option<&mut u32>) -> Option<Endpoint> {
        let (fd, peer_port, peer_ip) = socket::accept_address(self.socket_);
        if fd < 0 {
            eprintln!(
                "Endpoint::accept: accept failed ({}) for socket {}",
                std::io::Error::last_os_error(),
                self.socket_
            );
            return None;
        }
        if let Some(out) = port {
            *out = peer_port;
        }
        if let Some(out) = addr {
            *out = peer_ip;
        }
        Some(Endpoint { socket_: fd })
    }

    /// `send`.
    pub fn send(&self, data: &[u8]) -> i32 {
        socket::send(self.socket_, data)
    }

    /// `recv`: the byte count, 0 on a lost connection, -1 on error.
    pub fn recv(&self, buf: &mut [u8]) -> i32 {
        socket::recv(self.socket_, buf)
    }

    /// `recvAll`: read until `buf` is full. False when the connection was
    /// lost part way through.
    pub fn recv_all(&self, buf: &mut [u8]) -> bool {
        let mut read = 0usize;
        while read < buf.len() {
            let len = self.recv(&mut buf[read..]);
            if len <= 0 {
                if len == 0 {
                    eprintln!("Endpoint::recvAll: Connection lost");
                } else {
                    eprintln!(
                        "Endpoint::recvAll: Got error '{}'",
                        std::io::Error::last_os_error()
                    );
                }
                return false;
            }
            read += len as usize;
        }
        true
    }

    /// `numBytesAvailableForReading`: -1 when the socket is bad.
    pub fn num_bytes_available(&self) -> i32 {
        socket::bytes_available(self.socket_)
    }

    /// `Endpoint::getQueueSizes` (endpoint.cpp:543): 0 when NetworkStats
    /// knows the socket, -1 when it has no record of it.
    pub fn get_queue_sizes(&self, tx: &mut i32, rx: &mut i32) -> i32 {
        if crate::network_stats::get_queue_sizes(self, tx, rx) {
            0
        } else {
            -1
        }
    }

    /// `Endpoint::transmitQueueSize` (endpoint.ipp:726): the queued
    /// transmit bytes; the reference ignores the lookup result here, so
    /// an unknown socket answers 0.
    pub fn transmit_queue_size(&self) -> i32 {
        let mut tx = 0;
        let mut rx = 0;
        self.get_queue_sizes(&mut tx, &mut rx);
        tx
    }

    /// `Endpoint::receiveQueueSize` (endpoint.ipp:740), same story.
    pub fn receive_queue_size(&self) -> i32 {
        let mut tx = 0;
        let mut rx = 0;
        self.get_queue_sizes(&mut tx, &mut rx);
        rx
    }

    /// `getBufferSize(optname)`: only `SO_SNDBUF` / `SO_RCVBUF`, -1 on
    /// error. The reference asserts the option name in debug builds.
    pub fn get_buffer_size(&self, optname: i32) -> i32 {
        debug_assert!(
            optname == socket::SO_SNDBUF || optname == socket::SO_RCVBUF,
            "getBufferSize takes SO_SNDBUF or SO_RCVBUF"
        );
        match socket::get_int_option(self.socket_, socket::SOL_SOCKET, optname) {
            Some(size) => size,
            None => {
                eprintln!(
                    "Endpoint::getBufferSize: Failed to read option {}: {}",
                    if optname == socket::SO_SNDBUF {
                        "SO_SNDBUF"
                    } else {
                        "SO_RCVBUF"
                    },
                    std::io::Error::last_os_error()
                );
                -1
            }
        }
    }

    /// `setBufferSize`: set then re-read, reporting whether the kernel
    /// honoured the request. A request bigger than the kernel allows
    /// therefore reports false rather than silently failing.
    pub fn set_buffer_size(&self, optname: i32, size: i32) -> bool {
        socket::set_int_option(self.socket_, socket::SOL_SOCKET, optname, size);
        self.get_buffer_size(optname) >= size
    }

    /// `getInterfaceFlags(name, flags)`: the kernel flag bits, 0 or -1.
    pub fn get_interface_flags(&self, name: &str, flags: &mut i32) -> i32 {
        match interface::flags(self.socket_, name) {
            Some(value) => {
                *flags = value;
                0
            }
            None => -1,
        }
    }

    /// `getInterfaceAddress(name, address)`: the interface's IPv4
    /// address, 0 or -1 (non-`AF_INET` answers are -1 too).
    pub fn get_interface_address(&self, name: &str, address: &mut u32) -> i32 {
        match interface::address(self.socket_, name) {
            Some(value) => {
                *address = value;
                0
            }
            None => -1,
        }
    }

    /// `getInterfaces(interfaces, netmaskStr = NULL)`: every interface
    /// address, optionally filtered by a `"a.b.c.d/bits"` prefix. A
    /// duplicate IP overwrites its predecessor, exactly like the
    /// reference's `interfaces[s->sin_addr.s_addr] = ...`.
    pub fn get_interfaces(
        &self,
        interfaces: &mut BTreeMap<u32, String>,
        netmask_str: Option<&str>,
    ) -> bool {
        let Some(entries) = interface::conf(self.socket_) else {
            eprintln!("Endpoint::getInterfaces: ioctl(SIOCGIFCONF) failed.");
            return false;
        };

        let netmask = match netmask_str {
            Some(text) => {
                let mask = NetMask::parse(text);
                if !mask.parsed_ok() {
                    eprintln!(
                        "Endpoint::getInterfaces: failed to parse netmask string '{}'",
                        text
                    );
                }
                mask
            }
            None => NetMask::MATCH_ALL,
        };

        for (name, ip) in entries {
            if !netmask.contains_address(ip) {
                continue;
            }
            interfaces.insert(ip, name);
        }
        true
    }

    /// `findDefaultInterface(name)`: the first `IFF_UP|IFF_RUNNING`
    /// interface with an IPv4 address, walking past loopback entries and
    /// only settling for one if nothing better exists -- the reference's
    /// stopgap until "whatever interface the default route goes over".
    /// 0 when a name was found, -1 otherwise.
    pub fn find_default_interface(&self, name: &mut String) -> i32 {
        let mut ret = -1;
        for (_, candidate) in interface::name_index() {
            let mut flags = 0;
            self.get_interface_flags(&candidate, &mut flags);
            if (flags & interface::IFF_UP) != 0 && (flags & interface::IFF_RUNNING) != 0 {
                let mut addr = 0;
                if self.get_interface_address(&candidate, &mut addr) == 0 {
                    *name = candidate;
                    ret = 0;
                    // We only stop if it's not a loopback address,
                    // otherwise we continue, hoping to find a better one.
                    if (flags & interface::IFF_LOOPBACK) == 0 {
                        break;
                    }
                }
            }
        }
        ret
    }

    /// `findIndicatedInterface(spec, name)`: resolve a listening-interface
    /// spec to a real interface name. Accepted forms, in the reference's
    /// order: `"ip/bits"` (netmask), a bare interface name, a bare IP
    /// (netmask bits 32). Returns 0 on a match, -1 on an unparseable spec,
    /// -2 when it parsed but no interface matches.
    pub fn find_indicated_interface(&self, spec: &str, name: &mut String) -> i32 {
        // Start with it cleared.
        name.clear();
        if spec.is_empty() {
            return -1;
        }

        // `strncpy(iftemp, spec, IFNAMSIZ); iftemp[IFNAMSIZ] = 0`: the
        // working copy keeps at most 16 bytes.
        let iftemp: String =
            String::from_utf8_lossy(&spec.as_bytes()[..spec.len().min(16)]).into_owned();

        let mut netmask_bits = 32;
        let mut addr = 0;

        match spec.find('/') {
            // See if it's a netmask; the IP part itself must fit in an
            // interface-name slot for the reference to try it.
            Some(slash) if slash <= 16 => {
                let ip_part = &spec[..slash];
                let bits_part = &spec[slash + 1..];
                let mut ok = match convert_address(ip_part) {
                    Some(value) => {
                        addr = value;
                        true
                    }
                    None => false,
                };
                netmask_bits = atoi(bits_part);
                ok &= netmask_bits > 0 && netmask_bits <= 32;
                if !ok {
                    eprintln!(
                        "Endpoint::findIndicatedInterface: netmask match {} length {} is not valid.",
                        ip_part, bits_part
                    );
                    return -1;
                }
            }
            _ => {
                if self.get_interface_address(&iftemp, &mut addr) == 0 {
                    // Specified name of interface.
                    *name = iftemp.chars().take(interface::IFNAMSIZ - 1).collect();
                } else {
                    match convert_address(spec) {
                        Some(value) => {
                            addr = value;
                            netmask_bits = 32; // Redundant but instructive.
                        }
                        None => {
                            eprintln!(
                                "Endpoint::findIndicatedInterface: No interface matching interface spec '{}' found",
                                spec
                            );
                            return -1;
                        }
                    }
                }
            }
        }

        // If we haven't set a name yet then we're supposed to look up the
        // ip address.
        if name.is_empty() {
            let netmask_shift = (32 - netmask_bits) as u32;
            let netmask_match = socket::ntohl(addr);

            for candidate in interface::name_index().into_iter().map(|(_, name)| name) {
                let mut tip = 0;
                if self.get_interface_address(&candidate, &mut tip) == 0 {
                    let htip = socket::ntohl(tip);
                    if (htip >> netmask_shift) == (netmask_match >> netmask_shift) {
                        *name = candidate;
                        break;
                    }
                }
            }

            if name.is_empty() {
                // The raw bytes of `addr` in memory spell the dotted quad
                // back on a little-endian host, which is how the
                // reference's `(uint8 *)&addr` prints them.
                let qik = addr.to_ne_bytes();
                eprintln!(
                    "Endpoint::findIndicatedInterface: No interface matching netmask spec '{}' found (evals to {}:{}:{}:{}/{})",
                    spec, qik[0], qik[1], qik[2], qik[3], netmask_bits
                );
                return -2; // Parsing ok, just didn't match.
            }
        }

        0
    }
}

impl Drop for Endpoint {
    /// `~Endpoint`: closes the socket unless it was detached.
    fn drop(&mut self) {
        self.close();
    }
}

impl Default for Endpoint {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use socket::{htonl, htons};

    /// `127.0.0.1` as the kernel keeps it.
    const LOOPBACK: u32 = 0x0100_007f;

    fn udp_endpoint() -> Endpoint {
        let mut ep = Endpoint::new();
        assert!(!ep.good(), "a fresh endpoint holds no socket");
        assert_eq!(ep.fileno(), NO_SOCKET);
        ep.socket(socket::SOCK_DGRAM);
        assert!(ep.good(), "socket() failed");
        ep
    }

    /// A descriptor the kernel will never hand out, for driving failure
    /// paths. A *closed* descriptor would not do: closing frees its
    /// number, and a sibling test thread reopens it straight away, so the
    /// endpoint would be naming someone else's pipe or socket. The fd
    /// limit is refused forever, which keeps the failures deterministic.
    fn dead_fd() -> RawFd {
        socket::descriptor_limit()
    }

    /// `writeToString` prints octets as unsigned bytes, and a short buffer
    /// still reports the full length.
    #[test]
    fn the_address_renders_the_way_the_reference_prints_it() {
        let addr = Address::new(htonl(0x7f000001), htons(8080));
        assert_eq!(addr.c_str(), "127.0.0.1:8080");
        assert_eq!(addr.ip_as_string(), "127.0.0.1");

        let mut buf = [0u8; 32];
        let written = addr.write_to_string(&mut buf);
        assert_eq!(written, 14);
        assert_eq!(
            std::str::from_utf8(&buf[..written]).unwrap(),
            "127.0.0.1:8080"
        );

        let mut short = [0u8; 4];
        assert_eq!(
            addr.write_to_string(&mut short),
            14,
            "snprintf reports the full length"
        );
        assert_eq!(&short, b"127.");

        // No leading zeros, unlike C's %03d style formatters.
        let sparse = Address::new(htonl(0x0a000001), htons(1));
        assert_eq!(sparse.c_str(), "10.0.0.1:1");
    }

    /// Comparison is by IP then port, ignoring the salt.
    #[test]
    fn address_comparison_ignores_the_salt() {
        let a = Address::new(htonl(0x7f000001), htons(1));
        let same = Address::new(htonl(0x7f000001), htons(1));
        let higher = Address::new(htonl(0x7f000002), htons(1));
        let higher_port = Address::new(htonl(0x7f000001), htons(2));

        assert_eq!(a, same);
        assert_eq!(a, Address { salt: 99, ..a }, "salt is not compared");
        assert_ne!(a, higher);
        assert!(a < higher_port && higher_port > a);
        assert!(a < higher);

        // Ports are network byte order, so the ordering of the two wire values
        // is the ordering the reference's map uses.
        let mut map = std::collections::BTreeMap::new();
        map.insert(a, "first");
        map.insert(higher_port, "second");
        assert_eq!(
            map.range(..)
                .map(|(k, _)| socket::ntohs(k.port))
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
    }

    /// `NONE` and `isNone` mean "no IP", not "no port".
    #[test]
    fn none_is_the_zero_address() {
        assert_eq!(Address::default(), NONE);
        assert!(NONE.is_none());
        assert!(Address::new(0, htons(1234)).is_none());
        assert!(!Address::new(htonl(0x7f000001), 0).is_none());
        assert_eq!(
            Address::new(0, htons(1234)).c_str(),
            "0.0.0.0:1234",
            "no IP still renders as 0.0.0.0"
        );
    }

    /// A UDP endpoint binds, reports its port, and round-trips a packet
    /// back through `sendto`/`recvfrom_address`.
    #[test]
    fn udp_sendto_and_recvfrom_round_trip() {
        let local = udp_endpoint();
        assert_eq!(local.bind(0, INADDR_ANY), 0);
        let bound = local.local();
        assert_ne!(bound.port, 0, "the kernel picked a port");
        assert_eq!(bound.ip, 0);
        assert_eq!(
            local.c_str(),
            format!("0.0.0.0:{}", socket::ntohs(bound.port))
        );

        let peer = udp_endpoint();
        assert_eq!(peer.bind(0, LOOPBACK), 0);
        let peer_addr = peer.local();

        // One packet out on each `sendto` overload; both arrive at the peer.
        assert_eq!(local.sendto_address(b"ping", peer_addr), 4);
        assert_eq!(local.sendto(b"pong", peer_addr.port, peer_addr.ip), 4);

        let mut buf = [0u8; 16];
        let mut seen = Address::default();
        let got = peer.recvfrom_address(&mut buf, &mut seen);
        assert_eq!(got, 4);
        assert_eq!(&buf[..4], b"ping");
        // A socket bound to INADDR_ANY sends from the address the kernel picks
        // (loopback, since that is where the peer is); only the bound port
        // is echoed back.
        assert_eq!(seen.ip, LOOPBACK);
        assert_eq!(seen.port, bound.port);

        let mut again = [0u8; 16];
        let mut seen_again = Address::default();
        assert_eq!(
            peer.recvfrom(
                &mut again,
                Some(&mut seen_again.port),
                Some(&mut seen_again.ip)
            ),
            4
        );
        assert_eq!(&again[..4], b"pong");
        assert_eq!(seen_again.ip, LOOPBACK);

        // The out-parameters are cleared first, so a bad socket reports
        // emptiness instead of stale data.
        let mut port = 0u16;
        let mut ip = u32::MAX;
        assert_eq!(
            Endpoint::new().local_address(Some(&mut port), Some(&mut ip)),
            -1
        );
        assert_eq!(port, 0);
        assert_eq!(ip, 0, "local_address clears its out-parameters");
    }

    /// A TCP listener accepts, reports the peer, and `recvAll` reports a
    /// lost connection as false.
    #[test]
    fn tcp_accept_reports_the_peer_and_recv_all_detects_eof() {
        let mut listener = Endpoint::new();
        listener.socket(socket::SOCK_STREAM);
        assert_eq!(listener.set_reuseaddr(true), 0);
        assert_eq!(listener.bind(0, INADDR_ANY), 0);
        let port = listener.local().port;
        assert_eq!(listener.listen(5), 0);

        let mut client = Endpoint::new();
        client.socket(socket::SOCK_STREAM);
        assert_eq!(client.connect(port, LOOPBACK), 0);

        let mut peer_port = 0u16;
        let mut peer_ip = 0u32;
        let mut conn = listener
            .accept(Some(&mut peer_port), Some(&mut peer_ip))
            .expect("accept() failed");
        assert_eq!(peer_ip, LOOPBACK);
        assert_ne!(peer_port, 0);

        assert_eq!(conn.remote().ip, LOOPBACK);
        assert_eq!(client.remote().ip, LOOPBACK);

        let mut host = String::new();
        assert_eq!(conn.remote_host_name(&mut host), 0);
        assert!(!host.is_empty(), "loopback resolves to a name");

        // recvAll loops until the buffer is full, then reports a lost
        // connection when the peer closes.
        assert_eq!(client.send(b"hello"), 5);
        let mut buf = [0u8; 5];
        assert!(conn.recv_all(&mut buf));
        assert_eq!(&buf, b"hello");

        client.close();
        let mut eof = [0u8; 4];
        assert_eq!(conn.recv(&mut eof), 0, "EOF reads as zero");
        assert!(!conn.recv_all(&mut eof), "recvAll reports the loss");

        let conn_fd = conn.detach();
        assert!(!conn.good());
        assert_eq!(conn.fileno(), NO_SOCKET);
        assert_eq!(socket::close(conn_fd), 0);
        drop(listener);
    }

    /// An endpoint with no socket refuses work, and `close` is idempotent
    /// because a closed descriptor is reported as "nothing to close".
    #[test]
    fn close_is_a_noop_when_there_is_no_socket() {
        let mut empty = Endpoint::new();
        assert_eq!(empty.close(), 0, "NO_SOCKET closes to 0");
        assert_eq!(empty.close(), 0, "the second close is still 0");
        assert!(!empty.good());

        let mut live = Endpoint::new();
        live.socket(socket::SOCK_DGRAM);
        assert!(live.good());
        assert_eq!(live.close(), 0);
        assert!(!live.good(), "the descriptor is cleared");
        assert_eq!(live.fileno(), NO_SOCKET);
        assert_eq!(
            socket::recv(dead_fd(), &mut [0u8; 4]),
            -1,
            "a dead descriptor will not read"
        );

        // Detach hands the fd out instead of closing it.
        let mut handed = Endpoint::new();
        handed.socket(socket::SOCK_DGRAM);
        assert_eq!(handed.bind(0, INADDR_ANY), 0);
        let handed_off = handed.detach();
        assert!(!handed.good());
        assert_eq!(socket::bytes_available(handed_off), 0, "the fd still works");
        socket::close(handed_off);
    }

    /// `setBufferSize` reports success only when the kernel honoured the
    /// request, which is how an unsatisfiable request is detected.
    #[test]
    fn buffer_sizes_report_whether_they_were_honoured() {
        let ep = udp_endpoint();
        assert!(ep.set_buffer_size(socket::SO_SNDBUF, 4096));
        assert!(ep.set_buffer_size(socket::SO_RCVBUF, 4096));
        // Kernels double the requested size, so only a lower bound is
        // guaranteed; the point of `setBufferSize` is that it re-reads and
        // compares rather than trusting the setsockopt return value.
        assert!(ep.get_buffer_size(socket::SO_SNDBUF) >= 4096);
        assert!(ep.get_buffer_size(socket::SO_RCVBUF) >= 4096);

        // Kernels cap socket buffers; a huge request comes back short, so
        // the unsatisfiable case reports false instead of pretending.
        assert!(
            !ep.set_buffer_size(socket::SO_SNDBUF, 1 << 30),
            "1 GiB is refused"
        );
        assert!(
            !ep.set_buffer_size(socket::SO_RCVBUF, i32::MAX),
            "INT_MAX is refused"
        );
    }

    /// `numBytesAvailableForReading` counts what is queued, and -1 when the
    /// socket is gone.
    #[test]
    fn num_bytes_available_counts_the_queued_packet() {
        let mut ep = udp_endpoint();
        assert_eq!(ep.bind(0, LOOPBACK), 0);
        let addr = ep.local();
        assert_eq!(ep.num_bytes_available(), 0);
        let other = udp_endpoint();
        assert_eq!(other.sendto_address(b"one", addr), 3);
        assert_eq!(ep.num_bytes_available(), 3);

        ep.close();
        assert!(!ep.good(), "closing clears the descriptor");
        // A freed number could be a sibling test thread's pipe by now --
        // FIONREAD counts on those too -- so probe a number nothing owns.
        ep.set_file_descriptor(dead_fd());
        assert_eq!(ep.num_bytes_available(), -1, "a dead socket reports -1");
        let _ = other;
    }

    /// The per-socket error queue reports an undeliverable packet with the
    /// destination as its offender.
    #[test]
    fn the_error_queue_reports_a_refused_packet() {
        let ep = udp_endpoint();
        ep.enable_error_queue(true);
        assert_eq!(ep.sendto(b"no listener", htons(1), LOOPBACK), 11);

        let mut err_no = 0;
        let mut offender = Address::default();
        let mut info = 0u32;
        assert!(ep.read_error_queue(&mut err_no, &mut offender, &mut info));
        assert_eq!(err_no, socket::ECONNREFUSED);
        assert_eq!(offender.ip, LOOPBACK);
        assert!(!ep.read_error_queue(&mut err_no, &mut offender, &mut info));
    }

    /// Non-blocking mode is a plain option toggle, as is broadcast.
    #[test]
    fn socket_options_toggle() {
        let ep = udp_endpoint();
        assert_eq!(ep.set_nonblocking(true), 0);
        assert_eq!(ep.set_nonblocking(false), 0);
        assert_eq!(ep.set_broadcast(true), 0);
        assert_eq!(ep.set_keepalive(false), 0);
    }

    /// Dotted quads resolve straight away, names through the lookup, and
    /// garbage resolves to nothing.
    #[test]
    fn convert_address_handles_numbers_names_and_garbage() {
        assert_eq!(convert_address("127.0.0.1"), Some(LOOPBACK));
        assert_eq!(convert_address("0.0.0.0"), Some(0));
        assert_eq!(convert_address("255.255.255.255"), Some(BROADCAST));
        assert_eq!(convert_address("localhost"), Some(LOOPBACK));
        assert_eq!(convert_address("definitely.not.a.host.invalid"), None);
        assert_eq!(convert_address(""), None);
        assert_eq!(convert_address("1.2.3.4.5"), None);
    }

    /// `setFileDescriptor` adopts a descriptor created elsewhere, and the
    /// address accessors degrade gracefully when there is nothing behind
    /// the descriptor.
    #[test]
    fn adopted_descriptors_and_failed_lookups() {
        let raw = socket::create_socket(socket::SOCK_DGRAM);
        assert!(raw >= 0);

        let mut adopted = Endpoint::new();
        assert!(!adopted.good());
        adopted.set_file_descriptor(raw);
        assert!(adopted.good());
        assert_eq!(adopted.fileno(), raw);

        // Hand the descriptor back before `dead` borrows it: two endpoints
        // holding one number would both close it on drop, and the second
        // close can land on a number a sibling test thread has reopened.
        assert_eq!(adopted.detach(), raw);
        assert!(!adopted.good());

        // A descriptor the kernel no longer owns fails every lookup, which
        // is what `local`/`remote` log about.
        let mut dead = Endpoint::new();
        dead.set_file_descriptor(dead_fd());

        let mut port = 0u16;
        let mut ip = u32::MAX;
        assert_eq!(dead.local_address(Some(&mut port), Some(&mut ip)), -1);
        assert_eq!(port, 0);
        assert_eq!(ip, 0, "the out-parameters come back empty");

        // `local` and `remote` log the failure and hand back an empty
        // address rather than panicking.
        assert_eq!(dead.local(), Address::default());
        assert_eq!(dead.remote(), Address::default());
        assert_eq!(dead.c_str(), "0.0.0.0:0");

        // `getRemoteAddress` keeps whatever the caller had: the reference
        // does not clear these.
        let mut keep_port = htons(4321);
        let mut keep_ip = 0xDEAD_BEEF;
        assert_eq!(
            dead.remote_address(Some(&mut keep_port), Some(&mut keep_ip)),
            -1
        );
        assert_eq!(keep_port, htons(4321));
        assert_eq!(keep_ip, 0xDEAD_BEEF);

        // `recvfrom` with no out-parameters still counts its failure.
        assert_eq!(dead.recvfrom(&mut [0u8; 4], None, None), -1);
        assert_eq!(
            dead.recvfrom_address(&mut [0u8; 4], &mut Address::default()),
            -1
        );

        dead.set_file_descriptor(raw);
    }

    /// `Hash` ignores the salt, the way `==` does, so addresses stay usable
    /// as map keys for peers whose salt changes each send.
    #[test]
    fn addresses_hash_the_way_they_compare() {
        let mut set = std::collections::HashSet::new();
        let plain = Address::new(LOOPBACK, htons(4321));
        let mut salty = Address::new(LOOPBACK, htons(4321));
        salty.salt = 77;

        assert!(set.insert(plain));
        assert!(set.contains(&salty), "the salt is not part of the key");
        assert_eq!(set.len(), 1);

        let different = Address::new(LOOPBACK, htons(4322));
        assert!(set.insert(different));
        assert_eq!(set.len(), 2);
    }

    /// `close` only clears the descriptor when the kernel agrees, so a
    /// stuck descriptor stays attached instead of being leaked.
    #[test]
    fn a_failed_close_keeps_the_descriptor() {
        let stuck = dead_fd();

        let mut stubborn = Endpoint::new();
        stubborn.set_file_descriptor(stuck);
        assert!(stubborn.good());

        // The descriptor is only cleared on success, so every retry finds
        // it still attached and reports the failure again.
        assert_ne!(stubborn.close(), 0, "a dead descriptor cannot close");
        assert_ne!(stubborn.close(), 0, "the retry reports it again");
        assert!(stubborn.good(), "the descriptor is kept for the retry");
        assert_eq!(stubborn.fileno(), stuck);

        // Detach is the way out, and it is handed the number it still holds.
        assert_eq!(stubborn.detach(), stuck);
        assert!(!stubborn.good());
    }

    /// A peer whose address has no name and a peer that vanished both
    /// report -1, and `accept` on a socket that never listened logs the
    /// failure instead of handing out a bad endpoint.
    #[test]
    fn the_failure_paths_report_and_log() {
        let mut listener = Endpoint::new();
        listener.socket(socket::SOCK_STREAM);
        assert_eq!(listener.set_reuseaddr(true), 0);
        assert_eq!(listener.bind(0, INADDR_ANY), 0);
        let port = listener.local().port;
        assert_eq!(listener.listen(5), 0);

        let mut client = Endpoint::new();
        client.socket(socket::SOCK_STREAM);
        assert_eq!(client.connect(port, LOOPBACK), 0);

        // Loopback resolves, so a name comes back.
        let mut host = String::new();
        assert_eq!(client.remote_host_name(&mut host), 0);
        assert!(!host.is_empty());

        // A socket with no peer has no address to look a name up for, and
        // a failed lookup leaves the caller's buffer alone.
        let mut empty = String::from("stale");
        let mut unbound = Endpoint::new();
        unbound.socket(socket::SOCK_DGRAM);
        assert_eq!(unbound.remote_host_name(&mut empty), -1);
        assert_eq!(empty, "stale", "a failure leaves the buffer alone");

        // `accept` needs a listening socket.
        let mut junk_port = 0u16;
        let mut junk_ip = 0u32;
        assert!(
            client
                .accept(Some(&mut junk_port), Some(&mut junk_ip))
                .is_none()
        );
        assert!(
            Endpoint::new().accept(None, None).is_none(),
            "an endpoint with no socket cannot accept"
        );

        // `recvAll` reports an error once the descriptor stops answering;
        // an unconnected socket would block instead of failing.
        let mut broken = Endpoint::new();
        broken.set_file_descriptor(dead_fd());
        assert!(
            !broken.recv_all(&mut [0u8; 4]),
            "a dead descriptor is an error"
        );

        // `getBufferSize` reports the failure instead of a size.
        assert_eq!(broken.get_buffer_size(socket::SO_SNDBUF), -1);
        assert_eq!(broken.get_buffer_size(socket::SO_RCVBUF), -1);

        // The very same failures come back through `Default`.
        let blank = Endpoint::default();
        assert!(!blank.good());
        let mut blank_port = 0u16;
        let mut blank_ip = 0u32;
        assert_eq!(
            blank.local_address(Some(&mut blank_port), Some(&mut blank_ip)),
            -1
        );
        assert_eq!(
            blank.remote_address(Some(&mut blank_port), Some(&mut blank_ip)),
            -1
        );
        assert_eq!(blank.remote_host_name(&mut empty), -1);
        assert_eq!(
            blank.recvfrom(&mut [0u8; 4], Some(&mut blank_port), Some(&mut blank_ip)),
            -1
        );
        assert!(
            blank
                .accept(Some(&mut blank_port), Some(&mut blank_ip))
                .is_none()
        );
    }

    /// The address accessors write their out-parameters on the happy path,
    /// which is the half the failure tests above cannot reach.
    #[test]
    fn the_address_accessors_fill_their_out_parameters() {
        let local = udp_endpoint();
        assert_eq!(local.bind(0, LOOPBACK), 0);
        let bound = local.local();

        let mut port = 0u16;
        let mut ip = 0u32;
        assert_eq!(local.local_address(Some(&mut port), Some(&mut ip)), 0);
        assert_eq!(port, bound.port);
        assert_eq!(ip, LOOPBACK);

        // One out-parameter at a time, and neither at all.
        let mut only_port = 0u16;
        let mut only_ip = u32::MAX;
        assert_eq!(local.local_address(Some(&mut only_port), None), 0);
        assert_eq!(only_port, bound.port);
        assert_eq!(only_ip, u32::MAX, "an untouched parameter is untouched");
        assert_eq!(local.local_address(None, Some(&mut only_ip)), 0);
        assert_eq!(only_ip, LOOPBACK);
        assert_eq!(local.local_address(None, None), 0);

        // `remote_address` reports the peer of a connected socket.
        let mut listener = Endpoint::new();
        listener.socket(socket::SOCK_STREAM);
        assert_eq!(listener.set_reuseaddr(true), 0);
        assert_eq!(listener.bind(0, INADDR_ANY), 0);
        let listener_port = listener.local().port;
        assert_eq!(listener.listen(5), 0);

        let mut client = Endpoint::new();
        client.socket(socket::SOCK_STREAM);
        assert_eq!(client.connect(listener_port, LOOPBACK), 0);

        let mut peer_port = 0u16;
        let mut peer_ip = 0u32;
        let conn = listener
            .accept(Some(&mut peer_port), Some(&mut peer_ip))
            .expect("accept() failed");
        assert_eq!(peer_ip, LOOPBACK);
        assert_ne!(peer_port, 0);

        let mut seen_port = 0u16;
        let mut seen_ip = u32::MAX;
        assert_eq!(
            conn.remote_address(Some(&mut seen_port), Some(&mut seen_ip)),
            0
        );
        assert_eq!(seen_ip, LOOPBACK);
        assert_eq!(seen_port, peer_port, "the peer port is the client's own");
        assert_eq!(conn.remote_address(Some(&mut seen_port), None), 0);
        assert_eq!(conn.remote_address(None, Some(&mut seen_ip)), 0);
        assert_eq!(conn.remote_address(None, None), 0);

        // A name is found when the peer address resolves.
        let mut host = String::new();
        assert_eq!(conn.remote_host_name(&mut host), 0);
        assert!(!host.is_empty());
        drop(conn);

        // A UDP `connect` sets a peer without any packet travelling, so
        // the address lookup can be exercised for an address that has no
        // name in /etc/hosts.
        let nameless = socket::htonl(0x01ff_0a00);
        let mut unnameable = Endpoint::new();
        unnameable.socket(socket::SOCK_DGRAM);
        assert_eq!(unnameable.connect(9, nameless), 0);
        let mut nameless_host = String::from("stale");
        assert_eq!(
            unnameable.remote_host_name(&mut nameless_host),
            -1,
            "10.0.255.1 has no name"
        );
        assert_eq!(
            nameless_host, "stale",
            "a failed lookup leaves the buffer alone"
        );
        assert_eq!(unnameable.remote().ip, nameless);

        // Out-parameters set only once: `recvfrom` and `accept` write the
        // port or the address independently, and a success writes both.
        let receiver = udp_endpoint();
        assert_eq!(receiver.bind(0, LOOPBACK), 0);
        let receiver_addr = receiver.local();

        let sender = udp_endpoint();
        assert_eq!(sender.bind(0, LOOPBACK), 0);
        assert_eq!(
            sender.sendto(b"both", receiver_addr.port, receiver_addr.ip),
            4
        );

        assert_eq!(
            sender.sendto(b"again", receiver_addr.port, receiver_addr.ip),
            5
        );

        let mut mut_port = 0u16;
        let mut mut_ip = u32::MAX;
        assert_eq!(
            receiver.recvfrom(&mut [0u8; 4], Some(&mut mut_port), None),
            4,
            "the port is filled"
        );
        assert_eq!(mut_port, sender.local().port);
        assert_eq!(mut_ip, u32::MAX, "the untouched parameter is untouched");

        assert_eq!(receiver.recvfrom(&mut [0u8; 4], None, Some(&mut mut_ip)), 4);
        assert_eq!(mut_ip, LOOPBACK);

        let mut listener = Endpoint::new();
        listener.socket(socket::SOCK_STREAM);
        assert_eq!(listener.set_reuseaddr(true), 0);
        assert_eq!(listener.bind(0, INADDR_ANY), 0);
        let listener_port = listener.local().port;
        assert_eq!(listener.listen(5), 0);

        let mut visitor = Endpoint::new();
        visitor.socket(socket::SOCK_STREAM);
        assert_eq!(visitor.connect(listener_port, LOOPBACK), 0);

        let mut only_peer_port = 0u16;
        let mut mut_addr = u32::MAX;
        let conn = listener.accept(Some(&mut only_peer_port), Some(&mut mut_addr));
        assert!(conn.is_some(), "the listener has a connection");
        assert_ne!(only_peer_port, 0, "the peer port is filled");
        assert_eq!(mut_addr, LOOPBACK, "the peer address is filled");
        let _ = visitor;
    }

    /// `atoi`'s loose C parse: blanks, a sign, a trailing tail, and no
    /// digits at all.
    #[test]
    fn atoi_takes_the_leading_integer() {
        assert_eq!(super::atoi("24"), 24);
        assert_eq!(super::atoi("  -7x"), -7);
        assert_eq!(super::atoi("+3bits"), 3);
        assert_eq!(super::atoi(""), 0);
        assert_eq!(super::atoi("bits"), 0);
    }

    /// `NetMask::parse`'s grammar and its match-all failure mode.
    #[test]
    fn netmask_parsing_and_prefix_matching() {
        // 10.0.0.0/8: the mask word is built with the last octet highest,
        // so "10" lands in the low byte, which against the raw network-
        // order word is the leading octet.
        let mask = NetMask::parse("10.0.0.0/8");
        assert!(mask.parsed_ok());
        // The raw word of an address is the network-order byte string read
        // in host order, so 10.1.2.3 is from_ne_bytes([10, 1, 2, 3]) and
        // its leading octet sits in the low byte.
        let ten_raw = u32::from_ne_bytes([10, 1, 2, 3]);
        assert!(mask.contains_address(ten_raw));
        assert!(mask.contains_address(ten_raw | 0xffff_ff00), "10.x.y.z");
        assert!(!mask.contains_address(u32::from_ne_bytes([11, 0, 0, 0])));

        let exact = NetMask::parse("127.0.0.1/32");
        assert!(exact.contains_address(LOOPBACK));
        assert!(!exact.contains_address(u32::from_ne_bytes([126, 0, 0, 1])));

        // Unparseable forms all become the match-all mask, bits 0.
        for bad in ["no-slash", "1.2.3/8", "a.b.c.d/8", "1.2.3.4", "1.2.3.4/x"] {
            let mask = NetMask::parse(bad);
            assert!(!mask.parsed_ok(), "{bad} should not parse");
            assert!(mask.contains_address(0xdead_beef), "{bad} matches all");
        }
    }

    /// The `lo` interface carries the loopback address, under whichever
    /// name the kernel lists it; this is the fixture the reference's own
    /// unit test relies on.
    #[test]
    fn the_loopback_interface_is_listed_with_its_address() {
        let mut endpoint = Endpoint::new();
        endpoint.socket(socket::SOCK_DGRAM);
        let mut interfaces = BTreeMap::new();
        assert!(endpoint.get_interfaces(&mut interfaces, None));
        assert!(
            interfaces.values().any(|name| name == "lo"),
            "lo is listed: {interfaces:?}"
        );
        assert!(
            interfaces.contains_key(&LOOPBACK),
            "127.0.0.1 is a listed address: {interfaces:?}"
        );
    }

    /// The netmask filter narrows the enumeration, and a malformed filter
    /// is a warning (logged) plus the whole list.
    #[test]
    fn the_netmask_filter_narrows_get_interfaces() {
        let mut endpoint = Endpoint::new();
        endpoint.socket(socket::SOCK_DGRAM);

        // 127.0.0.0/8 keeps only loopback-shaped addresses.
        let mut filtered = BTreeMap::new();
        assert!(endpoint.get_interfaces(&mut filtered, Some("127.0.0.0/8")));
        assert!(filtered.contains_key(&LOOPBACK));
        assert!(
            filtered.keys().all(|ip| *ip & 0x0000_00ff == 127),
            "every kept address is a 127.x: {filtered:?}"
        );

        // 10.0.0.0/8: loopback is not in that range, and on a test host
        // nothing else need be either.
        let mut none = BTreeMap::new();
        assert!(endpoint.get_interfaces(&mut none, Some("10.255.0.0/16")));
        assert!(!none.contains_key(&LOOPBACK));

        // A malformed filter warns but still returns everything.
        let mut all = BTreeMap::new();
        assert!(endpoint.get_interfaces(&mut all, Some("not-a-mask")));
        let mut bare = BTreeMap::new();
        assert!(endpoint.get_interfaces(&mut bare, None));
        assert_eq!(all, bare, "the failed filter is treated as match-all");
    }

    /// `getInterfaceFlags`/`getInterfaceAddress` succeed on `lo` and fail
    /// on an unknown name, the 0 and -1 arms.
    #[test]
    fn interface_flags_and_address_queries() {
        let mut endpoint = Endpoint::new();
        endpoint.socket(socket::SOCK_DGRAM);

        let mut flags = 0;
        assert_eq!(endpoint.get_interface_flags("lo", &mut flags), 0);
        assert_ne!(
            flags & platform::interface::IFF_LOOPBACK,
            0,
            "lo is loopback"
        );

        let mut addr = 0;
        assert_eq!(endpoint.get_interface_address("lo", &mut addr), 0);
        assert_eq!(addr, LOOPBACK);

        assert_eq!(endpoint.get_interface_flags("no-such-if", &mut flags), -1);
        assert_eq!(endpoint.get_interface_address("no-such-if", &mut addr), -1);
    }

    /// `findDefaultInterface` finds something with an address; on a test
    /// host that is often `lo` itself, which the reference accepts.
    #[test]
    fn a_default_interface_is_found() {
        let mut endpoint = Endpoint::new();
        endpoint.socket(socket::SOCK_DGRAM);
        let mut name = String::new();
        assert_eq!(endpoint.find_default_interface(&mut name), 0);
        assert!(!name.is_empty(), "a name was chosen: {name:?}");

        // The chosen interface really has the flags the search requires.
        let mut flags = 0;
        assert_eq!(endpoint.get_interface_flags(&name, &mut flags), 0);
        assert_ne!(flags & platform::interface::IFF_UP, 0);
        assert_ne!(flags & platform::interface::IFF_RUNNING, 0);
    }

    /// `findIndicatedInterface`'s grammar, against the real loopback:
    /// bare name, bare IP, and netmask forms all resolve; garbage forms
    /// report -1 or -2 with a cleared name.
    #[test]
    fn interface_spec_resolution() {
        let mut endpoint = Endpoint::new();
        endpoint.socket(socket::SOCK_DGRAM);

        // A bare interface name: `lo` resolves to itself.
        let mut name = String::new();
        assert_eq!(endpoint.find_indicated_interface("lo", &mut name), 0);
        assert_eq!(name, "lo");

        // A bare IP: the netmask scan finds the interface carrying it.
        let mut name = String::new();
        assert_eq!(endpoint.find_indicated_interface("127.0.0.1", &mut name), 0);
        assert_eq!(name, "lo", "the loopback address resolves to lo");

        // ip/bits: same scan, prefix semantics.
        let mut name = String::new();
        assert_eq!(
            endpoint.find_indicated_interface("127.0.0.0/8", &mut name),
            0
        );
        assert_eq!(name, "lo");

        // Empty and garbage specs: -1, name cleared.
        let mut name = String::new();
        assert_eq!(endpoint.find_indicated_interface("", &mut name), -1);
        assert!(name.is_empty());
        assert_eq!(
            endpoint.find_indicated_interface("no-such-if-xyz", &mut name),
            -1
        );
        assert!(name.is_empty());

        // A valid netmask nothing carries: -2, parsing ok but no match.
        let mut name = String::new();
        assert_eq!(
            endpoint.find_indicated_interface("10.11.12.13/24", &mut name),
            -2
        );
        assert!(name.is_empty());

        // A malformed netmask: -1 with the not-valid message.
        let mut name = String::new();
        assert_eq!(endpoint.find_indicated_interface("1.2.3/40", &mut name), -1);
        assert!(name.is_empty());
        assert_eq!(
            endpoint.find_indicated_interface("1.2.3.4/0", &mut name),
            -1
        );
    }
}
