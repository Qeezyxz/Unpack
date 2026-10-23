//! "Send to Xbox": a small FTP client for the FTP servers in Aurora / XeXMenu / DashLaunch on a
//! modded Xbox 360. Network only from Rust and only when the user clicks.
//!
//! Rules (see HANDOFF.md security model):
//! - Targets are IPv4 literals in private LAN ranges only (10/8, 172.16/12, 192.168/16,
//!   169.254/16) — the inverse of the downloader's SSRF rule. No host names, so no DNS.
//! - Passive mode only; the address in the 227 reply is ignored and the data connection always goes
//!   to the console's own IP (no FTP bounce to another host).
//! - One control connection at a time (Xbox servers often allow just one), data connections
//!   strictly sequential.
//! - No MLSD/MLST: listings come from LIST, parsed in both Unix (`drwxr-xr-x ...`) and DOS
//!   (`01-01-00 12:00AM <DIR> name`) formats. SIZE is used when supported, else the LIST size.
//! - Local sources: links/junctions are never followed (root refused, inner links skipped and
//!   reported). Remote names are checked against FATX rules (42 chars, no `"*+,/:;<=>?\|`).
//! - Uploads go to `<name>.part`, can resume with REST, are renamed into place, then the remote
//!   size is verified. An existing remote file is never overwritten: same size = skipped,
//!   different size = the whole job stops before anything is sent.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;

pub const PART: &str = ".part";
pub const FATX_MAX: usize = 42;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(6);
const IO_TIMEOUT: Duration = Duration::from_secs(60);
/// Upload chunk and socket send buffer: big writes keep the console's link busy.
pub const CHUNK: usize = 1 << 20;
/// Longest FTP reply line accepted.
pub const REPLY_LINE_MAX: usize = 8192;
/// Largest folder listing accepted; a bigger one is an error (it could hide a file).
pub const LIST_MAX: u64 = 16 << 20;
/// Bytes of a .part compared at its start and end before resuming it.
const SNDBUF: usize = 4 << 20;

/// True only for IPv4 private LAN / link-local addresses.
pub fn is_lan_ip(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    o[0] == 10 || (o[0] == 172 && (16..=31).contains(&o[1])) || (o[0] == 192 && o[1] == 168) || (o[0] == 169 && o[1] == 254 && o[2] != 0 && o[2] != 255)
}

/// Parse and check the console address. Only an IPv4 literal on the LAN is accepted.
pub fn check_target(host: &str, port: u16) -> Result<SocketAddr, String> {
    let host = host.trim();
    let ip: Ipv4Addr = host.parse().map_err(|_| format!("\"{}\" isn't an IP address. Enter the console's IP, like 192.168.1.50 (Aurora shows it on the main screen).", crate::safecopy::plain(host)))?;
    if port == 0 {
        return Err("Port must be 1-65535.".into());
    }
    let allow_loop = crate::download::allow_test_loopback() && ip.is_loopback();
    if !is_lan_ip(ip) && !allow_loop {
        return Err(format!("{ip} isn't a local network address. Send to Xbox only connects to consoles on your LAN (10.x, 172.16-31.x, 192.168.x, 169.254.x)."));
    }
    Ok(SocketAddr::new(IpAddr::V4(ip), port))
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
}

const MONTHS: [&str; 12] = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];

/// Parse one LIST line (Unix or DOS style). None for totals/garbage, "." and "..".
pub fn parse_list_line(line: &str) -> Option<Entry> {
    let line = line.trim_end_matches(['\r', '\n']);
    if line.trim().is_empty() || line.starts_with("total ") {
        return None;
    }
    let first = line.chars().next()?;
    let e = if "d-lbcps".contains(first) && line.len() > 10 && line[1..10].chars().all(|c| "rwxsStT-".contains(c)) {
        // Unix: perms links owner group size Mon DD (YYYY|HH:MM) name — owner/group may be missing.
        let toks: Vec<(usize, &str)> = tokens(line);
        let mi = toks.iter().position(|(_, t)| MONTHS.contains(&t.to_ascii_lowercase().as_str()))?;
        if mi < 2 || toks.len() < mi + 4 {
            return None;
        }
        let size = toks[mi - 1].1.parse().ok()?;
        let name = &line[toks[mi + 3].0..];
        let name = if first == 'l' { name.split(" -> ").next().unwrap_or(name) } else { name };
        Entry { name: name.to_string(), is_dir: first == 'd', size }
    } else {
        // DOS: MM-DD-YY  HH:MMAM  <DIR>|size  name
        let toks = tokens(line);
        if toks.len() < 4 || !toks[0].1.contains('-') || !toks[1].1.contains(':') {
            return None;
        }
        let name = &line[toks[3].0..];
        if toks[2].1.eq_ignore_ascii_case("<DIR>") {
            Entry { name: name.to_string(), is_dir: true, size: 0 }
        } else {
            Entry { name: name.to_string(), is_dir: false, size: toks[2].1.replace(',', "").parse().ok()? }
        }
    };
    if e.name.is_empty() || e.name == "." || e.name == ".." || e.name.contains('/') {
        return None;
    }
    Some(e)
}

fn tokens(s: &str) -> Vec<(usize, &str)> {
    let mut v = vec![];
    let mut start = None;
    for (i, c) in s.char_indices() {
        match (c == ' ' || c == '\t', start) {
            (true, Some(st)) => {
                v.push((st, &s[st..i]));
                start = None;
            }
            (false, None) => start = Some(i),
            _ => {}
        }
    }
    if let Some(st) = start {
        v.push((st, &s[st..]));
    }
    v
}

