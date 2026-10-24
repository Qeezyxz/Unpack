use super::*;
use std::collections::BTreeMap;
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

#[test]
fn lan_only() {
    for ok in [
        "10.0.0.5",
        "172.16.0.1",
        "172.31.255.1",
        "192.168.1.50",
        "169.254.10.2",
    ] {
        assert!(check_target(ok, 21).is_ok(), "{ok}");
    }
    for bad in [
        "8.8.8.8",
        "172.32.0.1",
        "172.15.0.1",
        "100.64.0.1",
        "0.0.0.0",
        "255.255.255.255",
        "224.0.0.1",
        "192.169.0.1",
        "169.254.0.1",
        "xbox.local",
        "::1",
        "fe80::1",
        "192.168.1.5:21",
        " ",
        "1.1.1.1",
    ] {
        assert!(check_target(bad, 21).is_err(), "{bad}");
    }
    assert!(check_target("192.168.1.5", 0).is_err());
}

#[test]
fn loopback_refused_without_test_flag() {
    let _g = crate::download::tests_env_lock();
    std::env::remove_var("UNPACK_TEST_ALLOW_LOOPBACK");
    assert!(check_target("127.0.0.1", 21).is_err());
}

#[test]
fn parses_unix_and_dos_lists() {
    let u = parse_list_line("drwxr-xr-x   1 root  root         0 Jan 01  2000 Hdd1").unwrap();
    assert_eq!(
        u,
        Entry {
            name: "Hdd1".into(),
            is_dir: true,
            size: 0
        }
    );
    let u =
        parse_list_line("-rw-r--r-- 1 root root 4294967296 Oct 20 13:01 big file.bin\r").unwrap();
    assert_eq!(
        (u.name.as_str(), u.size, u.is_dir),
        ("big file.bin", 4294967296, false)
    );
    // owner/group missing (some Xbox servers)
    let u = parse_list_line("-rwxrwxrwx 1 123 Jan 1 2000 default.xex").unwrap();
    assert_eq!((u.name.as_str(), u.size), ("default.xex", 123));
    let d = parse_list_line("01-01-00  12:00AM       <DIR>          Games").unwrap();
    assert!(d.is_dir && d.name == "Games");
    let f = parse_list_line("10-20-26  01:05PM             1,234 a b.txt").unwrap();
    assert_eq!((f.name.as_str(), f.size), ("a b.txt", 1234));
    assert!(parse_list_line("total 12").is_none());
    assert!(parse_list_line("drwxr-xr-x 1 root root 0 Jan 01 2000 ..").is_none());
    assert!(parse_list_line("garbage").is_none());
    let l = parse_list_line("lrwxrwxrwx 1 root root 4 Jan 01 2000 x -> y").unwrap();
    assert_eq!(l.name, "x");
}

#[test]
fn fatx_names() {
    assert!(check_segment("default.xex").is_ok());
    assert!(check_segment(&"a".repeat(42)).is_ok());
    assert!(check_segment(&"a".repeat(43)).is_err());
    for bad in [
        "",
        ".",
        "..",
        "a/b",
        "a\\b",
        "a:b",
        "a*b",
        "a?b",
        "a\"b",
        "a<b",
        "a|b",
        "a+b",
        "a,b",
        "a;b",
        "a=b",
        "x\r\nDELE y",
        "é",
    ] {
        assert!(check_segment(bad).is_err(), "{bad:?}");
    }
    assert_eq!(
        split_remote("/Hdd1\\Games//x").unwrap(),
        vec!["Hdd1", "Games", "x"]
    );
    assert!(split_remote("/Hdd1/../x").is_err());
    assert_eq!(part_name("a.bin"), "a.bin.part");
    let p = part_name(&"b".repeat(42));
    assert!(p.len() <= 42 && p.ends_with(".part"));
}

#[test]
fn destinations() {
    let god = Path::new("/lib/4D5307E6");
    let (d, c) = destination("god", "Hdd1", god, Some("4d5307e6"), None).unwrap();
    assert_eq!(
        (join_remote(&d), c),
        ("/Hdd1/Content/0000000000000000/4D5307E6".into(), true)
    );
    let (d, c) = destination(
        "god",
        "Hdd1",
        Path::new("/x/Halo GOD"),
        Some("4D5307E6"),
        None,
    )
    .unwrap();
    assert_eq!(
        (join_remote(&d), c),
        (
            "/Hdd1/Content/0000000000000000/4D5307E6/00007000".into(),
            false
        )
    );
    assert_eq!(
        join_remote(
            &destination("dlc", "Usb0", Path::new("/a"), Some("4D5307E6"), None)
                .unwrap()
                .0
        ),
        "/Usb0/Content/0000000000000000/4D5307E6/00000002"
    );
    assert_eq!(
        join_remote(
            &destination("tu", "Hdd1", Path::new("/a"), Some("4D5307E6"), None)
                .unwrap()
                .0
        ),
        "/Hdd1/Content/0000000000000000/4D5307E6/000B0000"
    );
    assert_eq!(
        join_remote(
            &destination("xex", "Hdd1", Path::new("/a"), None, None)
                .unwrap()
                .0
        ),
        "/Hdd1/Games"
    );
    assert_eq!(
        join_remote(
            &destination("custom", "Hdd1", Path::new("/a"), None, Some("/Usb0/Apps"))
                .unwrap()
                .0
        ),
        "/Usb0/Apps"
    );
    assert!(destination("dlc", "Hdd1", Path::new("/a"), Some("00000000"), None).is_err());
    assert!(destination("dlc", "Hdd1", Path::new("/a"), Some("XYZ"), None).is_err());
    assert!(destination("xex", "Hdd1/Games", Path::new("/a"), None, None).is_err());
    assert!(destination("custom", "Hdd1", Path::new("/a"), None, Some("/Hdd1/../..")).is_err());
    assert!(destination("weird", "Hdd1", Path::new("/a"), None, None).is_err());
}

