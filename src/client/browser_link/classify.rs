//! Who dials a destination of the remote browser (fork): this machine (A), or
//! the remote machine (B) through its ssh forwards.
//!
//! Pages of that browser belong to B, so A dials a checked public address and
//! nothing else. Every doubt (a lookup that fails, mixed answers, interfaces
//! that cannot be read) goes to B, which reaches only what B could reach anyway.
//! [`Pinned`] is built in [`pin`] alone: the addresses that were checked are the
//! addresses that get dialed, with no second lookup in between.

use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs as _, UdpSocket};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::platform::network::{self, InterfaceAddress};

const HOST_MAX: usize = 253;
const LABEL_MAX: usize = 63;
/// How long a name keeps the answers of one lookup.
const ANSWER_TTL: Duration = Duration::from_secs(30);
const FAILURE_TTL: Duration = Duration::from_secs(5);
/// Pages choose the names, so the cache is bounded.
const CACHE_MAX: usize = 512;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Route {
    /// This machine dials one of these addresses, never the name.
    Direct(Pinned),
    /// The remote machine dials, by the canonical host.
    Remote,
    Refuse(Refusal),
}

/// Checked public addresses, never empty, in the order to try them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Pinned(Vec<SocketAddr>);

impl Pinned {
    pub(crate) fn addresses(&self) -> &[SocketAddr] {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refusal {
    BadHost,
    BadPort,
    /// `.invalid` never reaches the remote machine (contract 4.1). The proxy
    /// answers its own warm-up host before it asks.
    InvalidName,
}

impl Refusal {
    pub(crate) fn text(self) -> &'static str {
        match self {
            Self::BadHost => "the host is not a valid name or address",
            Self::BadPort => "the port is not valid",
            Self::InvalidName => "names under .invalid do not exist",
        }
    }
}

pub(crate) trait Resolver {
    /// Every A and AAAA answer of one lookup. `name` is canonical and has at
    /// least two labels.
    fn resolve(&self, name: &str) -> io::Result<Vec<IpAddr>>;
}

/// What this machine's network looks like now.
#[derive(Clone, Debug, Default)]
pub(crate) struct Network {
    pub(crate) addresses: Vec<InterfaceAddress>,
    pub(crate) default_v4: Option<String>,
    pub(crate) default_v6: Option<String>,
}

pub(crate) trait Interfaces {
    fn network(&self) -> io::Result<Network>;
    /// The local address a connection to `to` would leave from. Sends nothing.
    fn source(&self, to: IpAddr) -> io::Result<IpAddr>;
}

/// The one spelling of a host, as Chromium would canonicalise it. Its `Display`
/// is what may go to the remote machine (rule 12 of the contract).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Host {
    Ip(IpAddr),
    Name(String),
}

impl std::fmt::Display for Host {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ip(IpAddr::V4(ip)) => ip.fmt(f),
            Self::Ip(IpAddr::V6(ip)) => write!(f, "[{ip}]"),
            Self::Name(name) => f.write_str(name),
        }
    }
}

impl Host {
    pub(crate) fn parse(host: &str) -> Result<Self, Refusal> {
        if host.contains([':', '[', ']']) {
            let bare = match host.strip_prefix('[') {
                Some(rest) => rest.strip_suffix(']').ok_or(Refusal::BadHost)?,
                None => host,
            };
            let ip: Ipv6Addr = bare.parse().map_err(|_| Refusal::BadHost)?;
            return Ok(Self::Ip(ip.into()));
        }
        let decoded = percent_decoded(host).ok_or(Refusal::BadHost)?;
        let mut name = decoded.to_ascii_lowercase();
        if name.ends_with('.') {
            name.pop();
        }
        if name.len() > HOST_MAX || name.split('.').any(|l| l.is_empty() || l.len() > LABEL_MAX) {
            return Err(Refusal::BadHost);
        }
        let word = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_';
        if !name.chars().all(|c| word(c) || c == '.' || c == '-')
            || !name.starts_with(word)
            || !name.ends_with(word)
        {
            return Err(Refusal::BadHost);
        }
        let last = name.rsplit('.').next().unwrap_or_default();
        if last.bytes().all(|b| b.is_ascii_digit()) || ipv4_number(last).is_some() {
            return ipv4(&name)
                .map(|ip| Self::Ip(ip.into()))
                .ok_or(Refusal::BadHost);
        }
        if last == "invalid" {
            return Err(Refusal::InvalidName);
        }
        Ok(Self::Name(name))
    }
}

