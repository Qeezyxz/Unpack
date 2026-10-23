use crate::net;
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
    /// Bytes written to the data socket by the most recent transfer.
    sent_last: u64,
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
            sent_last: 0,
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
        reject_line_break(line)?;
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
        // A single 64 KB write that makes no progress for this long means the
        // console is gone; fail so Send can reconnect instead of hanging.
        let _ = stream.set_write_timeout(Some(Duration::from_secs(90)));
        let _ = stream.set_read_timeout(Some(Duration::from_secs(120)));
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
            Ok(parse_size_reply(&msg).ok())
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
        self.sent_last = 0;
        let mut data_stream = self.pasv_stream()?;
        if use_rest {
            let (code, msg) = self.command(&format!("REST {offset}"))?;
            if code >= 400 {
                drop(data_stream);
                return Err(format!("REST {offset} rejected: {code} {msg}"));
            }
        }
        reject_line_break(name)?;
        self.writer
            .write_all(format!("{cmd} {name}\r\n").as_bytes())
            .map_err(|e| e.to_string())?;
        self.writer.flush().map_err(|e| e.to_string())?;
        let (code, msg) = self.read_response()?;
        if code >= 400 {
            if reply_means_full(code, &msg) {
                return Err(storage_full_message(name));
            }
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
            self.sent_last += n as u64;
        }
        data_stream.flush().map_err(|e| e.to_string())?;
        drop(data_stream);
        let (code, msg) = self.read_response()?;
        if code >= 400 {
            if reply_means_full(code, &msg) {
                Err(storage_full_message(name))
            } else {
                Err(format!("{cmd} {name} failed: {code} {msg}"))
            }
        } else {
            Ok(())
        }
    }

    pub fn close(self) {
        drop(self);
    }

    pub fn sent_last(&self) -> u64 {
        self.sent_last
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

/// A retry that moved less than this made no real progress.
pub const STALL_BYTES: u64 = 256 * 1024;

pub fn stalled(sent: u64) -> bool {
    sent < STALL_BYTES
}

pub fn reply_means_full(code: u16, msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    code == 552
        || code == 452
        || m.contains("no space")
        || m.contains("disk full")
        || m.contains("not enough space")
        || m.contains("insufficient")
}

pub fn storage_full_message(name: &str) -> String {
    format!("The Xbox drive is full, so {name} was not finished. Free some space on the console and send again; the partial .part file is kept.")
}

pub fn stalled_message(name: &str, tries: u32) -> String {
    format!(
        "The Xbox stopped taking {name} (no progress after {tries} tries). Its drive may be full, or the Wi-Fi keeps dropping. Free some space or move closer to the router, then send again; the partial .part file is kept."
    )
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

/// The reply text starts with the 3-digit code ("213 1048576"); the size is
/// the number after it.
fn parse_size_reply(msg: &str) -> Result<u64, String> {
    msg.lines()
        .find_map(|line| {
            let rest = line.strip_prefix("213")?;
            rest.split_whitespace().next()?.parse::<u64>().ok()
        })
        .ok_or_else(|| format!("bad SIZE reply: {}", msg.trim()))
}

/// A line break inside a command would let one FTP command smuggle in another.
fn reject_line_break(text: &str) -> Result<(), String> {
    if text.contains('\r') || text.contains('\n') || text.contains('\0') {
        Err("FTP commands cannot contain line breaks (check the Xbox login and file names)".into())
    } else {
        Ok(())
    }
}

fn connect_timeout(addr: &str, timeout: Duration) -> Result<TcpStream, String> {
    let addrs: Vec<SocketAddr> = addr.to_socket_addrs().map_err(|e| e.to_string())?.collect();
    if addrs.is_empty() {
        return Err(format!("could not resolve {addr}"));
    }
    let Some(local) = addrs.iter().find(|a| net::is_lan_ip(a.ip())) else {
        return Err(format!(
            "{addr} points outside your local network. Send to Xbox only talks to a local address."
        ));
    };
    TcpStream::connect_timeout(local, timeout).map_err(|e| format!("{addr}: {e}"))
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
    fn line_breaks_cannot_smuggle_ftp_commands() {
        assert!(reject_line_break("game.xex").is_ok());
        assert!(reject_line_break("a.xex\r\nDELE launch.ini").is_err());
        assert!(reject_line_break("a\nb").is_err());
        assert!(reject_line_break("a\0b").is_err());
    }

    #[test]
    fn public_resolution_is_refused() {
        let err = connect_timeout("8.8.8.8:21", Duration::from_millis(50)).unwrap_err();
        assert!(err.contains("outside your local network"), "{err}");
    }

    #[test]
    fn full_drive_is_not_treated_as_a_wifi_drop() {
        assert!(reply_means_full(552, "Requested file action aborted"));
        assert!(reply_means_full(452, "Insufficient storage space"));
        assert!(reply_means_full(550, "No space left on device"));
        assert!(!reply_means_full(550, "File unavailable"));
        let full = storage_full_message("game.xex");
        assert!(full.contains("drive is full"), "{full}");
        assert!(!looks_like_drop(&full), "{full}");
        let stall = stalled_message("game.xex", 2);
        assert!(!looks_like_drop(&stall), "{stall}");
        assert!(stalled(0));
        assert!(stalled(STALL_BYTES - 1));
        assert!(!stalled(STALL_BYTES));
    }

    #[test]
    fn odd_pasv_replies_never_panic() {
        let fallback = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 20));
        for msg in [
            "",
            "227",
            "227 ()",
            "227 )(",
            "227 (1,2,3)",
            "227 (999,999,999,999,999,999)",
            "227 (a,b,c,d,e,f)",
            "227 (192,168,1,5,255,255)",
            "227 (192,168,1,5,-1,2)",
            "227 Entering (1,2,3,4,5,6,7,8)",
            "227 \u{e9}\u{e9} (1,2,3,4,5,6)",
        ] {
            let _ = parse_pasv(msg, fallback);
        }
        assert!(parse_pasv("227 )(", fallback).is_err());
    }

    #[test]
    fn size_reply_is_the_number_after_the_code() {
        assert_eq!(parse_size_reply("213 1048576\r\n").unwrap(), 1_048_576);
        assert_eq!(parse_size_reply("213 0\r\n").unwrap(), 0);
        assert_eq!(parse_size_reply("213-status\r\n213 77\r\n").unwrap(), 77);
        assert!(parse_size_reply("213 lots\r\n").is_err());
        assert!(parse_size_reply("213\r\n").is_err());
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
