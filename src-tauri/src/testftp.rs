//! A small in-memory FTP server for tests. It behaves like the console FTP
//! servers Unpack talks to, and can be told to drop a transfer, report a full
//! drive, or leave out SIZE / REST.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone)]
pub struct Cfg {
    /// Total bytes the drive can hold. `None` means unlimited.
    pub capacity: Option<usize>,
    /// For the nth STOR/APPE: keep this many bytes, then cut the connection.
    pub drop_stor: Vec<Option<usize>>,
    pub rest_ok: bool,
    pub size_ok: bool,
}

impl Default for Cfg {
    fn default() -> Self {
        Cfg {
            capacity: None,
            drop_stor: Vec::new(),
            rest_ok: true,
            size_ok: true,
        }
    }
}

#[derive(Default)]
pub struct State {
    pub files: HashMap<String, Vec<u8>>,
    pub dirs: HashSet<String>,
    pub cfg: Cfg,
    pub stors: usize,
    pub logins: usize,
    pub log: Vec<String>,
}

pub struct FakeFtp {
    pub port: u16,
    pub state: Arc<Mutex<State>>,
}

impl FakeFtp {
    pub fn start(cfg: Cfg) -> FakeFtp {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let state = Arc::new(Mutex::new(State {
            cfg,
            ..State::default()
        }));
        let accept_state = state.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let st = accept_state.clone();
                std::thread::spawn(move || serve(stream, st));
            }
        });
        FakeFtp { port, state }
    }

    pub fn file(&self, path: &str) -> Option<Vec<u8>> {
        self.state.lock().unwrap().files.get(path).cloned()
    }

    pub fn has(&self, path: &str) -> bool {
        self.state.lock().unwrap().files.contains_key(path)
    }

    pub fn names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.state.lock().unwrap().files.keys().cloned().collect();
        v.sort();
        v
    }

    pub fn logins(&self) -> usize {
        self.state.lock().unwrap().logins
    }

    pub fn stors(&self) -> usize {
        self.state.lock().unwrap().stors
    }

    pub fn saw(&self, prefix: &str) -> bool {
        self.state
            .lock()
            .unwrap()
            .log
            .iter()
            .any(|l| l.starts_with(prefix))
    }
}

fn dir_key(path: &str) -> String {
    path.replace('\\', "/").trim_matches('/').to_string()
}

fn file_key(cwd: &str, name: &str) -> String {
    if cwd.is_empty() {
        name.to_string()
    } else {
        format!("{cwd}/{name}")
    }
}

fn reply(w: &mut TcpStream, text: &str) {
    let _ = w.write_all(format!("{text}\r\n").as_bytes());
    let _ = w.flush();
}

