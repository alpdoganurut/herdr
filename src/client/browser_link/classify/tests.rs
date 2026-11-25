use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;

use super::*;

const PUBLIC_V4: &str = "93.184.216.34";
const PUBLIC_V6: &str = "2606:2800:220:1:248:1893:25c8:1946";

/// The contract's table, spelled independently of the code under test.
const NOT_PUBLIC: [&str; 44] = [
    "0.0.0.0/8",
    "10.0.0.0/8",
    "100.64.0.0/10",
    "127.0.0.0/8",
    "169.254.0.0/16",
    "172.16.0.0/12",
    "192.0.0.0/24",
    "192.0.2.0/24",
    "192.88.99.0/24",
    "192.168.0.0/16",
    "198.18.0.0/15",
    "198.51.100.0/24",
    "203.0.113.0/24",
    "224.0.0.0/4",
    "240.0.0.0/4",
    "255.255.255.255/32",
    "::/128",
    "::1/128",
    "::/96",
    "::ffff:0:0/96",
    "::ffff:0:0:0/96",
    "64:ff9b::/96",
    "64:ff9b:1::/48",
    "100::/64",
    "0000::/3",
    "2001::/32",
    "2001:2::/48",
    "2001:10::/28",
    "2001:20::/28",
    "2001::/23",
    "2001:db8::/32",
    "2002::/16",
    "3fff::/20",
    "4000::/3",
    "5f00::/16",
    "6000::/3",
    "8000::/3",
    "a000::/3",
    "c000::/3",
    "e000::/4",
    "fc00::/7",
    "fe80::/10",
    "fec0::/10",
    "ff00::/8",
];

#[derive(Clone, Copy)]
struct Cidr {
    v4: bool,
    base: u128,
    /// How many addresses, minus one.
    span: u128,
}

impl Cidr {
    fn parse(text: &str) -> Self {
        let (base, length) = text.split_once('/').unwrap();
        let length: u32 = length.parse().unwrap();
        let base: IpAddr = base.parse().unwrap();
        let (base, bits) = number(base);
        Self {
            v4: bits == 32,
            base,
            span: u128::MAX
                .checked_shr(128 - (bits - length))
                .unwrap_or_default(),
        }
    }

    fn contains(self, ip: IpAddr) -> bool {
        let (value, bits) = number(ip);
        self.v4 == (bits == 32) && value >= self.base && value - self.base <= self.span
    }
}

fn number(ip: IpAddr) -> (u128, u32) {
    match ip {
        IpAddr::V4(ip) => (u32::from(ip).into(), 32),
        IpAddr::V6(ip) => (ip.into(), 128),
    }
}

fn address(v4: bool, value: u128) -> IpAddr {
    if v4 {
        Ipv4Addr::from(value as u32).into()
    } else {
        Ipv6Addr::from(value).into()
    }
}

fn not_public(ip: IpAddr) -> bool {
    NOT_PUBLIC.iter().any(|cidr| Cidr::parse(cidr).contains(ip))
}

fn ip(text: &str) -> IpAddr {
    text.parse().unwrap()
}

fn ips(texts: &[&str]) -> Vec<IpAddr> {
    texts.iter().map(|text| ip(text)).collect()
}

/// Each lookup takes the next set of answers; the last one stays.
#[derive(Default)]
struct Zone {
    names: RefCell<BTreeMap<String, Vec<Option<Vec<IpAddr>>>>>,
    asked: RefCell<Vec<String>>,
}

impl Zone {
    fn with(name: &str, sets: &[&[&str]]) -> Self {
        let zone = Self::default();
        zone.names.borrow_mut().insert(
            name.to_owned(),
            sets.iter().map(|set| Some(ips(set))).collect(),
        );
        zone
    }

    fn lookups(&self) -> usize {
        self.asked.borrow().len()
    }
}

impl Resolver for &Zone {
    fn resolve(&self, name: &str) -> io::Result<Vec<IpAddr>> {
        (**self).resolve(name)
    }
}

impl Resolver for Zone {
    fn resolve(&self, name: &str) -> io::Result<Vec<IpAddr>> {
        self.asked.borrow_mut().push(name.to_owned());
        let mut names = self.names.borrow_mut();
        let sets = names
            .get_mut(name)
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
        let set = if sets.len() > 1 {
            sets.remove(0)
        } else {
            sets[0].clone()
        };
        set.ok_or_else(|| io::ErrorKind::TimedOut.into())
    }
}

/// Answers every name with the same set.
struct Always(Vec<IpAddr>);

impl Resolver for Always {
    fn resolve(&self, _name: &str) -> io::Result<Vec<IpAddr>> {
        Ok(self.0.clone())
    }
}

struct Panics;

impl Resolver for Panics {
    fn resolve(&self, name: &str) -> io::Result<Vec<IpAddr>> {
        panic!("{name} was looked up");
    }
}

fn own(name: &str, address: &str, mask: &str) -> InterfaceAddress {
    InterfaceAddress {
        name: name.to_owned(),
        address: ip(address),
        mask: Some(ip(mask)),
        peer: None,
    }
}

/// A Mac at home: en0 with both families and the default routes, a VPN tunnel.
struct Mac {
    network: Network,
    /// Destinations that leave by another local address than en0's.
    tunnelled: Vec<(Cidr, IpAddr)>,
}