// ---- A fake Aurora-like FTP server: one client at a time, DOS LIST, no MLSD, optional SIZE ----

#[derive(Default)]
pub(crate) struct Fs {
    pub(crate) files: BTreeMap<String, Vec<u8>>,
    pub(crate) dirs: std::collections::BTreeSet<String>,
    pub(crate) log: Vec<String>,
    /// Hostile/quirky behaviour switches for regression tests.
    pub(crate) list_fail: bool,
    pub(crate) no_rest: bool,
    pub(crate) huge_list: bool,
    pub(crate) rnto_truncate: bool,
    /// 1: the final STOR reply is 552 (drive full); 2: STOR itself is refused with 452.
    pub(crate) disk_full: u8,
    /// RNTO to this exact path answers 550, `rnto_fail_times` times (R12 HIGH-1).
    pub(crate) rnto_fail_to: Option<String>,
    pub(crate) rnto_fail_times: usize,
    /// Generic fault injection (R13): first matching fault fires and counts down.
    pub(crate) faults: Vec<Fault>,
}

#[derive(Clone, Debug)]
pub(crate) struct Fault {
    /// FTP verb, e.g. "RNTO", "STOR", "DELE", "SIZE".
    pub(crate) cmd: &'static str,
    /// The command's absolute path must equal this.
    pub(crate) path: String,
    /// true: close the control connection without replying; false: answer 550.
    pub(crate) drop: bool,
    pub(crate) times: usize,
    /// Armed only once an earlier log line contains this text (e.g. "RNTO /Hdd1/launch.ini");
    /// a trailing `$` means the line must equal it exactly.
    pub(crate) after: Option<String>,
}

impl Fault {
    pub(crate) fn new(cmd: &'static str, path: &str, drop: bool, times: usize) -> Fault {
        Fault {
            cmd,
            path: path.into(),
            drop,
            times,
            after: None,
        }
    }
}

pub(crate) struct Fake {
    pub(crate) port: u16,
    pub(crate) fs: Arc<Mutex<Fs>>,
}

pub(crate) fn fake(size_cmd: bool, unix_list: bool) -> Fake {
    fake_x(size_cmd, unix_list, false)
}
fn fake_x(size_cmd: bool, unix_list: bool, concurrent: bool) -> Fake {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let fs: Arc<Mutex<Fs>> = Default::default();
    {
        let mut f = fs.lock().unwrap();
        f.dirs.insert("/".into());
        f.dirs.insert("/Hdd1".into());
        f.dirs.insert("/Usb0".into());
    }
    let fs2 = fs.clone();
    std::thread::spawn(move || {
        for c in l.incoming() {
            let Ok(c) = c else { continue };
            if concurrent {
                let fs3 = fs2.clone();
                std::thread::spawn(move || serve(c, &fs3, size_cmd, unix_list));
            } else {
                serve(c, &fs2, size_cmd, unix_list);
            } // sequential = 1-connection limit
        }
    });
    Fake { port, fs }
}