/// One FATX-safe remote name segment, or why not.
pub fn check_segment(s: &str) -> Result<(), String> {
    if s.is_empty() || s == "." || s == ".." {
        return Err(format!("\"{s}\" isn't a valid name on the Xbox."));
    }
    if s.chars().any(|c| (c as u32) < 32 || c as u32 > 126 || "\"*+,/:;<=>?\\|".contains(c)) {
        return Err(format!("\"{s}\" has characters the Xbox drive can't store (only plain letters, digits and simple symbols; not \" * + , / : ; < = > ? \\ |)."));
    }
    if s.len() > FATX_MAX {
        return Err(format!("\"{s}\" is longer than 42 characters, the Xbox drive limit. Rename it first."));
    }
    Ok(())
}

/// Turn any display name into one FATX-safe segment (bad characters become "_", 42 max).
pub fn fatx_name(s: &str) -> String {
    let mut n: String = s.chars().map(|c| if (c as u32) < 32 || c as u32 > 126 || "\"*+,/:;<=>?\\|".contains(c) { '_' } else { c }).collect();
    n.truncate(FATX_MAX);
    let n = n.trim_matches([' ', '.']).to_string();
    if n.is_empty() || n == "." || n == ".." { "Mod".into() } else { n }
}

/// Normalize a remote path like "/Hdd1/Games" or "Hdd1\\Games" into checked segments.
pub fn split_remote(p: &str) -> Result<Vec<String>, String> {
    let segs: Vec<String> = p.split(['/', '\\']).filter(|s| !s.is_empty()).map(|s| s.to_string()).collect();
    for s in &segs {
        check_segment(s)?;
    }
    Ok(segs)
}

pub fn join_remote(segs: &[String]) -> String {
    format!("/{}", segs.join("/"))
}

/// Name used while uploading (fits the 42-char limit).
pub fn part_name(name: &str) -> String {
    if name.len() + PART.len() <= FATX_MAX {
        format!("{name}{PART}")
    } else {
        let mut cut = FATX_MAX - PART.len() - 1;
        while !name.is_char_boundary(cut) {
            cut -= 1;
        }
        format!("{}~{PART}", &name[..cut])
    }
}

pub struct Ftp {
    r: BufReader<TcpStream>,
    w: TcpStream,
    ip: IpAddr,
    pub log: Vec<String>,
    /// Set once SIZE answered 213 on this connection: from then on a 550 means "no such file".
    size_ok: bool,
}

fn io_err(e: std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => "The console stopped answering (timed out). Check it's on and the FTP server is running.".into(),
        std::io::ErrorKind::ConnectionRefused => "The console refused the connection. Is the FTP server on (Aurora: Settings > Network > FTP)?".into(),
        std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted | std::io::ErrorKind::UnexpectedEof => {
            "The connection to the console dropped. Check the Wi-Fi or cable and that the console is still on, then send again; the partial .part file is kept and the upload resumes.".into()
        }
        _ => format!("Network error: {e}"),
    }
}

/// A reply (or text) that means the console's drive has no room left.
pub fn reply_means_full(code: u32, msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    code == 552 || code == 452 || m.contains("no space") || m.contains("disk full") || m.contains("not enough space") || m.contains("insufficient")
}

pub fn storage_full_message(name: &str) -> String {
    format!("The Xbox drive is full, so {name} was not finished. Free some space on the console and send again; the partial .part file is kept.")
}

impl Ftp {
    /// Connect, retrying a few times on "421 too many connections": Xbox servers allow one
    /// connection and free the previous one a moment after it closes.
    pub fn connect(addr: SocketAddr, user: &str, pass: &str) -> Result<Ftp, String> {
        let mut last = String::new();
        for i in 0..6 {
            match Self::connect_once(addr, user, pass) {
                Err(e) if e.starts_with("421") => {
                    last = e;
                    std::thread::sleep(Duration::from_millis(300 + 200 * i));
                }
                r => return r,
            }
        }
        Err(format!("The console's FTP server is busy with another connection (only one is allowed). Close other FTP apps and try again. ({last})"))
    }