impl Mac {
    fn home() -> Self {
        Self {
            network: Network {
                addresses: vec![
                    own("lo0", "127.0.0.1", "255.0.0.0"),
                    own("lo0", "::1", "ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff"),
                    own("en0", "192.168.1.20", "255.255.255.0"),
                    own("en0", "2a02:8109:aa00:7::20", "ffff:ffff:ffff:ffff::"),
                    own("utun4", "100.85.59.104", "255.255.255.255"),
                ],
                default_v4: Some("en0".to_owned()),
                default_v6: Some("en0".to_owned()),
            },
            tunnelled: Vec::new(),
        }
    }

    fn plus(mut self, own: InterfaceAddress) -> Self {
        self.network.addresses.push(own);
        self
    }
}

impl Interfaces for Mac {
    fn network(&self) -> io::Result<Network> {
        Ok(self.network.clone())
    }

    fn source(&self, to: IpAddr) -> io::Result<IpAddr> {
        if let Some((_, source)) = self.tunnelled.iter().find(|(range, _)| range.contains(to)) {
            return Ok(*source);
        }
        Ok(match to {
            IpAddr::V4(_) => ip("192.168.1.20"),
            IpAddr::V6(_) => ip("2a02:8109:aa00:7::20"),
        })
    }
}

fn direct(addresses: &[&str], port: u16) -> Route {
    Route::Direct(Pinned(
        ips(addresses)
            .into_iter()
            .map(|ip| SocketAddr::new(ip, port))
            .collect(),
    ))
}

fn literal(host: &str) -> Route {
    classify(host, 443, &Panics, &Mac::home())
}

fn named(answers: &[&str]) -> Route {
    classify("www.example.com", 443, &Always(ips(answers)), &Mac::home())
}

#[test]
fn every_spelling_of_a_private_literal_goes_to_the_remote() {
    for host in [
        // loopback
        "127.0.0.1",
        "127.0.0.1.",
        "127.1",
        "127.0.1",
        "127.8.9.10",
        "127.255.255.255",
        "2130706433",
        "2130706433.",
        "0x7f000001",
        "0X7F000001",
        "0x7F.1",
        "0x7f.0x0.0x0.0x1",
        "0x7f.0.0.0x1",
        "017700000001",
        "0177.0.0.1",
        "0177.0.0.01",
        "00000000000000000000000000000177.0.0.1",
        "0x000000000000000000000000000000007f.1",
        "0177.0x0.1",
        "127.0x1",
        "127.0.0.0x1",
        "127%2e0%2E0%2e1",
        "%31%32%37.0.0.1",
        "%30x7f000001",
        "127.0.0.1%2e",
        // unspecified
        "0",
        "0.",
        "0x",
        "0x0",
        "00",
        "0.0",
        "0.0.0.0",
        "0x.0x.0",
        "0.0.0.1",
        // private, link-local, CGNAT, metadata
        "10.0.0.1",
        "10.1",
        "167772161",
        "012.1",
        "0xa000001",
        "172.16.0.1",
        "172.31.255.255",
        "0254.16.1",
        "192.168.1.1",
        "192.168.257",
        "0xC0A80101",
        "0300.0250.1.1",
        "169.254.169.254",
        "2852039166",
        "0xa9fea9fe",
        "0251.0376.0251.0376",
        "100.64.0.1",
        "100.100.100.100",
        "100.127.255.255",
        // special, multicast, broadcast
        "192.0.0.8",
        "192.0.2.1",
        "192.88.99.1",
        "198.18.0.1",
        "198.19.255.255",
        "198.51.100.1",
        "203.0.113.1",
        "224.0.0.251",
        "239.255.255.250",
        "240.0.0.1",
        "255.255.255.255",
        "4294967295",
        "0xffffffff",
        // IPv6
        "[::1]",
        "::1",
        "[0:0:0:0:0:0:0:1]",
        "[0000:0000:0000:0000:0000:0000:0000:0001]",
        "[::]",
        "::",
        "[::127.0.0.1]",
        "[::ffff:127.0.0.1]",
        "[::FFFF:7F00:1]",
        "[0:0:0:0:0:ffff:192.168.1.1]",
        "[::ffff:93.184.216.34]",
        "[::ffff:0:127.0.0.1]",
        "[64:ff9b::127.0.0.1]",
        "[64:ff9b::5db8:d822]",
        "[64:ff9b:1::c0a8:101]",
        "[2002:7f00:1::]",
        "[2002:c0a8:101::1]",
        "[2002:5db8:d822::1]",
        "[2001:0:4136:e378:8000:63bf:3fff:fdd2]",
        "[2001::1]",
        "[2001:10::1]",
        "[2001:20::1]",
        "[2001:1ff::1]",
        "[2001:db8::1]",
        "[2001:DB8::1]",
        "[3fff::1]",
        "[3fff:fff::1]",
        "[5f00::1]",
        "[100::1]",
        "[fc00::1]",
        "[fd7a:115c:a1e0::1]",
        "[fe80::1]",
        "[feb0::1]",
        "[fec0::1]",
        "[ff02::1]",
        "[ff02::fb]",
        "[ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff]",
        // NAT64 under an operator's own prefix, ISATAP
        "[2a00:1098:2c::5:192.168.1.1]",
        "[2606:4700:64::7f00:1]",
        "[2606:4700::200:5efe:10.0.0.1]",
        "[2606:4700::5efe:169.254.169.254]",
        "[2606:4700::5efe:172.20.0.1]",
        "[2606:4700::5efe:100.64.0.1]",
    ] {
        assert_eq!(literal(host), Route::Remote, "{host}");
    }
}