fn serve(c: TcpStream, fs: &Arc<Mutex<Fs>>, size_cmd: bool, unix_list: bool) {
    let mut r = BufReader::new(c.try_clone().unwrap());
    let mut w = c;
    let mut cwd = "/".to_string();
    let mut pasv: Option<TcpListener> = None;
    let mut rest = 0usize;
    let mut rnfr = None;
    let _ = w.write_all(b"220-Aurora FTP\r\n220 ready\r\n");
    let abs = |cwd: &str, p: &str| {
        if p.starts_with('/') {
            p.to_string()
        } else if cwd == "/" {
            format!("/{p}")
        } else {
            format!("{cwd}/{p}")
        }
    };
    loop {
        let mut line = String::new();
        if r.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let line = line.trim_end().to_string();
        fs.lock().unwrap().log.push(line.clone());
        let (cmd, arg) = line
            .split_once(' ')
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .unwrap_or((line.clone(), String::new()));
        let fault = {
            let mut f = fs.lock().unwrap();
            let target = abs(&cwd, &arg);
            let n = f.log.len().saturating_sub(1);
            let armed: Vec<bool> = f
                .faults
                .iter()
                .map(|x| {
                    x.after.as_ref().map_or(true, |a| {
                        f.log[..n].iter().any(|l| {
                            !l.starts_with("FAULT")
                                && match a.strip_suffix('$') {
                                    Some(exact) => l == exact,
                                    None => l.contains(a.as_str()),
                                }
                        })
                    })
                })
                .collect();
            let hit = f
                .faults
                .iter_mut()
                .zip(armed)
                .find(|(x, a)| *a && x.times > 0 && x.cmd == cmd && x.path == target)
                .map(|(x, _)| {
                    x.times -= 1;
                    x.drop
                });
            if hit.is_some() {
                f.log.push(format!("FAULT {cmd} {target}"));
            }
            hit
        };
        match fault {
            Some(true) => return, // connection dropped
            Some(false) => {
                if cmd == "STOR" {
                    pasv = None;
                }
                let _ = w.write_all(b"550 Refused (test fault)\r\n");
                continue;
            }
            None => {}
        }
        let reply = match cmd.as_str() {
            "USER" => "331 pw".into(),
            "PASS" => {
                if arg == "xboxftp" {
                    "230 ok".into()
                } else {
                    "530 no".into()
                }
            }
            "TYPE" => "200 ok".into(),
            "CWD" => {
                let p = abs(&cwd, &arg);
                if fs.lock().unwrap().dirs.contains(&p) {
                    cwd = p;
                    "250 ok".into()
                } else {
                    "550 no".into()
                }
            }
            "PASV" => {
                let l = TcpListener::bind("127.0.0.1:0").unwrap();
                let p = l.local_addr().unwrap().port();
                pasv = Some(l);
                format!(
                    "227 Entering Passive Mode (6,6,6,6,{},{})",
                    p / 256,
                    p % 256
                )
            } // bogus host: must be ignored
            "MLSD" => "500 unknown".into(),
            "SIZE" if size_cmd => {
                let p = abs(&cwd, &arg);
                match fs.lock().unwrap().files.get(&p) {
                    Some(b) => format!("213 {}", b.len()),
                    None => "550 no".into(),
                }
            }
            "SIZE" => "502 not implemented".into(),
            "MKD" => {
                let p = abs(&cwd, &arg);
                fs.lock().unwrap().dirs.insert(p);
                "257 ok".into()
            }
            "REST" if fs.lock().unwrap().no_rest => "502 REST not supported".into(),
            "REST" => {
                rest = arg.parse().unwrap();
                "350 ok".into()
            }
            "RETR" => {
                let p = abs(&cwd, &arg);
                let data = fs.lock().unwrap().files.get(&p).cloned();
                match data {
                    None => "550 no".into(),
                    Some(b) => {
                        let _ = w.write_all(b"150 sending\r\n");
                        let (mut d, _) = pasv.take().unwrap().accept().unwrap();
                        let _ = d.set_write_timeout(Some(Duration::from_secs(2)));
                        for c in b[rest.min(b.len())..].chunks(64 << 10) {
                            if d.write_all(c).is_err() {
                                break;
                            }
                        }
                        rest = 0;
                        "226 done".into()
                    }
                }
            }
            "RNFR" => {
                rnfr = Some(abs(&cwd, &arg));
                "350 ok".into()
            }
            "RNTO"
                if {
                    let mut f = fs.lock().unwrap();
                    let to = abs(&cwd, &arg);
                    let hit =
                        f.rnto_fail_to.as_deref() == Some(to.as_str()) && f.rnto_fail_times > 0;
                    if hit {
                        f.rnto_fail_times -= 1;
                    }
                    hit
                } =>
            {
                "550 Rename refused".into()
            }
            "RNTO"
                if !fs
                    .lock()
                    .unwrap()
                    .files
                    .contains_key(rnfr.as_deref().unwrap_or("")) =>
            {
                "550 No such file".into()
            }
            "RNTO" => {
                let mut f = fs.lock().unwrap();
                let mut b = f.files.remove(rnfr.as_ref().unwrap()).unwrap();
                if f.rnto_truncate {
                    b.truncate(1);
                }
                f.files.insert(abs(&cwd, &arg), b);
                "250 ok".into()
            }
            "LIST" if fs.lock().unwrap().list_fail => {
                pasv = None;
                "425 Can't open data connection".into()
            }
            "LIST" if fs.lock().unwrap().huge_list => {
                let _ = w.write_all(b"150 here\r\n");
                let (mut d, _) = pasv.take().unwrap().accept().unwrap();
                let line = vec![b'x'; 1 << 20];
                for _ in 0..17 {
                    if d.write_all(&line).is_err() {
                        break;
                    }
                }
                "226 done".into()
            }
            "LIST" => {
                let _ = w.write_all(b"150 here\r\n");
                let (mut d, _) = pasv.take().unwrap().accept().unwrap();
                let f = fs.lock().unwrap();
                let pre = if cwd == "/" {
                    "/".to_string()
                } else {
                    format!("{cwd}/")
                };
                let mut out = String::new();
                for dname in f.dirs.iter().filter(|x| {
                    x.starts_with(&pre) && x.len() > pre.len() && !x[pre.len()..].contains('/')
                }) {
                    let n = &dname[pre.len()..];
                    out += &if unix_list {
                        format!("drwxr-xr-x 1 root root 0 Jan 01 2000 {n}\r\n")
                    } else {
                        format!("01-01-00  12:00AM       <DIR>          {n}\r\n")
                    };
                }
                for (fname, b) in f
                    .files
                    .iter()
                    .filter(|(x, _)| x.starts_with(&pre) && !x[pre.len()..].contains('/'))
                {
                    let n = &fname[pre.len()..];
                    out += &if unix_list {
                        format!("-rw-r--r-- 1 root root {} Jan 01 2000 {n}\r\n", b.len())
                    } else {
                        format!("01-01-00  12:00AM  {}  {n}\r\n", b.len())
                    };
                }
                let _ = d.write_all(out.as_bytes());
                "226 done".into()
            }
            "STOR" if fs.lock().unwrap().disk_full == 2 => {
                pasv = None;
                "452 Insufficient storage space".into()
            }
            "STOR" => {
                let _ = w.write_all(b"150 send\r\n");
                let (mut d, _) = pasv.take().unwrap().accept().unwrap();
                let mut buf = vec![];
                let _ = d.read_to_end(&mut buf);
                let p = abs(&cwd, &arg);
                let mut f = fs.lock().unwrap();
                let e = f.files.entry(p).or_default();
                e.truncate(rest);
                e.extend(buf);
                rest = 0;
                if f.disk_full == 1 {
                    f.files.remove(&abs(&cwd, &arg));
                    "552 Requested file action aborted, exceeded storage allocation".into()
                } else {
                    "226 stored".into()
                }
            }
            "DELE" => {
                let p = abs(&cwd, &arg);
                if fs.lock().unwrap().files.remove(&p).is_some() {
                    "250 deleted".into()
                } else {
                    "550 no such file".into()
                }
            }
            "QUIT" => {
                let _ = w.write_all(b"221 bye\r\n");
                return;
            }
            _ => "502 no".into(),
        };
        if w.write_all(format!("{reply}\r\n").as_bytes()).is_err() {
            return;
        }
    }
}

