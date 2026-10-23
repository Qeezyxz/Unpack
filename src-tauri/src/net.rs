use std::net::{IpAddr, Ipv4Addr};

/// True for RFC1918, loopback, link-local, or `*.local` names.
/// Send to Xbox must never leave the LAN.
pub fn is_lan_host(host: &str) -> bool {
    let host = host.trim();
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    if let Ok(ip) = host.parse::<Ipv4Addr>() {
        return ip.is_private() || ip.is_loopback() || ip.is_link_local();
    }
    let lower = host.to_ascii_lowercase();
    lower.ends_with(".local") || lower.ends_with(".lan")
}

/// Same rule as `is_lan_host`, for an address a name resolved to.
pub fn is_lan_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => v.is_private() || v.is_loopback() || v.is_link_local(),
        IpAddr::V6(v) => {
            let first = v.segments()[0];
            v.is_loopback() || (first & 0xfe00) == 0xfc00 || (first & 0xffc0) == 0xfe80
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolved_addresses_must_be_local() {
        assert!(is_lan_ip("192.168.1.20".parse().unwrap()));
        assert!(is_lan_ip("::1".parse().unwrap()));
        assert!(is_lan_ip("fd00::5".parse().unwrap()));
        assert!(is_lan_ip("fe80::1".parse().unwrap()));
        assert!(!is_lan_ip("8.8.8.8".parse().unwrap()));
        assert!(!is_lan_ip("2606:4700::1111".parse().unwrap()));
    }

    #[test]
    fn accepts_private_and_rejects_public() {
        assert!(is_lan_host("192.168.1.20"));
        assert!(is_lan_host("10.0.0.5"));
        assert!(is_lan_host("172.16.4.8"));
        assert!(is_lan_host("127.0.0.1"));
        assert!(is_lan_host("xbox.local"));
        assert!(!is_lan_host("8.8.8.8"));
        assert!(!is_lan_host("example.com"));
        assert!(!is_lan_host(""));
    }
}