#[test]
fn a_public_literal_is_dialed_as_written_without_a_lookup() {
    for (host, dialed) in [
        ("93.184.216.34", PUBLIC_V4),
        ("93.184.216.34.", PUBLIC_V4),
        ("1572395042", PUBLIC_V4),
        ("0x5DB8D822", PUBLIC_V4),
        ("0135.0270.0330.042", PUBLIC_V4),
        ("93.12113954", PUBLIC_V4),
        ("%393.184.216.34", PUBLIC_V4),
        ("1.1", "1.0.0.1"),
        ("1.2.3.0x", "1.2.3.0"),
        ("8.8.8.8", "8.8.8.8"),
        ("[2606:2800:220:1:248:1893:25C8:1946]", PUBLIC_V6),
        ("2606:2800:0220:0001:0248:1893:25c8:1946", PUBLIC_V6),
        ("[2001:4860:4860::8888]", "2001:4860:4860::8888"),
    ] {
        assert_eq!(literal(host), direct(&[dialed], 443), "{host}");
    }
}

#[test]
fn the_edges_of_every_class() {
    for (inside, outside) in [
        ("0.255.255.255", "1.0.0.0"),
        ("10.0.0.0", "9.255.255.255"),
        ("10.255.255.255", "11.0.0.0"),
        ("100.64.0.0", "100.63.255.255"),
        ("100.127.255.255", "100.128.0.0"),
        ("127.0.0.0", "126.255.255.255"),
        ("127.255.255.255", "128.0.0.0"),
        ("169.254.0.0", "169.253.255.255"),
        ("169.254.255.255", "169.255.0.0"),
        ("172.16.0.0", "172.15.255.255"),
        ("172.31.255.255", "172.32.0.0"),
        ("192.0.0.0", "191.255.255.255"),
        ("192.0.0.255", "192.0.1.0"),
        ("192.0.2.0", "192.0.1.255"),
        ("192.0.2.255", "192.0.3.0"),
        ("192.88.99.0", "192.88.98.255"),
        ("192.88.99.255", "192.88.100.0"),
        ("192.168.0.0", "192.167.255.255"),
        ("192.168.255.255", "192.169.0.0"),
        ("198.18.0.0", "198.17.255.255"),
        ("198.19.255.255", "198.20.0.0"),
        ("198.51.100.0", "198.51.99.255"),
        ("198.51.100.255", "198.51.101.0"),
        ("203.0.113.0", "203.0.112.255"),
        ("203.0.113.255", "203.0.114.0"),
        ("224.0.0.0", "223.255.255.255"),
        ("2001:1ff:ffff::1", "2001:200::1"),
        ("2001:db8::1", "2001:db7:ffff::1"),
        ("2001:db8:ffff::1", "2001:db9::1"),
        ("2002::1", "2001:ffff::1"),
        ("2002:ffff::1", "2003::1"),
        ("3fff::1", "3ffe:ffff::1"),
        ("3fff:fff:ffff::1", "3fff:1000::1"),
        ("1fff:ffff::1", "2000::1"),
        ("4000::1", "3fff:ffff::1"),
    ] {
        assert!(not_public(ip(inside)) && !not_public(ip(outside)));
        assert_eq!(literal(inside), Route::Remote, "{inside}");
        assert_eq!(literal(outside), direct(&[outside], 443), "{outside}");
        assert_eq!(named(&[inside]), Route::Remote, "{inside}");
        assert_eq!(named(&[outside]), direct(&[outside], 443), "{outside}");
    }
}

/// On its own: the loopback interface's prefix covers 127/8 in the other tests.
#[test]
fn the_address_table_holds_without_the_interfaces() {
    for text in NOT_PUBLIC {
        let cidr = Cidr::parse(text);
        for value in [cidr.base, cidr.base + cidr.span / 2, cidr.base + cidr.span] {
            assert!(!is_public(address(cidr.v4, value)), "{text}");
        }
    }
    // a public IPv4 address inside makes none of these public
    for text in [
        "::ffff:93.184.216.34",
        "::93.184.216.34",
        "::ffff:0:93.184.216.34",
        "64:ff9b::93.184.216.34",
        "2002:5db8:d822::1",
        "2001:0:5db8:d822::1",
    ] {
        assert!(!is_public(ip(text)), "{text}");
    }
    for text in [PUBLIC_V4, PUBLIC_V6, "8.8.8.8", "2001:4860:4860::8888"] {
        assert!(is_public(ip(text)), "{text}");
    }
}

#[test]
fn local_names_go_to_the_remote_without_a_lookup() {
    for host in [
        "localhost",
        "LOCALHOST",
        "localhost.",
        "LocalHost.",
        "local%68ost",
        "a.localhost",
        "a.b.LOCALHOST.",
        "localhost.localdomain",
        "printer.local",
        "Alps-MacBook.local.",
        "router",
        "router.",
        "mac_mini",
        "wpad",
        "db.internal",
        "metadata.google.internal",
        "nas.home.arpa",
        "home.arpa",
        "home.arpa.",
        "nas.lan",
        "nas.home",
        "git.corp",
        "wiki.intranet",
        "x.private",
        "x.mail",
        "x.test",
        "x.example",
        "x.alt",
        "x.onion",
        "x.workgroup",
        "xn--lcalhost-n4a",
        "com",
    ] {
        assert_eq!(literal(host), Route::Remote, "{host}");
    }
}