pub(crate) fn con(port: u16) -> Console {
    Console {
        addr: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port),
        user: "xboxftp".into(),
        pass: "xboxftp".into(),
        resume_dir: Some(crate::testutil::fixture_root().join("ftp-resume")),
    }
}

fn tmp(n: &str) -> crate::testutil::TmpDir {
    crate::testutil::TmpDir::new(&format!("ftp-{n}"))
}

fn segs(s: &str) -> Vec<String> {
    split_remote(s).unwrap()
}

#[test]
fn login_list_and_bad_password() {
    let f = fake(true, true);
    let mut c = Ftp::connect(con(f.port).addr, "xboxftp", "xboxftp").unwrap();
    let l = c.list(&[]).unwrap();
    assert_eq!(
        l.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
        vec!["Hdd1", "Usb0"]
    );
    assert!(c.list(&segs("/Nope")).is_err());
    c.quit();
    let e = Ftp::connect(con(f.port).addr, "xboxftp", "wrong")
        .err()
        .unwrap();
    assert!(e.contains("Login refused"), "{e}");
    assert!(Ftp::connect(con(f.port).addr, "a\r\nDELE x", "p").is_err());
}

#[test]
fn send_folder_dos_list_no_size_and_skip_identical() {
    let f = fake(false, false);
    let d = tmp("send");
    let g = d.join("4D5307E6");
    std::fs::create_dir_all(g.join("00007000")).unwrap();
    std::fs::write(g.join("00007000").join("ABCDEF01"), vec![7u8; 300_000]).unwrap();
    std::fs::create_dir_all(g.join("00000002")).unwrap();
    std::fs::write(g.join("00000002").join("dlc1"), b"dlc").unwrap();
    let (dest, co) = destination("god", "Hdd1", &g, Some("4D5307E6"), None).unwrap();
    let mut last = 0;
    let r = send(
        &con(f.port),
        &g,
        &dest,
        co,
        false,
        &mut |p, _| last = p,
        &|| false,
    )
    .unwrap();
    assert_eq!((r.sent, r.skipped), (2, 0));
    assert_eq!(last, 100);
    {
        let fs = f.fs.lock().unwrap();
        assert_eq!(
            fs.files["/Hdd1/Content/0000000000000000/4D5307E6/00007000/ABCDEF01"].len(),
            300_000
        );
        assert_eq!(
            fs.files["/Hdd1/Content/0000000000000000/4D5307E6/00000002/dlc1"],
            b"dlc"
        );
        assert!(!fs.files.keys().any(|k| k.ends_with(".part")));
        assert!(!fs.log.iter().any(|l| l.starts_with("MLSD")));
    }
    let r = send(&con(f.port), &g, &dest, co, false, &mut |_, _| {}, &|| {
        false
    })
    .unwrap();
    assert_eq!((r.sent, r.skipped), (0, 2));
}

#[test]
fn conflict_stops_before_sending() {
    let f = fake(true, true);
    let d = tmp("conflict");
    std::fs::write(d.join("a.xex"), b"new content").unwrap();
    std::fs::write(d.join("b.xex"), b"bbb").unwrap();
    let name = d.file_name().unwrap().to_string_lossy().into_owned();
    {
        let mut fs = f.fs.lock().unwrap();
        fs.dirs.insert("/Hdd1/Apps".into());
        fs.dirs.insert(format!("/Hdd1/Apps/{name}"));
        fs.files
            .insert(format!("/Hdd1/Apps/{name}/b.xex"), b"older".to_vec()); // 5 bytes vs 3
    }
    let e = send(
        &con(f.port),
        &d,
        &segs("/Hdd1/Apps"),
        false,
        false,
        &mut |_, _| {},
        &|| false,
    )
    .err()
    .unwrap();
    assert!(e.contains("different size"), "{e}");
    assert!(!f
        .fs
        .lock()
        .unwrap()
        .log
        .iter()
        .any(|l| l.starts_with("STOR")));
}

#[test]
fn cancel_then_resume_with_rest() {
    let f = fake(true, true);
    let d = tmp("resume");
    let src = d.join("big.bin");
    let data: Vec<u8> = (0..64_000_000u32).map(|i| (i % 251) as u8).collect();
    std::fs::write(&src, &data).unwrap();
    let n = std::cell::Cell::new(0u32);
    let e = send(
        &con(f.port),
        &src,
        &segs("/Usb0"),
        false,
        false,
        &mut |_, _| {},
        &|| {
            n.set(n.get() + 1);
            n.get() > 5
        },
    )
    .err()
    .unwrap();
    assert_eq!(e, crate::archive::ERR_CANCELLED);
    std::thread::sleep(Duration::from_millis(300));
    let partial =
        f.fs.lock()
            .unwrap()
            .files
            .get("/Usb0/big.bin.part")
            .map(|b| b.len())
            .unwrap_or(0);
    assert!(partial > 0 && partial < data.len(), "partial {partial}");
    assert!(!f.fs.lock().unwrap().files.contains_key("/Usb0/big.bin"));
    let r = send(
        &con(f.port),
        &src,
        &segs("/Usb0"),
        false,
        false,
        &mut |_, _| {},
        &|| false,
    )
    .unwrap();
    assert_eq!((r.sent, r.resumed), (1, 1));
    let fs = f.fs.lock().unwrap();
    assert_eq!(fs.files["/Usb0/big.bin"], data);
    assert!(fs.log.iter().any(|l| l == &format!("REST {partial}")));
}