    pub fn connect_once(addr: SocketAddr, user: &str, pass: &str) -> Result<Ftp, String> {
        if user.contains(['\r', '\n']) || pass.contains(['\r', '\n']) {
            return Err("User name and password can't contain line breaks.".into());
        }
        let s = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT).map_err(io_err)?;
        // Short wait for the greeting: a console that allows one connection may accept a second
        // TCP connection and never greet it.
        s.set_read_timeout(Some(CONNECT_TIMEOUT)).ok();
        s.set_write_timeout(Some(IO_TIMEOUT)).ok();
        s.set_nodelay(true).ok();
        let w = s.try_clone().map_err(io_err)?;
        let mut f = Ftp { r: BufReader::new(s), w, ip: addr.ip(), log: vec![], size_ok: false };
        let (c, t) = f.reply()?;
        f.w.set_read_timeout(Some(IO_TIMEOUT)).ok();
        if c == 421 {
            return Err(format!("421 {t}"));
        }
        if c != 220 {
            return Err(format!("Not an FTP server greeting: {c} {t}"));
        }
        let (c, t) = f.cmd(&format!("USER {user}"))?;
        let (c, t) = if c == 331 { f.cmd(&format!("PASS {pass}"))? } else { (c, t) };
        if c != 230 && c != 202 {
            let _ = f.cmd("QUIT");
            return Err(if c == 530 { "Login refused. Check the user name and password (Aurora's default is xboxftp / xboxftp).".to_string() } else { format!("Login failed: {c} {t}") });
        }
        let (c, t) = f.cmd("TYPE I")?;
        if c != 200 {
            return Err(format!("Server refused binary mode: {c} {t}"));
        }
        Ok(f)
    }

    /// One reply line, at most REPLY_LINE_MAX bytes (a device that never sends a newline can't
    /// make Unpack buffer without limit).
    fn read_line_capped(&mut self, line: &mut String) -> Result<usize, String> {
        let mut raw = Vec::new();
        let n = (&mut self.r).take(REPLY_LINE_MAX as u64).read_until(b'\n', &mut raw).map_err(io_err)?;
        if n == REPLY_LINE_MAX && raw.last() != Some(&b'\n') {
            return Err("FTP reply too long.".into());
        }
        line.push_str(&String::from_utf8_lossy(&raw));
        Ok(n)
    }

    fn reply(&mut self) -> Result<(u32, String), String> {
        let mut line = String::new();
        if self.read_line_capped(&mut line)? == 0 {
            return Err("The console closed the connection.".into());
        }
        let first = line.trim_end().to_string();
        let code: u32 = first.get(..3).and_then(|c| c.parse().ok()).ok_or_else(|| format!("Bad FTP reply: {first}"))?;
        let mut text = first.get(4..).unwrap_or("").to_string();
        if first.as_bytes().get(3) == Some(&b'-') {
            let end = format!("{code} ");
            let mut n = 0;
            loop {
                line.clear();
                if self.read_line_capped(&mut line)? == 0 {
                    return Err("The console closed the connection.".into());
                }
                let l = line.trim_end();
                if l.starts_with(&end) || l == code.to_string() {
                    break;
                }
                n += 1;
                if n > 1000 {
                    return Err("FTP reply too long.".into());
                }
                text.push('\n');
                text.push_str(l);
            }
        }
        if self.log.len() < 200 {
            self.log.push(format!("< {code} {}", text.lines().next().unwrap_or("")));
        }
        Ok((code, text))
    }

    pub fn cmd(&mut self, c: &str) -> Result<(u32, String), String> {
        if c.contains(['\r', '\n']) {
            return Err("Bad characters in FTP command.".into());
        }
        if self.log.len() < 200 {
            self.log.push(format!("> {}", if c.starts_with("PASS ") { "PASS ****" } else { c }));
        }
        self.w.write_all(format!("{c}\r\n").as_bytes()).map_err(io_err)?;
        self.reply()
    }

    /// PASV; the host part of the reply is ignored, the console's own IP is used.
    fn pasv(&mut self) -> Result<TcpStream, String> {
        let (c, t) = self.cmd("PASV")?;
        if c != 227 {
            return Err(format!("Server refused passive mode: {c} {t}"));
        }
        let inner = t.split('(').nth(1).and_then(|s| s.split(')').next()).map(|s| s.to_string()).unwrap_or_else(|| t.chars().skip_while(|c| !c.is_ascii_digit()).collect());
        let nums: Vec<u16> = inner.split(',').filter_map(|s| s.trim().parse().ok()).collect();
        if nums.len() != 6 || nums[4] > 255 || nums[5] > 255 {
            return Err(format!("Couldn't read the passive reply: {t}"));
        }
        let port = nums[4] * 256 + nums[5];
        if port == 0 {
            return Err("Bad passive port 0.".into());
        }
        let s = TcpStream::connect_timeout(&SocketAddr::new(self.ip, port), CONNECT_TIMEOUT).map_err(io_err)?;
        s.set_read_timeout(Some(IO_TIMEOUT)).ok();
        s.set_write_timeout(Some(IO_TIMEOUT)).ok();
        s.set_nodelay(true).ok();
        let _ = socket2::SockRef::from(&s).set_send_buffer_size(SNDBUF);
        Ok(s)
    }

    pub fn cwd(&mut self, segs: &[String]) -> Result<bool, String> {
        let (c, _) = self.cmd(&format!("CWD {}", join_remote(segs)))?;
        Ok(c / 100 == 2)
    }

    /// List a folder: CWD into it then plain LIST (Xbox servers handle that best).
    pub fn list(&mut self, segs: &[String]) -> Result<Vec<Entry>, String> {
        if !self.cwd(segs)? {
            return Err(format!("Folder {} doesn't exist on the console.", join_remote(segs)));
        }
        let mut d = self.pasv()?;
        let (c, t) = self.cmd("LIST")?;
        if c != 150 && c != 125 {
            return Err(format!("LIST failed: {c} {t}"));
        }
        let mut buf = vec![];
        (&mut d).take(LIST_MAX + 1).read_to_end(&mut buf).map_err(io_err)?;
        if buf.len() as u64 > LIST_MAX {
            return Err(format!("The listing of {} is larger than 16 MB; Unpack can't check it for existing files.", join_remote(segs)));
        }
        drop(d);
        let (c, t) = self.reply()?;
        if c / 100 != 2 {
            return Err(format!("LIST failed: {c} {t}"));
        }
        let text = String::from_utf8_lossy(&buf);
        let mut v: Vec<Entry> = text.lines().filter_map(parse_list_line).collect();
        v.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then(a.name.to_lowercase().cmp(&b.name.to_lowercase())));
        v.dedup_by(|a, b| a.name == b.name);
        Ok(v)
    }

    /// Size of a remote file: SIZE if supported, else from the parent's LIST. None = not there.
    pub fn size_of(&mut self, segs: &[String]) -> Result<Option<u64>, String> {
        let (c, t) = self.cmd(&format!("SIZE {}", join_remote(segs)))?;
        if c == 213 {
            if let Ok(n) = t.trim().parse() {
                self.size_ok = true;
                return Ok(Some(n));
            }
        }
        if c == 550 && self.size_ok {
            return Ok(None);
        }
        let (name, parent) = segs.split_last().ok_or("empty path")?;
        // SIZE unsupported / refused: the parent's LIST decides. A LIST failure is an error,
        // never "not there" (Overseer R10 HIGH-1).
        if !self.cwd(parent)? {
            return Ok(None);
        }
        let l = self.list(parent)?;
        Ok(l.into_iter().find(|e| e.name.eq_ignore_ascii_case(name)).map(|e| if e.is_dir { u64::MAX } else { e.size }))
    }

    /// Create every folder of `segs` that's missing. `known` holds folders already seen to exist
    /// (from the plan's listings or earlier MKDs), so a batch makes each folder once and doesn't
    /// re-check folders per file.
    pub fn mkdirs(&mut self, segs: &[String], known: &mut std::collections::HashSet<Vec<String>>) -> Result<(), String> {
        let have = (0..=segs.len()).rev().find(|&i| i == 0 || known.contains(&segs[..i])).unwrap_or(0);
        for i in have + 1..=segs.len() {
            let p = &segs[..i];
            if i == 1 {
                if !self.cwd(p)? {
                    return Err(format!("Drive {} isn't on the console (try Hdd1 or Usb0).", segs[0]));
                }
            } else {
                let (c, t) = self.cmd(&format!("MKD {}", join_remote(p)))?;
                if c / 100 != 2 && !self.cwd(p)? {
                    return Err(format!("Couldn't create {}: {c} {t}", join_remote(p)));
                }
            }
            known.insert(p.to_vec());
        }
        Ok(())
    }

    /// STOR `local` (from byte `offset`, using REST) to `remote`.
    /// Returns the offset really used (0 when the server refused REST).
    pub fn stor(&mut self, local: &Path, offset: u64, remote: &[String], progress: &mut dyn FnMut(u64), cancelled: &dyn Fn() -> bool) -> Result<u64, String> {
        let mut f = open_no_follow(local)?;
        let mut offset = offset;
        if offset > 0 {
            let (c, _) = self.cmd(&format!("REST {offset}"))?;
            if c != 350 {
                offset = 0; // server can't resume; .part is ours, start it over
            }
        }
        use std::io::Seek;
        f.seek(std::io::SeekFrom::Start(offset)).map_err(|e| e.to_string())?;
        progress(offset);
        let mut d = self.pasv()?;
        let (c, t) = self.cmd(&format!("STOR {}", join_remote(remote)))?;
        if c != 150 && c != 125 {
            if reply_means_full(c, &t) {
                return Err(storage_full_message(&remote.last().cloned().unwrap_or_default()));
            }
            return Err(format!("Upload refused for {}: {c} {t}", join_remote(remote)));
        }
        let mut buf = vec![0u8; CHUNK];
        loop {
            if cancelled() {
                // Close the data connection cleanly and read the server's reply, so the control
                // connection stays usable to ask how much of the .part arrived (resume record).
                let _ = d.shutdown(std::net::Shutdown::Write);
                drop(d);
                let _ = self.reply();
                return Err(crate::archive::ERR_CANCELLED.into());
            }
            let k = f.read(&mut buf).map_err(|e| format!("Read failed: {e}"))?;
            if k == 0 {
                break;
            }
            if let Err(e) = d.write_all(&buf[..k]) {
                // A full drive makes the server close the data connection and say why on the
                // control connection; look before calling it a network drop.
                let _ = self.r.get_ref().set_read_timeout(Some(Duration::from_secs(3)));
                let said = self.reply();
                let _ = self.r.get_ref().set_read_timeout(Some(IO_TIMEOUT));
                if let Ok((c, t)) = said {
                    if reply_means_full(c, &t) {
                        return Err(storage_full_message(&remote.last().cloned().unwrap_or_default()));
                    }
                }
                return Err(io_err(e));
            }
            progress(k as u64);
        }
        d.flush().ok();
        let _ = d.shutdown(std::net::Shutdown::Write);
        drop(d);
        let (c, t) = self.reply()?;
        if c / 100 != 2 {
            if reply_means_full(c, &t) {
                return Err(storage_full_message(&remote.last().cloned().unwrap_or_default()));
            }
            return Err(format!("Upload failed for {}: {c} {t}", join_remote(remote)));
        }
        Ok(offset)
    }

    /// Download at most `max` bytes of a remote file (whole file if smaller). `whole`: error if
    /// the file is larger than `max` instead of returning its start.
    pub fn retr(&mut self, segs: &[String], max: usize, whole: bool) -> Result<Vec<u8>, String> {
        let mut d = self.pasv()?;
        let (c, t) = self.cmd(&format!("RETR {}", join_remote(segs)))?;
        if c != 150 && c != 125 {
            return Err(format!("Couldn't read {}: {c} {t}", join_remote(segs)));
        }
        let mut buf = vec![];
        (&mut d).take(max as u64 + 1).read_to_end(&mut buf).map_err(io_err)?;
        let more = buf.len() > max;
        buf.truncate(max);
        if more {
            // Stop the rest of the stream (reset, not a slow drain).
            let _ = socket2::SockRef::from(&d).set_linger(Some(Duration::ZERO));
            let _ = d.shutdown(std::net::Shutdown::Both);
        }
        drop(d);
        let _ = self.reply()?;
        if more && whole {
            return Err(format!("{} is larger than {} bytes.", join_remote(segs), max));
        }
        Ok(buf)
    }

    /// Delete a remote file. Ok(false) when the server refused (e.g. it's not there).
    pub fn dele(&mut self, segs: &[String]) -> Result<bool, String> {
        let (c, _) = self.cmd(&format!("DELE {}", join_remote(segs)))?;
        Ok(c / 100 == 2)
    }

    pub fn rename(&mut self, from: &[String], to: &[String]) -> Result<(), String> {
        let (c, t) = self.cmd(&format!("RNFR {}", join_remote(from)))?;
        if c != 350 {
            return Err(format!("The console's FTP server can't rename files ({c} {t}). The upload is kept as {}.", join_remote(from)));
        }
        let (c, t) = self.cmd(&format!("RNTO {}", join_remote(to)))?;
        if c / 100 != 2 {
            return Err(format!("Rename to {} failed: {c} {t}", join_remote(to)));
        }
        Ok(())
    }

    pub fn quit(mut self) {
        self.close();
    }
    /// Say QUIT (best effort) and shut the socket, so a one-connection server accepts the next
    /// login. Later commands on this connection fail. Safe to call twice.
    pub fn close(&mut self) {
        let _ = self.w.set_write_timeout(Some(std::time::Duration::from_secs(2)));
        let _ = self.w.set_read_timeout(Some(std::time::Duration::from_secs(2)));
        let _ = self.cmd("QUIT");
        let _ = self.w.shutdown(std::net::Shutdown::Both);
    }
}

