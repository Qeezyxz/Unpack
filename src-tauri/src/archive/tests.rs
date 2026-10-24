//! Integration-style tests against real archives made on the build box.
//! Set UNPACK_TESTDATA to the folder holding plain.zip, pw.7z, evil.zip, big.7z, ...
use super::*;
use std::path::PathBuf;

fn data(name: &str) -> PathBuf {
    match name {
        "symlink.zip" | "wrapped.zip" | "big.7z" => return fixture(name),
        _ => {}
    }
    PathBuf::from(std::env::var("UNPACK_TESTDATA").unwrap_or("/workspace/testdata".into())).join(name)
}

// ---------- generated fixtures (no external files needed) ----------

/// Minimal stored (uncompressed) ZIP writer. `mode` = Unix mode bits (0 = plain file).
pub(crate) fn write_zip(path: &Path, entries: &[(&str, &[u8], u32)]) {
    let mut out: Vec<u8> = Vec::new();
    let mut central: Vec<u8> = Vec::new();
    for (name, body, mode) in entries {
        let crc = crc32fast::hash(body);
        let off = out.len() as u32;
        let n = name.as_bytes();
        let le16 = |v: &mut Vec<u8>, x: u16| v.extend_from_slice(&x.to_le_bytes());
        let le32 = |v: &mut Vec<u8>, x: u32| v.extend_from_slice(&x.to_le_bytes());
        out.extend_from_slice(b"PK\x03\x04");
        le16(&mut out, 20); le16(&mut out, 0x0800); le16(&mut out, 0); le16(&mut out, 0); le16(&mut out, 0x21);
        le32(&mut out, crc); le32(&mut out, body.len() as u32); le32(&mut out, body.len() as u32);
        le16(&mut out, n.len() as u16); le16(&mut out, 0);
        out.extend_from_slice(n); out.extend_from_slice(body);
        central.extend_from_slice(b"PK\x01\x02");
        le16(&mut central, if *mode != 0 { 0x0314 } else { 20 }); le16(&mut central, 20);
        le16(&mut central, 0x0800); le16(&mut central, 0); le16(&mut central, 0); le16(&mut central, 0x21);
        le32(&mut central, crc); le32(&mut central, body.len() as u32); le32(&mut central, body.len() as u32);
        le16(&mut central, n.len() as u16); le16(&mut central, 0); le16(&mut central, 0); le16(&mut central, 0); le16(&mut central, 0);
        le32(&mut central, mode << 16); le32(&mut central, off);
        central.extend_from_slice(n);
    }
    let cd_off = out.len() as u32;
    let cd_len = central.len() as u32;
    out.extend_from_slice(&central);
    out.extend_from_slice(b"PK\x05\x06");
    out.extend_from_slice(&[0, 0, 0, 0]);
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&cd_len.to_le_bytes());
    out.extend_from_slice(&cd_off.to_le_bytes());
    out.extend_from_slice(&[0, 0]);
    fs::write(path, out).unwrap();
}

/// Minimal ustar writer: (name, body, typeflag, linkname).
fn write_tar(path: &Path, entries: &[(&str, &[u8], u8, &str)]) {
    let mut out = Vec::new();
    for (name, body, kind, link) in entries {
        let mut h = [0u8; 512];
        h[..name.len()].copy_from_slice(name.as_bytes());
        h[100..107].copy_from_slice(b"0000644");
        h[108..115].copy_from_slice(b"0000000");
        h[116..123].copy_from_slice(b"0000000");
        h[124..135].copy_from_slice(format!("{:011o}", body.len()).as_bytes());
        h[136..147].copy_from_slice(b"00000000000");
        h[148..156].copy_from_slice(b"        ");
        h[156] = *kind;
        h[157..157 + link.len()].copy_from_slice(link.as_bytes());
        h[257..263].copy_from_slice(b"ustar\0");
        h[263..265].copy_from_slice(b"00");
        let sum: u32 = h.iter().map(|&b| b as u32).sum();
        h[148..155].copy_from_slice(format!("{:06o}\0", sum).as_bytes());
        out.extend_from_slice(&h);
        out.extend_from_slice(body);
        out.resize(out.len().div_ceil(512) * 512, 0);
    }
    out.resize(out.len() + 1024, 0);
    fs::write(path, out).unwrap();
}

fn fixture_dir() -> PathBuf {
    let d = crate::testutil::fixture_root();
    fs::create_dir_all(&d).unwrap();
    d
}

/// Build a fixture on first use. Panics (= test FAILS) if it can't be made; never skips.
fn fixture(name: &str) -> PathBuf {
    static LOCK: Mutex<()> = Mutex::new(());
    let _g = LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let p = fixture_dir().join(name);
    if p.exists() {
        return p;
    }
    let part = fixture_dir().join(format!("{name}.part"));
    match name {
        "symlink.zip" => write_zip(&part, &[
            ("link", b"/tmp", 0o120777),
            ("link/unpack_symlink_pwned.txt", b"pwned", 0o100600),
        ]),
        "wrapped.zip" => write_zip(&part, &[("MyGame/", b"", 0o40755), ("MyGame/readme.txt", b"hi", 0)]),
        "big.7z" => {
            // 300 MiB of incompressible data, stored (-mx=0), built with the bundled 7-Zip.
            let src = fixture_dir().join("big-src");
            let _ = fs::remove_dir_all(&src);
            fs::create_dir_all(&src).unwrap();
            {
                use std::io::Write;
                let mut f = std::io::BufWriter::new(fs::File::create(src.join("big.bin")).unwrap());
                let mut x: u64 = 0x9E3779B97F4A7C15;
                let mut buf = vec![0u8; 1 << 20];
                for _ in 0..300 {
                    for c in buf.chunks_mut(8) {
                        x ^= x << 13; x ^= x >> 7; x ^= x << 17;
                        c.copy_from_slice(&x.to_le_bytes());
                    }
                    f.write_all(&buf).unwrap();
                }
            }
            let st = std::process::Command::new(exe())
                .args(["a", "-t7z", "-mx=0", "-bso0", "-bsp0"])
                .arg(&part)
                .arg(src.join("big.bin"))
                .status()
                .expect("run 7za to build big.7z");
            assert!(st.success(), "could not build big.7z fixture");
            fs::remove_dir_all(&src).unwrap();
            // 7-Zip refuses to write a .part name with no 7z ext? it appends nothing; check
            assert!(part.exists() || part.with_extension("part.7z").exists());
            if !part.exists() {
                fs::rename(part.with_extension("part.7z"), &part).unwrap();
            }
        }
        other => panic!("unknown fixture {other}"),
    }
    fs::rename(&part, &p).unwrap();
    p
}
fn exe() -> PathBuf {
    find_7za().expect("7za")
}
fn tmp(name: &str) -> crate::testutil::TmpDir {
    crate::testutil::TmpDir::new(&format!("test-{name}"))
}
fn run(archive: &str, dest: &Path, pw: Option<&str>, c: Collision, l: Layout) -> Result<ExtractResult, String> {
    let a = data(archive);
    let opts = ExtractOptions { archive: &a, dest_parent: dest, password: pw, collision: c, layout: l, skip_junk: true, nested: false };
    extract(&exe(), &opts, &Jobs::default(), "t", &mut |_, _| {})
}
fn no_staging_left(dir: &Path) -> bool {
    fs::read_dir(dir).unwrap().flatten().all(|e| !e.file_name().to_string_lossy().starts_with(".unpack-tmp"))
}