#[test]
fn bad_names_and_missing_drive() {
    let f = fake(true, true);
    let d = tmp("names");
    std::fs::write(d.join("bad:name.txt"), b"x").ok();
    std::fs::write(d.join(&"n".repeat(50)), b"x").unwrap();
    let e = send(
        &con(f.port),
        &d,
        &segs("/Hdd1"),
        false,
        false,
        &mut |_, _| {},
        &|| false,
    )
    .err()
    .unwrap();
    assert!(e.contains("Xbox"), "{e}");
    let ok = d.join("ok.txt");
    std::fs::write(&ok, b"x").unwrap();
    let e = send(
        &con(f.port),
        &ok,
        &segs("/Hdd9"),
        false,
        false,
        &mut |_, _| {},
        &|| false,
    )
    .err()
    .unwrap();
    assert!(e.contains("Hdd9"), "{e}");
}

#[cfg(unix)]
#[test]
fn links_never_followed() {
    let f = fake(true, true);
    let d = tmp("links");
    let outside = tmp("links-out");
    std::fs::write(outside.join("secret.txt"), b"secret").unwrap();
    let mods = d.join("Mod");
    std::fs::create_dir_all(&mods).unwrap();
    std::fs::write(mods.join("real.xex"), b"real").unwrap();
    std::os::unix::fs::symlink(outside.join("secret.txt"), mods.join("link.txt")).unwrap();
    std::os::unix::fs::symlink(&outside, mods.join("linkdir")).unwrap();
    let r = send(
        &con(f.port),
        &mods,
        &segs("/Hdd1"),
        false,
        false,
        &mut |_, _| {},
        &|| false,
    )
    .unwrap();
    assert_eq!((r.sent, r.links_skipped), (1, 2));
    assert!(!f.fs.lock().unwrap().files.values().any(|b| b == b"secret"));
    std::os::unix::fs::symlink(&mods, d.join("ModLink")).unwrap();
    assert!(send(
        &con(f.port),
        &d.join("ModLink"),
        &segs("/Hdd1"),
        false,
        false,
        &mut |_, _| {},
        &|| false
    )
    .is_err());
}

#[test]
fn parallel_uses_two_connections_or_falls_back() {
    let d = tmp("par");
    for i in 0..6 {
        std::fs::write(d.join(format!("f{i}.bin")), vec![i as u8; 3_000_000]).unwrap();
    }
    let f = fake_x(true, true, true);
    let r = send(
        &con(f.port),
        &d,
        &segs("/Hdd1"),
        true,
        true,
        &mut |_, _| {},
        &|| false,
    )
    .unwrap();
    assert_eq!((r.sent, r.connections), (6, 2));
    assert!(f
        .fs
        .lock()
        .unwrap()
        .files
        .iter()
        .filter(|(k, _)| k.starts_with("/Hdd1/f"))
        .all(|(_, b)| b.len() == 3_000_000));
    // 1-connection server: the second login never gets a greeting -> 1 connection, still works.
    let f = fake(true, true);
    let t = std::time::Instant::now();
    let r = send(
        &con(f.port),
        &d,
        &segs("/Hdd1"),
        true,
        true,
        &mut |_, _| {},
        &|| false,
    )
    .unwrap();
    assert_eq!((r.sent, r.connections), (6, 1));
    assert!(t.elapsed() < Duration::from_secs(20));
}

#[test]
fn few_round_trips_per_file() {
    let f = fake(true, true);
    let d = tmp("rtt");
    std::fs::create_dir_all(d.join("A/B")).unwrap();
    for i in 0..20 {
        std::fs::write(d.join(format!("A/B/s{i}.txt")), b"tiny").unwrap();
    }
    let mut labels = vec![];
    let r = send(
        &con(f.port),
        &d.join("A"),
        &segs("/Hdd1"),
        false,
        false,
        &mut |_, l| labels.push(l.to_string()),
        &|| false,
    )
    .unwrap();
    assert_eq!(r.sent, 20);
    let log = f.fs.lock().unwrap().log.clone();
    let n = |c: &str| log.iter().filter(|l| l.starts_with(c)).count();
    assert_eq!(n("MKD"), 2, "{log:?}"); // A and A/B made once
    assert_eq!(n("STOR"), 20);
    assert_eq!(n("SIZE"), 40); // name-still-free check before RNTO (R10 HIGH-1) + post-upload verify
    assert!(n("LIST") <= 2);
    assert!(labels.last().unwrap().contains("MB/s"));
}

/// Benchmark against a real local server: BENCH_PORT=2121 BENCH_SRC=/path cargo test --release --lib ftp::tests::bench -- --ignored --nocapture
#[test]
#[ignore]
fn bench() {
    let port: u16 = std::env::var("BENCH_PORT").unwrap().parse().unwrap();
    let src = PathBuf::from(std::env::var("BENCH_SRC").unwrap());
    let par = std::env::var("BENCH_PAR").is_ok();
    let dest = segs(&std::env::var("BENCH_DEST").unwrap_or("/Hdd1".into()));
    let r = send(&con(port), &src, &dest, false, par, &mut |_, _| {}, &|| {
        false
    })
    .unwrap();
    println!(
        "BENCH files={} bytes={} secs={:.2} MBps={:.1} conns={}",
        r.sent,
        r.bytes,
        r.seconds,
        r.bytes as f64 / 1048576.0 / r.seconds,
        r.connections
    );
}

