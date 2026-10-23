use std::net::Ipv4Addr;

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

#[cfg(test)]
mod tests {
    use super::*;

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
