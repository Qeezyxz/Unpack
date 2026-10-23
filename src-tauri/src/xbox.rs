use crate::ftp::{self, FtpClient, UploadOutcome};
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
    pub resumed: u32,
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
        resumed: 0,
        files: Vec::new(),
        warnings: Vec::new(),
    };
    let files = expand_files(paths_in)?;
    if files.is_empty() {
        ftp.close();
        return Err("no files found to send".into());
    }
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
        let remote_dir = if let Some(parent) = Path::new(&relative).parent() {
            if parent != Path::new("") {
                Some(format!(
                    "{}/{}",
                    dest_root.trim_end_matches(['/', '\\']),
                    parent.to_string_lossy().replace('\\', "/")
                ))
            } else {
                None
            }
        } else {
            None
        };
        let mut attempts = 0u32;
        let mut stalls = 0u32;
        loop {
            let dir_ok = if let Some(dir) = &remote_dir {
                ftp.mkdir_p(dir)
            } else {
                ftp.cwd(dest_root)
            };
            if let Err(e) = dir_ok {
                if ftp::looks_like_drop(&e) && attempts < 3 {
                    attempts += 1;
                    progress(
                        format!("Connection dropped. Reconnecting ({attempts}/3)…"),
                        i as u64,
                        total.max(1),
                    );
                    ftp = reconnect(host, port, user, pass, dest_root, attempts)?;
                    continue;
                }
                return Err(e);
            }
            match ftp.upload_file(file, &name, include_launch_ini) {
                Ok(UploadOutcome::SkippedProtected) => {
                    report.skipped_protected += 1;
                    report.warnings.push(format!(
                        "left existing {name} in place instead of deleting it mid-transfer"
                    ));
                    break;
                }
                Ok(UploadOutcome::AlreadyPresent) => {
                    report.uploaded += 1;
                    report.files.push(file.display().to_string());
                    report
                        .warnings
                        .push(format!("{name} was already on the Xbox (same size)"));
                    break;
                }
                Ok(UploadOutcome::Resumed) => {
                    report.uploaded += 1;
                    report.resumed += 1;
                    report.files.push(file.display().to_string());
                    break;
                }
                Ok(UploadOutcome::Uploaded | UploadOutcome::UploadedAtomic) => {
                    report.uploaded += 1;
                    report.files.push(file.display().to_string());
                    break;
                }
                Err(e) if ftp::looks_like_drop(&e) && attempts < 3 => {
                    stalls = if ftp::stalled(ftp.sent_last()) { stalls + 1 } else { 0 };
                    if stalls >= 2 {
                        return Err(ftp::stalled_message(&name, stalls));
                    }
                    attempts += 1;
                    progress(
                        format!("Wi-Fi dropped on {name}. Reconnecting ({attempts}/3)…"),
                        i as u64,
                        total.max(1),
                    );
                    ftp = reconnect(host, port, user, pass, dest_root, attempts)?;
                }
                Err(e) => return Err(e),
            }
        }
    }
    ftp.close();
    progress("Done".into(), 1, 1);
    Ok(report)
}

fn reconnect(
    host: &str,
    port: u16,
    user: &str,
    pass: &str,
    dest_root: &str,
    attempt: u32,
) -> Result<FtpClient, String> {
    std::thread::sleep(Duration::from_secs(u64::from(attempt).min(4)));
    let mut ftp = FtpClient::connect(host, port, user, pass)
        .map_err(|e| format!("reconnect failed after a drop: {e}"))?;
    ftp.mkdir_p(dest_root)?;
    Ok(ftp)
}