#[test]
fn custom_destination_rules() {
    assert_eq!(
        join_remote(&custom_dest("/Hdd1/My Apps/x").unwrap()),
        "/Hdd1/My Apps/x"
    );
    assert_eq!(
        join_remote(&custom_dest("Usb0\\Games").unwrap()),
        "/Usb0/Games"
    );
    for bad in [
        "",
        "/",
        "/1Hdd/x",
        "/Hdd-1/x",
        "/Hdd1/../x",
        "/Hdd1/a:b",
        "/Hdd1/a*b",
        &format!("/Hdd1/{}", "x".repeat(43)),
        "/Hdd1/a\r\nDELE x",
    ] {
        assert!(custom_dest(bad).is_err(), "{bad:?}");
    }
    assert!(destination("dlc", "1bad", Path::new("/a"), Some("4D5307E6"), None).is_err());
}

#[test]
fn send_to_edited_destination_and_missing_drive() {
    let f = fake(true, true);
    let d = tmp("edited");
    let src = d.join("DLC1");
    std::fs::write(&src, b"dlcdata").unwrap();
    let dest = custom_dest("/Usb0/My DLC/halo").unwrap();
    let r = send(
        &con(f.port),
        &src,
        &dest,
        false,
        false,
        &mut |_, _| {},
        &|| false,
    )
    .unwrap();
    assert_eq!(r.dest, "/Usb0/My DLC/halo");
    assert_eq!(
        f.fs.lock().unwrap().files["/Usb0/My DLC/halo/DLC1"],
        b"dlcdata"
    );
    let e = send(
        &con(f.port),
        &src,
        &custom_dest("/Usb7/x").unwrap(),
        false,
        false,
        &mut |_, _| {},
        &|| false,
    )
    .err()
    .unwrap();
    assert!(e.contains("Usb7"), "{e}");
}

// ---- Overseer R10 regressions ----

#[test]
fn r10_high1_failed_list_never_overwrites() {
    // Overseer repro: LIST answers 425, an existing /Hdd1/Games/game.bin must survive.
    let f = fake(true, true);
    {
        let mut fs = f.fs.lock().unwrap();
        fs.dirs.insert("/Hdd1/Games".into());
        fs.files.insert(
            "/Hdd1/Games/game.bin".into(),
            b"existing-18-bytes!".to_vec(),
        );
        fs.list_fail = true;
    }
    let d = tmp("r10-list");
    std::fs::write(d.join("game.bin"), vec![1u8; 5000]).unwrap();
    let e = send(
        &con(f.port),
        &d.join("game.bin"),
        &segs("/Hdd1/Games"),
        false,
        false,
        &mut |_, _| {},
        &|| false,
    )
    .err()
    .unwrap();
    assert!(e.contains("Couldn't list"), "{e}");
    let fs = f.fs.lock().unwrap();
    assert_eq!(fs.files["/Hdd1/Games/game.bin"], b"existing-18-bytes!");
    assert!(!fs
        .log
        .iter()
        .any(|l| l.starts_with("STOR") || l.starts_with("RNTO")));
}

#[test]
fn r10_high1_missing_folder_still_sends() {
    let f = fake(true, true);
    let d = tmp("r10-missing");
    std::fs::write(d.join("a.bin"), b"abc").unwrap();
    let r = send(
        &con(f.port),
        &d.join("a.bin"),
        &segs("/Hdd1/New/Deeper"),
        false,
        false,
        &mut |_, _| {},
        &|| false,
    )
    .unwrap();
    assert_eq!(r.sent, 1);
    assert_eq!(f.fs.lock().unwrap().files["/Hdd1/New/Deeper/a.bin"], b"abc");
}

#[test]
fn r10_high1_listing_over_cap_is_an_error() {
    let f = fake(true, true);
    f.fs.lock().unwrap().huge_list = true;
    let d = tmp("r10-huge");
    std::fs::write(d.join("a.bin"), b"abc").unwrap();
    let e = send(
        &con(f.port),
        &d.join("a.bin"),
        &segs("/Hdd1"),
        false,
        false,
        &mut |_, _| {},
        &|| false,
    )
    .err()
    .unwrap();
    assert!(e.contains("16 MB"), "{e}");
    assert!(!f
        .fs
        .lock()
        .unwrap()
        .log
        .iter()
        .any(|l| l.starts_with("STOR")));
}

#[test]
fn r10_high1_file_appearing_before_rename_is_kept() {
    // The file shows up after the plan (another client): the check right before RNTO refuses.
    let f = fake(true, true);
    let d = tmp("r10-race");
    let data = vec![3u8; 8 << 20];
    std::fs::write(d.join("race.bin"), &data).unwrap();
    let fs2 = f.fs.clone();
    let mut planted = false;
    let e = send(
        &con(f.port),
        &d.join("race.bin"),
        &segs("/Usb0"),
        false,
        false,
        &mut |_, _| {
            if !planted {
                planted = true;
                fs2.lock()
                    .unwrap()
                    .files
                    .insert("/Usb0/race.bin".into(), b"theirs".to_vec());
            }
        },
        &|| false,
    )
    .err()
    .unwrap();
    assert!(e.contains("appeared on the console"), "{e}");
    let fs = f.fs.lock().unwrap();
    assert_eq!(fs.files["/Usb0/race.bin"], b"theirs");
    assert!(!fs.log.iter().any(|l| l.starts_with("RNTO")));
}

#[test]
fn r10_medium1_stale_part_restarts_from_zero() {
    // Overseer repro: a foreign 3000-byte "ZZZ…" .part must not be resumed.
    let f = fake(true, true);
    f.fs.lock()
        .unwrap()
        .files
        .insert("/Usb0/game.bin.part".into(), vec![b'Z'; 3000]);
    let d = tmp("r10-stale");
    let data: Vec<u8> = (0..5000u32).map(|i| (i % 253) as u8).collect();
    std::fs::write(d.join("game.bin"), &data).unwrap();
    let r = send(
        &con(f.port),
        &d.join("game.bin"),
        &segs("/Usb0"),
        false,
        false,
        &mut |_, _| {},
        &|| false,
    )
    .unwrap();
    assert_eq!((r.sent, r.resumed, r.bytes), (1, 0, 5000));
    assert_eq!(f.fs.lock().unwrap().files["/Usb0/game.bin"], data);
}

