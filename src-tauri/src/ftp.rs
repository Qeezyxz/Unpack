use crate::paths;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream, ToSocketAddrs};
use std::path::Path;
use std::time::Duration;

pub struct FtpClient {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
    /// Address of the Xbox from the control connection. Used when PASV
    /// returns 0.0.0.0 or a non-LAN IP (common on FSD / Aurora).
    control_ip: IpAddr,
}

impl FtpClient {
    pub fn connect(host: &str, port: u16, user: &str, pass: &str) -> Result<Self, String> {
        let addr = format!("{host}:{port}");
        let stream = connect_timeout(&addr, Duration::from_secs(5))?;
        stream
            .set_read_timeout(Some(Duration::from_secs(120)))
            .map_err(|e| e.to_string())?;
        stream
            .set_write_timeout(Some(Duration::from_secs(120)))
            .map_err(|e| e.to_string())?;
        let control_ip = stream
            .peer_addr()
            .map(|a| a.ip())
            .unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
        let writer = stream.try_clone().map_err(|e| e.to_string())?;
        let mut client = FtpClient {
            reader: BufReader::new(stream),
            writer,
            control_ip,
        };
        client.read_response()?; // greeting
        let user = if user.trim().is_empty() { "anonymous" } else { user };
        client.command(&format!("USER {user}"))?;
        let (code, msg) = client.command(&format!("PASS {pass}"))?;
        if code >= 400 {
            return Err(format!("FTP login failed: {code} {msg}"));
        }
        client.command("TYPE I")?;
        Ok(client)
    }

    pub fn command(&mut self, line: &str) -> Result<(u16, String), String> {
        self.writer
            .write_all(format!("{line}\r\n").as_bytes())
            .map_err(|e| e.to_string())?;
        self.writer.flush().map_err(|e| e.to_string())?;
        self.read_response()
    }

    fn read_response(&mut self) -> Result<(u16, String), String> {
        let mut text = String::new();
        loop {
            let mut line = String::new();
            let n = self
                .reader
                .read_line(&mut line)
                .map_err(|e| e.to_string())?;
            if n == 0 {
                return Err("FTP connection closed".into());
            }
            text.push_str(&line);
            if line.len() >= 4 {
                let code: u16 = line[0..3].parse().unwrap_or(0);
                if line.as_bytes()[3] == b' ' {
                    return Ok((code, text));
                }
            }
        }
    }

    fn pasv_stream(&mut self) -> Result<TcpStream, String> {
        let (code, msg) = self.command("PASV")?;
        if code != 227 {
            return Err(format!("PASV failed: {code} {msg}"));
        }
        let addr = parse_pasv(&msg, self.control_ip)?;
        let stream = connect_timeout(&addr, Duration::from_secs(8))?;
        let _ = stream.set_write_timeout(Some(Duration::from_secs(600)));
        let _ = stream.set_read_timeout(Some(Duration::from_secs(600)));
        Ok(stream)
    }

    pub fn cwd(&mut self, path: &str) -> Result<(), String> {
        let (code, msg) = self.command(&format!("CWD {path}"))?;
        if code >= 400 {
            Err(format!("CWD {path} failed: {code} {msg}"))
        } else {
            Ok(())
        }
    }

    pub fn mkdir_p(&mut self, path: &str) -> Result<(), String> {
        let mut last_err = String::new();
        for candidate in xbox_path_candidates(path) {
            match self.mkdir_p_one(&candidate) {
                Ok(()) => return Ok(()),
                Err(e) => last_err = e,
            }
        }
        Err(last_err)
    }

    fn mkdir_p_one(&mut self, normalized: &str) -> Result<(), String> {
        let mut current = String::new();
        let sep = if normalized.contains('\\') { '\\' } else { '/' };
        for part in normalized.split(['/', '\\']).filter(|p| !p.is_empty()) {
            if current.is_empty() && !normalized.starts_with(['/', '\\']) {
                current.push_str(part);
            } else {
                current.push(sep);
                current.push_str(part);
            }
            // 550 = already exists on FSD/Aurora; CWD below is the real check.
            let _ = self.command(&format!("MKD {current}"));
        }
        if !normalized.is_empty() {
            self.cwd(normalized)?;
        }
        Ok(())
    }