#[test]
fn look_alikes_of_local_names_are_ordinary_names() {
    for host in [
        "localhost.example.com",
        "127.0.0.1.example.com",
        "10.0.0.1.nip.io",
        "notlocal.locals",
        "thehome.arpa",
        "internal.com",
        "xn--lcalhost-n4a.com",
        "xn--127-0-0-1-x2o.com",
        "0x7f.0.0.1.com",
        "a_b.example.com",
        "1.2.3.4.5a",
        "x.0xg",
    ] {
        let zone = Zone::with(host, &[&[PUBLIC_V4]]);
        assert_eq!(
            classify(host, 80, &zone, &Mac::home()),
            direct(&[PUBLIC_V4], 80),
            "{host}"
        );
        assert_eq!(zone.lookups(), 1, "{host}");
        assert_eq!(
            classify(host, 80, &Always(ips(&["127.0.0.1"])), &Mac::home()),
            Route::Remote,
            "{host}"
        );
    }
}

#[test]
fn hosts_that_are_neither_a_name_nor_an_address_are_refused() {
    let long_label = format!("{}.com", "a".repeat(64));
    let long_name = format!("{}com", "a.".repeat(126));
    let long_literal = format!("{}1", "0".repeat(300));
    for host in [
        "",
        ".",
        "..",
        "a..com",
        ".a.com",
        "example.com..",
        "127.0.0.1..",
        "-a.com",
        "a.com-",
        "a b.com",
        " a.com",
        "a.com\n",
        "a\0.com",
        "a/b",
        "/tmp/sock",
        "a@b.com",
        "a:80",
        "a#b",
        "a?b",
        "a\\b",
        "$HOME",
        "~",
        "*",
        // numbers that are no address
        "256.1.1.1",
        "1.2.3.256",
        "1.256.1",
        "1.2.256.4",
        "1.16777216x.1",
        "1.16777216",
        "1.2.65536",
        "1.2.3.4.5",
        "0.0.0.0.0",
        "4294967296",
        "0x100000000",
        "0xfffffffffffffffffffffffffffffffff",
        "99999999999999999999999999999999999",
        "a.99999999999999999999999999999999999",
        "a.0xfffffffffffffffffffffffffffffffff",
        "08",
        "1.2.3.09",
        "example.com.1",
        "example.0x7f",
        "foo.0",
        "+1.2.3.4",
        // percent-encoding
        "%",
        "%2",
        "%zz",
        "a%2",
        "a%20b.com",
        "a%00.com",
        "a%2fb.com",
        "%2531",
        "%5b::1%5d",
        "%3a%3a1",
        "a%2525.com",
        "%e2%91%a0.com",
        "%ff",
        // not ASCII: Chromium sends punycode
        "１２７.０.０.１",
        "127。0。0。1",
        "①②⑦.0.0.1",
        "lоcalhost",
        "bücher.de",
        "127.0.0.1\u{200b}",
        // IPv6
        "[",
        "]",
        "[]",
        "[::1",
        "::1]",
        "[[::1]]",
        "[::1]x",
        "[127.0.0.1]",
        "[localhost]",
        "[v1.a]",
        "[::1%25en0]",
        "[fe80::1%en0]",
        "fe80::1%lo0",
        "[::1%2f]",
        "%5B::1]",
        "[:::1]",
        "[1:2:3:4:5:6:7:8:9]",
        "[::ffff:127.0.0.256]",
        "[::ffff:0x7f.1]",
        "1:2",
        ":",
        &long_label,
        &long_name,
        &long_literal,
    ] {
        let expected = Route::Refuse(Refusal::BadHost);
        assert_eq!(literal(host), expected, "{host:?}");
    }
}

#[test]
fn port_zero_and_invalid_names_are_refused() {
    assert_eq!(
        classify("example.com", 0, &Panics, &Mac::home()),
        Route::Refuse(Refusal::BadPort)
    );
    for host in ["invalid", "a.invalid", "herdr-proxy.INVALID."] {
        assert_eq!(literal(host), Route::Refuse(Refusal::InvalidName), "{host}");
    }
    assert!(matches!(
        classify(
            "notinvalid.com",
            1,
            &Always(ips(&[PUBLIC_V4])),
            &Mac::home()
        ),
        Route::Direct(_)
    ));
}

#[test]
fn long_and_odd_names_that_are_still_names() {
    let longest = format!(
        "{}.{}.{}.{}",
        "a".repeat(63),
        "b".repeat(63),
        "c".repeat(63),
        {
            let mut end = "d".repeat(57);
            end.push_str(".com");
            end
        }
    );
    assert_eq!(longest.len(), 253);
    let dotted = format!("{longest}.");
    for host in [
        longest.as_str(),
        &dotted,
        "a.-b.com",
        "_dmarc.a.com",
        "a--b.com",
        "1a.2b",
    ] {
        assert_eq!(
            classify(host, 65535, &Always(ips(&[PUBLIC_V4])), &Mac::home()),
            direct(&[PUBLIC_V4], 65535),
            "{host}"
        );
    }
}