/// Open a local file without following a link at the final component.
pub(crate) fn open_no_follow(p: &Path) -> Result<std::fs::File, String> {
    let m = std::fs::symlink_metadata(p).map_err(|e| format!("Cannot open {}: {e}", p.display()))?;
    if crate::safecopy::is_link(&m) || !m.is_file() {
        return Err(format!("{} is a link or not a file; not sending it.", p.display()));
    }
    let mut o = std::fs::OpenOptions::new();
    o.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.custom_flags(0o400000); // O_NOFOLLOW
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        o.custom_flags(0x0020_0000); // FILE_FLAG_OPEN_REPARSE_POINT
    }
    let f = o.open(p).map_err(|e| format!("Cannot open {}: {e}", p.display()))?;
    let m2 = f.metadata().map_err(|e| e.to_string())?;
    if crate::safecopy::is_link(&m2) || !m2.is_file() {
        return Err(format!("{} changed into a link; not sending it.", p.display()));
    }
    Ok(f)
}

#[derive(Debug, Serialize, Default)]
pub struct SendResult {
    pub dest: String,
    pub sent: usize,
    pub resumed: usize,
    pub skipped: usize,
    pub links_skipped: usize,
    pub bytes: u64,
    /// Connections used for the transfer (1, or 2 when parallel was allowed by the console).
    pub connections: usize,
    pub seconds: f64,
}

