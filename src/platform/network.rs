//! This machine's interface addresses and default routes, read from the system
//! without sending anything.

use std::io;
use std::net::IpAddr;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InterfaceAddress {
    pub(crate) name: String,
    pub(crate) address: IpAddr,
    pub(crate) mask: Option<IpAddr>,
    /// The far end of a point-to-point interface.
    pub(crate) peer: Option<IpAddr>,
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn interface_addresses() -> io::Result<Vec<InterfaceAddress>> {
    Err(io::ErrorKind::Unsupported.into())
}

/// The interface of the one default route; `None` when there is none or more
/// than one.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn default_route_interface(_v6: bool) -> io::Result<Option<String>> {
    Err(io::ErrorKind::Unsupported.into())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn interface_addresses() -> io::Result<Vec<InterfaceAddress>> {
    let mut first: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs fills `first` with a list that is freed below.
    if unsafe { libc::getifaddrs(&mut first) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let mut addresses = Vec::new();
    let mut next = first;
    while !next.is_null() {
        // SAFETY: `next` is a node of the live list; its pointers are null or
        // point into it.
        unsafe {
            let entry = &*next;
            next = entry.ifa_next;
            if entry.ifa_addr.is_null() || entry.ifa_name.is_null() {
                continue;
            }
            let family = libc::c_int::from((*entry.ifa_addr).sa_family);
            let Some(address) = sockaddr_ip(entry.ifa_addr, family) else {
                continue;
            };
            let point_to_point = entry.ifa_flags & libc::IFF_POINTOPOINT as libc::c_uint != 0;
            addresses.push(InterfaceAddress {
                name: std::ffi::CStr::from_ptr(entry.ifa_name)
                    .to_string_lossy()
                    .into_owned(),
                address,
                mask: sockaddr_ip(entry.ifa_netmask, family),
                peer: if point_to_point {
                    sockaddr_ip(far_end(entry), family)
                } else {
                    None
                },
            });
        }
    }
    // SAFETY: the list of getifaddrs, freed once, no longer referenced.
    unsafe { libc::freeifaddrs(first) };
    Ok(addresses)
}

#[cfg(target_os = "macos")]
fn far_end(entry: &libc::ifaddrs) -> *const libc::sockaddr {
    entry.ifa_dstaddr
}

#[cfg(target_os = "linux")]
fn far_end(entry: &libc::ifaddrs) -> *const libc::sockaddr {
    entry.ifa_ifu
}

/// `family` is the interface address's: macOS leaves a mask's own family empty
/// and cuts its trailing zeros off.
///
/// # Safety
/// `sockaddr` is null or a socket address of `family`, at least `sa_len` long
/// where the system has that field.
#[cfg(any(target_os = "linux", target_os = "macos"))]
unsafe fn sockaddr_ip(sockaddr: *const libc::sockaddr, family: libc::c_int) -> Option<IpAddr> {
    let (offset, length) = match family {
        libc::AF_INET => (4, 4),
        libc::AF_INET6 => (8, 16),
        _ => return None,
    };
    if sockaddr.is_null() {
        return None;
    }
    #[cfg(target_os = "macos")]
    let present = usize::from((*sockaddr).sa_len).min(offset + length);
    #[cfg(target_os = "linux")]
    let present = offset + length;
    let bytes = std::slice::from_raw_parts(sockaddr.cast::<u8>(), present);
    let mut octets = [0; 16];
    let given = bytes.get(offset..).unwrap_or_default();
    octets[..given.len()].copy_from_slice(given);
    Some(if family == libc::AF_INET {
        IpAddr::from([octets[0], octets[1], octets[2], octets[3]])
    } else {
        IpAddr::from(octets)
    })
}

/// The interface of the one default route; `None` when there is none or more
/// than one.
#[cfg(target_os = "macos")]
pub(crate) fn default_route_interface(v6: bool) -> io::Result<Option<String>> {
    let family = if v6 { libc::AF_INET6 } else { libc::AF_INET };
    let Some(index) = macos::default_route(&macos::route_dump(family)?) else {
        return Ok(None);
    };
    let mut name = [0; libc::IF_NAMESIZE];
    // SAFETY: the buffer has the size the call asks for.
    if unsafe { libc::if_indextoname(index.into(), name.as_mut_ptr()) }.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the call above filled it with a terminated name.
    let name = unsafe { std::ffi::CStr::from_ptr(name.as_ptr()) };
    Ok(Some(name.to_string_lossy().into_owned()))
}

#[cfg(target_os = "macos")]
mod macos {
    use std::io;

    pub(super) fn route_dump(family: libc::c_int) -> io::Result<Vec<u8>> {
        let mut mib = [
            libc::CTL_NET,
            libc::PF_ROUTE,
            0,
            family,
            libc::NET_RT_DUMP,
            0,
        ];
        // the table may grow between the two calls
        for _ in 0..4 {
            let mut size = 0;
            sysctl(&mut mib, std::ptr::null_mut(), &mut size)?;
            size += size / 4 + 1024;
            let mut buffer = vec![0; size];
            match sysctl(&mut mib, buffer.as_mut_ptr().cast(), &mut size) {
                Ok(()) => {
                    buffer.truncate(size);
                    return Ok(buffer);
                }
                Err(error) if error.raw_os_error() == Some(libc::ENOMEM) => {}
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::other("the route table keeps growing"))
    }

    fn sysctl(
        mib: &mut [libc::c_int; 6],
        buffer: *mut libc::c_void,
        size: &mut usize,
    ) -> io::Result<()> {
        // SAFETY: `buffer` is null (size query) or holds `size` bytes.
        let result = unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                mib.len() as libc::c_uint,
                buffer,
                size,
                std::ptr::null_mut(),
                0,
            )
        };
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    /// The interface index of the one unscoped default route in a dump.
    pub(super) fn default_route(mut dump: &[u8]) -> Option<u16> {
        const HEADER: usize = std::mem::size_of::<libc::rt_msghdr>();
        let mut found = None;
        while dump.len() >= HEADER {
            // SAFETY: the slice holds a whole header, read unaligned.
            let header: libc::rt_msghdr = unsafe { std::ptr::read_unaligned(dump.as_ptr().cast()) };
            let length = usize::from(header.rtm_msglen);
            if length < HEADER || length > dump.len() {
                return None;
            }
            let (message, rest) = dump.split_at(length);
            dump = rest;
            let refused =
                libc::RTF_IFSCOPE | libc::RTF_HOST | libc::RTF_REJECT | libc::RTF_BLACKHOLE;
            if libc::c_int::from(header.rtm_version) != libc::RTM_VERSION
                || header.rtm_flags & libc::RTF_UP == 0
                || header.rtm_flags & refused != 0
                || !is_default(&message[HEADER..], header.rtm_addrs)
            {
                continue;
            }
            match found {
                Some(index) if index != header.rtm_index => return None,
                _ => found = Some(header.rtm_index),
            }
        }
        found
    }

    /// Destination and mask are present and all zero.
    fn is_default(mut sockaddrs: &[u8], present: libc::c_int) -> bool {
        let mut zero = [false; 3];
        for (index, zero) in zero.iter_mut().enumerate() {
            if present & (1 << index) == 0 {
                continue;
            }
            let Some(length) = sockaddrs.first().map(|length| usize::from(*length)) else {
                return false;
            };
            if sockaddrs.len() < length {
                return false;
            }
            // the address of both families starts after family and port
            let address = sockaddrs.get(4..length).unwrap_or_default();
            *zero = address.iter().all(|byte| *byte == 0);
            let stride = length.max(1).div_ceil(4) * 4;
            sockaddrs = sockaddrs.get(stride..).unwrap_or_default();
        }
        let [destination, _, mask] = zero;
        destination && mask
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        const UP: libc::c_int = libc::RTF_UP | libc::RTF_GATEWAY;

        fn sockaddr(address: [u8; 4]) -> Vec<u8> {
            let mut bytes = vec![16, libc::AF_INET as u8, 0, 0];
            bytes.extend(address);
            bytes.resize(16, 0);
            bytes
        }

        fn message(index: u16, flags: libc::c_int, parts: &[&[u8]; 3]) -> Vec<u8> {
            let length = std::mem::size_of::<libc::rt_msghdr>()
                + parts.iter().map(|part| part.len()).sum::<usize>();
            // SAFETY: the header is plain integers.
            let mut header: libc::rt_msghdr = unsafe { std::mem::zeroed() };
            header.rtm_msglen = length as u16;
            header.rtm_version = libc::RTM_VERSION as u8;
            header.rtm_index = index;
            header.rtm_flags = flags;
            header.rtm_addrs = 7;
            // SAFETY: a plain struct seen as its bytes.
            let bytes = unsafe {
                std::slice::from_raw_parts(
                    (&raw const header).cast::<u8>(),
                    std::mem::size_of::<libc::rt_msghdr>(),
                )
            };
            [bytes, parts[0], parts[1], parts[2]].concat()
        }

        fn default(index: u16, flags: libc::c_int) -> Vec<u8> {
            let (zero, gateway) = (sockaddr([0; 4]), sockaddr([192, 168, 1, 1]));
            message(index, flags, &[&zero, &gateway, &[0; 4]])
        }

        #[test]
        fn the_unscoped_default_route_is_found() {
            let scoped = default(29, UP | libc::RTF_IFSCOPE);
            let mask: &[u8] = &[5, 0, 0, 0, 255, 0, 0, 0];
            let network = message(
                7,
                UP,
                &[&sockaddr([10, 0, 0, 0]), &sockaddr([10, 0, 0, 1]), mask],
            );
            let half = message(
                8,
                UP,
                &[
                    &sockaddr([0; 4]),
                    &sockaddr([10, 0, 0, 1]),
                    &[5, 0, 0, 0, 128, 0, 0, 0],
                ],
            );
            let odd = message(
                9,
                UP,
                &[&sockaddr([10, 0, 0, 0]), &sockaddr([10, 0, 0, 1]), &[0; 4]],
            );
            let dump = [scoped.clone(), network.clone(), half, odd, default(14, UP)].concat();
            assert_eq!(default_route(&dump), Some(14));
            assert_eq!(default_route(&[scoped, network].concat()), None);
            assert_eq!(default_route(&[]), None);
        }

        #[test]
        fn two_default_interfaces_or_a_broken_dump_give_none() {
            let two = [default(14, UP), default(15, UP)].concat();
            assert_eq!(default_route(&two), None);
            let mut broken = default(14, UP);
            broken.extend(default(14, UP));
            broken[0] = 1;
            broken[1] = 0;
            assert_eq!(default_route(&broken), None);
            for end in 0..two.len() {
                default_route(&two[..end]);
            }
        }

        #[test]
        fn rejecting_and_down_routes_do_not_count() {
            for flags in [UP | libc::RTF_REJECT, UP | libc::RTF_BLACKHOLE, 0] {
                assert_eq!(default_route(&default(14, flags)), None);
            }
        }
    }
}

/// The interface of the one default route; `None` when there is none or more
/// than one.
#[cfg(target_os = "linux")]
pub(crate) fn default_route_interface(v6: bool) -> io::Result<Option<String>> {
    Ok(if v6 {
        linux::default_v6(&std::fs::read_to_string("/proc/net/ipv6_route")?)
    } else {
        linux::default_v4(&std::fs::read_to_string("/proc/net/route")?)
    })
}

#[cfg(any(target_os = "linux", test))]
// Off Linux these parsers compile only for their tests, so nothing else calls them.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod linux {
    const UP: u32 = 1;
    const REJECT: u32 = 0x200;

    pub(super) fn default_v4(table: &str) -> Option<String> {
        best(table.lines().skip(1).filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            let [name, destination, _, flags, _, _, metric, mask, ..] = fields[..] else {
                return None;
            };
            let flags = u32::from_str_radix(flags, 16).ok()?;
            let usable = flags & UP != 0 && flags & REJECT == 0;
            (usable && destination == "00000000" && mask == "00000000")
                .then_some((metric.parse().ok()?, name))
        }))
    }

    pub(super) fn default_v6(table: &str) -> Option<String> {
        best(table.lines().filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            let [destination, length, _, _, _, metric, _, _, flags, name] = fields[..] else {
                return None;
            };
            let flags = u32::from_str_radix(flags, 16).ok()?;
            let usable = flags & UP != 0 && flags & REJECT == 0;
            (usable && length == "00" && destination.bytes().all(|b| b == b'0'))
                .then_some((u32::from_str_radix(metric, 16).ok()?, name))
        }))
    }

    /// The lowest metric's interface, when it is the only one with it.
    fn best<'a>(routes: impl Iterator<Item = (u32, &'a str)>) -> Option<String> {
        let mut routes: Vec<_> = routes.collect();
        routes.sort_unstable();
        routes.dedup();
        match routes[..] {
            [] => None,
            [(first, _), (second, _), ..] if first == second => None,
            [(_, name), ..] => Some(name.to_owned()),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn the_lowest_metric_default_wins() {
            let table = "Iface\tDestination\tGateway\tFlags\tRefCnt\tUse\tMetric\tMask\tMTU\tWindow\tIRTT\n\
                wlan0\t00000000\t0101A8C0\t0003\t0\t0\t600\t00000000\t0\t0\t0\n\
                eth0\t00000000\t0101A8C0\t0003\t0\t0\t100\t00000000\t0\t0\t0\n\
                tun0\t00000000\t00000000\t0001\t0\t0\t0\t00000080\t0\t0\t0\n\
                eth1\t00000000\t0101A8C0\t0002\t0\t0\t1\t00000000\t0\t0\t0\n";
            assert_eq!(default_v4(table).as_deref(), Some("eth0"));
            assert_eq!(default_v4("Iface\n"), None);
            let tie = table.replace("\t600\t", "\t100\t");
            assert_eq!(default_v4(&tie), None);
        }

        #[test]
        fn the_v6_default_skips_the_unreachable_one() {
            let zero = "0".repeat(32);
            let table = format!(
                "{zero} 00 {zero} 00 {zero} ffffffff 00000001 00000000 00200200 lo\n\
                 {zero} 00 {zero} 00 fe800000000000000000000000000001 00000400 00000001 00000000 00450003 eth0\n\
                 20010db8{} 40 {zero} 00 {zero} 00000100 00000001 00000000 00000001 eth1\n",
                "0".repeat(24)
            );
            assert_eq!(default_v6(&table).as_deref(), Some("eth0"));
        }
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn the_loopback_interface_is_listed_with_its_mask() {
        let addresses = interface_addresses().unwrap();
        let loopback = addresses
            .iter()
            .find(|own| own.address == IpAddr::V4(Ipv4Addr::LOCALHOST))
            .unwrap();
        assert_eq!(loopback.mask, Some(IpAddr::from([255, 0, 0, 0])));
    }

    #[test]
    fn a_default_route_names_a_listed_interface() {
        let addresses = interface_addresses().unwrap();
        for v6 in [false, true] {
            if let Ok(Some(name)) = default_route_interface(v6) {
                assert!(addresses.iter().any(|own| own.name == name), "{name}");
            }
        }
    }
}