#[test]
fn r10_medium1_matching_part_with_same_head_but_wrong_tail_restarts() {
    let f = fake(true, true);
    let data: Vec<u8> = (0..400_000u32).map(|i| (i % 249) as u8).collect();
    let mut part = data[..200_000].to_vec();
    let n = part.len();
    part[n - 10] ^= 0xff; // corrupt near the end of the .part
    f.fs.lock()
        .unwrap()
        .files
        .insert("/Usb0/v.bin.part".into(), part);
    let d = tmp("r10-tail");
    std::fs::write(d.join("v.bin"), &data).unwrap();
    let r = send(
        &con(f.port),
        &d.join("v.bin"),
        &segs("/Usb0"),
        false,
        false,
        &mut |_, _| {},
        &|| false,
    )
    .unwrap();
    assert_eq!(r.resumed, 0);
    assert_eq!(f.fs.lock().unwrap().files["/Usb0/v.bin"], data);
}

#[test]
fn r10_low2_rest_refused_is_not_counted_as_resumed() {
    let f = fake(true, true);
    let data: Vec<u8> = (0..100_000u32).map(|i| (i % 241) as u8).collect();
    {
        let mut fs = f.fs.lock().unwrap();
        fs.files
            .insert("/Usb0/r.bin.part".into(), data[..40_000].to_vec()); // a genuine prefix
        fs.no_rest = true;
    }
    let d = tmp("r10-rest");
    std::fs::write(d.join("r.bin"), &data).unwrap();
    let r = send(
        &con(f.port),
        &d.join("r.bin"),
        &segs("/Usb0"),
        false,
        false,
        &mut |_, _| {},
        &|| false,
    )
    .unwrap();
    assert_eq!((r.sent, r.resumed, r.bytes), (1, 0, 100_000));
    assert_eq!(f.fs.lock().unwrap().files["/Usb0/r.bin"], data);
}

#[test]
fn r10_low1_endless_reply_line_is_refused() {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        if let Ok((mut c, _)) = l.accept() {
            let _ = c.write_all(b"220 ");
            let chunk = vec![b'A'; 64 * 1024];
            for _ in 0..64 {
                if c.write_all(&chunk).is_err() {
                    break;
                }
            } // 4 MB, no newline
        }
    });
    let e = Ftp::connect(con(port).addr, "xboxftp", "xboxftp")
        .err()
        .unwrap();
    assert!(e.contains("too long"), "{e}");
}

#[test]
fn r10_low3_temp_dirs_are_removed() {
    let p = {
        let t = tmp("r10-gone");
        std::fs::write(t.join("x"), b"x").unwrap();
        t.to_path_buf()
    };
    assert!(!p.exists());
}

/// Cancel a send part-way so a local resume record is written; returns (fake, src, data, partial).
fn r11_cancelled(name: &str) -> (Fake, crate::testutil::TmpDir, Vec<u8>, usize) {
    let f = fake(true, true);
    let d = tmp(name);
    let data: Vec<u8> = (0..24_000_000u32).map(|i| (i % 251) as u8).collect();
    std::fs::write(d.join("m.bin"), &data).unwrap();
    let n = std::cell::Cell::new(0u32);
    let e = send(
        &con(f.port),
        &d.join("m.bin"),
        &segs("/Usb0"),
        false,
        false,
        &mut |_, _| {},
        &|| {
            n.set(n.get() + 1);
            n.get() > 3
        },
    )
    .err()
    .unwrap();
    assert_eq!(e, crate::archive::ERR_CANCELLED);
    std::thread::sleep(Duration::from_millis(300));
    let partial = f.fs.lock().unwrap().files["/Usb0/m.bin.part"].len();
    assert!(partial > 0 && partial < data.len());
    (f, d, data, partial)
}

#[test]
fn r11_record_resumes_and_is_removed_after() {
    let (f, d, data, partial) = r11_cancelled("r11-ok");
    let rp = super::record_path(
        &con(f.port).resume_dir.unwrap(),
        con(f.port).addr,
        &segs("/Usb0/m.bin.part"),
    );
    assert!(rp.exists());
    let r = send(
        &con(f.port),
        &d.join("m.bin"),
        &segs("/Usb0"),
        false,
        false,
        &mut |_, _| {},
        &|| false,
    )
    .unwrap();
    assert_eq!(
        (r.sent, r.resumed, r.bytes),
        (1, 1, (data.len() - partial) as u64)
    );
    assert_eq!(f.fs.lock().unwrap().files["/Usb0/m.bin"], data);
    assert!(!rp.exists(), "record removed after success");
}

#[test]
fn r11_middle_byte_differs_restarts_from_zero() {
    let (f, d, mut data, partial) = r11_cancelled("r11-mid");
    // Change one byte in the middle of the already-sent part; keep size and mtime the same.
    let p = d.join("m.bin");
    let mtime = std::fs::metadata(&p).unwrap().modified().unwrap();
    data[partial / 2] ^= 0xff;
    std::fs::write(&p, &data).unwrap();
    std::fs::File::options()
        .write(true)
        .open(&p)
        .unwrap()
        .set_modified(mtime)
        .unwrap();
    let r = send(
        &con(f.port),
        &p,
        &segs("/Usb0"),
        false,
        false,
        &mut |_, _| {},
        &|| false,
    )
    .unwrap();
    assert_eq!((r.resumed, r.bytes), (0, data.len() as u64));
    assert_eq!(f.fs.lock().unwrap().files["/Usb0/m.bin"], data);
}