/// A local file to send: path and remote path segments relative to the destination folder.
struct PlanFile {
    local: PathBuf,
    rel: Vec<String>,
    len: u64,
}

/// Walk `src` (file or folder) without following links. A folder keeps its own name unless
/// `contents_only`. Every remote name is FATX-checked before anything is sent.
fn plan_local(src: &Path, contents_only: bool) -> Result<(Vec<PlanFile>, usize), String> {
    let m = std::fs::symlink_metadata(src).map_err(|e| format!("Cannot open {}: {e}", src.display()))?;
    if crate::safecopy::is_link(&m) {
        return Err(format!("{} is a link or junction; Unpack doesn't follow links.", src.display()));
    }
    let name = src.file_name().map(|s| s.to_string_lossy().into_owned()).ok_or("bad name")?;
    if m.is_file() {
        check_segment(&name)?;
        return Ok((vec![PlanFile { local: src.to_path_buf(), rel: vec![name], len: m.len() }], 0));
    }
    let base: Vec<String> = if contents_only { vec![] } else { check_segment(&name).map(|_| vec![name])? };
    let mut files = vec![];
    let mut links = 0;
    let mut st = vec![(src.to_path_buf(), base)];
    while let Some((dir, rel)) = st.pop() {
        if rel.len() > 32 {
            return Err("Folder is nested too deeply.".into());
        }
        let rd = std::fs::read_dir(&dir).map_err(|e| format!("Cannot read {}: {e}", dir.display()))?;
        for e in rd.flatten() {
            let p = e.path();
            let m = std::fs::symlink_metadata(&p).map_err(|e| e.to_string())?;
            let n = e.file_name().to_string_lossy().into_owned();
            if crate::safecopy::is_link(&m) {
                links += 1;
                continue;
            }
            check_segment(&n).map_err(|e| format!("{e} ({})", p.display()))?;
            let mut r = rel.clone();
            r.push(n);
            if m.is_dir() {
                st.push((p, r));
            } else if m.is_file() {
                if files.len() >= 200_000 {
                    return Err("Too many files.".into());
                }
                files.push(PlanFile { local: p, rel: r, len: m.len() });
            }
        }
    }
    files.sort_by(|a, b| a.rel.cmp(&b.rel));
    Ok((files, links))
}

#[derive(Clone)]
pub struct Console {
    pub addr: SocketAddr,
    pub user: String,
    pub pass: String,
    /// Folder for local resume records (app data). None: never resume, always restart.
    pub resume_dir: Option<PathBuf>,
}

static RESUME_DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
/// Set once at startup to `<app data>/ftp-resume`.
pub fn set_resume_dir(p: PathBuf) { let _ = RESUME_DIR.set(p); }
pub fn resume_dir() -> Option<PathBuf> { RESUME_DIR.get().cloned() }

/// Local record of an interrupted upload (R11). Kept on the PC, never trusted from the console:
/// the source's identity plus the SHA-256 of the bytes the console confirmed it holds.
#[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
struct ResumeRecord {
    local: String,
    len: u64,
    mtime_ns: u128,
    offset: u64,
    sha256: String,
}

fn record_path(con_dir: &Path, addr: SocketAddr, part: &[String]) -> PathBuf {
    use sha2::{Digest, Sha256};
    let key = Sha256::digest(format!("{addr}|{}", join_remote(part)).as_bytes());
    con_dir.join(format!("{}.json", key.iter().map(|b| format!("{b:02x}")).collect::<String>()))
}

fn local_ident(local: &Path) -> Option<(String, u64, u128)> {
    let m = std::fs::symlink_metadata(local).ok()?;
    let t = m.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos();
    Some((local.to_string_lossy().into_owned(), m.len(), t))
}