/// One level, as Chromium does; a `%` that is left over is not a host character.
fn percent_decoded(host: &str) -> Option<String> {
    let mut out = Vec::with_capacity(host.len());
    let mut bytes = host.bytes();
    while let Some(byte) = bytes.next() {
        if byte != b'%' {
            out.push(byte);
            continue;
        }
        let high = char::from(bytes.next()?).to_digit(16)?;
        let low = char::from(bytes.next()?).to_digit(16)?;
        out.push(u8::try_from(high * 16 + low).ok()?);
    }
    String::from_utf8(out).ok()
}

/// The URL standard's IPv4 parser: 1 to 4 parts, each decimal, octal or hex, the
/// last one filling the bytes that are left.
fn ipv4(name: &str) -> Option<Ipv4Addr> {
    let parts = name
        .split('.')
        .map(ipv4_number)
        .collect::<Option<Vec<u64>>>()?;
    let (last, head) = parts.split_last()?;
    if head.len() > 3 || head.iter().any(|part| *part > 255) {
        return None;
    }
    let room = 8 * (4 - head.len());
    if *last >= 1 << room {
        return None;
    }
    let mut value = *last;
    for (index, part) in head.iter().enumerate() {
        value |= part << (8 * (3 - index));
    }
    Some(Ipv4Addr::from(u32::try_from(value).ok()?))
}

fn ipv4_number(part: &str) -> Option<u64> {
    let (digits, radix) = match part.strip_prefix("0x") {
        Some("") => return Some(0),
        Some(hex) => (hex, 16),
        None if part.len() > 1 && part.starts_with('0') => (part, 8),
        None => (part, 10),
    };
    if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
        return None;
    }
    let digits = digits.trim_start_matches('0');
    if digits.is_empty() {
        return Some(0);
    }
    // too long for a number is still a number, only never an address
    Some(u64::from_str_radix(digits, radix).unwrap_or(u64::MAX))
}

/// Names that mean something only on some machine or network: never looked up
/// on A.
fn is_local_name(name: &str) -> bool {
    const TLDS: [&str; 15] = [
        "alt",
        "corp",
        "example",
        "home",
        "internal",
        "intranet",
        "lan",
        "local",
        "localdomain",
        "localhost",
        "mail",
        "onion",
        "private",
        "test",
        "workgroup",
    ];
    let Some((_, tld)) = name.rsplit_once('.') else {
        return true;
    };
    TLDS.contains(&tld) || name == "home.arpa" || name.ends_with(".home.arpa")
}

/// The contract's address table. IPv6 is an allowlist, so mapped, NAT64, ULA,
/// link-local and multicast need no rule, and an embedded IPv4 address can never
/// make one public.
fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => is_public_v4(ip),
        IpAddr::V6(ip) => is_public_v6(ip),
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    !matches!(
        ip.octets(),
        [0 | 10 | 127 | 224.., ..]
            | [100, 64..=127, ..]
            | [169, 254, ..]
            | [172, 16..=31, ..]
            | [192, 0, 0 | 2, _]
            | [192, 88, 99, _]
            | [192, 168, ..]
            | [198, 18 | 19, ..]
            | [198, 51, 100, _]
            | [203, 0, 113, _]
    )
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    let special = matches!(
        ip.segments(),
        [0x2001, ..=0x01ff, ..] | [0x2001, 0x0db8, ..] | [0x2002, ..] | [0x3fff, ..=0x0fff, ..]
    );
    ip.segments()[0] & 0xe000 == 0x2000 && !special && !ends_in_private_v4(ip)
}

/// An operator's own NAT64 prefix (RFC 6052, /96) and ISATAP lie inside 2000::/3
/// and carry the real IPv4 target in the last 32 bits.
fn ends_in_private_v4(ip: Ipv6Addr) -> bool {
    let [.., a, b, c, d] = ip.octets();
    matches!(
        [a, b, c, d],
        [10 | 127, ..] | [100, 64..=127, ..] | [169, 254, ..] | [172, 16..=31, ..] | [192, 168, ..]
    )
}

