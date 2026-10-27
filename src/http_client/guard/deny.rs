// ABOUTME: The address deny list of the SSRF guard: every range a server-side fetch must never reach
// ABOUTME: Explicit CIDR masks for IPv4 and IPv6, with v4 embedded in v6 (mapped, NAT64, 6to4) unwrapped
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

//! The deny list a guarded fetch checks every resolved address against.
//!
//! Written as explicit prefix masks rather than the standard library's
//! `is_private`/`is_global` family: those cover a different, partly unstable
//! set, and the list here is the contract the guard documents.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Denied IPv4 ranges, as `(network, prefix length)`.
const DENIED_V4: &[(Ipv4Addr, u8)] = &[
    // "This network", including the unspecified address 0.0.0.0.
    (Ipv4Addr::UNSPECIFIED, 8),
    // RFC 1918 private ranges.
    (Ipv4Addr::new(10, 0, 0, 0), 8),
    (Ipv4Addr::new(172, 16, 0, 0), 12),
    (Ipv4Addr::new(192, 168, 0, 0), 16),
    // Carrier-grade NAT (RFC 6598); Alibaba Cloud's metadata service lives here.
    (Ipv4Addr::new(100, 64, 0, 0), 10),
    // Loopback.
    (Ipv4Addr::new(127, 0, 0, 0), 8),
    // Link-local, which holds the cloud metadata address 169.254.169.254.
    (Ipv4Addr::new(169, 254, 0, 0), 16),
    // IETF protocol assignments (RFC 6890).
    (Ipv4Addr::new(192, 0, 0, 0), 24),
    // Benchmarking (RFC 2544).
    (Ipv4Addr::new(198, 18, 0, 0), 15),
    // Multicast.
    (Ipv4Addr::new(224, 0, 0, 0), 4),
    // Reserved, which holds the limited broadcast address 255.255.255.255.
    (Ipv4Addr::new(240, 0, 0, 0), 4),
];

/// Denied IPv6 ranges that are refused whole, as `(network, prefix length)`.
const DENIED_V6: &[(Ipv6Addr, u8)] = &[
    // Unspecified (::) and loopback (::1), and the deprecated v4-compatible ::/96.
    (Ipv6Addr::UNSPECIFIED, 96),
    // NAT64 well-known prefix: a gateway would carry the request to the v4 inside.
    (Ipv6Addr::new(0x64, 0xff9b, 0, 0, 0, 0, 0, 0), 96),
    // NAT64 local-use prefix (RFC 8215).
    (Ipv6Addr::new(0x64, 0xff9b, 1, 0, 0, 0, 0, 0), 48),
    // Teredo, which tunnels to an embedded IPv4 address.
    (Ipv6Addr::new(0x2001, 0, 0, 0, 0, 0, 0, 0), 32),
    // 6to4, which tunnels to an embedded IPv4 address.
    (Ipv6Addr::new(0x2002, 0, 0, 0, 0, 0, 0, 0), 16),
    // Unique local (fc00::/7); AWS's IPv6 metadata address fd00:ec2::254 is here.
    (Ipv6Addr::new(0xfc00, 0, 0, 0, 0, 0, 0, 0), 7),
    // Link-local.
    (Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 0), 10),
    // Deprecated site-local.
    (Ipv6Addr::new(0xfec0, 0, 0, 0, 0, 0, 0, 0), 10),
    // Multicast.
    (Ipv6Addr::new(0xff00, 0, 0, 0, 0, 0, 0, 0), 8),
];

/// Whether `ip` lies in a range a guarded fetch must never connect to.
///
/// An IPv4-mapped IPv6 address (`::ffff:a.b.c.d`) is judged by the IPv4
/// address inside it, so the deny list cannot be stepped around by writing a
/// private v4 address in v6 form.
#[must_use]
pub fn is_denied_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_denied_v4(v4),
        IpAddr::V6(v6) => v6
            .to_ipv4_mapped()
            .map_or_else(|| is_denied_v6(v6), is_denied_v4),
    }
}

fn is_denied_v4(ip: Ipv4Addr) -> bool {
    let bits = u32::from(ip);
    DENIED_V4.iter().any(|&(network, prefix)| {
        let mask = u32::MAX.checked_shl(32 - u32::from(prefix)).unwrap_or(0);
        bits & mask == u32::from(network) & mask
    })
}

fn is_denied_v6(ip: Ipv6Addr) -> bool {
    let bits = u128::from(ip);
    DENIED_V6.iter().any(|&(network, prefix)| {
        let mask = u128::MAX.checked_shl(128 - u32::from(prefix)).unwrap_or(0);
        bits & mask == u128::from(network) & mask
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn denied(text: &str) -> bool {
        is_denied_ip(text.parse().expect("test address parses")) // Safe: test assertion
    }

    #[test]
    fn every_denied_ipv4_range_is_refused_at_both_ends() {
        for address in [
            "0.0.0.0",
            "0.255.255.255",
            "10.0.0.0",
            "10.255.255.255",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.0.1",
            "192.168.255.255",
            "100.64.0.0",
            "100.127.255.255",
            "100.100.100.200",
            "127.0.0.1",
            "127.255.255.254",
            "169.254.0.1",
            "169.254.169.254",
            "192.0.0.1",
            "198.18.0.0",
            "198.19.255.255",
            "224.0.0.1",
            "239.255.255.255",
            "240.0.0.1",
            "255.255.255.255",
        ] {
            assert!(denied(address), "{address} must be denied");
        }
    }

    #[test]
    fn the_addresses_just_outside_each_ipv4_range_are_allowed() {
        for address in [
            "1.0.0.0",
            "9.255.255.255",
            "11.0.0.0",
            "172.15.255.255",
            "172.32.0.0",
            "192.167.255.255",
            "192.169.0.0",
            "100.63.255.255",
            "100.128.0.0",
            "126.255.255.255",
            "128.0.0.0",
            "169.253.255.255",
            "169.255.0.0",
            "198.17.255.255",
            "198.20.0.0",
            "223.255.255.255",
            "8.8.8.8",
            "93.184.216.34",
        ] {
            assert!(!denied(address), "{address} must be allowed");
        }
    }

    #[test]
    fn every_denied_ipv6_range_is_refused() {
        for address in [
            "::",
            "::1",
            "::127.0.0.1",
            "64:ff9b::a9fe:a9fe",
            "64:ff9b::808:808",
            "64:ff9b:1::1",
            "2001::1",
            "2002:a9fe:a9fe::1",
            "2002:808:808::1",
            "fc00::1",
            "fd00:ec2::254",
            "fe80::1",
            "febf::1",
            "fec0::1",
            "ff02::1",
        ] {
            assert!(denied(address), "{address} must be denied");
        }
    }

    #[test]
    fn a_v4_mapped_address_is_judged_by_its_v4_part() {
        assert!(denied("::ffff:169.254.169.254"));
        assert!(denied("::ffff:10.0.0.1"));
        assert!(denied("::ffff:127.0.0.1"));
        assert!(!denied("::ffff:93.184.216.34"));
    }

    #[test]
    fn public_ipv6_addresses_are_allowed() {
        for address in [
            "2606:4700:4700::1111",
            "2a00:1450:4001:80b::200e",
            "2001:4860::1",
        ] {
            assert!(!denied(address), "{address} must be allowed");
        }
    }
}