/// SHA-256 of the first `n` bytes of `local` (hex), read without following links.
fn prefix_sha(local: &Path, n: u64) -> Option<String> {
    use sha2::{Digest, Sha256};
    let f = open_no_follow(local).ok()?;
    let mut h = Sha256::new();
    let mut r = f.take(n);
    let mut buf = vec![0u8; 1 << 20];
    let mut got = 0u64;
    loop {
        let k = r.read(&mut buf).ok()?;
        if k == 0 { break; }
        h.update(&buf[..k]);
        got += k as u64;
    }
    (got == n).then(|| h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// After a failed/cancelled upload: ask the console how much of the .part it holds and record
/// it with the hash of exactly those local bytes. Best effort; no record = restart next time.
fn save_record(ftp: &mut Ftp, dir: &Path, addr: SocketAddr, part: &[String], local: &Path) {
    let _ = (|| -> Option<()> {
        let have = ftp.size_of(part).ok()??;
        let (l, len, mtime_ns) = local_ident(local)?;
        if have == 0 || have == u64::MAX || have >= len { return None; }
        let rec = ResumeRecord { local: l, len, mtime_ns, offset: have, sha256: prefix_sha(local, have)? };
        std::fs::create_dir_all(dir).ok()?;
        let p = record_path(dir, addr, part);
        let tmp = p.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_vec(&rec).ok()?).ok()?;
        std::fs::rename(&tmp, &p).ok()
    })();
}

/// Resume only when the record matches the source now (path, size, mtime, hash of the prefix)
/// and the console's .part is exactly the recorded size. Anything else: start from 0.
fn record_allows(dir: &Path, addr: SocketAddr, part: &[String], local: &Path, remote_have: u64) -> bool {
    let Ok(b) = std::fs::read(record_path(dir, addr, part)) else { return false };
    let Ok(rec) = serde_json::from_slice::<ResumeRecord>(&b) else { return false };
    let Some((l, len, mtime_ns)) = local_ident(local) else { return false };
    rec.local == l && rec.len == len && rec.mtime_ns == mtime_ns && rec.offset == remote_have && remote_have > 0 && prefix_sha(local, rec.offset).as_deref() == Some(rec.sha256.as_str())
}

fn drop_record(dir: &Path, addr: SocketAddr, part: &[String]) {
    let _ = std::fs::remove_file(record_path(dir, addr, part));
}

struct Job<'a> {
    pf: &'a PlanFile,
    full: Vec<String>,
    parent: Vec<String>,
    part: String,
    offset: u64,
}

enum Msg {
    Bytes(u64, String),
    Done { resumed: bool, sent: u64 },
    Fail(String),
}

/// Upload one planned file on `ftp`: STOR (REST-resumed) to .part, rename, verify the final size.
/// Returns the resume offset really used.
fn send_one(ftp: &mut Ftp, addr: SocketAddr, rd: Option<&Path>, j: &Job, known: &std::sync::Mutex<std::collections::HashSet<Vec<String>>>, tx: &std::sync::mpsc::Sender<Msg>, stop: &std::sync::atomic::AtomicBool) -> Result<u64, String> {
    {
        let mut k = known.lock().unwrap_or_else(|p| p.into_inner()).clone();
        ftp.mkdirs(&j.parent, &mut k)?;
        known.lock().unwrap_or_else(|p| p.into_inner()).extend(k);
    }
    let mut part_segs = j.parent.clone();
    part_segs.push(j.part.clone());
    let label = j.pf.rel.join("/");
    let offset = match rd { Some(d) if j.offset > 0 && record_allows(d, addr, &part_segs, &j.pf.local, j.offset) => j.offset, _ => 0 };
    // The first callback is the resume offset: counted as done, not as speed.
    let mut first = true;
    let used = match ftp.stor(&j.pf.local, offset, &part_segs, &mut |k| { let l = if std::mem::take(&mut first) { String::new() } else { label.clone() }; let _ = tx.send(Msg::Bytes(k, l)); }, &|| stop.load(std::sync::atomic::Ordering::Relaxed)) {
        Ok(u) => u,
        Err(e) => {
            match rd { Some(d) => save_record(ftp, d, addr, &part_segs, &j.pf.local), None => {} }
            return Err(e);
        }
    };
    if let Some(d) = rd { drop_record(d, addr, &part_segs); }
    // Right before the rename: the final name must still be free (it may have appeared since
    // the plan; many servers let RNTO replace a file).
    if let Some(n) = ftp.size_of(&j.full)? {
        return Err(format!("{} appeared on the console during the send ({} bytes); not replacing it. Your upload is kept as {}.", join_remote(&j.full), if n == u64::MAX { 0 } else { n }, join_remote(&part_segs)));
    }
    ftp.rename(&part_segs, &j.full)?;
    // Verify after RNTO too: the size must be ours. A file created by someone else in the gap
    // between the check above and RNTO may have been replaced (FTP has no no-replace rename);
    // see HANDOFF "Known limits".
    match ftp.size_of(&j.full)? {
        Some(n) if n == j.pf.len => Ok(used),
        Some(n) => Err(format!("Size check failed for {} after the rename: console has {n} bytes, PC has {}.", join_remote(&j.full), j.pf.len)),
        None => Err(format!("Uploaded {} but the console doesn't list it.", join_remote(&j.full))),
    }
}

