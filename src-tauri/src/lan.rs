//! Opt-in LAN console discovery: scan local /24 for FTP port 21 (rate-limited).

use std::io::Read;
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::time::Duration;

use serde::Serialize;

use crate::ftp;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct DiscoveredConsole {
    pub host: String,
    pub port: u16,
    pub banner: String,
}

const TIMEOUT: Duration = Duration::from_millis(350);

/// Guess this machine's IPv4 address (best effort).
pub fn local_ipv4() -> Option<Ipv4Addr> {
    let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("8.8.8.8:80").ok()?;
    s.local_addr().ok().and_then(|a| match a.ip() {
        std::net::IpAddr::V4(v) => Some(v),
        _ => None,
    })
}

/// Scan `base.1` … `base.254` except `skip_host` (usually this machine). Rate-limited, LAN only.
pub fn discover(base: Ipv4Addr, skip_host: Option<Ipv4Addr>, port: u16) -> Vec<DiscoveredConsole> {
    if !ftp::is_lan_ip(base) {
        return vec![];
    }
    let mut out = vec![];
    let o = base.octets();
    for last in 1u8..=254 {
        if out.len() >= 8 {
            break;
        }
        let ip = Ipv4Addr::new(o[0], o[1], o[2], last);
        if skip_host == Some(ip) {
            continue;
        }
        if !ftp::is_lan_ip(ip) {
            continue;
        }
        let addr = SocketAddr::new(std::net::IpAddr::V4(ip), port);
        if let Ok(mut s) = TcpStream::connect_timeout(&addr, TIMEOUT) {
            s.set_read_timeout(Some(TIMEOUT)).ok();
            s.set_write_timeout(Some(TIMEOUT)).ok();
            let mut buf = [0u8; 256];
            let n = s.read(&mut buf).unwrap_or(0);
            let banner = String::from_utf8_lossy(&buf[..n]).trim().to_string();
            if banner.contains("220") || banner.to_ascii_lowercase().contains("ftp") {
                out.push(DiscoveredConsole {
                    host: ip.to_string(),
                    port,
                    banner,
                });
            }
        }
        std::thread::sleep(Duration::from_millis(12));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_non_lan_base() {
        assert!(discover("8.8.8.8".parse().unwrap(), None, 21).is_empty());
    }
}