#[test]
fn lists_plain_zip_without_dirs_as_files() {
    let l = list(&exe(), &data("plain.zip"), None).unwrap();
    let files: Vec<_> = l.entries.iter().filter(|e| !e.dir).map(|e| e.path.clone()).collect();
    assert_eq!(files.len(), 4, "{files:?}");
    assert!(files.contains(&"nested/deep/er/d.txt".to_string()));
    assert_eq!(l.format, "zip");
}

#[test]
fn password_required_then_wrong_then_right() {
    assert_eq!(list(&exe(), &data("pw.7z"), None).unwrap_err(), ERR_PASSWORD_REQUIRED);
    assert_eq!(list(&exe(), &data("pw.7z"), Some("nope")).unwrap_err(), ERR_WRONG_PASSWORD);
    assert_eq!(list(&exe(), &data("pw.7z"), Some("secret")).unwrap().entries.iter().filter(|e| !e.dir).count(), 4);
}

#[test]
fn extract_password_7z_and_zip() {
    for a in ["pw.7z", "pwnohe.7z", "pw.zip"] {
        let d = tmp(&format!("pw-{a}"));
        let e = run(a, &d, None, Collision::Number, Layout::Folder).unwrap_err();
        assert!(e == ERR_PASSWORD_REQUIRED || e == ERR_WRONG_PASSWORD, "{a}: {e}");
        assert!(no_staging_left(&d), "staging left after failure for {a}");
        let r = run(a, &d, Some("secret"), Collision::Number, Layout::Folder).unwrap();
        let stem = a.trim_end_matches(".7z").trim_end_matches(".zip");
        assert_eq!(fs::read_to_string(d.join(stem).join("a.txt")).unwrap(), "hello\n", "{a}");
        assert_eq!(r.files, 4);
        assert!(no_staging_left(&d));
    }
}

#[test]
fn nested_folders_and_html_names_survive() {
    let d = tmp("nested");
    run("plain.7z", &d, None, Collision::Number, Layout::Folder).unwrap();
    assert_eq!(fs::read_to_string(d.join("plain/nested/deep/er/d.txt")).unwrap(), "deep\n");
    // Linux allows < > in names; on Windows 7-Zip replaces them with '_'.
    assert!(fs::read_dir(d.join("plain")).unwrap().count() >= 3);
}

#[test]
fn zip_slip_entries_stay_inside() {
    let d = tmp("evil");
    let outer = d.join("outer");
    fs::create_dir_all(&outer).unwrap();
    let r = run("evil.zip", &outer, None, Collision::Number, Layout::Folder).unwrap();
    // nothing may appear in d or above
    let names: Vec<_> = fs::read_dir(&d).unwrap().flatten().map(|e| e.file_name()).collect();
    assert_eq!(names, vec![std::ffi::OsString::from("outer")]);
    assert!(!Path::new("/abs_evil.txt").exists());
    assert!(!std::env::temp_dir().join("evil_escape.txt").exists());
    assert!(Path::new(&r.output).starts_with(&outer));
    assert!(outer.join("evil/evil_escape.txt").exists());
}

#[test]
fn symlink_escape_is_neutralised() {
    let d = tmp("sym");
    let _ = run("symlink.zip", &d, None, Collision::Number, Layout::Folder);
    let mut stack = vec![d.clone()];
    while let Some(p) = stack.pop() {
        for e in fs::read_dir(&p).unwrap().flatten() {
            let m = fs::symlink_metadata(e.path()).unwrap();
            assert!(!m.file_type().is_symlink(), "symlink survived: {:?}", e.path());
            if m.is_dir() { stack.push(e.path()) }
        }
    }
    assert!(!Path::new("/tmp/unpack_symlink_pwned.txt").exists());
}

#[test]
fn collisions_number_skip_replace() {
    let d = tmp("coll");
    let r1 = run("plain.zip", &d, None, Collision::Number, Layout::Folder).unwrap();
    let r2 = run("plain.zip", &d, None, Collision::Number, Layout::Folder).unwrap();
    assert!(r1.output.ends_with("plain") && r2.output.ends_with("plain-2"), "{} {}", r1.output, r2.output);
    let r3 = run("plain.zip", &d, None, Collision::Skip, Layout::Folder).unwrap();
    assert!(r3.skipped);
    fs::write(d.join("plain/a.txt"), "changed").unwrap();
    fs::write(d.join("plain/keep.txt"), "mine").unwrap();
    run("plain.zip", &d, None, Collision::Replace, Layout::Folder).unwrap();
    assert_eq!(fs::read_to_string(d.join("plain/a.txt")).unwrap(), "hello\n");
    assert_eq!(fs::read_to_string(d.join("plain/keep.txt")).unwrap(), "mine");
    assert!(no_staging_left(&d));
}

#[test]
fn single_root_folder_is_not_double_nested() {
    let d = tmp("wrap");
    let r = run("wrapped.zip", &d, None, Collision::Number, Layout::Folder).unwrap();
    assert!(r.output.ends_with("MyGame"), "{}", r.output);
    assert!(d.join("MyGame/readme.txt").exists());
}