#[test]
fn the_canonical_host_is_what_may_go_to_the_remote() {
    for (host, canonical) in [
        ("LocalHost.", "localhost"),
        ("127.1", "127.0.0.1"),
        ("0x7f000001", "127.0.0.1"),
        ("%6cocalhost", "localhost"),
        ("[0:0::1]", "[::1]"),
        ("FE80::1", "[fe80::1]"),
        ("[::ffff:7f00:1]", "[::ffff:127.0.0.1]"),
        ("Dev_Box", "dev_box"),
    ] {
        let parsed = Host::parse(host).unwrap();
        assert_eq!(parsed.to_string(), canonical);
        assert_eq!(Host::parse(canonical), Ok(parsed));
    }
}

#[test]
fn a_name_is_pinned_only_when_every_answer_is_public() {
    let other = "151.101.1.69";
    for (answers, expected) in [
        (&[PUBLIC_V4][..], direct(&[PUBLIC_V4], 443)),
        (&[PUBLIC_V4, other], direct(&[PUBLIC_V4, other], 443)),
        (&[PUBLIC_V4, PUBLIC_V4], direct(&[PUBLIC_V4], 443)),
        // the A records win, in whatever order they come
        (&[PUBLIC_V6, PUBLIC_V4], direct(&[PUBLIC_V4], 443)),
        (&[PUBLIC_V6], direct(&[PUBLIC_V6], 443)),
        (&[], Route::Remote),
        (&["127.0.0.1"], Route::Remote),
        (&[PUBLIC_V4, "127.0.0.1"], Route::Remote),
        (&["192.168.1.1", PUBLIC_V4], Route::Remote),
        (&[PUBLIC_V4, other, "169.254.169.254"], Route::Remote),
        // a record that would not be dialed counts as well
        (&[PUBLIC_V4, "::1"], Route::Remote),
        (&[PUBLIC_V4, "fd00::1"], Route::Remote),
        (&[PUBLIC_V4, "::ffff:127.0.0.1"], Route::Remote),
        (&[PUBLIC_V4, "64:ff9b::5db8:d822"], Route::Remote),
        (&[PUBLIC_V6, "10.0.0.1"], Route::Remote),
        (&[PUBLIC_V6, "fe80::1"], Route::Remote),
        (&["2002:7f00:1::"], Route::Remote),
        (&["2001:0:4136:e378:8000:63bf:3fff:fdd2"], Route::Remote),
        (&["255.255.255.255"], Route::Remote),
        (&["224.0.0.1"], Route::Remote),
        (&["0.0.0.0"], Route::Remote),
        (&["::"], Route::Remote),
    ] {
        assert_eq!(named(answers), expected, "{answers:?}");
    }
    assert_eq!(
        classify("gone.example.com", 443, &Zone::default(), &Mac::home()),
        Route::Remote
    );
}

#[test]
fn a_name_is_looked_up_once_by_its_canonical_form() {
    let zone = Zone::with("www.example.com", &[&[PUBLIC_V4], &["127.0.0.1"]]);
    assert_eq!(
        classify("WWW.Example.%63om.", 443, &zone, &Mac::home()),
        direct(&[PUBLIC_V4], 443)
    );
    assert_eq!(*zone.asked.borrow(), ["www.example.com"]);
}

#[test]
fn addresses_of_this_machines_networks_go_to_the_remote() {
    let mac = || {
        Mac::home()
            .plus(own("en5", "81.10.20.30", "255.255.255.240"))
            .plus(InterfaceAddress {
                peer: Some(ip("45.1.2.3")),
                ..own("utun7", "45.9.9.9", "255.255.255.255")
            })
            .plus(InterfaceAddress {
                mask: None,
                ..own("en6", "62.1.1.1", "255.255.255.255")
            })
            .plus(InterfaceAddress {
                mask: None,
                ..own("en6", "2a10::1", "ffff::")
            })
    };
    for (address, expected) in [
        ("81.10.20.30", false),
        ("81.10.20.16", false),
        ("81.10.20.31", false),
        ("81.10.20.15", true),
        ("81.10.20.32", true),
        ("45.9.9.9", false),
        ("45.1.2.3", false),
        ("45.1.2.4", true),
        ("62.1.1.1", false),
        ("62.1.1.2", true),
        // own address, own /64, the site's other subnets, the next site
        ("2a02:8109:aa00:7::20", false),
        ("2a02:8109:aa00:7::1", false),
        ("2a02:8109:aa00:8::1", false),
        ("2a02:8109:aa00:ffff::1", false),
        ("2a10::2", false),
        ("2a11::1", true),
        ("2a02:8109:aa01::1", true),
        ("2a02:8109:a9ff:ffff::1", true),
    ] {
        let route = if expected {
            direct(&[address], 443)
        } else {
            Route::Remote
        };
        assert_eq!(classify(address, 443, &Panics, &mac()), route, "{address}");
        assert_eq!(
            classify("a.example.com", 443, &Always(ips(&[address])), &mac()),
            route,
            "{address}"
        );
    }
    // one answer on an own network spoils the rest, also the one not dialed
    for spoiled in [
        ["81.10.20.17", PUBLIC_V4],
        ["2a02:8109:aa00:9::1", PUBLIC_V4],
    ] {
        assert_eq!(
            classify("a.example.com", 443, &Always(ips(&spoiled)), &mac()),
            Route::Remote
        );
    }
}