fn serve(stream: TcpStream, state: Arc<Mutex<State>>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(20)));
    let mut writer = stream.try_clone().unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    state.lock().unwrap().logins += 1;
    reply(&mut writer, "220 fake xbox ftp");

    let mut cwd = String::new();
    let mut pasv: Option<TcpListener> = None;
    let mut rest = 0usize;
    let mut rnfr = String::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        let line = line.trim_end().to_string();
        let (cmd, arg) = match line.split_once(' ') {
            Some((c, a)) => (c.to_ascii_uppercase(), a.to_string()),
            None => (line.to_ascii_uppercase(), String::new()),
        };
        state.lock().unwrap().log.push(line.clone());
        match cmd.as_str() {
            "USER" => reply(&mut writer, "331 password please"),
            "PASS" => reply(&mut writer, "230 welcome"),
            "TYPE" => reply(&mut writer, "200 binary"),
            "MKD" => {
                let key = dir_key(&arg);
                let mut s = state.lock().unwrap();
                let mut acc = String::new();
                for part in key.split('/').filter(|p| !p.is_empty()) {
                    if !acc.is_empty() {
                        acc.push('/');
                    }
                    acc.push_str(part);
                    s.dirs.insert(acc.clone());
                }
                drop(s);
                reply(&mut writer, "257 made");
            }
            "CWD" => {
                let key = dir_key(&arg);
                let ok = key.is_empty() || state.lock().unwrap().dirs.contains(&key);
                if ok {
                    cwd = key;
                    reply(&mut writer, "250 ok");
                } else {
                    reply(&mut writer, "550 no such folder");
                }
            }
            "SIZE" => {
                let s = state.lock().unwrap();
                if !s.cfg.size_ok {
                    drop(s);
                    reply(&mut writer, "550 SIZE not supported");
                } else if let Some(f) = s.files.get(&file_key(&cwd, &arg)) {
                    let n = f.len();
                    drop(s);
                    reply(&mut writer, &format!("213 {n}"));
                } else {
                    drop(s);
                    reply(&mut writer, "550 no such file");
                }
            }
            "PASV" => {
                let l = TcpListener::bind("127.0.0.1:0").unwrap();
                let port = l.local_addr().unwrap().port();
                pasv = Some(l);
                reply(
                    &mut writer,
                    &format!(
                        "227 Entering Passive Mode (127,0,0,1,{},{})",
                        port / 256,
                        port % 256
                    ),
                );
            }
            "REST" => {
                if state.lock().unwrap().cfg.rest_ok {
                    rest = arg.parse().unwrap_or(0);
                    reply(&mut writer, "350 restarting");
                } else {
                    reply(&mut writer, "502 REST not supported");
                }
            }
            "STOR" | "APPE" => {
                let Some(listener) = pasv.take() else {
                    reply(&mut writer, "425 use PASV first");
                    continue;
                };
                reply(&mut writer, "150 opening data connection");
                let Ok((mut data, _)) = listener.accept() else {
                    reply(&mut writer, "425 no data connection");
                    continue;
                };
                let _ = data.set_read_timeout(Some(Duration::from_secs(10)));
                let key = file_key(&cwd, &arg);
                let (plan, capacity, other_bytes, existing) = {
                    let mut s = state.lock().unwrap();
                    let idx = s.stors;
                    s.stors += 1;
                    let plan = s.cfg.drop_stor.get(idx).copied().flatten();
                    let other: usize = s
                        .files
                        .iter()
                        .filter(|(k, _)| **k != key)
                        .map(|(_, v)| v.len())
                        .sum();
                    (
                        plan,
                        s.cfg.capacity,
                        other,
                        s.files.get(&key).cloned().unwrap_or_default(),
                    )
                };
                let mut stored: Vec<u8> = if cmd == "APPE" {
                    existing
                } else if rest > 0 {
                    existing.into_iter().take(rest).collect()
                } else {
                    Vec::new()
                };
                rest = 0;
                let mut received = 0usize;
                let mut full = false;
                let mut dropped = false;
                let mut buf = [0u8; 64 * 1024];
                loop {
                    let n = match data.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => n,
                    };
                    if full {
                        continue;
                    }
                    let mut keep = n;
                    if let Some(p) = plan {
                        if received + keep >= p {
                            keep = p.saturating_sub(received);
                            dropped = true;
                        }
                    }
                    if let Some(cap) = capacity {
                        let room = cap.saturating_sub(other_bytes + stored.len());
                        if keep > room {
                            keep = room;
                            full = true;
                        }
                    }
                    stored.extend_from_slice(&buf[..keep]);
                    received += keep;
                    if dropped {
                        break;
                    }
                }
                state.lock().unwrap().files.insert(key, stored);
                if dropped {
                    let _ = data.shutdown(Shutdown::Both);
                    let _ = stream.shutdown(Shutdown::Both);
                    return;
                }
                if full {
                    reply(&mut writer, "552 insufficient storage space");
                } else {
                    reply(&mut writer, "226 transfer complete");
                }
            }
            "RNFR" => {
                rnfr = file_key(&cwd, &arg);
                if state.lock().unwrap().files.contains_key(&rnfr) {
                    reply(&mut writer, "350 ready for RNTO");
                } else {
                    reply(&mut writer, "550 no such file");
                }
            }
            "RNTO" => {
                let to = file_key(&cwd, &arg);
                let mut s = state.lock().unwrap();
                if let Some(body) = s.files.remove(&rnfr) {
                    s.files.insert(to, body);
                    drop(s);
                    reply(&mut writer, "250 renamed");
                } else {
                    drop(s);
                    reply(&mut writer, "550 rename failed");
                }
            }
            "DELE" => {
                state.lock().unwrap().files.remove(&file_key(&cwd, &arg));
                reply(&mut writer, "250 deleted");
            }
            "QUIT" => {
                reply(&mut writer, "221 bye");
                break;
            }
            _ => reply(&mut writer, "502 not implemented"),
        }
    }
}
