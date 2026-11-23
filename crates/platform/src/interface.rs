//! `ioctl`-based network-interface enumeration (`endpoint.hpp:175-192`).
//!
//! The reference keeps these on `Endpoint`; the OS calls themselves live
//! here (BW-16-02-01). Linux exposes the `SIOCGIF*` ioctls through
//! `sockios.h`, which libc does not re-export, so the numbers and the
//! `ifreq`/`ifconf` layouts are defined here against the kernel UAPI.

use std::ffi::CStr;
use std::os::fd::RawFd;

/// `IFNAMSIZ`: the kernel's interface-name buffer, NUL included.
pub const IFNAMSIZ: usize = 16;

/// `IFF_UP` (`<net/if.h>`).
pub const IFF_UP: i32 = libc::IFF_UP;
/// `IFF_RUNNING` (`<net/if.h>`).
pub const IFF_RUNNING: i32 = libc::IFF_RUNNING;
/// `IFF_LOOPBACK` (`<net/if.h>`).
pub const IFF_LOOPBACK: i32 = libc::IFF_LOOPBACK;

/// `SIOCGIFCONF` (`linux/sockios.h`): list interfaces and their addresses.
pub const SIOCGIFCONF: libc::c_ulong = 0x8912;
/// `SIOCGIFFLAGS` (`linux/sockios.h`): read an interface's flags.
pub const SIOCGIFFLAGS: libc::c_ulong = 0x8913;
/// `SIOCGIFADDR` (`linux/sockios.h`): read an interface's IPv4 address.
pub const SIOCGIFADDR: libc::c_ulong = 0x8915;

/// The `ifr_ifru` union. Only the two members the reference touches are
/// named; the padding keeps the 40-byte `struct ifreq` stride the kernel
/// writes into an `SIOCGIFCONF` buffer.
#[repr(C)]
union IfrIfru {
    flags: libc::c_short,
    addr: libc::sockaddr,
    pad: [u8; 24],
}

/// `struct ifreq`: the per-entry shape of an `SIOCGIFCONF` buffer and the
/// request shape of `SIOCGIFFLAGS`/`SIOCGIFADDR`.
#[repr(C)]
struct IfReq {
    name: [libc::c_char; IFNAMSIZ],
    ifru: IfrIfru,
}

impl IfReq {
    fn zeroed() -> Self {
        Self {
            name: [0; IFNAMSIZ],
            ifru: IfrIfru { pad: [0; 24] },
        }
    }

    /// The reference's `strncpy(request.ifr_name, name, IFNAMSIZ - 1)`:
    /// clamp to 15 bytes so the buffer stays NUL-terminated.
    fn set_name(&mut self, name: &str) {
        for (dst, &src) in self.name.iter_mut().zip(name.as_bytes()).take(IFNAMSIZ - 1) {
            *dst = src as libc::c_char;
        }
    }