#[test]
fn a_mask_of_zero_covers_everything() {
    let mac = Mac::home().plus(own("ppp0", "45.9.9.9", "0.0.0.0"));
    assert_eq!(classify(PUBLIC_V4, 443, &Panics, &mac), Route::Remote);
    assert_eq!(
        classify(PUBLIC_V6, 443, &Panics, &mac),
        direct(&[PUBLIC_V6], 443)
    );
    let mac = Mac::home().plus(own("ppp0", "2a00::1", "::"));
    assert_eq!(classify(PUBLIC_V6, 443, &Panics, &mac), Route::Remote);
}

#[test]
fn a_route_beside_the_default_one_goes_to_the_remote() {
    let mut mac = Mac::home();
    mac.tunnelled = vec![
        (Cidr::parse("151.101.0.0/16"), ip("100.85.59.104")),
        (Cidr::parse("8.8.8.8/32"), ip("203.0.113.9")),
    ];
    let route = |host: &str, answers: &[&str]| classify(host, 443, &Always(ips(answers)), &mac);
    assert_eq!(route("151.101.1.69", &[]), Route::Remote);
    assert_eq!(route("a.example.com", &["151.101.1.69"]), Route::Remote);
    assert_eq!(
        route("a.example.com", &[PUBLIC_V4, "151.101.1.69"]),
        Route::Remote
    );
    // a source that belongs to no interface
    assert_eq!(route("8.8.8.8", &[]), Route::Remote);
    assert_eq!(route(PUBLIC_V4, &[]), direct(&[PUBLIC_V4], 443));

    // the same address on two interfaces says nothing
    let mac = Mac::home().plus(own("bridge0", "192.168.1.20", "255.255.255.0"));
    assert_eq!(classify(PUBLIC_V4, 443, &Panics, &mac), Route::Remote);
}

#[test]
fn a_family_without_a_default_route_is_not_dialed() {
    let mut mac = Mac::home();
    mac.network.default_v6 = None;
    assert_eq!(classify(PUBLIC_V6, 443, &Panics, &mac), Route::Remote);
    // the AAAA of a dual-stack name is not dialed, so its route does not matter
    assert_eq!(
        classify("a.com", 443, &Always(ips(&[PUBLIC_V6, PUBLIC_V4])), &mac),
        direct(&[PUBLIC_V4], 443)
    );
    mac.network.default_v4 = None;
    assert_eq!(classify(PUBLIC_V4, 443, &Panics, &mac), Route::Remote);
}

struct Unreadable;

impl Interfaces for Unreadable {
    fn network(&self) -> io::Result<Network> {
        Err(io::ErrorKind::PermissionDenied.into())
    }

    fn source(&self, _to: IpAddr) -> io::Result<IpAddr> {
        Ok(ip("192.168.1.20"))
    }
}

struct NoSource;

impl Interfaces for NoSource {
    fn network(&self) -> io::Result<Network> {
        Mac::home().network()
    }

    fn source(&self, _to: IpAddr) -> io::Result<IpAddr> {
        Err(io::ErrorKind::NetworkUnreachable.into())
    }
}

#[test]
fn unknown_interfaces_send_everything_to_the_remote() {
    let mut empty = Mac::home();
    empty.network.addresses.clear();
    for interfaces in [&Unreadable as &dyn Interfaces, &NoSource, &empty] {
        assert_eq!(classify(PUBLIC_V4, 443, &Panics, interfaces), Route::Remote);
        assert_eq!(
            classify("a.com", 443, &Always(ips(&[PUBLIC_V4])), interfaces),
            Route::Remote
        );
    }
}

struct Clock(Cell<Instant>);

impl Clock {
    fn pass(&self, time: Duration) {
        self.0.set(self.0.get() + time);
    }
}

#[test]
fn rebinding_changes_the_route_and_never_a_pin() {
    let name = "rebind.example.com";
    let zone = Zone::with(
        name,
        &[
            &[PUBLIC_V4],
            &["127.0.0.1"],
            &[PUBLIC_V4, "192.168.1.1"],
            &[],
            &["151.101.1.69"],
        ],
    );
    let clock = Clock(Cell::new(Instant::now()));
    let cached = CachedResolver::with_clock(&zone, || clock.0.get());
    let mac = Mac::home();
    let route = || classify(name, 443, &cached, &mac);
    assert_eq!(route(), direct(&[PUBLIC_V4], 443));
    clock.pass(ANSWER_TTL - Duration::from_millis(1));
    assert_eq!(route(), direct(&[PUBLIC_V4], 443));
    assert_eq!(zone.lookups(), 1);
    clock.pass(Duration::from_millis(1));
    assert_eq!(route(), Route::Remote);
    assert_eq!(route(), Route::Remote);
    assert_eq!(zone.lookups(), 2);
    clock.pass(ANSWER_TTL);
    assert_eq!(route(), Route::Remote);
    clock.pass(ANSWER_TTL);
    assert_eq!(route(), Route::Remote);
    assert_eq!(zone.lookups(), 4);
    clock.pass(FAILURE_TTL - Duration::from_millis(1));
    assert_eq!(route(), Route::Remote);
    assert_eq!(zone.lookups(), 4);
    clock.pass(Duration::from_millis(1));
    assert_eq!(route(), direct(&["151.101.1.69"], 443));
    assert_eq!(zone.lookups(), 5);
}