    pub fn upload_file(
        &mut self,
        local: &Path,
        remote_name: &str,
        overwrite_protected: bool,
    ) -> Result<UploadOutcome, String> {
        if paths::is_protected_filename(remote_name) && !overwrite_protected {
            return Ok(UploadOutcome::SkippedProtected);
        }
        let local_len = std::fs::metadata(local)
            .map_err(|e| format!("{}: {e}", local.display()))?
            .len();
        // Store beside the destination as name.part. If Wi-Fi drops, the .part
        // stays on the Xbox; the next send measures it with SIZE and appends
        // the rest. The real file is only renamed into place when complete.
        let tmp_name = format!("{remote_name}.part");
        let dest_len = self.size_of(remote_name)?;
        let part_len = self.size_of(&tmp_name)?;
        match resume_action(local_len, dest_len, part_len) {
            ResumeAction::SkipFinished => return Ok(UploadOutcome::AlreadyPresent),
            ResumeAction::FinishRename => {}
            ResumeAction::ResumeFrom(offset) => {
                self.send_remaining(&tmp_name, local, offset)?;
            }
            ResumeAction::Restart => {
                if part_len.unwrap_or(0) > 0 {
                    let _ = self.command(&format!("DELE {tmp_name}"));
                }
                self.stor_from(&tmp_name, local, 0, false)?;
            }
        }
        self.rename_part(&tmp_name, remote_name)?;
        if let Some(remote_len) = self.size_of(remote_name)? {
            if remote_len != local_len {
                return Err(format!(
                    "Xbox size for {remote_name} is {remote_len} bytes, local file is {local_len}"
                ));
            }
        }
        if paths::is_protected_filename(remote_name) {
            Ok(UploadOutcome::UploadedAtomic)
        } else if matches!(resume_action(local_len, dest_len, part_len), ResumeAction::ResumeFrom(_))
        {
            Ok(UploadOutcome::Resumed)
        } else {
            Ok(UploadOutcome::Uploaded)
        }
    }

    fn send_remaining(&mut self, tmp_name: &str, local: &Path, offset: u64) -> Result<(), String> {
        // Prefer REST+STOR (resume at byte offset). If the console FTP does
        // not support REST, APPE the remainder. Last resort: send .part again.
        match self.stor_from(tmp_name, local, offset, true) {
            Ok(()) => Ok(()),
            Err(e) if e.contains("REST") => match self.appe_from(tmp_name, local, offset) {
                Ok(()) => Ok(()),
                Err(_) => self.stor_from(tmp_name, local, 0, false),
            },
            Err(e) => Err(e),
        }
    }

    fn rename_part(&mut self, tmp_name: &str, remote_name: &str) -> Result<(), String> {
        let (code, msg) = self.command(&format!("RNFR {tmp_name}"))?;
        if code >= 400 {
            return Err(format!("RNFR {tmp_name} failed: {code} {msg}"));
        }
        let (code, msg) = self.command(&format!("RNTO {remote_name}"))?;
        if code >= 400 {
            Err(format!(
                "could not replace {remote_name} via rename (left original in place): {code} {msg}"
            ))
        } else {
            Ok(())
        }
    }

    fn size_of(&mut self, name: &str) -> Result<Option<u64>, String> {
        let (code, msg) = self.command(&format!("SIZE {name}"))?;
        if code == 213 {
            let n = msg
                .split_whitespace()
                .find_map(|p| p.parse::<u64>().ok())
                .ok_or_else(|| format!("bad SIZE reply: {msg}"))?;
            Ok(Some(n))
        } else {
            // FSD/FileZilla-on-XeXMenu often lack SIZE. Skip rather than fail.
            Ok(None)
        }
    }