    /// The name, read back the way the kernel laid it down.
    fn name(&self) -> String {
        let end = self.name.iter().position(|&c| c == 0).unwrap_or(IFNAMSIZ);
        let bytes: Vec<u8> = self.name[..end].iter().map(|&c| c as u8).collect();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    /// The four raw bytes of the IPv4 address, the way `sin_addr` sits at
    /// offset 4 of `sockaddr_in`. The reference stores them as-is.
    fn raw_ipv4(&self) -> u32 {
        let addr = unsafe { self.ifru.addr };
        let bytes: Vec<u8> = addr.sa_data[2..6].iter().map(|&b| b as u8).collect();
        u32::from_ne_bytes(bytes.try_into().unwrap())
    }
}

/// `struct ifconf`.
#[repr(C)]
struct IfConf {
    len: libc::c_int,
    buf: *mut libc::c_char,
}

/// `if_nameindex()`: the kernel's interface list, `(index, name)` pairs,
/// released with `if_freenameindex`. An empty list when the kernel refuses
/// (the reference logs the failure; the caller sees no names either way).
pub fn name_index() -> Vec<(u32, String)> {
    let mut result = Vec::new();
    unsafe {
        let list = libc::if_nameindex();
        if list.is_null() {
            return result;
        }
        let mut current = list;
        while !(*current).if_name.is_null() {
            result.push((
                (*current).if_index,
                CStr::from_ptr((*current).if_name)
                    .to_string_lossy()
                    .into_owned(),
            ));
            current = current.add(1);
        }
        libc::if_freenameindex(list);
    }
    result
}

/// `ioctl(SIOCGIFFLAGS)`: the interface's flag bits, or `None` when the
/// kernel refuses (unknown name, no such interface).
pub fn flags(fd: RawFd, name: &str) -> Option<i32> {
    let mut request = IfReq::zeroed();
    request.set_name(name);
    if unsafe { libc::ioctl(fd, SIOCGIFFLAGS, &mut request) } != 0 {
        return None;
    }
    Some(unsafe { request.ifru.flags } as i32)
}

/// `ioctl(SIOCGIFADDR)`: the interface's IPv4 address as the raw
/// network-order byte pattern (the repo's address convention), or `None`
/// when the kernel refuses or the address is not `AF_INET` -- the
/// reference's `getInterfaceAddress` returns -1 for both.
pub fn address(fd: RawFd, name: &str) -> Option<u32> {
    let mut request = IfReq::zeroed();
    request.set_name(name);
    if unsafe { libc::ioctl(fd, SIOCGIFADDR, &mut request) } != 0 {
        return None;
    }
    if unsafe { request.ifru.addr }.sa_family as i32 != libc::AF_INET {
        return None;
    }
    Some(request.raw_ipv4())
}

/// `ioctl(SIOCGIFCONF)`: every listed interface as `(name, ip)`, entries
/// walked at the kernel's `sizeof(struct ifreq)` stride. The reference
/// casts each address straight to `sockaddr_in` without a family check,
/// and so does this.
///
/// The reference fills a fixed 1024-byte buffer once, silently dropping
/// everything past it; the port retries with a doubled buffer until the
/// kernel reports a fit (documented deviation), and returns `None` when
/// the ioctl itself fails.
pub fn conf(fd: RawFd) -> Option<Vec<(String, u32)>> {
    let (buffer, written) = conf_buffer(fd)?;
    let stride = std::mem::size_of::<IfReq>();
    let mut result = Vec::new();
    for index in 0..(written / stride) {
        let request = unsafe { &*(buffer.as_ptr().add(index * stride) as *const IfReq) };
        result.push((request.name(), request.raw_ipv4()));
    }
    Some(result)
}

/// Fill a buffer until the kernel reports it did not hit the end. The
/// buffer is returned with the byte count the kernel wrote.
fn conf_buffer(fd: RawFd) -> Option<(Vec<u8>, usize)> {
    conf_buffer_sized(fd, 1024)
}

/// `conf_buffer` with the first capacity injected, so tests can start
/// the retry loop from a buffer that cannot fit.
fn conf_buffer_sized(fd: RawFd, first_capacity: usize) -> Option<(Vec<u8>, usize)> {
    let mut capacity = first_capacity;
    loop {
        let mut buffer = vec![0u8; capacity];
        let mut ifc = IfConf {
            len: buffer.len() as libc::c_int,
            buf: buffer.as_mut_ptr() as *mut libc::c_char,
        };
        if unsafe { libc::ioctl(fd, SIOCGIFCONF, &mut ifc) } < 0 {
            return None;
        }
        let written = ifc.len as usize;
        if written < capacity {
            return Some((buffer, written));
        }
        // Full to the brim: entries past the end were cut off, grow once.
        capacity *= 2;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::socket;

    /// Any live descriptor works -- the reference calls these on whatever
    /// socket the `Endpoint` holds.
    fn probe_fd() -> RawFd {
        let fd = socket::create_socket(socket::SOCK_DGRAM);
        assert!(fd >= 0, "socket for the probe");
        fd
    }

    /// `lo` exists on every Linux box and is the loopback interface.
    #[test]
    fn the_loopback_interface_is_enumerated() {
        let index = name_index();
        let lo = index.iter().find(|(_, name)| name == "lo");
        assert!(lo.is_some(), "lo is in the kernel list: {index:?}");

        let fd = probe_fd();
        let lo_flags = flags(fd, "lo").expect("SIOCGIFFLAGS on lo");
        assert_ne!(lo_flags & IFF_LOOPBACK, 0, "lo is loopback");
        assert_ne!(lo_flags & IFF_UP, 0, "lo is up");

        // 127.0.0.1 as the repo stores it: the raw network-order bytes in
        // host order (`mercury::endpoint::LOOPBACK`).
        assert_eq!(address(fd, "lo"), Some(0x0100_007f), "lo is 127.0.0.1");
        socket::close(fd);
    }

    /// An unknown name is refused on both queries, the -1 arms of the
    /// reference.
    #[test]
    fn an_unknown_interface_fails_both_queries() {
        let fd = probe_fd();
        assert_eq!(flags(fd, "definitely-no-such-if"), None);
        assert_eq!(address(fd, "definitely-no-such-if"), None);
        socket::close(fd);
    }

    /// `SIOCGIFCONF` lists the loopback address under `lo`.
    #[test]
    fn conf_refuses_a_bad_descriptor() {
        assert_eq!(conf(-1), None);
    }

    /// A first buffer that cannot fit grows once and still answers
    /// (the documented deviation from the fixed 1024-byte buffer).
    #[test]
    fn conf_grows_when_the_first_buffer_is_full() {
        let fd = crate::socket::create_socket(crate::socket::SOCK_DGRAM);
        assert!(fd >= 0);

        let (buffer, written) = conf_buffer_sized(fd, std::mem::size_of::<IfReq>()).unwrap();
        assert!(written > 0, "at least one interface carries an address");
        assert!(
            written >= std::mem::size_of::<IfReq>(),
            "the kernel wrote whole ifreq entries: {written}"
        );

        // The public walk reads the entries out of the grown buffer.
        let entries = conf(fd).unwrap();
        assert!(!entries.is_empty());
        assert!(buffer.len() >= written);
        crate::socket::close(fd);
    }

    #[test]
    fn conf_lists_the_loopback_address() {
        let fd = probe_fd();
        let entries = conf(fd).expect("SIOCGIFCONF");
        assert!(
            entries
                .iter()
                .any(|(name, ip)| name == "lo" && *ip == 0x0100_007f),
            "lo/127.0.0.1 in the conf list: {entries:?}"
        );
        socket::close(fd);
    }

    /// The `ifreq`/`ifconf` strides the kernel writes into an
    /// `SIOCGIFCONF` buffer.
    #[test]
    fn the_kernel_layouts_are_pinned() {
        assert_eq!(std::mem::size_of::<IfReq>(), 40);
        assert_eq!(
            std::mem::size_of::<IfConf>(),
            std::mem::size_of::<usize>() + 8
        );
    }

    /// Names longer than `IFNAMSIZ - 1` are clamped, the reference's
    /// `strncpy(..., IFNAMSIZ - 1)` + explicit NUL.
    #[test]
    fn long_names_are_clamped_to_ifnamsiz() {
        let mut request = IfReq::zeroed();
        let long = "an-extremely-long-interface-name";
        assert!(long.len() > IFNAMSIZ - 1);
        request.set_name(long);
        assert_eq!(request.name().len(), IFNAMSIZ - 1);
    }
}