#[test]
fn extract_here_places_items_with_numbering() {
    let d = tmp("here");
    fs::write(d.join("a.txt"), "existing").unwrap();
    let r = run("plain.zip", &d, None, Collision::Number, Layout::Here).unwrap();
    assert_eq!(fs::read_to_string(d.join("a.txt")).unwrap(), "existing");
    assert_eq!(fs::read_to_string(d.join("a-2.txt")).unwrap(), "hello\n");
    assert!(d.join("nested/n.json").exists());
    assert!(r.warnings.iter().any(|w| w.contains("a-2.txt")));
}

#[test]
fn big_archive_streams_with_progress_and_cancel() {
    let d = tmp("big");
    let a = data("big.7z");
    let opts = ExtractOptions { archive: &a, dest_parent: &d, password: None, collision: Collision::Number, layout: Layout::Folder, skip_junk: true, nested: false };
    let mut seen = Vec::new();
    let r = extract(&exe(), &opts, &Jobs::default(), "big", &mut |p, _| seen.push(p)).unwrap();
    assert_eq!(fs::metadata(Path::new(&r.output).join("big.bin")).unwrap().len(), 300 * 1024 * 1024);
    assert!(seen.len() >= 2 && *seen.last().unwrap() == 100, "{seen:?}");
    fs::remove_dir_all(&d).unwrap();

    // cancel half-way
    let d = tmp("bigcancel");
    let jobs = Jobs::default();
    jobs.register("c");
    let j2 = jobs.clone();
    let opts = ExtractOptions { archive: &a, dest_parent: &d, password: None, collision: Collision::Number, layout: Layout::Folder, skip_junk: true, nested: false };
    // cancel as soon as the first progress report arrives (deterministic, unlike a timer)
    let e = extract(&exe(), &opts, &jobs, "c", &mut |_, _| { j2.cancel("c"); }).unwrap_err();
    assert_eq!(e, ERR_CANCELLED);
    assert_eq!(fs::read_dir(&d).unwrap().count(), 0, "cancel must leave nothing behind");
    fs::remove_dir_all(&d).unwrap();
}

#[test]
fn read_entry_literal_names_and_limit() {
    let b = read_entry(&exe(), &data("plain.zip"), "nested/n.json", None, 1024).unwrap();
    assert_eq!(String::from_utf8(b).unwrap(), "{\"k\":1}\n");
    assert_eq!(read_entry(&exe(), &data("big.7z"), "big.bin", None, 1024).unwrap_err(), "TOO_LARGE");
}

#[test]
fn helpers() {
    assert_eq!(parse_progress(" 44% 3 - big.bin"), Some((44, "big.bin".into())));
    assert_eq!(parse_progress("  0%"), Some((0, String::new())));
    assert_eq!(parse_progress("hello"), None);
    assert_eq!(safe_segment("con.txt"), "_con.txt");
    // R8: same reserved-name rule as downloads
    assert!(safe_segment("CONIN$").starts_with('_'));
    assert!(safe_segment("conout$.log").starts_with('_'));
    assert!(safe_segment("nul .txt").starts_with('_'));
    assert!(safe_segment("LPT\u{00B9}.dat").starts_with('_'));
    assert_eq!(safe_segment("a:b?.  "), "a_b_");
    assert_eq!(safe_segment(".."), "file");
    assert_eq!(clean_entry_path("../../x/./y\\..\\z"), "x/y/z");
    assert_eq!(archive_stem(Path::new("/a/Game.v1.tar.gz")), "Game.v1");
}

#[test]
fn create_zip_and_7z_with_password_roundtrip() {
    let d = tmp("create");
    let src = data("src");
    for (fmt, pw) in [("zip", None), ("7z", None), ("zip", Some("s3cret")), ("7z", Some("s3cret"))] {
        let out = d.join(format!("made-{}-{}.{fmt}", fmt, pw.is_some()));
        let mut pct = Vec::new();
        create(&exe(), &[src.join("a.txt"), src.join("nested")], &out, fmt, pw, 5, &Jobs::default(), "mk", &mut |p, _| pct.push(p)).unwrap();
        assert!(out.exists());
        let l = list(&exe(), &out, pw);
        let l = l.unwrap();
        let names: Vec<_> = l.entries.iter().filter(|e| !e.dir).map(|e| e.path.as_str()).collect();
        assert!(names.contains(&"a.txt") && names.contains(&"nested/deep/er/d.txt"), "{fmt}: {names:?}");
        if let Some(_) = pw {
            assert!(l.encrypted, "{fmt} should be encrypted");
            if fmt == "7z" {
                // names hidden too
                assert_eq!(list(&exe(), &out, None).unwrap_err(), ERR_PASSWORD_REQUIRED);
            } else {
                assert!(read_entry(&exe(), &out, "a.txt", None, 1024).is_err());
            }
            assert_eq!(read_entry(&exe(), &out, "a.txt", pw, 1024).unwrap(), b"hello\n");
        }
        assert_eq!(*pct.last().unwrap(), 100);
    }
    // existing file is replaced, not "updated"
    let out = d.join("made-zip-false.zip");
    create(&exe(), &[src.join("a.txt")], &out, "zip", None, 5, &Jobs::default(), "mk2", &mut |_, _| {}).unwrap();
    assert_eq!(list(&exe(), &out, None).unwrap().entries.len(), 1);
    // refuse to write the archive inside a folder being archived
    let e = create(&exe(), &[d.clone()], &d.join("x.zip"), "zip", None, 5, &Jobs::default(), "mk3", &mut |_, _| {}).unwrap_err();
    assert!(e.contains("inside"), "{e}");
    assert!(fs::read_dir(&d).unwrap().flatten().all(|e| !e.file_name().to_string_lossy().starts_with(".unpack-new")));
    fs::remove_dir_all(&d).unwrap();
}

#[test]
fn awkward_passwords_and_paths_roundtrip() {
    // spaces + trailing backslash: broken by MSVC-style quoting on Windows if not handled
    let d = tmp("awk");
    let sub = d.join("dir with spaces");
    fs::create_dir_all(&sub).unwrap();
    for pw in ["a b\\", "x\\\\ y", "ünï cødé", "p%PATH%&|^"] {
        let out = sub.join("out file.7z");
        create(&exe(), &[data("src").join("a.txt")], &out, "7z", Some(pw), 1, &Jobs::default(), "awk", &mut |_, _| {}).unwrap();
        assert_eq!(read_entry(&exe(), &out, "a.txt", Some(pw), 1024).unwrap(), b"hello\n", "pw {pw:?}");
        assert_eq!(read_entry(&exe(), &out, "a.txt", Some("wrong"), 1024).unwrap_err(), ERR_WRONG_PASSWORD);
    }
    if cfg!(windows) {
        assert!(check_password(Some("has\"quote")).is_err());
    }
    fs::remove_dir_all(&d).unwrap();
}