    fn stor_from(
        &mut self,
        name: &str,
        local: &Path,
        offset: u64,
        use_rest: bool,
    ) -> Result<(), String> {
        self.transfer("STOR", name, local, offset, use_rest && offset > 0)
    }

    fn appe_from(&mut self, name: &str, local: &Path, offset: u64) -> Result<(), String> {
        self.transfer("APPE", name, local, offset, false)
    }

    fn transfer(
        &mut self,
        cmd: &str,
        name: &str,
        local: &Path,
        offset: u64,
        use_rest: bool,
    ) -> Result<(), String> {
        let mut file = File::open(local).map_err(|e| format!("{}: {e}", local.display()))?;
        if offset > 0 {
            file.seek(SeekFrom::Start(offset))
                .map_err(|e| format!("seek {}: {e}", local.display()))?;
        }
        let mut data_stream = self.pasv_stream()?;
        if use_rest {
            let (code, msg) = self.command(&format!("REST {offset}"))?;
            if code >= 400 {
                drop(data_stream);
                return Err(format!("REST {offset} rejected: {code} {msg}"));
            }
        }
        self.writer
            .write_all(format!("{cmd} {name}\r\n").as_bytes())
            .map_err(|e| e.to_string())?;
        self.writer.flush().map_err(|e| e.to_string())?;
        let (code, msg) = self.read_response()?;
        if code >= 400 {
            return Err(format!("{cmd} {name} rejected: {code} {msg}"));
        }
        let mut buf = [0u8; 64 * 1024];
        loop {
            let n = file
                .read(&mut buf)
                .map_err(|e| format!("read {}: {e}", local.display()))?;
            if n == 0 {
                break;
            }
            data_stream.write_all(&buf[..n]).map_err(|e| {
                format!("transfer dropped while sending {name}: {e}")
            })?;
        }
        data_stream.flush().map_err(|e| e.to_string())?;
        drop(data_stream);
        let (code, msg) = self.read_response()?;
        if code >= 400 {
            Err(format!("{cmd} {name} failed: {code} {msg}"))
        } else {
            Ok(())
        }
    }

    pub fn close(self) {
        drop(self);
    }
}