/// The address, the prefix or the far end of one of A's interfaces.
fn on_own_network(ip: IpAddr, addresses: &[InterfaceAddress]) -> bool {
    addresses.iter().any(|own| {
        ip == own.address || Some(ip) == own.peer || shares_prefix(ip, own.address, own.mask)
    })
}

fn shares_prefix(ip: IpAddr, own: IpAddr, mask: Option<IpAddr>) -> bool {
    match (ip, own, mask) {
        (IpAddr::V4(ip), IpAddr::V4(own), Some(IpAddr::V4(mask))) => {
            (u32::from(ip) ^ u32::from(own)) & u32::from(mask) == 0
        }
        (IpAddr::V6(ip), IpAddr::V6(own), mask) => {
            let mask = match mask {
                Some(IpAddr::V6(mask)) => u128::from(mask),
                _ => u128::MAX,
            };
            // a home or office is delegated up to a /48; its other subnets
            // (guest, IoT) answer behind the same router
            let site = u128::MAX << 80;
            (u128::from(ip) ^ u128::from(own)) & mask & site == 0
        }
        _ => false,
    }
}

/// A split-tunnel VPN may route public ranges into a private network.
fn leaves_by_default_route(to: IpAddr, network: &Network, interfaces: &dyn Interfaces) -> bool {
    let default = match to {
        IpAddr::V4(_) => &network.default_v4,
        IpAddr::V6(_) => &network.default_v6,
    };
    let (Some(default), Ok(source)) = (default, interfaces.source(to)) else {
        return false;
    };
    let mut names = network
        .addresses
        .iter()
        .filter(|own| own.address == source)
        .map(|own| &own.name)
        .peekable();
    names.peek().is_some() && names.all(|name| name == default)
}

/// The only place a [`Pinned`] is made: every answer is public and foreign to
/// A's networks, and every pinned one leaves by the default route.
fn pin(answers: &[IpAddr], port: u16, interfaces: &dyn Interfaces) -> Option<Pinned> {
    let network = interfaces.network().ok()?;
    if answers.is_empty() || network.addresses.is_empty() {
        return None;
    }
    if !answers
        .iter()
        .all(|ip| is_public(*ip) && !on_own_network(*ip, &network.addresses))
    {
        return None;
    }
    let has_v4 = answers.iter().any(IpAddr::is_ipv4);
    let mut pinned = Vec::new();
    for ip in answers.iter().filter(|ip| ip.is_ipv4() == has_v4) {
        let address = SocketAddr::new(*ip, port);
        if pinned.contains(&address) {
            continue;
        }
        if !leaves_by_default_route(*ip, &network, interfaces) {
            return None;
        }
        pinned.push(address);
    }
    Some(Pinned(pinned))
}

/// Who dials `host:port`. `host` is the host of the request line, with or
/// without IPv6 brackets; at most one lookup is made.
pub(crate) fn classify(
    host: &str,
    port: u16,
    resolver: &dyn Resolver,
    interfaces: &dyn Interfaces,
) -> Route {
    if port == 0 {
        return Route::Refuse(Refusal::BadPort);
    }
    let answers = match Host::parse(host) {
        Err(refusal) => return Route::Refuse(refusal),
        Ok(Host::Ip(ip)) => vec![ip],
        Ok(Host::Name(name)) if is_local_name(&name) => return Route::Remote,
        Ok(Host::Name(name)) => match resolver.resolve(&name) {
            Ok(answers) => answers,
            Err(_) => return Route::Remote,
        },
    };
    pin(&answers, port, interfaces).map_or(Route::Remote, Route::Direct)
}

struct Answer {
    /// When its lookup began.
    at: Instant,
    /// `None`: the lookup failed or was empty.
    addresses: Option<Vec<IpAddr>>,
}

impl Answer {
    fn fresh(&self, now: Instant) -> bool {
        let ttl = match self.addresses {
            Some(_) => ANSWER_TTL,
            None => FAILURE_TTL,
        };
        now.saturating_duration_since(self.at) < ttl
    }
}

/// Keeps answers, never routes: each request judges them against the network as
/// it is now, and a new lookup's answers replace the old ones whole.
pub(crate) struct CachedResolver<R, C = fn() -> Instant> {
    inner: R,
    clock: C,
    answers: Mutex<HashMap<String, Answer>>,
}