#[test]
fn cached_answers_are_judged_against_the_network_as_it_is_now() {
    let zone = Zone::with("a.example.com", &[&[PUBLIC_V4]]);
    let cached = CachedResolver::new(&zone);
    let route = |mac: &Mac| classify("a.example.com", 443, &cached, mac);
    assert_eq!(route(&Mac::home()), direct(&[PUBLIC_V4], 443));
    let joined = Mac::home().plus(own("en5", "93.184.216.1", "255.255.255.0"));
    assert_eq!(route(&joined), Route::Remote);
    let mut tunnelled = Mac::home();
    tunnelled.tunnelled = vec![(Cidr::parse("93.184.0.0/16"), ip("100.85.59.104"))];
    assert_eq!(route(&tunnelled), Route::Remote);
    assert_eq!(zone.lookups(), 1);
}

#[test]
fn the_cache_is_keyed_by_name_and_shared_by_ports() {
    let zone = Zone::with("a.example.com", &[&[PUBLIC_V4]]);
    zone.names
        .borrow_mut()
        .insert("b.example.com".to_owned(), vec![Some(ips(&["10.0.0.1"]))]);
    let cached = CachedResolver::new(&zone);
    let mac = Mac::home();
    for (host, port, expected) in [
        ("a.example.com", 80, direct(&[PUBLIC_V4], 80)),
        ("A.EXAMPLE.COM.", 443, direct(&[PUBLIC_V4], 443)),
        ("b.example.com", 80, Route::Remote),
        ("a.example.com", 80, direct(&[PUBLIC_V4], 80)),
    ] {
        assert_eq!(classify(host, port, &cached, &mac), expected);
    }
    assert_eq!(zone.lookups(), 2);
}

#[test]
fn a_failed_lookup_is_remembered_for_a_moment() {
    let zone = Zone::default();
    zone.names
        .borrow_mut()
        .insert("a.com".to_owned(), vec![None, Some(ips(&[PUBLIC_V4]))]);
    let clock = Clock(Cell::new(Instant::now()));
    let cached = CachedResolver::with_clock(&zone, || clock.0.get());
    let mac = Mac::home();
    assert_eq!(classify("a.com", 1, &cached, &mac), Route::Remote);
    assert_eq!(classify("a.com", 1, &cached, &mac), Route::Remote);
    assert_eq!(zone.lookups(), 1);
    clock.pass(FAILURE_TTL);
    assert_eq!(classify("a.com", 1, &cached, &mac), direct(&[PUBLIC_V4], 1));
}

#[test]
fn the_cache_is_bounded_and_drops_the_oldest() {
    let clock = Clock(Cell::new(Instant::now()));
    let cached = CachedResolver::with_clock(Always(ips(&[PUBLIC_V4])), || clock.0.get());
    let names = || {
        let mut names: Vec<_> = cached.answers.lock().unwrap().keys().cloned().collect();
        names.sort();
        names
    };
    for index in 0..CACHE_MAX + 20 {
        cached.resolve(&format!("n{index:04}.com")).unwrap();
        clock.pass(Duration::from_millis(1));
    }
    let kept = names();
    assert_eq!(kept.len(), CACHE_MAX);
    assert_eq!(kept[0], "n0020.com");
    clock.pass(ANSWER_TTL);
    cached.resolve("last.com").unwrap();
    assert_eq!(names(), ["last.com"]);
}

#[test]
fn a_slow_lookup_does_not_replace_a_later_one() {
    let cached = CachedResolver::new(Panics);
    let now = Instant::now();
    cached.store("a.com", now, Some(ips(&["127.0.0.1"])));
    cached.store(
        "a.com",
        now - Duration::from_secs(1),
        Some(ips(&[PUBLIC_V4])),
    );
    assert_eq!(cached.resolve("a.com").unwrap(), ips(&["127.0.0.1"]));
}

#[test]
fn the_classifier_keeps_answers_between_requests() {
    let zone = Zone::with("www.example.com", &[&[PUBLIC_V4], &["127.0.0.1"]]);
    let classifier = Classifier::new(&zone, Mac::home());
    for port in [443, 80] {
        assert_eq!(
            classifier.route("www.example.com", port),
            direct(&[PUBLIC_V4], port)
        );
    }
    assert_eq!(zone.lookups(), 1);
}

/// The proxy shares it between its threads.
#[test]
fn the_system_classifier_is_shared() {
    fn shared<T: Send + Sync>(_: &T) {}
    shared(&Classifier::system());
}

/// Reads the real interfaces and asks the kernel for routes; sends nothing.
#[test]
fn the_real_interfaces_never_allow_a_private_address() {
    for host in ["127.0.0.1", "10.0.0.1", "192.168.1.1", "[::1]", "[fe80::1]"] {
        assert_eq!(
            classify(host, 443, &Panics, &SystemInterfaces),
            Route::Remote
        );
    }
    if let Ok(network) = SystemInterfaces.network() {
        for own in network.addresses {
            let route = classify(&own.address.to_string(), 443, &Panics, &SystemInterfaces);
            assert_eq!(route, Route::Remote, "{own:?}");
        }
    }
}

/// xorshift, fixed seed: a failure names the address.
struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn wide(&mut self) -> u128 {
        u128::from(self.next()) << 64 | u128::from(self.next())
    }
}

