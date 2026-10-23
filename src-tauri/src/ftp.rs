use crate::paths;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::path::Path;
use std::time::Duration;

pub struct FtpClient {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
}

impl FtpClient {
    pub fn connect(host: &str, port: u16, user: &str, pass: &str) -> Result<Self, String> {
        let addr = format!("{host}:{port}");
        let stream = connect_timeout(&addr, Duration::from_secs(5))?;
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .map_err(|e| e.to_string())?;
        stream
            .set_write_timeout(Some(Duration::from_secs(30)))
            .map_err(|e| e.to_string())?;
        let writer = stream.try_clone().map_err(|e| e.to_string())?;
        let mut client = FtpClient {
            reader: BufReader::new(stream),
            writer,
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
        let addr = parse_pasv(&msg)?;
        connect_timeout(&addr, Duration::from_secs(8))
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
        let data = std::fs::read(local).map_err(|e| e.to_string())?;
        if paths::is_protected_filename(remote_name) {
            // Never DELE launch.ini. Store beside it, then rename over.
            let tmp_name = format!("{remote_name}.unpack-tmp");
            self.stor(&tmp_name, &data)?;
            let _ = self.command(&format!("RNFR {tmp_name}"));
            let (code, msg) = self.command(&format!("RNTO {remote_name}"))?;
            if code >= 400 {
                return Err(format!(
                    "could not replace {remote_name} via rename (left original in place): {code} {msg}"
                ));
            }
            return Ok(UploadOutcome::UploadedAtomic);
        }
        self.stor(remote_name, &data)?;
        Ok(UploadOutcome::Uploaded)
    }

    fn stor(&mut self, name: &str, data: &[u8]) -> Result<(), String> {
        let mut data_stream = self.pasv_stream()?;
        self.writer
            .write_all(format!("STOR {name}\r\n").as_bytes())
            .map_err(|e| e.to_string())?;
        self.writer.flush().map_err(|e| e.to_string())?;
        let (code, msg) = self.read_response()?;
        if code >= 400 {
            return Err(format!("STOR {name} rejected: {code} {msg}"));
        }
        data_stream.write_all(data).map_err(|e| e.to_string())?;
        data_stream.flush().map_err(|e| e.to_string())?;
        drop(data_stream);
        let (code, msg) = self.read_response()?;
        if code >= 400 {
            Err(format!("STOR {name} failed: {code} {msg}"))
        } else {
            Ok(())
        }
    }

    pub fn close(mut self) {
        let _ = self.command("QUIT");
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UploadOutcome {
    Uploaded,
    UploadedAtomic,
    SkippedProtected,
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

fn parse_pasv(msg: &str) -> Result<String, String> {
    let start = msg.find('(').ok_or_else(|| "bad PASV response".to_string())?;
    let end = msg.find(')').ok_or_else(|| "bad PASV response".to_string())?;
    let inner = &msg[start + 1..end];
    let n: Vec<u32> = inner
        .split(',')
        .filter_map(|p| p.trim().parse().ok())
        .collect();
    if n.len() < 6 {
        return Err(format!("bad PASV tuple: {msg}"));
    }
    Ok(format!(
        "{}.{}.{}.{}:{}",
        n[0],
        n[1],
        n[2],
        n[3],
        n[4] * 256 + n[5]
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pasv() {
        let addr = parse_pasv("227 Entering Passive Mode (192,168,1,20,20,80)").unwrap();
        assert_eq!(addr, "192.168.1.20:5200");
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