impl Drop for FtpClient {
    fn drop(&mut self) {
        let _ = self.command("QUIT");
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UploadOutcome {
    Uploaded,
    UploadedAtomic,
    Resumed,
    AlreadyPresent,
    SkippedProtected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumeAction {
    SkipFinished,
    FinishRename,
    ResumeFrom(u64),
    Restart,
}

/// Decide whether a half-sent `.part` on the Xbox can be continued.
pub fn resume_action(local_len: u64, dest_len: Option<u64>, part_len: Option<u64>) -> ResumeAction {
    if dest_len == Some(local_len) {
        return ResumeAction::SkipFinished;
    }
    match part_len {
        Some(n) if n == local_len && local_len > 0 => ResumeAction::FinishRename,
        Some(n) if n > 0 && n < local_len => ResumeAction::ResumeFrom(n),
        _ => ResumeAction::Restart,
    }
}

pub fn looks_like_drop(err: &str) -> bool {
    let e = err.to_ascii_lowercase();
    e.contains("connection closed")
        || e.contains("dropped")
        || e.contains("timed out")
        || e.contains("timeout")
        || e.contains("broken pipe")
        || e.contains("connection reset")
        || e.contains("os error 10054")
        || e.contains("os error 104")
}

fn connect_timeout(addr: &str, timeout: Duration) -> Result<TcpStream, String> {
    let addrs: Vec<SocketAddr> = addr.to_socket_addrs().map_err(|e| e.to_string())?.collect();
    if addrs.is_empty() {
        return Err(format!("could not resolve {addr}"));
    }
    TcpStream::connect_timeout(&addrs[0], timeout).map_err(|e| format!("{addr}: {e}"))
}

fn xbox_path_candidates(path: &str) -> Vec<String> {
    let n = path.replace('\\', "/");
    let mut out = vec![n.clone()];
    if n.starts_with('/') {
        let trimmed = n.trim_start_matches('/').to_string();
        out.push(trimmed.clone());
        if let Some((vol, rest)) = trimmed.split_once('/') {
            out.push(format!("{vol}:\\{}", rest.replace('/', "\\")));
        }
    }
    out
}

fn parse_pasv(msg: &str, fallback_ip: IpAddr) -> Result<String, String> {
    let start = msg.find('(').ok_or_else(|| "bad PASV response".to_string())?;
    let end = msg.find(')').ok_or_else(|| "bad PASV response".to_string())?;
    if end <= start {
        return Err(format!("bad PASV response: {msg}"));
    }
    let inner = &msg[start + 1..end];
    let n: Vec<u32> = inner
        .split(',')
        .filter_map(|p| p.trim().parse().ok())
        .collect();
    if n.len() < 6 {
        return Err(format!("bad PASV tuple: {msg}"));
    }
    let advertised = Ipv4Addr::new(n[0] as u8, n[1] as u8, n[2] as u8, n[3] as u8);
    let ip = pasv_ip(advertised, fallback_ip);
    let port = n[4] * 256 + n[5];
    Ok(format!("{ip}:{port}"))
}

/// Aurora/FSD often advertise 0.0.0.0. Never follow a public PASV IP off-LAN.
fn pasv_ip(advertised: Ipv4Addr, fallback: IpAddr) -> IpAddr {
    if advertised.is_unspecified()
        || advertised.is_loopback()
        || advertised.is_broadcast()
        || !(advertised.is_private() || advertised.is_link_local())
    {
        match fallback {
            IpAddr::V4(v) if !v.is_unspecified() => fallback,
            IpAddr::V6(_) => fallback,
            _ => IpAddr::V4(advertised),
        }
    } else {
        IpAddr::V4(advertised)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pasv() {
        let addr = parse_pasv(
            "227 Entering Passive Mode (192,168,1,20,20,80)",
            IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)),
        )
        .unwrap();
        assert_eq!(addr, "192.168.1.20:5200");
    }

    #[test]
    fn pasv_zero_uses_control_ip() {
        let addr = parse_pasv(
            "227 Entering Passive Mode (0,0,0,0,20,80)",
            IpAddr::V4(Ipv4Addr::new(192, 168, 1, 20)),
        )
        .unwrap();
        assert_eq!(addr, "192.168.1.20:5200");
    }

    #[test]
    fn pasv_public_ip_uses_control_ip() {
        let addr = parse_pasv(
            "227 Entering Passive Mode (8,8,8,8,20,80)",
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5)),
        )
        .unwrap();
        assert_eq!(addr, "10.0.0.5:5200");
    }

    #[test]
    fn resume_picks_up_a_partial_part_file() {
        assert_eq!(
            resume_action(1000, None, Some(400)),
            ResumeAction::ResumeFrom(400)
        );
        assert_eq!(
            resume_action(1000, Some(1000), Some(400)),
            ResumeAction::SkipFinished
        );
        assert_eq!(
            resume_action(1000, None, Some(1000)),
            ResumeAction::FinishRename
        );
        assert_eq!(resume_action(1000, None, None), ResumeAction::Restart);
        assert_eq!(resume_action(1000, None, Some(0)), ResumeAction::Restart);
        assert!(looks_like_drop("transfer dropped while sending game.xex: reset"));
        assert!(!looks_like_drop("CWD /Hdd1 failed: 550"));
    }

    #[test]
    fn protected_upload_skips_without_flag() {
        assert!(paths::is_protected_filename("launch.ini"));
    }

    #[test]
    fn xbox_paths_include_drive_style() {
        let c = xbox_path_candidates("/Hdd1/Games");
        assert!(c.contains(&"/Hdd1/Games".into()));
        assert!(c.contains(&"Hdd1/Games".into()));
        assert!(c.iter().any(|p| p.contains("Hdd1:\\Games")));
    }
}