fn spellings(ip: IpAddr) -> Vec<String> {
    let IpAddr::V4(ip) = ip else {
        return vec![ip.to_string(), format!("[{ip}]").to_uppercase()];
    };
    let value = u32::from(ip);
    let [a, b, c, d] = ip.octets();
    let rest = value & 0x00ff_ffff;
    vec![
        ip.to_string(),
        format!("{ip}."),
        value.to_string(),
        format!("{value:#x}"),
        format!("{value:#X}").replace("0X", "0x000"),
        format!("0{value:o}"),
        format!("{a}.{rest}"),
        format!("{a}.{b}.{}", u32::from(c) << 8 | u32::from(d)),
        format!("0{a:o}.0{b:o}.0{c:o}.0{d:o}"),
        format!("{a:#x}.{b:#x}.{c}.0{d:o}"),
        format!("{a}%2e{b}%2E{c}.{d}"),
    ]
}

#[test]
fn no_address_outside_the_public_table_is_ever_dialed() {
    let mut random = Random(0x9e37_79b9_7f4a_7c15);
    // the interfaces must not do the table's work
    let mut mac = Mac::home();
    mac.network.addresses.retain(|own| own.name == "en0");
    let dialed = |route: &Route| match route {
        Route::Direct(pinned) => pinned.addresses().to_vec(),
        _ => Vec::new(),
    };
    let mut candidates = Vec::new();
    for cidr in NOT_PUBLIC.iter().map(|text| Cidr::parse(text)) {
        let full = if cidr.v4 { u32::MAX.into() } else { u128::MAX };
        for edge in [
            cidr.base,
            cidr.base + cidr.span,
            cidr.base + cidr.span / 2,
            cidr.base.saturating_sub(1),
            (cidr.base + cidr.span).saturating_add(1).min(full),
        ] {
            candidates.push(address(cidr.v4, edge));
        }
        for _ in 0..300 {
            let offset = match cidr.span.checked_add(1) {
                Some(size) => random.wide() % size,
                None => random.wide(),
            };
            candidates.push(address(cidr.v4, cidr.base + offset));
        }
    }
    for _ in 0..4000 {
        candidates.push(address(true, random.wide() & u128::from(u32::MAX)));
        candidates.push(address(false, random.wide()));
        candidates.push(address(false, random.wide() >> 3 | 1 << 125));
    }
    let mut reached = 0;
    for ip in candidates {
        let mut routes: Vec<Route> = spellings(ip)
            .iter()
            .map(|host| classify(host, 443, &Panics, &mac))
            .collect();
        for answers in [
            vec![ip],
            vec![ip, self::ip(PUBLIC_V4)],
            vec![self::ip(PUBLIC_V4), ip],
            vec![self::ip(PUBLIC_V6), ip],
        ] {
            routes.push(classify("a.com", 443, &Always(answers), &mac));
        }
        for route in routes {
            if not_public(ip) {
                assert_eq!(route, Route::Remote, "{ip}");
            }
            for address in dialed(&route) {
                assert!(!not_public(address.ip()), "{ip} dialed {address}");
                reached += 1;
            }
        }
    }
    // the other half of the property is not empty
    assert!(reached > 10_000, "{reached}");
}

#[test]
fn every_spelling_gives_the_same_route() {
    let mut random = Random(42);
    for _ in 0..4000 {
        let ip = address(true, random.wide() & u128::from(u32::MAX));
        let expected = literal(&ip.to_string());
        for host in spellings(ip) {
            assert_eq!(literal(&host), expected, "{host}");
        }
        // an IPv6 address around it is judged as IPv6
        let IpAddr::V4(v4) = ip else { unreachable!() };
        for host in [
            format!("[::ffff:{v4}]"),
            format!("[::{v4}]"),
            format!("[64:ff9b::{v4}]"),
            format!(
                "[2002:{:x}:{:x}::1]",
                u32::from(v4) >> 16,
                u32::from(v4) & 0xffff
            ),
        ] {
            assert_eq!(literal(&host), Route::Remote, "{host}");
        }
    }
}

#[test]
fn odd_bytes_never_panic_and_never_reach_past_the_host_rule() {
    let alphabet: Vec<char> = "0123456789abcdefxXzZ.-_%:[]/@ \\\0é。１".chars().collect();
    let mut random = Random(7);
    for _ in 0..200_000 {
        let length = random.next() % 24;
        let host: String = (0..length)
            .map(|_| alphabet[random.next() as usize % alphabet.len()])
            .collect();
        let route = classify(&host, 443, &Always(ips(&["127.0.0.1"])), &Mac::home());
        match Host::parse(&host) {
            Err(refusal) => assert_eq!(route, Route::Refuse(refusal)),
            Ok(Host::Ip(ip)) => {
                assert!(
                    matches!(route, Route::Remote | Route::Direct(_)),
                    "{host:?}"
                );
                assert_eq!(matches!(route, Route::Direct(_)), is_public(ip), "{host:?}");
            }
            Ok(canonical @ Host::Name(_)) => {
                assert_eq!(route, Route::Remote, "{host:?}");
                let text = canonical.to_string();
                assert!(text
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_.-".contains(&b)));
                assert!(!text.starts_with(['-', '.']), "{host:?}");
            }
        }
    }
}