fn expand_files(paths_in: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
    let mut out = Vec::new();
    for p in paths_in {
        if p.is_dir() {
            collect_files(p, &mut out, 0)?;
        } else if p.exists() {
            out.push(p.clone());
        } else {
            return Err(format!("file not found: {}", p.display()));
        }
    }
    Ok(out)
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>, depth: u32) -> Result<(), String> {
    if depth > crate::fileops::MAX_FOLDER_DEPTH {
        return Err(crate::fileops::too_deep_message(dir));
    }
    let entries = fs::read_dir(dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("{}: {e}", dir.display()))?;
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, out, depth + 1)?;
        } else {
            out.push(path);
        }
    }
    Ok(())
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
    fn missing_local_file_is_an_error() {
        let err = expand_files(&[PathBuf::from("/no/such/unpack-file.bin")]).unwrap_err();
        assert!(err.contains("not found"));
    }

    use crate::testftp::{Cfg, FakeFtp};
    use std::sync::atomic::{AtomicU32, Ordering};

    struct Scratch(PathBuf);

    impl std::ops::Deref for Scratch {
        type Target = PathBuf;
        fn deref(&self) -> &PathBuf {
            &self.0
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn scratch(tag: &str) -> Scratch {
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "unpack-xbox-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    fn blob(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i * 31 % 251) as u8).collect()
    }

    fn send(
        fake: &FakeFtp,
        paths: &[PathBuf],
        launch: bool,
    ) -> Result<SendReport, String> {
        send_paths(
            "127.0.0.1",
            fake.port,
            "xbox",
            "xbox",
            "/Hdd1/Games",
            paths,
            launch,
            |_, _, _| {},
        )
    }

    #[test]
    fn uploads_a_folder_and_renames_the_part_file() {
        let fake = FakeFtp::start(Cfg::default());
        let dir = scratch("ok");
        let game = dir.join("Halo");
        fs::create_dir_all(game.join("Data")).unwrap();
        fs::write(game.join("default.xex"), blob(5000)).unwrap();
        fs::write(game.join("Data/0000"), blob(70_000)).unwrap();
        let report = send(&fake, &[game], false).unwrap();
        assert_eq!(report.uploaded, 2);
        assert_eq!(report.resumed, 0);
        assert_eq!(fake.file("Hdd1/Games/Halo/default.xex").unwrap(), blob(5000));
        assert_eq!(fake.file("Hdd1/Games/Halo/Data/0000").unwrap(), blob(70_000));
        assert!(fake.names().iter().all(|n| !n.ends_with(".part")), "{:?}", fake.names());
    }

    #[test]
    fn a_second_send_of_the_same_file_is_skipped() {
        let fake = FakeFtp::start(Cfg::default());
        let dir = scratch("again");
        let file = dir.join("game.bin");
        fs::write(&file, blob(9000)).unwrap();
        send(&fake, &[file.clone()], false).unwrap();
        let before = fake.stors();
        let report = send(&fake, &[file], false).unwrap();
        assert_eq!(fake.stors(), before, "nothing should be uploaded twice");
        assert!(report.warnings.iter().any(|w| w.contains("already on the Xbox")));
    }

    #[test]
    fn a_dropped_transfer_resumes_with_rest() {
        let fake = FakeFtp::start(Cfg {
            drop_stor: vec![Some(600_000)],
            ..Cfg::default()
        });
        let dir = scratch("rest");
        let file = dir.join("big.bin");
        let body = blob(1_500_000);
        fs::write(&file, &body).unwrap();
        let report = send(&fake, &[file], false).unwrap();
        assert_eq!(report.resumed, 1);
        assert_eq!(fake.logins(), 2, "one reconnect expected");
        assert!(fake.saw("REST "));
        assert_eq!(fake.file("Hdd1/Games/big.bin").unwrap(), body);
        assert!(!fake.has("Hdd1/Games/big.bin.part"));
    }

    #[test]
    fn resume_falls_back_to_append_without_rest() {
        let fake = FakeFtp::start(Cfg {
            drop_stor: vec![Some(600_000)],
            rest_ok: false,
            ..Cfg::default()
        });
        let dir = scratch("appe");
        let file = dir.join("big.bin");
        let body = blob(1_500_000);
        fs::write(&file, &body).unwrap();
        let report = send(&fake, &[file], false).unwrap();
        assert_eq!(report.resumed, 1);
        assert!(fake.saw("APPE "));
        assert_eq!(fake.file("Hdd1/Games/big.bin").unwrap(), body);
    }

    #[test]
    fn a_server_without_size_still_receives_the_file() {
        let fake = FakeFtp::start(Cfg {
            size_ok: false,
            ..Cfg::default()
        });
        let dir = scratch("nosize");
        let file = dir.join("game.bin");
        fs::write(&file, blob(12_345)).unwrap();
        let report = send(&fake, &[file], false).unwrap();
        assert_eq!(report.uploaded, 1);
        assert_eq!(fake.file("Hdd1/Games/game.bin").unwrap(), blob(12_345));
    }

    #[test]
    fn a_full_drive_stops_at_once_with_a_plain_message() {
        let fake = FakeFtp::start(Cfg {
            capacity: Some(400_000),
            ..Cfg::default()
        });
        let dir = scratch("full");
        let file = dir.join("big.bin");
        fs::write(&file, blob(1_500_000)).unwrap();
        let err = send(&fake, &[file], false).unwrap_err();
        assert!(err.contains("drive is full"), "{err}");
        assert_eq!(fake.logins(), 1, "a full drive is not a Wi-Fi drop, so no reconnect");
        assert_eq!(fake.stors(), 1);
    }

    #[test]
    fn a_connection_that_never_gets_going_gives_up_with_a_plain_message() {
        let fake = FakeFtp::start(Cfg {
            drop_stor: vec![Some(1000), Some(1000), Some(1000), Some(1000)],
            ..Cfg::default()
        });
        let dir = scratch("stall");
        let file = dir.join("big.bin");
        fs::write(&file, blob(1_500_000)).unwrap();
        let err = send(&fake, &[file], false).unwrap_err();
        assert!(err.contains("stopped taking"), "{err}");
        assert!(fake.logins() <= 3, "{} logins", fake.logins());
    }

    #[test]
    fn launch_ini_is_skipped_unless_asked_and_replaced_atomically() {
        let fake = FakeFtp::start(Cfg::default());
        let dir = scratch("ini");
        let ini = dir.join("launch.ini");
        fs::write(&ini, b"[Paths]\nDefault = Hdd1:\\Games\n").unwrap();
        let report = send(&fake, &[ini.clone()], false).unwrap();
        assert_eq!(report.skipped_protected, 1);
        assert!(!fake.has("Hdd1/Games/launch.ini"));
        let report = send(&fake, &[ini], true).unwrap();
        assert_eq!(report.uploaded, 1);
        assert!(fake.has("Hdd1/Games/launch.ini"));
        assert!(!fake.has("Hdd1/Games/launch.ini.part"));
    }

    #[test]
    fn a_login_with_line_breaks_is_refused_before_it_is_sent() {
        let fake = FakeFtp::start(Cfg::default());
        let dir = scratch("crlf");
        let file = dir.join("a.bin");
        fs::write(&file, b"x").unwrap();
        let err = send_paths(
            "127.0.0.1",
            fake.port,
            "xbox\r\nDELE everything",
            "xbox",
            "/Hdd1/Games",
            &[file],
            false,
            |_, _, _| {},
        )
        .unwrap_err();
        assert!(err.contains("line breaks"), "{err}");
        assert!(!fake.saw("DELE everything"));
    }

    #[cfg(unix)]
    #[test]
    fn a_link_loop_in_the_folder_is_an_error() {
        let dir = scratch("loop");
        fs::create_dir_all(dir.join("game/sub")).unwrap();
        fs::write(dir.join("game/sub/a.bin"), b"a").unwrap();
        std::os::unix::fs::symlink(dir.join("game"), dir.join("game/sub/back")).unwrap();
        let err = expand_files(&[dir.join("game")]).unwrap_err();
        assert!(err.contains("nested too deeply"), "{err}");
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