#[test]
fn r11_part_size_not_matching_record_restarts() {
    let (f, d, data, _) = r11_cancelled("r11-size");
    f.fs.lock()
        .unwrap()
        .files
        .get_mut("/Usb0/m.bin.part")
        .unwrap()
        .push(b'?');
    let r = send(
        &con(f.port),
        &d.join("m.bin"),
        &segs("/Usb0"),
        false,
        false,
        &mut |_, _| {},
        &|| false,
    )
    .unwrap();
    assert_eq!(r.resumed, 0);
    assert_eq!(f.fs.lock().unwrap().files["/Usb0/m.bin"], data);
}

#[test]
fn r11_changed_mtime_restarts() {
    let (f, d, data, _) = r11_cancelled("r11-mtime");
    let p = d.join("m.bin");
    std::fs::File::options()
        .write(true)
        .open(&p)
        .unwrap()
        .set_modified(std::time::SystemTime::now() + Duration::from_secs(60))
        .unwrap();
    let r = send(
        &con(f.port),
        &p,
        &segs("/Usb0"),
        false,
        false,
        &mut |_, _| {},
        &|| false,
    )
    .unwrap();
    assert_eq!(r.resumed, 0);
    assert_eq!(f.fs.lock().unwrap().files["/Usb0/m.bin"], data);
}

#[test]
fn r11_size_is_verified_after_rename() {
    let f = fake(true, true);
    f.fs.lock().unwrap().rnto_truncate = true;
    let d = tmp("r11-rnto");
    std::fs::write(d.join("a.bin"), vec![7u8; 5000]).unwrap();
    let e = send(
        &con(f.port),
        &d.join("a.bin"),
        &segs("/Usb0"),
        false,
        false,
        &mut |_, _| {},
        &|| false,
    )
    .err()
    .unwrap();
    assert!(
        e.contains("after the rename") && e.contains("1 bytes"),
        "{e}"
    );
}

#[test]
fn full_drive_reply_is_recognised() {
    assert!(reply_means_full(552, "Requested file action aborted"));
    assert!(reply_means_full(452, "Insufficient storage space"));
    assert!(reply_means_full(550, "No space left on device"));
    assert!(!reply_means_full(550, "File unavailable"));
    assert!(!reply_means_full(226, "stored"));
    assert!(storage_full_message("game.xex").contains("drive is full"));
}

#[test]
fn full_drive_final_reply_gives_full_message_not_generic_failure() {
    let f = fake(true, true);
    f.fs.lock().unwrap().disk_full = 1;
    let d = tmp("full-final");
    std::fs::write(d.join("big.bin"), vec![1u8; 4000]).unwrap();
    let e = send(
        &con(f.port),
        &d.join("big.bin"),
        &segs("/Usb0"),
        false,
        false,
        &mut |_, _| {},
        &|| false,
    )
    .err()
    .unwrap();
    assert!(
        e.contains("drive is full") && e.contains("big.bin.part"),
        "{e}"
    );
    assert!(!f.fs.lock().unwrap().files.contains_key("/Usb0/big.bin"));
}

#[test]
fn full_drive_refused_stor_gives_full_message() {
    let f = fake(true, true);
    f.fs.lock().unwrap().disk_full = 2;
    let d = tmp("full-refuse");
    std::fs::write(d.join("big.bin"), vec![1u8; 4000]).unwrap();
    let e = send(
        &con(f.port),
        &d.join("big.bin"),
        &segs("/Usb0"),
        false,
        false,
        &mut |_, _| {},
        &|| false,
    )
    .err()
    .unwrap();
    assert!(e.contains("drive is full"), "{e}");
}

#[test]
fn dropped_connection_is_called_a_drop() {
    for k in [
        std::io::ErrorKind::BrokenPipe,
        std::io::ErrorKind::ConnectionReset,
        std::io::ErrorKind::ConnectionAborted,
    ] {
        let m = io_err(std::io::Error::from(k));
        assert!(m.contains("dropped") && m.contains("resumes"), "{m}");
    }
}

/// An existing DashLaunch launch.ini on the console is never replaced, and nothing else is sent either.
#[test]
fn existing_different_launch_ini_is_never_replaced() {
    let f = fake(true, true);
    let d = tmp("launch-ini");
    std::fs::write(d.join("launch.ini"), b"new ini, longer").unwrap();
    std::fs::write(d.join("a.xex"), b"aaa").unwrap();
    let name = d.file_name().unwrap().to_string_lossy().into_owned();
    {
        let mut fs = f.fs.lock().unwrap();
        fs.dirs.insert("/Hdd1/Apps".into());
        fs.dirs.insert(format!("/Hdd1/Apps/{name}"));
        fs.files
            .insert(format!("/Hdd1/Apps/{name}/launch.ini"), b"mine".to_vec());
    }
    let e = send(
        &con(f.port),
        &d,
        &segs("/Hdd1/Apps"),
        false,
        false,
        &mut |_, _| {},
        &|| false,
    )
    .err()
    .unwrap();
    assert!(e.contains("different size"), "{e}");
    let fs = f.fs.lock().unwrap();
    assert_eq!(fs.files[&format!("/Hdd1/Apps/{name}/launch.ini")], b"mine");
    assert!(
        !fs.log
            .iter()
            .any(|l| l.starts_with("STOR") || l.starts_with("DELE") || l.starts_with("RNTO")),
        "{:?}",
        fs.log
    );
}