fn fmt_eta(secs: u64) -> String {
    if secs >= 3600 { format!("{}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60) } else { format!("{}:{:02}", secs / 60, secs % 60) }
}

/// Send `src` into remote folder `dest` (segments). Plans everything, refuses conflicts before
/// sending, resumes `.part` files, renames into place and verifies every remote size.
/// `parallel`: try a second connection (2 transfers at once); falls back to 1 if the console
/// refuses it. The progress label carries live MB/s and ETA.
pub fn send(con: &Console, src: &Path, dest: &[String], contents_only: bool, parallel: bool, progress: &mut dyn FnMut(u32, &str), cancelled: &dyn Fn() -> bool) -> Result<SendResult, String> {
    if dest.is_empty() {
        return Err("Pick a drive on the console first (like Hdd1).".into());
    }
    for s in dest {
        check_segment(s)?;
    }
    let (files, links) = plan_local(src, contents_only)?;
    if files.is_empty() {
        return Err("Nothing to send (no files, links are skipped).".into());
    }
    let mut ftp = Ftp::connect(con.addr, &con.user, &con.pass)?;
    // Plan against the console: one LIST per remote folder, nothing per file.
    let mut dir_cache: std::collections::HashMap<Vec<String>, Option<Vec<Entry>>> = Default::default();
    let mut known: std::collections::HashSet<Vec<String>> = Default::default();
    let mut todo = vec![];
    let mut res = SendResult { dest: join_remote(dest), links_skipped: links, ..Default::default() };
    for pf in &files {
        let mut full = dest.to_vec();
        full.extend(pf.rel.iter().cloned());
        let (name, parent) = full.split_last().unwrap();
        let parent = parent.to_vec();
        if !dir_cache.contains_key(&parent) {
            // Only list folders whose parent exists (or is unknown); a missing parent means
            // everything under it is new.
            let missing_parent = parent.len() > 1 && matches!(dir_cache.get(&parent[..parent.len() - 1]), Some(None));
            // Missing only when CWD says so; any other LIST failure stops the job (R10 HIGH-1).
            let l = if missing_parent || !ftp.cwd(&parent)? { None } else { Some(ftp.list(&parent).map_err(|e| format!("Couldn't list {} to check for existing files, so nothing was sent: {e}", join_remote(&parent)))?) };
            if l.is_some() {
                for i in 1..=parent.len() { known.insert(parent[..i].to_vec()); }
            }
            dir_cache.insert(parent.clone(), l);
        }
        let listing = dir_cache[&parent].as_deref().unwrap_or(&[]);
        let find = |n: &str| listing.iter().find(|e| e.name.eq_ignore_ascii_case(n));
        match find(name) {
            Some(e) if e.is_dir => return Err(format!("{} is a folder on the console; nothing was sent.", join_remote(&full))),
            Some(e) if e.size == pf.len => {
                res.skipped += 1;
                continue;
            }
            Some(e) => return Err(format!("{} already exists on the console with a different size ({} vs {} bytes). Nothing was sent; delete or rename it on the console first.", join_remote(&full), e.size, pf.len)),
            None => {}
        }
        let part = part_name(name);
        let offset = match find(&part) {
            Some(e) if !e.is_dir && e.size < pf.len => e.size,
            Some(e) if e.is_dir => return Err(format!("{}/{part} is a folder on the console; nothing was sent.", join_remote(&parent))),
            _ => 0,
        };
        todo.push(Job { pf, full: full.clone(), parent, part, offset });
    }
    if todo.is_empty() {
        ftp.quit();
        return Ok(res);
    }
    // Optional second connection; many Xbox servers allow only one, so a refusal is normal.
    let mut conns = vec![ftp];
    if parallel && todo.len() > 1 {
        if let Ok(f2) = Ftp::connect_once(con.addr, &con.user, &con.pass) {
            conns.push(f2);
        }
    }
    let total: u64 = todo.iter().map(|t| t.pf.len).sum::<u64>().max(1);
    let next = std::sync::atomic::AtomicUsize::new(0);
    let stop = std::sync::atomic::AtomicBool::new(false);
    let known = std::sync::Mutex::new(known);
    let (tx, rx) = std::sync::mpsc::channel::<Msg>();
    let todo = &todo;
    let mut first_err: Option<String> = None;
    // A console that allowed the 2nd login may still refuse its data connections (425/421):
    // that worker hands its file back and retires, the other carries on.
    let retry = std::sync::Mutex::new(Vec::<usize>::new());
    let alive = std::sync::atomic::AtomicUsize::new(conns.len());
    let used = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|sc| {
        for mut f in conns {
            let (tx, next, stop, known, retry, alive, used) = (tx.clone(), &next, &stop, &known, &retry, &alive, &used);
            sc.spawn(move || {
                use std::sync::atomic::Ordering::Relaxed;
                let mut did_one = false;
                loop {
                    if stop.load(Relaxed) { break; }
                    let i = match retry.lock().unwrap_or_else(|p| p.into_inner()).pop() { Some(i) => i, None => next.fetch_add(1, Relaxed) };
                    let Some(j) = todo.get(i) else { break };
                    match send_one(&mut f, con.addr, con.resume_dir.as_deref(), j, known, &tx, stop) {
                        Ok(off) => {
                            if !did_one { did_one = true; used.fetch_add(1, Relaxed); }
                            let _ = tx.send(Msg::Done { resumed: off > 0, sent: j.pf.len - off });
                        }
                        Err(e) if (e.contains(": 425") || e.contains(": 421")) && alive.load(Relaxed) > 1 => {
                            retry.lock().unwrap_or_else(|p| p.into_inner()).push(i);
                            break;
                        }
                        Err(e) => { stop.store(true, Relaxed); let _ = tx.send(Msg::Fail(e)); alive.fetch_sub(1, Relaxed); return; }
                    }
                }
                alive.fetch_sub(1, Relaxed);
                f.quit();
            });
        }
        drop(tx);
        let t0 = std::time::Instant::now();
        let (mut done, mut fresh) = (0u64, 0u64);
        let mut last_emit = std::time::Instant::now() - Duration::from_secs(1);
        let mut label = String::new();
        // speed over a sliding window (~3 s)
        let mut window: std::collections::VecDeque<(std::time::Instant, u64)> = Default::default();
        loop {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(Msg::Bytes(k, l)) => {
                    done += k;
                    if !l.is_empty() { fresh += k; label = l; }
                }
                Ok(Msg::Done { resumed, sent }) => {
                    res.sent += 1;
                    res.bytes += sent;
                    if resumed { res.resumed += 1 }
                }
                Ok(Msg::Fail(e)) => { if first_err.is_none() || first_err.as_deref() == Some(crate::archive::ERR_CANCELLED) { first_err = Some(e) } }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
            if cancelled() { stop.store(true, std::sync::atomic::Ordering::Relaxed); }
            let now = std::time::Instant::now();
            if now.duration_since(last_emit) >= Duration::from_millis(150) {
                last_emit = now;
                window.push_back((now, fresh));
                while window.len() > 2 && now.duration_since(window[0].0) > Duration::from_secs(3) { window.pop_front(); }
                let (t, b) = window[0];
                let dt = now.duration_since(t).as_secs_f64();
                let bps = if dt > 0.2 { (fresh - b) as f64 / dt } else if t0.elapsed().as_secs_f64() > 0.2 { fresh as f64 / t0.elapsed().as_secs_f64() } else { 0.0 };
                let eta = if bps > 1.0 { format!(" · ETA {}", fmt_eta(((total.saturating_sub(done)) as f64 / bps) as u64)) } else { String::new() };
                progress(((done * 100) / total).min(99) as u32, &format!("{label} · {:.1} MB/s{eta}", bps / 1_048_576.0));
            }
        }
        res.seconds = t0.elapsed().as_secs_f64();
    });
    res.connections = used.load(std::sync::atomic::Ordering::Relaxed).max(1);
    // A file handed back after the other worker had already finished: send it on one connection.
    let left: Vec<usize> = std::mem::take(&mut *retry.lock().unwrap_or_else(|p| p.into_inner()));
    if first_err.is_none() && !left.is_empty() && !cancelled() {
        let mut f = Ftp::connect(con.addr, &con.user, &con.pass)?;
        let (tx, rx) = std::sync::mpsc::channel::<Msg>();
        for i in left {
            let off = send_one(&mut f, con.addr, con.resume_dir.as_deref(), &todo[i], &known, &tx, &stop)?;
            res.sent += 1;
            res.bytes += todo[i].pf.len - off;
            if off > 0 { res.resumed += 1 }
        }
        drop(rx);
        f.quit();
    }
    if let Some(e) = first_err {
        return Err(e);
    }
    if cancelled() && res.sent < todo.len() {
        return Err(crate::archive::ERR_CANCELLED.into());
    }
    progress(100, &format!("{} file(s) · {:.1} MB/s average", res.sent, res.bytes as f64 / 1_048_576.0 / res.seconds.max(0.001)));
    Ok(res)
}

/// A drive name as Xbox FTP servers show it at the root (Hdd1, Usb0, UsbMu0, Flash, Game…):
/// letters then letters/digits, at most 16 characters.
pub fn is_drive(seg: &str) -> bool {
    !seg.is_empty() && seg.len() <= 16 && seg.chars().next().unwrap().is_ascii_alphabetic() && seg.chars().all(|c| c.is_ascii_alphanumeric())
}

/// A user-edited destination folder: FATX-valid segments, and the first one must be a drive
/// (that drive must also exist on the console; `send` checks that before writing).
pub fn custom_dest(p: &str) -> Result<Vec<String>, String> {
    let segs = split_remote(p)?;
    let Some(first) = segs.first() else { return Err("Enter a folder on the console, like /Hdd1/Games.".into()) };
    if !is_drive(first) {
        return Err(format!("\"{first}\" isn't a console drive. Start the path with a drive like /Hdd1 or /Usb0."));
    }
    if segs.len() > 24 {
        return Err("That folder is nested too deeply.".into());
    }
    Ok(segs)
}

/// One-click destination folders (same layout as Copy to USB / DLC install).
/// kind: god | dlc | tu | xex | plugins | custom. Returns (dest segments, contents_only).
pub fn destination(kind: &str, drive: &str, src: &Path, title_id: Option<&str>, custom: Option<&str>) -> Result<(Vec<String>, bool), String> {
    let mut d = split_remote(drive)?;
    if d.len() != 1 || !is_drive(&d[0]) {
        return Err("Pick a drive (like Hdd1 or Usb0).".into());
    }
    let tid = || -> Result<String, String> {
        let t = title_id.map(|s| s.trim().to_ascii_uppercase()).unwrap_or_default();
        if t.len() != 8 || !t.chars().all(|c| c.is_ascii_hexdigit()) || t == "00000000" || t == "FFFFFFFF" {
            return Err("This needs the game's TitleID (8 hex digits) to know its Content folder.".into());
        }
        Ok(t)
    };
    let content = |t: String, ct: &str| -> Vec<String> { vec!["Content".into(), "0000000000000000".into(), t, ct.into()] };
    match kind {
        // A library GOD game: send the whole <TitleID> folder (00007000 + DLC + TU) like Copy to USB.
        "god" => {
            let t = tid()?;
            let n = src.file_name().map(|s| s.to_string_lossy().to_ascii_uppercase()).unwrap_or_default();
            if n == t {
                d.extend(["Content".to_string(), "0000000000000000".into(), t]);
                Ok((d, true))
            } else {
                d.extend(content(t, "00007000"));
                Ok((d, false))
            }
        }
        "dlc" => { d.extend(content(tid()?, "00000002")); Ok((d, false)) }
        "tu" => { d.extend(content(tid()?, "000B0000")); Ok((d, false)) }
        "xex" => { d.push("Games".into()); Ok((d, false)) }
        "plugins" => { d.push("Plugins".into()); Ok((d, false)) }
        "custom" => {
            let c = split_remote(custom.unwrap_or(""))?;
            if c.is_empty() {
                return Err("Pick a folder on the console first.".into());
            }
            Ok((c, false))
        }
        _ => Err("Unknown destination.".into()),
    }
}

#[cfg(test)]
pub(crate) mod tests;