// ---------- regression tests for the Overseer review ----------

fn files_under(root: &Path) -> Vec<String> {
    let mut v = Vec::new();
    let mut st = vec![root.to_path_buf()];
    while let Some(d) = st.pop() {
        for e in fs::read_dir(&d).unwrap().flatten() {
            if e.path().is_dir() { st.push(e.path()) } else { v.push(e.path().strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/")) }
        }
    }
    v.sort();
    v
}

/// H1: many parallel extractions of two *different* archives with the same name must each
/// get their own folder; nothing may be merged.
#[test]
fn h1_parallel_same_name_extractions_never_merge() {
    let d = tmp("h1");
    let (a, b) = (d.join("src-a"), d.join("src-b"));
    fs::create_dir_all(&a).unwrap();
    fs::create_dir_all(&b).unwrap();
    write_zip(&a.join("same.zip"), &[("from-a.txt", b"A", 0), ("x/a2.txt", b"A2", 0)]);
    write_zip(&b.join("same.zip"), &[("from-b.txt", b"B", 0), ("x/b2.txt", b"B2", 0)]);
    let out = d.join("out");
    fs::create_dir_all(&out).unwrap();
    let mut hs = Vec::new();
    for i in 0..12 {
        let src = if i % 2 == 0 { a.join("same.zip") } else { b.join("same.zip") };
        let out = out.clone();
        hs.push(std::thread::spawn(move || {
            let opts = ExtractOptions { archive: &src, dest_parent: &out, password: None, collision: Collision::Number, layout: Layout::Folder, skip_junk: true, nested: false };
            extract(&find_7za().unwrap(), &opts, &Jobs::default(), &format!("j{i}"), &mut |_, _| {}).unwrap().output
        }));
    }
    let outs: std::collections::BTreeSet<String> = hs.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(outs.len(), 12, "two jobs reported the same folder: {outs:?}");
    let dirs: Vec<_> = fs::read_dir(&out).unwrap().flatten().collect();
    assert_eq!(dirs.len(), 12);
    for e in dirs {
        let f = files_under(&e.path());
        assert!(f == ["from-a.txt", "x/a2.txt"] || f == ["from-b.txt", "x/b2.txt"], "merged folder {:?}: {f:?}", e.path());
    }
    // claim_dir itself: racing claims always get distinct folders
    let c = d.join("claims");
    fs::create_dir_all(&c).unwrap();
    let hs: Vec<_> = (0..16).map(|_| { let c = c.clone(); std::thread::spawn(move || claim_dir(&c, "x").unwrap()) }).collect();
    let got: std::collections::BTreeSet<_> = hs.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(got.len(), 16);
    fs::remove_dir_all(&d).unwrap();
}

/// M1: "Replace" must replace links in the destination, never write through them.
#[cfg(unix)]
#[test]
fn m1_replace_does_not_follow_symlinks() {
    let d = tmp("m1");
    let victim = d.join("victim");
    fs::create_dir_all(victim.join("nested")).unwrap();
    fs::write(victim.join("a.txt"), "victim").unwrap();
    fs::write(victim.join("nested/n.json"), "victim").unwrap();
    let out = d.join("out");
    fs::create_dir_all(&out).unwrap();
    // case 1: the destination folder itself is a link
    std::os::unix::fs::symlink(&victim, out.join("plain")).unwrap();
    run("plain.zip", &out, None, Collision::Replace, Layout::Folder).unwrap();
    assert!(!fs::symlink_metadata(out.join("plain")).unwrap().file_type().is_symlink());
    assert_eq!(fs::read_to_string(out.join("plain/a.txt")).unwrap(), "hello\n");
    // case 2: a sub-folder and a file inside it are links
    fs::remove_dir_all(out.join("plain/nested")).unwrap();
    std::os::unix::fs::symlink(victim.join("nested"), out.join("plain/nested")).unwrap();
    fs::remove_file(out.join("plain/a.txt")).unwrap();
    std::os::unix::fs::symlink(victim.join("a.txt"), out.join("plain/a.txt")).unwrap();
    run("plain.zip", &out, None, Collision::Replace, Layout::Folder).unwrap();
    // case 3: Extract-here replace onto a link
    let here = d.join("here");
    fs::create_dir_all(&here).unwrap();
    std::os::unix::fs::symlink(&victim, here.join("nested")).unwrap();
    run("plain.zip", &here, None, Collision::Replace, Layout::Here).unwrap();
    assert!(here.join("nested/n.json").exists() && !fs::symlink_metadata(here.join("nested")).unwrap().file_type().is_symlink());
    assert_eq!(fs::read_to_string(victim.join("a.txt")).unwrap(), "victim");
    assert_eq!(fs::read_to_string(victim.join("nested/n.json")).unwrap(), "victim");
    assert_eq!(files_under(&victim), ["a.txt", "nested/n.json"]);
    fs::remove_dir_all(&d).unwrap();
}

/// M3: asking for an entry that isn't there is an error, not an empty file.
#[test]
fn m3_missing_entry_is_an_error() {
    let d = tmp("m3");
    let e = read_entry(&exe(), &data("plain.zip"), "does/not/exist.txt", None, 1024).unwrap_err();
    assert!(e.contains("not found"), "{e}");
    let e = read_entry(&exe(), &data("plain.zip"), "nested", None, 1024).unwrap_err();
    assert!(e.contains("folder"), "{e}");
    let out = d.join("saved.txt");
    let e = extract_entry_to(&exe(), &data("plain.zip"), "nope.txt", None, &out, &Jobs::default(), "m3").unwrap_err();
    assert!(e.contains("not found"), "{e}");
    assert!(!out.exists());
    assert_eq!(fs::read_dir(&d).unwrap().count(), 0, "no temp file left");
    fs::remove_dir_all(&d).unwrap();
}

/// L1: entry names with quotes, leading dashes, wildcards and spaces are fetched exactly.
#[test]
fn l1_awkward_entry_names_are_literal() {
    let d = tmp("l1");
    let z = d.join("names.zip");
    write_zip(&z, &[
        ("a \"q\" b.txt", b"QUOTE", 0),
        ("-x.txt", b"DASH", 0),
        ("*.txt", b"STAR", 0),
        ("dir/e f\\.txt", b"BACKSLASH", 0),
        ("dir/ünï.txt", b"UNI", 0),
    ]);
    for (n, want) in [("a \"q\" b.txt", "QUOTE"), ("-x.txt", "DASH"), ("*.txt", "STAR"), ("dir/ünï.txt", "UNI")] {
        assert_eq!(read_entry(&exe(), &z, n, None, 1024).unwrap(), want.as_bytes(), "{n}");
        let out = d.join("o.bin");
        extract_entry_to(&exe(), &z, n, None, &out, &Jobs::default(), "l1").unwrap();
        assert_eq!(fs::read(&out).unwrap(), want.as_bytes(), "{n}");
    }
    fs::remove_dir_all(&d).unwrap();
}

/// L2: a cancel that lands before 7-Zip is attached still cancels, and leaves nothing.
#[test]
fn l2_cancel_before_attach() {
    let d = tmp("l2");
    let jobs = Jobs::default();
    jobs.register("x");
    jobs.cancel("x");
    let out = d.join("o.bin");
    let e = extract_entry_to(&exe(), &data("big.7z"), "big.bin", None, &out, &jobs, "x").unwrap_err();
    assert_eq!(e, ERR_CANCELLED);
    assert_eq!(fs::read_dir(&d).unwrap().count(), 0);
    fs::remove_dir_all(&d).unwrap();
}

/// L4: a tar whose hard link points outside the archive still extracts the rest.
#[test]
fn l4_bad_hard_link_is_a_warning() {
    let d = tmp("l4");
    let t = d.join("hl.tar");
    write_tar(&t, &[("pkg/ok.txt", b"ok", b'0', ""), ("pkg/hl", b"", b'1', "/etc/hostname")]);
    let out = d.join("out");
    let opts = ExtractOptions { archive: &t, dest_parent: &out, password: None, collision: Collision::Number, layout: Layout::Folder, skip_junk: true, nested: false };
    let r = extract(&exe(), &opts, &Jobs::default(), "l4", &mut |_, _| {}).unwrap();
    assert_eq!(fs::read_to_string(Path::new(&r.output).join("ok.txt")).unwrap(), "ok");
    assert!(r.warnings.iter().any(|w| w.contains("hard link")), "{:?}", r.warnings);
    fs::remove_dir_all(&d).unwrap();
}

/// M4 support: read_file is limited to archive names.
#[test]
fn read_file_allow_list() {
    assert!(is_archive_name("C:\\x\\Game.RAR") && is_archive_name("/a/b.tar.gz") && is_archive_name("a.7z"));
    assert!(!is_archive_name("/etc/passwd") && !is_archive_name("C:\\Users\\me\\secrets.txt") && !is_archive_name("x.iso"));
}

/// Overseer C1 regression: the ISO list is the exact set of paths this extraction placed.
/// Pre-existing ISOs in a Replace-merged folder (same name, other subfolder) are never listed,
/// and a merge is flagged so the UI turns delete-ISO off.
#[test]
fn c1_iso_list_is_exact_and_replace_is_flagged() {
    let d = tmp("c1");
    let src = d.join("src");
    fs::create_dir_all(src.join("disc")).unwrap();
    fs::write(src.join("game.iso"), b"top").unwrap();
    fs::write(src.join("disc/game.iso"), b"nested").unwrap();
    let zip = d.join("merge.zip");
    let st = std::process::Command::new(exe()).current_dir(&src).args(["a", "-tzip", "-bso0"]).arg(&zip).args(["game.iso", "disc"]).status().unwrap();
    assert!(st.success());
    let out = d.join("out");
    fs::create_dir_all(out.join("merge/keep")).unwrap();
    fs::write(out.join("merge/keep/game.iso"), b"MINE").unwrap();
    fs::write(out.join("merge/disc2.iso"), b"MINE2").unwrap();
    let ex = |c| { let o = ExtractOptions { archive: &zip, dest_parent: &out, password: None, collision: c, layout: Layout::Folder, skip_junk: true, nested: false }; extract(&exe(), &o, &Jobs::default(), "c1", &mut |_, _| {}).unwrap() };
    let r = ex(Collision::Replace);
    assert!(r.merged);
    let mut got = r.isos.clone(); got.sort();
    let mut want = vec![out.join("merge").join("disc").join("game.iso").display().to_string(), out.join("merge").join("game.iso").display().to_string()]; want.sort();
    assert_eq!(got, want);
    assert_eq!(fs::read(out.join("merge/keep/game.iso")).unwrap(), b"MINE");
    let r = ex(Collision::Number);
    assert!(!r.merged);
    assert!(r.isos.iter().all(|p| p.contains("merge-2")), "{:?}", r.isos);
    assert_eq!(r.isos.len(), 2);
    fs::remove_dir_all(&d).unwrap();
}

/// Overseer round-3 H1/M2: Rust alone decides delete-allowed. A Replace-mode extraction into a
/// *fresh* folder still never allows deleting; Number mode does.
#[test]
fn r3_replace_mode_never_allows_delete() {
    let d = tmp("r3rep");
    let src = d.join("src");
    fs::create_dir_all(&src).unwrap();
    let mk = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/make_iso.py");
    assert!(std::process::Command::new("python3").arg(&mk).arg(src.join("game.iso")).args(["4D5307E6", "0"]).status().map(|s| s.success()).unwrap_or(false) || cfg!(windows));
    if !src.join("game.iso").exists() { return; } // no python under Wine: covered on Linux
    let zip = d.join("g.zip");
    assert!(std::process::Command::new(exe()).current_dir(&src).args(["a", "-tzip", "-bso0"]).arg(&zip).arg("game.iso").status().unwrap().success());
    let out = d.join("out");
    for (c, allowed) in [(Collision::Replace, false), (Collision::Number, true)] {
        let o = ExtractOptions { archive: &zip, dest_parent: &out, password: None, collision: c, layout: Layout::Folder, skip_junk: true, nested: false };
        let r = extract(&exe(), &o, &Jobs::default(), "r", &mut |_, _| {}).unwrap();
        assert!(!r.merged);
        assert_eq!(r.iso_tokens.len(), 1);
        crate::god::seal_tokens(&r.iso_tokens);
        let g = crate::god::convert_registered(&r.iso_tokens[0], true, &Jobs::default(), "g", &mut |_, _| {}).unwrap();
        assert_eq!(g.iso_deleted, allowed, "{c:?}");
        fs::remove_dir_all(&out).unwrap();
    }
    fs::remove_dir_all(&d).unwrap();
}

#[test]
fn r5_skip_does_not_set_created() {
    // Overseer R5 addendum: Skip on an existing folder must not report created=true.
    let d = tmp("r5skip");
    let arch = d.join("secret.zip");
    write_zip(&arch, &[("a.txt", b"x", 0o644)]);
    let out = d.join("out");
    fs::create_dir_all(out.join("secret")).unwrap(); // same name as archive stem
    let opts = ExtractOptions { archive: &arch, dest_parent: &out, password: None, collision: Collision::Skip, layout: Layout::Folder, skip_junk: true, nested: false };
    let r = extract(&exe(), &opts, &Jobs::default(), "s", &mut |_, _| {}).unwrap();
    assert!(r.skipped && !r.created);
    let out2 = d.join("out2");
    fs::create_dir_all(&out2).unwrap();
    let opts = ExtractOptions { archive: &arch, dest_parent: &out2, password: None, collision: Collision::Number, layout: Layout::Folder, skip_junk: true, nested: false };
    let r2 = extract(&exe(), &opts, &Jobs::default(), "s2", &mut |_, _| {}).unwrap();
    assert!(!r2.skipped && r2.created, "claim_dir folder is created");
    let opts = ExtractOptions { archive: &arch, dest_parent: &out2, password: None, collision: Collision::Number, layout: Layout::Here, skip_junk: true, nested: false };
    let r3 = extract(&exe(), &opts, &Jobs::default(), "s3", &mut |_, _| {}).unwrap();
    assert!(!r3.created, "Extract here never sets created");
    fs::remove_dir_all(&d).unwrap();
}

// ---------- nested archives ("Open zips inside zips") ----------

fn zip_bytes(entries: &[(&str, &[u8], u32)]) -> Vec<u8> {
    let p = std::env::temp_dir().join(format!("unpack-zipbytes-{}-{}.zip", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    write_zip(&p, entries);
    let b = fs::read(&p).unwrap();
    let _ = fs::remove_file(p);
    b
}

fn run_nested(archive: &Path, dest: &Path, nested: bool, job: &str) -> Result<ExtractResult, String> {
    let opts = ExtractOptions { archive, dest_parent: dest, password: None, collision: Collision::Number, layout: Layout::Folder, skip_junk: true, nested };
    extract(&exe(), &opts, &Jobs::default(), job, &mut |_, _| {})
}

#[test]
fn nested_zip_inside_zip_opens_when_on_and_is_counted_when_off() {
    let d = tmp("nested-basic");
    let inner = zip_bytes(&[("note.txt", b"inner-secret", 0)]);
    let outer = d.join("outer.zip");
    write_zip(&outer, &[("readme.txt", b"top", 0), ("sub/inner.zip", &inner, 0)]);
    let on = d.join("on");
    let r = run_nested(&outer, &on, true, "n1").unwrap();
    assert_eq!((r.nested, r.inner_left), (1, 0), "{:?}", r.warnings);
    let out = Path::new(&r.output);
    assert_eq!(fs::read_to_string(out.join("sub/inner/note.txt")).unwrap(), "inner-secret");
    assert!(out.join("sub/inner.zip").is_file(), "the inner archive itself is kept");
    assert!(no_staging_left(&on));
    let off = d.join("off");
    let r = run_nested(&outer, &off, false, "n2").unwrap();
    assert_eq!((r.nested, r.inner_left), (0, 1), "turning it off still reports the closed inner archive");
    assert!(!Path::new(&r.output).join("sub/inner").exists());
    assert!(Path::new(&r.output).join("sub/inner.zip").is_file());
}

#[test]
fn nested_depth_and_count_are_limited() {
    let d = tmp("nested-limits");
    // five zips, each holding the next: only three levels are opened
    let mut blob = zip_bytes(&[("core.txt", b"core", 0)]);
    for _ in 0..5 {
        blob = zip_bytes(&[("next.zip", &blob, 0)]);
    }
    let chain = d.join("chain.zip");
    fs::write(&chain, &blob).unwrap();
    let r = run_nested(&chain, &d.join("o1"), true, "d1").unwrap();
    assert_eq!(r.nested, 3, "depth limit: {:?}", r.warnings);
    assert!(r.inner_left >= 1);
    assert!(!files_under(Path::new(&r.output)).iter().any(|f| f.ends_with("core.txt")), "{:?}", files_under(Path::new(&r.output)));
    // 45 siblings: the first 40 are opened, the rest are left closed
    let tiny = zip_bytes(&[("t.txt", b"t", 0)]);
    let names: Vec<String> = (0..45).map(|i| format!("z{i:02}.zip")).collect();
    let entries: Vec<(&str, &[u8], u32)> = names.iter().map(|n| (n.as_str(), tiny.as_slice(), 0)).collect();
    let many = d.join("many.zip");
    write_zip(&many, &entries);
    let r = run_nested(&many, &d.join("o2"), true, "d2").unwrap();
    assert_eq!((r.nested, r.inner_left), (40, 5));
    assert!(no_staging_left(&d.join("o1")) && no_staging_left(&d.join("o2")));
}

#[test]
fn nested_password_protected_inner_archive_is_left_closed_with_a_warning() {
    let d = tmp("nested-pw");
    let pw7z = fs::read(data("pw.7z")).unwrap();
    let ok = zip_bytes(&[("fine.txt", b"fine", 0)]);
    let outer = d.join("pack.zip");
    write_zip(&outer, &[("locked.7z", &pw7z, 0), ("ok.zip", &ok, 0)]);
    let r = run_nested(&outer, &d.join("out"), true, "p1").unwrap();
    assert_eq!((r.nested, r.inner_left), (1, 1));
    assert!(r.warnings.iter().any(|w| w.contains("locked.7z") && w.contains("password")), "{:?}", r.warnings);
    let out = Path::new(&r.output);
    assert!(out.join("ok/fine.txt").is_file());
    assert!(!out.join("locked").exists(), "no half-made folder for the locked archive");
}

#[test]
fn nested_hostile_inner_archive_stays_inside_the_destination() {
    let d = tmp("nested-evil");
    let evil = fs::read(data("evil.zip")).unwrap();
    let sym = fs::read(fixture("symlink.zip")).unwrap();
    let outer = d.join("outer.zip");
    write_zip(&outer, &[("evil.zip", &evil, 0), ("sym.zip", &sym, 0)]);
    let dest = d.join("dest");
    let r = run_nested(&outer, &dest, true, "e1").unwrap();
    assert_eq!(r.nested, 2, "{:?}", r.warnings);
    let names: Vec<_> = fs::read_dir(&d).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    assert!(names.iter().all(|n| n == "dest" || n == "outer.zip"), "nothing may appear next to the destination: {names:?}");
    let mut stack = vec![PathBuf::from(&r.output)];
    while let Some(p) = stack.pop() {
        for e in fs::read_dir(&p).unwrap().flatten() {
            let m = fs::symlink_metadata(e.path()).unwrap();
            assert!(!m.file_type().is_symlink(), "symlink survived: {:?}", e.path());
            if m.is_dir() { stack.push(e.path()) }
        }
    }
    assert!(!Path::new("/tmp/unpack_symlink_pwned.txt").exists());
}

#[test]
fn nested_works_with_extract_here_and_cancel_leaves_nothing() {
    let d = tmp("nested-here");
    let inner = zip_bytes(&[("deep.txt", b"deep", 0)]);
    let outer = d.join("outer.zip");
    write_zip(&outer, &[("inner.zip", &inner, 0)]);
    let dest = d.join("here");
    fs::create_dir_all(&dest).unwrap();
    let opts = ExtractOptions { archive: &outer, dest_parent: &dest, password: None, collision: Collision::Number, layout: Layout::Here, skip_junk: true, nested: true };
    let r = extract(&exe(), &opts, &Jobs::default(), "h1", &mut |_, _| {}).unwrap();
    assert_eq!(r.nested, 1);
    assert_eq!(fs::read_to_string(dest.join("inner/deep.txt")).unwrap(), "deep");
    // a cancel that arrives while the inner archive is opened removes everything
    let d2 = d.join("cancel");
    fs::create_dir_all(&d2).unwrap();
    let jobs = Jobs::default();
    jobs.register("c1");
    let j2 = jobs.clone();
    let opts = ExtractOptions { archive: &outer, dest_parent: &d2, password: None, collision: Collision::Number, layout: Layout::Folder, skip_junk: true, nested: true };
    let e = extract(&exe(), &opts, &jobs, "c1", &mut |_, f| { if f.starts_with("Opening inner") { j2.cancel("c1"); } }).unwrap_err();
    assert_eq!(e, ERR_CANCELLED);
    assert_eq!(fs::read_dir(&d2).unwrap().count(), 0, "cancel must leave nothing behind");
}

#[test]
fn nested_archive_name_list_is_narrow() {
    assert!(is_nested_archive("Pack.ZIP") && is_nested_archive("a.tar.gz") && is_nested_archive("a.7z") && is_nested_archive("a.rar"));
    assert!(!is_nested_archive("data.cab") && !is_nested_archive("disc.iso") && !is_nested_archive("readme.txt"));
}

#[test]
fn replacing_a_file_keeps_the_old_one_when_the_swap_fails() {
    let d = tmp("replace-safe");
    let dst = d.join("out.zip");
    fs::write(&dst, "old").unwrap();
    // the temp file vanished: the swap fails, the previous file is still there and nothing is left over
    let e = replace_file_safely(&d.join(".unpack-new-x.zip"), &dst).unwrap_err();
    assert!(e.contains("Could not save"), "{e}");
    assert_eq!(fs::read_to_string(&dst).unwrap(), "old");
    assert_eq!(fs::read_dir(&d).unwrap().count(), 1, "no aside file left behind");
    // success path replaces, leaves no aside
    let t = d.join(".unpack-new-y.zip");
    fs::write(&t, "new").unwrap();
    replace_file_safely(&t, &dst).unwrap();
    assert_eq!(fs::read_to_string(&dst).unwrap(), "new");
    assert_eq!(fs::read_dir(&d).unwrap().count(), 1);
    // a folder is never swapped for a file, and the temp file is cleaned up
    let folder = d.join("folder");
    fs::create_dir(&folder).unwrap();
    let t = d.join(".unpack-new-z.zip");
    fs::write(&t, "x").unwrap();
    assert!(replace_file_safely(&t, &folder).unwrap_err().contains("is a folder"));
    assert!(folder.is_dir() && !t.exists());
    // a link at the destination is moved aside, its target is untouched
    #[cfg(unix)]
    {
        let victim = d.join("victim.txt");
        fs::write(&victim, "victim").unwrap();
        let link = d.join("link.zip");
        std::os::unix::fs::symlink(&victim, &link).unwrap();
        let t = d.join(".unpack-new-w.zip");
        fs::write(&t, "fresh").unwrap();
        replace_file_safely(&t, &link).unwrap();
        assert_eq!(fs::read_to_string(&victim).unwrap(), "victim");
        assert!(!fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        assert_eq!(fs::read_to_string(&link).unwrap(), "fresh");
    }
}

// ---------------- Round 15 regressions ----------------

/// R15 M2: a few-KB zip holding a 7z of 2 GiB of zeros must not be opened (Overseer wrote 2.15 GB).
#[test]
fn r15_m2_nested_zip_bomb_is_left_closed() {
    use std::io::Write;
    let d = tmp("bomb");
    let inner = d.join("zeros.7z");
    let mut c = std::process::Command::new(exe())
        .args(["a", "-t7z", "-mx=1", "-mmt=1", "-md=1m", "-sizeros.bin"]).arg(&inner)
        .stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
        .spawn().unwrap();
    {
        let mut si = c.stdin.take().unwrap();
        let z = vec![0u8; 1 << 20];
        for _ in 0..2048 { si.write_all(&z).unwrap(); }
    }
    assert!(c.wait().unwrap().success());
    let inner_bytes = fs::read(&inner).unwrap();
    fs::remove_file(&inner).unwrap();
    let outer = d.join("bomb.zip");
    write_zip(&outer, &[("zeros.7z", &inner_bytes, 0)]);
    assert!(fs::metadata(&outer).unwrap().len() < 1 << 20, "bomb outer is small");
    let dest = d.join("out");
    fs::create_dir_all(&dest).unwrap();
    let opts = ExtractOptions { archive: &outer, dest_parent: &dest, password: None, collision: Collision::Number, layout: Layout::Folder, skip_junk: true, nested: true };
    let r = extract(&exe(), &opts, &Jobs::default(), "bomb", &mut |_, _| {}).unwrap();
    assert_eq!((r.nested, r.inner_left), (0, 1), "{r:?}");
    assert!(r.warnings.iter().any(|w| w.contains("zeros.7z was left closed") && w.contains("zip bomb")), "{:?}", r.warnings);
    assert!(super::tree_bytes(&dest) < 2 << 20, "nothing big written");
    assert!(no_staging_left(&dest));
}

#[test]
fn r15_m2_nested_limits_budget_free_space_unknown() {
    let e = |size: u64| Entry { path: "f".into(), size, dir: false, encrypted: false };
    let l = |v: Vec<Entry>| Listing { entries: v, format: "zip".into(), encrypted: false };
    // fine: 10 MB from 5 MB
    assert_eq!(super::nested_refusal(5 << 20, &l(vec![e(10 << 20)]), 0, 1 << 30, Some(1 << 40)), Ok(10 << 20));
    // ratio
    assert!(super::nested_refusal(100 << 10, &l(vec![e(100 << 20)]), 0, 1 << 40, None).unwrap_err().contains("zip bomb"));
    // total budget across inners
    assert!(super::nested_refusal(400 << 20, &l(vec![e(400 << 20)]), 200 << 20, 512 << 20, None).unwrap_err().contains("in total"));
    // free space
    assert!(super::nested_refusal(100 << 20, &l(vec![e(100 << 20)]), 0, 1 << 40, Some(200 << 20)).unwrap_err().contains("free space"));
    // unknown size
    assert!(super::nested_refusal(1 << 20, &l(vec![e(0)]), 0, 1 << 40, None).unwrap_err().contains("can't be read"));
}

/// R16 MEDIUM-1: a .gz whose footer claims 1000 bytes but holds 512 MiB of zeros is stopped while
/// 7-Zip runs; the peak written stays near the live cap (2x listed + 1 MB), far below the real size.
#[test]
fn r16_m1_forged_gz_size_is_capped_live() {
    use std::io::Write;
    let d = tmp("forged-gz");
    let gz = d.join("zeros.gz");
    let mut c = std::process::Command::new(exe())
        .args(["a", "-tgzip", "-mx=1", "-sizeros.bin"]).arg(&gz)
        .stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
        .spawn().unwrap();
    {
        let mut si = c.stdin.take().unwrap();
        let z = vec![0u8; 1 << 20];
        for _ in 0..512 { si.write_all(&z).unwrap(); }
    }
    assert!(c.wait().unwrap().success());
    let mut b = fs::read(&gz).unwrap();
    fs::remove_file(&gz).unwrap();
    let n = b.len();
    b[n - 4..].copy_from_slice(&1000u32.to_le_bytes()); // forged ISIZE footer
    let outer = d.join("forged.zip");
    write_zip(&outer, &[("zeros.gz", &b, 0)]);
    let dest = d.join("out");
    fs::create_dir_all(&dest).unwrap();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (s2, d2) = (stop.clone(), dest.clone());
    let peak = std::thread::spawn(move || {
        let mut peak = 0u64;
        while !s2.load(std::sync::atomic::Ordering::SeqCst) { peak = peak.max(super::tree_bytes(&d2)); std::thread::sleep(std::time::Duration::from_millis(2)); }
        peak
    });
    let opts = ExtractOptions { archive: &outer, dest_parent: &dest, password: None, collision: Collision::Number, layout: Layout::Folder, skip_junk: true, nested: true };
    let r = extract(&exe(), &opts, &Jobs::default(), "forged", &mut |_, _| {}).unwrap();
    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    let peak = peak.join().unwrap();
    assert_eq!((r.nested, r.inner_left), (0, 1), "{r:?}");
    assert!(r.warnings.iter().any(|w| w.contains("zeros.gz was left closed") && w.contains("stopped")), "{:?}", r.warnings);
    // outer gz (~0.5 MB) + cap (~1 MB) + what 7-Zip can write in one 20 ms poll
    assert!(peak < 96 << 20, "peak {peak} bytes written");
    assert!(super::tree_bytes(&dest) < 4 << 20, "partial output removed");
    assert!(no_staging_left(&dest));
}

/// R18 L2: an inner tar with 100,500 tiny files is refused from its listing (file cap), before
/// anything is written.
#[test]
fn r18_l2_inner_file_count_cap_from_listing() {
    fn hdr(name: &str, size: usize) -> [u8; 512] {
        let mut h = [0u8; 512];
        h[..name.len()].copy_from_slice(name.as_bytes());
        h[100..108].copy_from_slice(b"0000644\0");
        h[108..116].copy_from_slice(b"0000000\0");
        h[116..124].copy_from_slice(b"0000000\0");
        h[124..136].copy_from_slice(format!("{size:011o}\0").as_bytes());
        h[136..148].copy_from_slice(b"00000000000\0");
        h[156] = b'0';
        h[257..263].copy_from_slice(b"ustar\0");
        h[263..265].copy_from_slice(b"00");
        h[148..156].copy_from_slice(b"        ");
        let sum: u32 = h.iter().map(|&b| b as u32).sum();
        h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        h
    }
    let n = super::NESTED_MAX_FILES + 500;
    let mut tar = Vec::with_capacity(n * 1024 + 1024);
    for i in 0..n {
        tar.extend_from_slice(&hdr(&format!("f/{i}"), 1));
        let mut b = [0u8; 512];
        b[0] = b'x';
        tar.extend_from_slice(&b);
    }
    tar.extend_from_slice(&[0u8; 1024]);
    let d = tmp("filecap");
    let outer = d.join("many.zip");
    write_zip(&outer, &[("many.tar", &tar, 0)]);
    drop(tar);
    let dest = d.join("out");
    fs::create_dir_all(&dest).unwrap();
    let opts = ExtractOptions { archive: &outer, dest_parent: &dest, password: None, collision: Collision::Number, layout: Layout::Folder, skip_junk: true, nested: true };
    let r = extract(&exe(), &opts, &Jobs::default(), "filecap", &mut |_, _| {}).unwrap();
    assert_eq!((r.nested, r.inner_left), (0, 1), "{r:?}");
    assert!(r.warnings.iter().any(|w| w.contains("many.tar") && w.contains("100500 files")), "{:?}", r.warnings);
    assert!(super::count_files(&dest) < 5, "nothing unpacked from the tar");
    assert!(no_staging_left(&dest));
}
