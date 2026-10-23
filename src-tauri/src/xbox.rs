use crate::ftp::{FtpClient, UploadOutcome};
use crate::net;
use crate::paths;
use serde::Serialize;
use std::fs;
use std::net::{Ipv4Addr, SocketAddrV4, TcpStream, UdpSocket};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone, Serialize)]
pub struct XboxHost {
    pub ip: String,
    pub port: u16,
    pub banner: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SendReport {
    pub host: String,
    pub uploaded: u32,
    pub skipped_protected: u32,
    pub files: Vec<String>,
    pub warnings: Vec<String>,
}

pub fn discover(port: u16, timeout_ms: u64) -> Vec<XboxHost> {
    let Some(local) = local_ipv4() else {
        return Vec::new();
    };
    let octets = local.octets();
    let timeout = Duration::from_millis(timeout_ms.max(80));
    let (tx, rx) = std::sync::mpsc::channel();
    let mut handles = Vec::new();
    for host in 1..=254 {
        if host == octets[3] {
            continue;
        }
        let ip = Ipv4Addr::new(octets[0], octets[1], octets[2], host);
        let tx = tx.clone();
        handles.push(std::thread::spawn(move || {
            let addr = SocketAddrV4::new(ip, port);
            if let Ok(stream) = TcpStream::connect_timeout(&addr.into(), timeout) {
                let _ = stream.set_read_timeout(Some(Duration::from_millis(400)));
                let mut banner = String::new();
                use std::io::Read;
                let mut buf = [0u8; 128];
                if let Ok(n) = (&stream).take(128).read(&mut buf) {
                    banner = String::from_utf8_lossy(&buf[..n]).trim().to_string();
                }
                let looks_ftp = banner.starts_with("220")
                    || banner.to_ascii_lowercase().contains("ftp")
                    || banner.to_ascii_lowercase().contains("aurora")
                    || banner.to_ascii_lowercase().contains("xexmenu")
                    || banner.to_ascii_lowercase().contains("filezilla");
                if looks_ftp || banner.is_empty() {
                    let _ = tx.send(XboxHost {
                        ip: ip.to_string(),
                        port,
                        banner,
                    });
                }
            }
        }));
        if handles.len() >= 48 {
            for h in handles.drain(..) {
                let _ = h.join();
            }
        }
    }
    drop(tx);
    for h in handles {
        let _ = h.join();
    }
    let mut found: Vec<XboxHost> = rx.into_iter().collect();
    found.sort_by(|a, b| a.ip.cmp(&b.ip));
    found
}

fn local_ipv4() -> Option<Ipv4Addr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    match socket.local_addr().ok()?.ip() {
        std::net::IpAddr::V4(v) => Some(v),
        _ => None,
    }
}

pub fn test_login(host: &str, port: u16, user: &str, pass: &str) -> Result<String, String> {
    if !net::is_lan_host(host) {
        return Err("Send to Xbox only accepts local network addresses".into());
    }
    let client = FtpClient::connect(host, port, user, pass)?;
    client.close();
    Ok(format!("Connected to {host}:{port}"))
}

pub fn send_paths(
    host: &str,
    port: u16,
    user: &str,
    pass: &str,
    dest_root: &str,
    paths_in: &[PathBuf],
    include_launch_ini: bool,
    mut progress: impl FnMut(String, u64, u64),
) -> Result<SendReport, String> {
    if host.trim().is_empty() {
        return Err("Xbox IP is required".into());
    }
    if !net::is_lan_host(host) {
        return Err("Send to Xbox only accepts a local-network IP (192.168/10/172.16) or a .local name".into());
    }
    progress(format!("Connecting to {host}:{port}"), 0, 1);
    let mut ftp = FtpClient::connect(host, port, user, pass)?;
    ftp.mkdir_p(dest_root)?;
    let mut report = SendReport {
        host: format!("{host}:{port}"),
        uploaded: 0,
        skipped_protected: 0,
        files: Vec::new(),
        warnings: Vec::new(),
    };
    let files = expand_files(paths_in);
    let total = files.len() as u64;
    for (i, file) in files.iter().enumerate() {
        let name = file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".into());
        progress(format!("Sending {name}"), i as u64, total.max(1));
        if paths::is_protected_filename(&name) && !include_launch_ini {
            report.skipped_protected += 1;
            report
                .warnings
                .push("skipped launch.ini (enable the DashLaunch option to replace it atomically)".into());
            continue;
        }
        let relative = relative_to_batch(paths_in, file);
        if let Some(parent) = Path::new(&relative).parent() {
            if parent != Path::new("") {
                let remote_dir = format!(
                    "{}/{}",
                    dest_root.trim_end_matches(['/', '\\']),
                    parent.to_string_lossy().replace('\\', "/")
                );
                let _ = ftp.mkdir_p(&remote_dir);
            } else {
                let _ = ftp.cwd(dest_root);
            }
        } else {
            let _ = ftp.cwd(dest_root);
        }
        match ftp.upload_file(file, &name, include_launch_ini)? {
            UploadOutcome::SkippedProtected => {
                report.skipped_protected += 1;
                report.warnings.push(format!(
                    "left existing {name} in place instead of deleting it mid-transfer"
                ));
            }
            UploadOutcome::Uploaded | UploadOutcome::UploadedAtomic => {
                report.uploaded += 1;
                report.files.push(file.display().to_string());
            }
        }
    }
    ftp.close();
    progress("Done".into(), 1, 1);
    Ok(report)
}

fn expand_files(paths_in: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for p in paths_in {
        if p.is_dir() {
            collect_files(p, &mut out);
        } else {
            out.push(p.clone());
        }
    }
    out
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_files(&path, out);
            } else {
                out.push(path);
            }
        }
    }
}

fn relative_to_batch(roots: &[PathBuf], file: &Path) -> String {
    for root in roots {
        if let Ok(rel) = file.strip_prefix(root) {
            let folder = root
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            if rel.as_os_str().is_empty() {
                return folder;
            }
            if folder.is_empty() {
                return rel.to_string_lossy().into_owned();
            }
            return Path::new(&folder).join(rel).to_string_lossy().into_owned();
        }
        if let Some(parent) = root.parent() {
            if let Ok(rel) = file.strip_prefix(parent) {
                return rel.to_string_lossy().into_owned();
            }
        }
    }
    file.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths_keep_folder_shape() {
        let root = PathBuf::from("/tmp/Games/Halo");
        let file = PathBuf::from("/tmp/Games/Halo/default.xex");
        assert_eq!(relative_to_batch(&[root], &file), "Halo/default.xex");
    }

    #[test]
    fn refuses_public_hosts() {
        let err = send_paths(
            "8.8.8.8",
            21,
            "xbox",
            "xbox",
            "/Hdd1",
            &[],
            false,
            |_, _, _| {},
        )
        .unwrap_err();
        assert!(err.contains("local-network"));
    }
}