impl<R> CachedResolver<R> {
    pub(crate) fn new(inner: R) -> Self {
        Self::with_clock(inner, Instant::now)
    }
}

impl<R, C> CachedResolver<R, C> {
    fn with_clock(inner: R, clock: C) -> Self {
        Self {
            inner,
            clock,
            answers: Mutex::new(HashMap::new()),
        }
    }
}

impl<R: Resolver, C: Fn() -> Instant> Resolver for CachedResolver<R, C> {
    fn resolve(&self, name: &str) -> io::Result<Vec<IpAddr>> {
        let at = (self.clock)();
        let known = self
            .answers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(name)
            .filter(|answer| answer.fresh(at))
            .map(|answer| answer.addresses.clone());
        let addresses = match known {
            Some(addresses) => addresses,
            None => {
                let addresses = self.inner.resolve(name).ok().filter(|all| !all.is_empty());
                self.store(name, at, addresses.clone());
                addresses
            }
        };
        addresses.ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no address"))
    }
}

impl<R, C: Fn() -> Instant> CachedResolver<R, C> {
    fn store(&self, name: &str, at: Instant, addresses: Option<Vec<IpAddr>>) {
        let now = (self.clock)();
        let mut answers = self.answers.lock().unwrap_or_else(PoisonError::into_inner);
        // a slow lookup must not replace the answers of a later one
        if answers.get(name).is_some_and(|known| known.at > at) {
            return;
        }
        if answers.len() >= CACHE_MAX && !answers.contains_key(name) {
            answers.retain(|_, answer| answer.fresh(now));
        }
        if answers.len() >= CACHE_MAX && !answers.contains_key(name) {
            let oldest = answers
                .iter()
                .min_by_key(|(_, answer)| answer.at)
                .map(|(name, _)| name.clone());
            if let Some(oldest) = oldest {
                answers.remove(&oldest);
            }
        }
        answers.insert(name.to_owned(), Answer { at, addresses });
    }
}

/// The classifier the proxy asks, once per request.
pub(crate) struct Classifier<R, I> {
    resolver: CachedResolver<R>,
    interfaces: I,
}

impl Classifier<SystemResolver, SystemInterfaces> {
    pub(crate) fn system() -> Self {
        Self::new(SystemResolver, SystemInterfaces)
    }
}

impl<R: Resolver, I: Interfaces> Classifier<R, I> {
    pub(crate) fn new(resolver: R, interfaces: I) -> Self {
        Self {
            resolver: CachedResolver::new(resolver),
            interfaces,
        }
    }

    /// May block for a lookup.
    pub(crate) fn route(&self, host: &str, port: u16) -> Route {
        classify(host, port, &self.resolver, &self.interfaces)
    }
}

/// `getaddrinfo`, blocking.
pub(crate) struct SystemResolver;

impl Resolver for SystemResolver {
    fn resolve(&self, name: &str) -> io::Result<Vec<IpAddr>> {
        // the final dot keeps the search domains of A's network out
        let absolute = format!("{name}.");
        Ok((absolute.as_str(), 0)
            .to_socket_addrs()?
            .map(|address| address.ip())
            .collect())
    }
}

pub(crate) struct SystemInterfaces;

impl Interfaces for SystemInterfaces {
    fn network(&self) -> io::Result<Network> {
        Ok(Network {
            addresses: network::interface_addresses()?,
            // a family that cannot be read is not dialed; the other one may be
            default_v4: network::default_route_interface(false).unwrap_or_default(),
            default_v6: network::default_route_interface(true).unwrap_or_default(),
        })
    }

    fn source(&self, to: IpAddr) -> io::Result<IpAddr> {
        let any: IpAddr = match to {
            IpAddr::V4(_) => Ipv4Addr::UNSPECIFIED.into(),
            IpAddr::V6(_) => Ipv6Addr::UNSPECIFIED.into(),
        };
        // connecting a datagram socket only picks the route
        let socket = UdpSocket::bind((any, 0))?;
        socket.connect((to, 443))?;
        Ok(socket.local_addr()?.ip())
    }
}

#[cfg(test)]
mod tests;
