//! Integration-style tests against real archives made on the build box.
//! Set UNPACK_TESTDATA to the folder holding plain.zip, pw.7z, evil.zip, big.7z, ...
use super::*;
use std::path::{Path, PathBuf};

fn with_type_lock<F, R>(ct: &Path, f: F) -> R
where
    F: FnOnce(&FolderLock) -> R,
{
    let lock = lock_type_folder(ct, &|| false, &mut || {}).unwrap();
    f(&lock)
}

fn recover_asides_locked(ct: &Path, w: &mut Vec<String>) {
    with_type_lock(ct, |lock| recover_asides(ct, lock, w));
}

fn replace_unit_locked(ct: &Path, old: [Option<PathBuf>; 2], new: [(&PathBuf, &PathBuf); 2]) -> Result<Vec<String>, String> {
    with_type_lock(ct, |lock| replace_unit(ct, lock, old, new))
}

fn data(name: &str) -> PathBuf {
    match name {
        "symlink.zip" | "wrapped.zip" | "big.7z" => return fixture(name),
        _ => {}
    }
    PathBuf::from(std::env::var("UNPACK_TESTDATA").unwrap_or("/workspace/testdata".into()))
        .join(name)
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
        le16(&mut out, 20);
        le16(&mut out, 0x0800);
        le16(&mut out, 0);
        le16(&mut out, 0);
        le16(&mut out, 0x21);
        le32(&mut out, crc);
        le32(&mut out, body.len() as u32);
        le32(&mut out, body.len() as u32);
        le16(&mut out, n.len() as u16);
        le16(&mut out, 0);
        out.extend_from_slice(n);
        out.extend_from_slice(body);
        central.extend_from_slice(b"PK\x01\x02");
        le16(&mut central, if *mode != 0 { 0x0314 } else { 20 });
        le16(&mut central, 20);
        le16(&mut central, 0x0800);
        le16(&mut central, 0);
        le16(&mut central, 0);
        le16(&mut central, 0x21);
        le32(&mut central, crc);
        le32(&mut central, body.len() as u32);
        le32(&mut central, body.len() as u32);
        le16(&mut central, n.len() as u16);
        le16(&mut central, 0);
        le16(&mut central, 0);
        le16(&mut central, 0);
        le16(&mut central, 0);
        le32(&mut central, mode << 16);
        le32(&mut central, off);
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
        "symlink.zip" => write_zip(
            &part,
            &[
                ("link", b"/tmp", 0o120777),
                ("link/unpack_symlink_pwned.txt", b"pwned", 0o100600),
            ],
        ),
        "wrapped.zip" => write_zip(
            &part,
            &[("MyGame/", b"", 0o40755), ("MyGame/readme.txt", b"hi", 0)],
        ),
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
                        x ^= x << 13;
                        x ^= x >> 7;
                        x ^= x << 17;
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
fn run(
    archive: &str,
    dest: &Path,
    pw: Option<&str>,
    c: Collision,
    l: Layout,
) -> Result<ExtractResult, String> {
    let a = data(archive);
    let opts = ExtractOptions {
        archive: &a,
        dest_parent: dest,
        password: pw,
        collision: c,
        layout: l,
        skip_junk: true,
        nested: false,
    };
    extract(&exe(), &opts, &Jobs::default(), "t", &mut |_, _| {})
}
fn no_staging_left(dir: &Path) -> bool {
    fs::read_dir(dir)
        .unwrap()
        .flatten()
        .all(|e| !e.file_name().to_string_lossy().starts_with(".unpack-tmp"))
}

#[test]
fn lists_plain_zip_without_dirs_as_files() {
    let l = list(&exe(), &data("plain.zip"), None).unwrap();
    let files: Vec<_> = l
        .entries
        .iter()
        .filter(|e| !e.dir)
        .map(|e| e.path.clone())
        .collect();
    assert_eq!(files.len(), 4, "{files:?}");
    assert!(files.contains(&"nested/deep/er/d.txt".to_string()));
    assert_eq!(l.format, "zip");
}

#[test]
fn password_required_then_wrong_then_right() {
    assert_eq!(
        list(&exe(), &data("pw.7z"), None).unwrap_err(),
        ERR_PASSWORD_REQUIRED
    );
    assert_eq!(
        list(&exe(), &data("pw.7z"), Some("nope")).unwrap_err(),
        ERR_WRONG_PASSWORD
    );
    assert_eq!(
        list(&exe(), &data("pw.7z"), Some("secret"))
            .unwrap()
            .entries
            .iter()
            .filter(|e| !e.dir)
            .count(),
        4
    );
}

#[test]
fn extract_password_7z_and_zip() {
    for a in ["pw.7z", "pwnohe.7z", "pw.zip"] {
        let d = tmp(&format!("pw-{a}"));
        let e = run(a, &d, None, Collision::Number, Layout::Folder).unwrap_err();
        assert!(
            e == ERR_PASSWORD_REQUIRED || e == ERR_WRONG_PASSWORD,
            "{a}: {e}"
        );
        assert!(no_staging_left(&d), "staging left after failure for {a}");
        let r = run(a, &d, Some("secret"), Collision::Number, Layout::Folder).unwrap();
        let stem = a.trim_end_matches(".7z").trim_end_matches(".zip");
        assert_eq!(
            fs::read_to_string(d.join(stem).join("a.txt")).unwrap(),
            "hello\n",
            "{a}"
        );
        assert_eq!(r.files, 4);
        assert!(no_staging_left(&d));
    }
}

#[test]
fn nested_folders_and_html_names_survive() {
    let d = tmp("nested");
    run("plain.7z", &d, None, Collision::Number, Layout::Folder).unwrap();
    assert_eq!(
        fs::read_to_string(d.join("plain/nested/deep/er/d.txt")).unwrap(),
        "deep\n"
    );
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
    let names: Vec<_> = fs::read_dir(&d)
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .collect();
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
            assert!(
                !m.file_type().is_symlink(),
                "symlink survived: {:?}",
                e.path()
            );
            if m.is_dir() {
                stack.push(e.path())
            }
        }
    }
    assert!(!Path::new("/tmp/unpack_symlink_pwned.txt").exists());
}

#[test]
fn collisions_number_skip_replace() {
    let d = tmp("coll");
    let r1 = run("plain.zip", &d, None, Collision::Number, Layout::Folder).unwrap();
    let r2 = run("plain.zip", &d, None, Collision::Number, Layout::Folder).unwrap();
    assert!(
        r1.output.ends_with("plain") && r2.output.ends_with("plain-2"),
        "{} {}",
        r1.output,
        r2.output
    );
    let r3 = run("plain.zip", &d, None, Collision::Skip, Layout::Folder).unwrap();
    assert!(r3.skipped);
    fs::write(d.join("plain/a.txt"), "changed").unwrap();
    fs::write(d.join("plain/keep.txt"), "mine").unwrap();
    run("plain.zip", &d, None, Collision::Replace, Layout::Folder).unwrap();
    assert_eq!(
        fs::read_to_string(d.join("plain/a.txt")).unwrap(),
        "hello\n"
    );
    assert_eq!(
        fs::read_to_string(d.join("plain/keep.txt")).unwrap(),
        "mine"
    );
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
    let opts = ExtractOptions {
        archive: &a,
        dest_parent: &d,
        password: None,
        collision: Collision::Number,
        layout: Layout::Folder,
        skip_junk: true,
        nested: false,
    };
    let mut seen = Vec::new();
    let r = extract(&exe(), &opts, &Jobs::default(), "big", &mut |p, _| {
        seen.push(p)
    })
    .unwrap();
    assert_eq!(
        fs::metadata(Path::new(&r.output).join("big.bin"))
            .unwrap()
            .len(),
        300 * 1024 * 1024
    );
    assert!(seen.len() >= 2 && *seen.last().unwrap() == 100, "{seen:?}");
    fs::remove_dir_all(&d).unwrap();

    // cancel half-way
    let d = tmp("bigcancel");
    let jobs = Jobs::default();
    jobs.register("c");
    let j2 = jobs.clone();
    let opts = ExtractOptions {
        archive: &a,
        dest_parent: &d,
        password: None,
        collision: Collision::Number,
        layout: Layout::Folder,
        skip_junk: true,
        nested: false,
    };
    // cancel as soon as the first progress report arrives (deterministic, unlike a timer)
    let e = extract(&exe(), &opts, &jobs, "c", &mut |_, _| {
        j2.cancel("c");
    })
    .unwrap_err();
    assert_eq!(e, ERR_CANCELLED);
    assert_eq!(
        fs::read_dir(&d).unwrap().count(),
        0,
        "cancel must leave nothing behind"
    );
    fs::remove_dir_all(&d).unwrap();
}

#[test]
fn read_entry_literal_names_and_limit() {
    let b = read_entry(&exe(), &data("plain.zip"), "nested/n.json", None, 1024).unwrap();
    assert_eq!(String::from_utf8(b).unwrap(), "{\"k\":1}\n");
    assert_eq!(
        read_entry(&exe(), &data("big.7z"), "big.bin", None, 1024).unwrap_err(),
        "TOO_LARGE"
    );
}

#[test]
fn helpers() {
    assert_eq!(
        parse_progress(" 44% 3 - big.bin"),
        Some((44, "big.bin".into()))
    );
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
    for (fmt, pw) in [
        ("zip", None),
        ("7z", None),
        ("zip", Some("s3cret")),
        ("7z", Some("s3cret")),
    ] {
        let out = d.join(format!("made-{}-{}.{fmt}", fmt, pw.is_some()));
        let mut pct = Vec::new();
        create(
            &exe(),
            &[src.join("a.txt"), src.join("nested")],
            &out,
            fmt,
            pw,
            5,
            &Jobs::default(),
            "mk",
            &mut |p, _| pct.push(p),
        )
        .unwrap();
        assert!(out.exists());
        let l = list(&exe(), &out, pw);
        let l = l.unwrap();
        let names: Vec<_> = l
            .entries
            .iter()
            .filter(|e| !e.dir)
            .map(|e| e.path.as_str())
            .collect();
        assert!(
            names.contains(&"a.txt") && names.contains(&"nested/deep/er/d.txt"),
            "{fmt}: {names:?}"
        );
        if pw.is_some() {
            assert!(l.encrypted, "{fmt} should be encrypted");
            if fmt == "7z" {
                // names hidden too
                assert_eq!(list(&exe(), &out, None).unwrap_err(), ERR_PASSWORD_REQUIRED);
            } else {
                assert!(read_entry(&exe(), &out, "a.txt", None, 1024).is_err());
            }
            assert_eq!(
                read_entry(&exe(), &out, "a.txt", pw, 1024).unwrap(),
                b"hello\n"
            );
        }
        assert_eq!(*pct.last().unwrap(), 100);
    }
    // existing file is replaced, not "updated"
    let out = d.join("made-zip-false.zip");
    create(
        &exe(),
        &[src.join("a.txt")],
        &out,
        "zip",
        None,
        5,
        &Jobs::default(),
        "mk2",
        &mut |_, _| {},
    )
    .unwrap();
    assert_eq!(list(&exe(), &out, None).unwrap().entries.len(), 1);
    // refuse to write the archive inside a folder being archived
    let e = create(
        &exe(),
        std::slice::from_ref(&d),
        &d.join("x.zip"),
        "zip",
        None,
        5,
        &Jobs::default(),
        "mk3",
        &mut |_, _| {},
    )
    .unwrap_err();
    assert!(e.contains("inside"), "{e}");
    assert!(fs::read_dir(&d)
        .unwrap()
        .flatten()
        .all(|e| !e.file_name().to_string_lossy().starts_with(".unpack-new")));
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
        create(
            &exe(),
            &[data("src").join("a.txt")],
            &out,
            "7z",
            Some(pw),
            1,
            &Jobs::default(),
            "awk",
            &mut |_, _| {},
        )
        .unwrap();
        assert_eq!(
            read_entry(&exe(), &out, "a.txt", Some(pw), 1024).unwrap(),
            b"hello\n",
            "pw {pw:?}"
        );
        assert_eq!(
            read_entry(&exe(), &out, "a.txt", Some("wrong"), 1024).unwrap_err(),
            ERR_WRONG_PASSWORD
        );
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
            if e.path().is_dir() {
                st.push(e.path())
            } else {
                v.push(
                    e.path()
                        .strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/"),
                )
            }
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
    write_zip(
        &a.join("same.zip"),
        &[("from-a.txt", b"A", 0), ("x/a2.txt", b"A2", 0)],
    );
    write_zip(
        &b.join("same.zip"),
        &[("from-b.txt", b"B", 0), ("x/b2.txt", b"B2", 0)],
    );
    let out = d.join("out");
    fs::create_dir_all(&out).unwrap();
    let mut hs = Vec::new();
    for i in 0..12 {
        let src = if i % 2 == 0 {
            a.join("same.zip")
        } else {
            b.join("same.zip")
        };
        let out = out.clone();
        hs.push(std::thread::spawn(move || {
            let opts = ExtractOptions {
                archive: &src,
                dest_parent: &out,
                password: None,
                collision: Collision::Number,
                layout: Layout::Folder,
                skip_junk: true,
                nested: false,
            };
            extract(
                &find_7za().unwrap(),
                &opts,
                &Jobs::default(),
                &format!("j{i}"),
                &mut |_, _| {},
            )
            .unwrap()
            .output
        }));
    }
    let outs: std::collections::BTreeSet<String> =
        hs.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(
        outs.len(),
        12,
        "two jobs reported the same folder: {outs:?}"
    );
    let dirs: Vec<_> = fs::read_dir(&out).unwrap().flatten().collect();
    assert_eq!(dirs.len(), 12);
    for e in dirs {
        let f = files_under(&e.path());
        assert!(
            f == ["from-a.txt", "x/a2.txt"] || f == ["from-b.txt", "x/b2.txt"],
            "merged folder {:?}: {f:?}",
            e.path()
        );
    }
    // claim_dir itself: racing claims always get distinct folders
    let c = d.join("claims");
    fs::create_dir_all(&c).unwrap();
    let hs: Vec<_> = (0..16)
        .map(|_| {
            let c = c.clone();
            std::thread::spawn(move || claim_dir(&c, "x").unwrap())
        })
        .collect();
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
    assert!(!fs::symlink_metadata(out.join("plain"))
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(
        fs::read_to_string(out.join("plain/a.txt")).unwrap(),
        "hello\n"
    );
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
    assert!(
        here.join("nested/n.json").exists()
            && !fs::symlink_metadata(here.join("nested"))
                .unwrap()
                .file_type()
                .is_symlink()
    );
    assert_eq!(fs::read_to_string(victim.join("a.txt")).unwrap(), "victim");
    assert_eq!(
        fs::read_to_string(victim.join("nested/n.json")).unwrap(),
        "victim"
    );
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
    let e = extract_entry_to(
        &exe(),
        &data("plain.zip"),
        "nope.txt",
        None,
        &out,
        &Jobs::default(),
        "m3",
    )
    .unwrap_err();
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
    write_zip(
        &z,
        &[
            ("a \"q\" b.txt", b"QUOTE", 0),
            ("-x.txt", b"DASH", 0),
            ("*.txt", b"STAR", 0),
            ("dir/e f\\.txt", b"BACKSLASH", 0),
            ("dir/ünï.txt", b"UNI", 0),
        ],
    );
    for (n, want) in [
        ("a \"q\" b.txt", "QUOTE"),
        ("-x.txt", "DASH"),
        ("*.txt", "STAR"),
        ("dir/ünï.txt", "UNI"),
    ] {
        assert_eq!(
            read_entry(&exe(), &z, n, None, 1024).unwrap(),
            want.as_bytes(),
            "{n}"
        );
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
    let e =
        extract_entry_to(&exe(), &data("big.7z"), "big.bin", None, &out, &jobs, "x").unwrap_err();
    assert_eq!(e, ERR_CANCELLED);
    assert_eq!(fs::read_dir(&d).unwrap().count(), 0);
    fs::remove_dir_all(&d).unwrap();
}

/// L4: a tar whose hard link points outside the archive still extracts the rest.
#[test]
fn l4_bad_hard_link_is_a_warning() {
    let d = tmp("l4");
    let t = d.join("hl.tar");
    write_tar(
        &t,
        &[
            ("pkg/ok.txt", b"ok", b'0', ""),
            ("pkg/hl", b"", b'1', "/etc/hostname"),
        ],
    );
    let out = d.join("out");
    let opts = ExtractOptions {
        archive: &t,
        dest_parent: &out,
        password: None,
        collision: Collision::Number,
        layout: Layout::Folder,
        skip_junk: true,
        nested: false,
    };
    let r = extract(&exe(), &opts, &Jobs::default(), "l4", &mut |_, _| {}).unwrap();
    assert_eq!(
        fs::read_to_string(Path::new(&r.output).join("ok.txt")).unwrap(),
        "ok"
    );
    assert!(
        r.warnings.iter().any(|w| w.contains("hard link")),
        "{:?}",
        r.warnings
    );
    fs::remove_dir_all(&d).unwrap();
}

/// M4 support: read_file is limited to archive names.
#[test]
fn read_file_allow_list() {
    assert!(
        is_archive_name("C:\\x\\Game.RAR")
            && is_archive_name("/a/b.tar.gz")
            && is_archive_name("a.7z")
    );
    assert!(
        !is_archive_name("/etc/passwd")
            && !is_archive_name("C:\\Users\\me\\secrets.txt")
            && !is_archive_name("x.iso")
    );
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
    let st = std::process::Command::new(exe())
        .current_dir(&src)
        .args(["a", "-tzip", "-bso0"])
        .arg(&zip)
        .args(["game.iso", "disc"])
        .status()
        .unwrap();
    assert!(st.success());
    let out = d.join("out");
    fs::create_dir_all(out.join("merge/keep")).unwrap();
    fs::write(out.join("merge/keep/game.iso"), b"MINE").unwrap();
    fs::write(out.join("merge/disc2.iso"), b"MINE2").unwrap();
    let ex = |c| {
        let o = ExtractOptions {
            archive: &zip,
            dest_parent: &out,
            password: None,
            collision: c,
            layout: Layout::Folder,
            skip_junk: true,
            nested: false,
        };
        extract(&exe(), &o, &Jobs::default(), "c1", &mut |_, _| {}).unwrap()
    };
    let r = ex(Collision::Replace);
    assert!(r.merged);
    let mut got = r.isos.clone();
    got.sort();
    let mut want = vec![
        out.join("merge")
            .join("disc")
            .join("game.iso")
            .display()
            .to_string(),
        out.join("merge").join("game.iso").display().to_string(),
    ];
    want.sort();
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
    assert!(
        std::process::Command::new("python3")
            .arg(&mk)
            .arg(src.join("game.iso"))
            .args(["4D5307E6", "0"])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
            || cfg!(windows)
    );
    if !src.join("game.iso").exists() {
        return;
    } // no python under Wine: covered on Linux
    let zip = d.join("g.zip");
    assert!(std::process::Command::new(exe())
        .current_dir(&src)
        .args(["a", "-tzip", "-bso0"])
        .arg(&zip)
        .arg("game.iso")
        .status()
        .unwrap()
        .success());
    let out = d.join("out");
    for (c, allowed) in [(Collision::Replace, false), (Collision::Number, true)] {
        let o = ExtractOptions {
            archive: &zip,
            dest_parent: &out,
            password: None,
            collision: c,
            layout: Layout::Folder,
            skip_junk: true,
            nested: false,
        };
        let r = extract(&exe(), &o, &Jobs::default(), "r", &mut |_, _| {}).unwrap();
        assert!(!r.merged);
        assert_eq!(r.iso_tokens.len(), 1);
        crate::god::seal_tokens(&r.iso_tokens);
        let g = crate::god::convert_registered(
            &r.iso_tokens[0],
            true,
            &Jobs::default(),
            "g",
            &mut |_, _| {},
        )
        .unwrap();
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
    let opts = ExtractOptions {
        archive: &arch,
        dest_parent: &out,
        password: None,
        collision: Collision::Skip,
        layout: Layout::Folder,
        skip_junk: true,
        nested: false,
    };
    let r = extract(&exe(), &opts, &Jobs::default(), "s", &mut |_, _| {}).unwrap();
    assert!(r.skipped && !r.created);
    let out2 = d.join("out2");
    fs::create_dir_all(&out2).unwrap();
    let opts = ExtractOptions {
        archive: &arch,
        dest_parent: &out2,
        password: None,
        collision: Collision::Number,
        layout: Layout::Folder,
        skip_junk: true,
        nested: false,
    };
    let r2 = extract(&exe(), &opts, &Jobs::default(), "s2", &mut |_, _| {}).unwrap();
    assert!(!r2.skipped && r2.created, "claim_dir folder is created");
    let opts = ExtractOptions {
        archive: &arch,
        dest_parent: &out2,
        password: None,
        collision: Collision::Number,
        layout: Layout::Here,
        skip_junk: true,
        nested: false,
    };
    let r3 = extract(&exe(), &opts, &Jobs::default(), "s3", &mut |_, _| {}).unwrap();
    assert!(!r3.created, "Extract here never sets created");
    fs::remove_dir_all(&d).unwrap();
}

// ---------- nested archives ("Open zips inside zips") ----------

fn zip_bytes(entries: &[(&str, &[u8], u32)]) -> Vec<u8> {
    let p = std::env::temp_dir().join(format!(
        "unpack-zipbytes-{}-{}.zip",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    write_zip(&p, entries);
    let b = fs::read(&p).unwrap();
    let _ = fs::remove_file(p);
    b
}

fn run_nested(
    archive: &Path,
    dest: &Path,
    nested: bool,
    job: &str,
) -> Result<ExtractResult, String> {
    let opts = ExtractOptions {
        archive,
        dest_parent: dest,
        password: None,
        collision: Collision::Number,
        layout: Layout::Folder,
        skip_junk: true,
        nested,
    };
    extract(&exe(), &opts, &Jobs::default(), job, &mut |_, _| {})
}

#[test]
fn nested_zip_inside_zip_opens_when_on_and_is_counted_when_off() {
    let d = tmp("nested-basic");
    let inner = zip_bytes(&[("note.txt", b"inner-secret", 0)]);
    let outer = d.join("outer.zip");
    write_zip(
        &outer,
        &[("readme.txt", b"top", 0), ("sub/inner.zip", &inner, 0)],
    );
    let on = d.join("on");
    let r = run_nested(&outer, &on, true, "n1").unwrap();
    assert_eq!((r.nested, r.inner_left), (1, 0), "{:?}", r.warnings);
    let out = Path::new(&r.output);
    assert_eq!(
        fs::read_to_string(out.join("sub/inner/note.txt")).unwrap(),
        "inner-secret"
    );
    assert!(
        out.join("sub/inner.zip").is_file(),
        "the inner archive itself is kept"
    );
    assert!(no_staging_left(&on));
    let off = d.join("off");
    let r = run_nested(&outer, &off, false, "n2").unwrap();
    assert_eq!(
        (r.nested, r.inner_left),
        (0, 1),
        "turning it off still reports the closed inner archive"
    );
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
    assert!(
        !files_under(Path::new(&r.output))
            .iter()
            .any(|f| f.ends_with("core.txt")),
        "{:?}",
        files_under(Path::new(&r.output))
    );
    // 45 siblings: the first 40 are opened, the rest are left closed
    let tiny = zip_bytes(&[("t.txt", b"t", 0)]);
    let names: Vec<String> = (0..45).map(|i| format!("z{i:02}.zip")).collect();
    let entries: Vec<(&str, &[u8], u32)> = names
        .iter()
        .map(|n| (n.as_str(), tiny.as_slice(), 0))
        .collect();
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
    assert!(
        r.warnings
            .iter()
            .any(|w| w.contains("locked.7z") && w.contains("password")),
        "{:?}",
        r.warnings
    );
    let out = Path::new(&r.output);
    assert!(out.join("ok/fine.txt").is_file());
    assert!(
        !out.join("locked").exists(),
        "no half-made folder for the locked archive"
    );
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
    let names: Vec<_> = fs::read_dir(&d)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        names.iter().all(|n| n == "dest" || n == "outer.zip"),
        "nothing may appear next to the destination: {names:?}"
    );
    let mut stack = vec![PathBuf::from(&r.output)];
    while let Some(p) = stack.pop() {
        for e in fs::read_dir(&p).unwrap().flatten() {
            let m = fs::symlink_metadata(e.path()).unwrap();
            assert!(
                !m.file_type().is_symlink(),
                "symlink survived: {:?}",
                e.path()
            );
            if m.is_dir() {
                stack.push(e.path())
            }
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
    let opts = ExtractOptions {
        archive: &outer,
        dest_parent: &dest,
        password: None,
        collision: Collision::Number,
        layout: Layout::Here,
        skip_junk: true,
        nested: true,
    };
    let r = extract(&exe(), &opts, &Jobs::default(), "h1", &mut |_, _| {}).unwrap();
    assert_eq!(r.nested, 1);
    assert_eq!(
        fs::read_to_string(dest.join("inner/deep.txt")).unwrap(),
        "deep"
    );
    // a cancel that arrives while the inner archive is opened removes everything
    let d2 = d.join("cancel");
    fs::create_dir_all(&d2).unwrap();
    let jobs = Jobs::default();
    jobs.register("c1");
    let j2 = jobs.clone();
    let opts = ExtractOptions {
        archive: &outer,
        dest_parent: &d2,
        password: None,
        collision: Collision::Number,
        layout: Layout::Folder,
        skip_junk: true,
        nested: true,
    };
    let e = extract(&exe(), &opts, &jobs, "c1", &mut |_, f| {
        if f.starts_with("Opening inner") {
            j2.cancel("c1");
        }
    })
    .unwrap_err();
    assert_eq!(e, ERR_CANCELLED);
    assert_eq!(
        fs::read_dir(&d2).unwrap().count(),
        0,
        "cancel must leave nothing behind"
    );
}

#[test]
fn nested_archive_name_list_is_narrow() {
    assert!(
        is_nested_archive("Pack.ZIP")
            && is_nested_archive("a.tar.gz")
            && is_nested_archive("a.7z")
            && is_nested_archive("a.rar")
    );
    assert!(
        !is_nested_archive("data.cab")
            && !is_nested_archive("disc.iso")
            && !is_nested_archive("readme.txt")
    );
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
    assert_eq!(
        fs::read_dir(&d).unwrap().count(),
        1,
        "no aside file left behind"
    );
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
    assert!(replace_file_safely(&t, &folder)
        .unwrap_err()
        .contains("is a folder"));
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
        assert!(!fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
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
        .args(["a", "-t7z", "-mx=1", "-mmt=1", "-md=1m", "-sizeros.bin"])
        .arg(&inner)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    {
        let mut si = c.stdin.take().unwrap();
        let z = vec![0u8; 1 << 20];
        for _ in 0..2048 {
            si.write_all(&z).unwrap();
        }
    }
    assert!(c.wait().unwrap().success());
    let inner_bytes = fs::read(&inner).unwrap();
    fs::remove_file(&inner).unwrap();
    let outer = d.join("bomb.zip");
    write_zip(&outer, &[("zeros.7z", &inner_bytes, 0)]);
    assert!(
        fs::metadata(&outer).unwrap().len() < 1 << 20,
        "bomb outer is small"
    );
    let dest = d.join("out");
    fs::create_dir_all(&dest).unwrap();
    let opts = ExtractOptions {
        archive: &outer,
        dest_parent: &dest,
        password: None,
        collision: Collision::Number,
        layout: Layout::Folder,
        skip_junk: true,
        nested: true,
    };
    let r = extract(&exe(), &opts, &Jobs::default(), "bomb", &mut |_, _| {}).unwrap();
    assert_eq!((r.nested, r.inner_left), (0, 1), "{r:?}");
    assert!(
        r.warnings
            .iter()
            .any(|w| w.contains("zeros.7z was left closed") && w.contains("zip bomb")),
        "{:?}",
        r.warnings
    );
    assert!(super::tree_bytes(&dest) < 2 << 20, "nothing big written");
    assert!(no_staging_left(&dest));
}

#[test]
fn r15_m2_nested_limits_budget_free_space_unknown() {
    let e = |size: u64| Entry {
        path: "f".into(),
        size,
        dir: false,
        encrypted: false,
    };
    let l = |v: Vec<Entry>| Listing {
        entries: v,
        format: "zip".into(),
        encrypted: false,
    };
    // fine: 10 MB from 5 MB
    assert_eq!(
        super::nested_refusal(5 << 20, &l(vec![e(10 << 20)]), 0, 1 << 30, Some(1 << 40)),
        Ok(10 << 20)
    );
    // ratio
    assert!(
        super::nested_refusal(100 << 10, &l(vec![e(100 << 20)]), 0, 1 << 40, None)
            .unwrap_err()
            .contains("zip bomb")
    );
    // total budget across inners
    assert!(super::nested_refusal(
        400 << 20,
        &l(vec![e(400 << 20)]),
        200 << 20,
        512 << 20,
        None
    )
    .unwrap_err()
    .contains("in total"));
    // free space
    assert!(super::nested_refusal(
        100 << 20,
        &l(vec![e(100 << 20)]),
        0,
        1 << 40,
        Some(200 << 20)
    )
    .unwrap_err()
    .contains("free space"));
    // unknown size
    assert!(
        super::nested_refusal(1 << 20, &l(vec![e(0)]), 0, 1 << 40, None)
            .unwrap_err()
            .contains("can't be read")
    );
}

/// R16 MEDIUM-1: a .gz whose footer claims 1000 bytes but holds 512 MiB of zeros is stopped while
/// 7-Zip runs; the peak written stays near the live cap (2x listed + 1 MB), far below the real size.
#[test]
fn r16_m1_forged_gz_size_is_capped_live() {
    use std::io::Write;
    let d = tmp("forged-gz");
    let gz = d.join("zeros.gz");
    let mut c = std::process::Command::new(exe())
        .args(["a", "-tgzip", "-mx=1", "-sizeros.bin"])
        .arg(&gz)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    {
        let mut si = c.stdin.take().unwrap();
        let z = vec![0u8; 1 << 20];
        for _ in 0..512 {
            si.write_all(&z).unwrap();
        }
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
        while !s2.load(std::sync::atomic::Ordering::SeqCst) {
            peak = peak.max(super::tree_bytes(&d2));
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        peak
    });
    let opts = ExtractOptions {
        archive: &outer,
        dest_parent: &dest,
        password: None,
        collision: Collision::Number,
        layout: Layout::Folder,
        skip_junk: true,
        nested: true,
    };
    let r = extract(&exe(), &opts, &Jobs::default(), "forged", &mut |_, _| {}).unwrap();
    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    let peak = peak.join().unwrap();
    assert_eq!((r.nested, r.inner_left), (0, 1), "{r:?}");
    assert!(
        r.warnings
            .iter()
            .any(|w| w.contains("zeros.gz was left closed") && w.contains("stopped")),
        "{:?}",
        r.warnings
    );
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
    let opts = ExtractOptions {
        archive: &outer,
        dest_parent: &dest,
        password: None,
        collision: Collision::Number,
        layout: Layout::Folder,
        skip_junk: true,
        nested: true,
    };
    let r = extract(&exe(), &opts, &Jobs::default(), "filecap", &mut |_, _| {}).unwrap();
    assert_eq!((r.nested, r.inner_left), (0, 1), "{r:?}");
    assert!(
        r.warnings
            .iter()
            .any(|w| w.contains("many.tar") && w.contains("100500 files")),
        "{:?}",
        r.warnings
    );
    assert!(
        super::count_files(&dest) < 5,
        "nothing unpacked from the tar"
    );
    assert!(no_staging_left(&dest));
}

/// Justin: extracting into a console Content profile folder places just the GOD <TitleID> tree,
/// with no archive-name or "(GOD)" wrapper folders. Disc 2 joins, the same MediaID is skipped,
/// non-GOD content keeps today's behaviour plus a hint, and normal folders are unchanged.
#[test]
fn content_profile_gets_only_the_title_id_tree() {
    let d = tmp("content-prof");
    let prof = d.join("usb").join("content").join("0000000000000000");
    fs::create_dir_all(&prof).unwrap();
    let ex = |zip: &Path, dest: &Path, c: Collision, l: Layout| {
        let o = ExtractOptions {
            archive: zip,
            dest_parent: dest,
            password: None,
            collision: c,
            layout: l,
            skip_junk: true,
            nested: false,
        };
        extract(&exe(), &o, &Jobs::default(), "cp", &mut |_, _| {}).unwrap()
    };
    let wrapped = d.join("bo3.zip");
    write_zip(
        &wrapped,
        &[
            ("Bo3game (GOD)/415608C3/00007000/AAAA0001", b"hdr1", 0o644),
            (
                "Bo3game (GOD)/415608C3/00007000/AAAA0001.data/Data0000",
                b"d1",
                0o644,
            ),
            ("Bo3game (GOD)/readme.txt", b"hi", 0o644),
        ],
    );
    let r = ex(&wrapped, &prof, Collision::Number, Layout::Folder);
    assert_eq!(names_in_dir(&prof), vec!["415608C3"]);
    assert_eq!(Path::new(&r.output), prof.join("415608C3"));
    assert!(prof
        .join("415608C3/00007000/AAAA0001.data/Data0000")
        .is_file());
    assert!(
        r.warnings.iter().any(|w| w.contains("1 other file")),
        "{:?}",
        r.warnings
    );
    // Disc 2 with a bare TitleID at the archive root joins the existing folder.
    let bare = d.join("disc2.zip");
    write_zip(
        &bare,
        &[
            ("415608C3/00007000/AAAA0002", b"hdr2", 0o644),
            ("415608C3/00007000/AAAA0002.data/Data0000", b"d2", 0o644),
        ],
    );
    let r = ex(&bare, &prof, Collision::Number, Layout::Here);
    assert!(r.merged && !r.skipped);
    assert_eq!(names_in_dir(&prof), vec!["415608C3"]);
    assert_eq!(names_in_dir(&prof.join("415608C3/00007000")).len(), 4);
    // Same MediaID again: skipped (Number and Skip), never numbered or replaced.
    for c in [Collision::Number, Collision::Skip] {
        let r = ex(&wrapped, &prof, c, Layout::Folder);
        assert!(r.skipped, "{c:?}");
        assert!(
            r.warnings
                .iter()
                .any(|w| w.starts_with("Skipped:") && w.contains("AAAA0001")),
            "{:?}",
            r.warnings
        );
        assert_eq!(
            fs::read(prof.join("415608C3/00007000/AAAA0001")).unwrap(),
            b"hdr1"
        );
    }
    // Replace swaps the package (header and data folder) in place.
    let newer = d.join("newer.zip");
    write_zip(
        &newer,
        &[
            ("415608C3/00007000/AAAA0001", b"NEW", 0o644),
            ("415608C3/00007000/AAAA0001.data/Data0000", b"N", 0o644),
        ],
    );
    ex(&newer, &prof, Collision::Replace, Layout::Folder);
    assert_eq!(
        fs::read(prof.join("415608C3/00007000/AAAA0001")).unwrap(),
        b"NEW"
    );
    assert_eq!(names_in_dir(&prof), vec!["415608C3"]);
    // Picking the drive root sends the GOD tree into Content\0000000000000000.
    fs::remove_dir_all(prof.join("415608C3")).unwrap();
    ex(&wrapped, &d.join("usb"), Collision::Number, Layout::Folder);
    assert_eq!(names_in_dir(&prof), vec!["415608C3"]);
    assert_eq!(names_in_dir(&d.join("usb")), vec!["content"]);
    // Non-GOD content: extracted as before, with the hint.
    let plain = d.join("notes.zip");
    write_zip(&plain, &[("a.txt", b"x", 0o644)]);
    let r = ex(&plain, &prof, Collision::Number, Layout::Folder);
    assert!(prof.join("notes/a.txt").is_file());
    assert!(r
        .warnings
        .iter()
        .any(|w| w.contains("doesn't look like a GOD game")));
    // A normal destination is unchanged: the wrapper folder stays.
    let out = d.join("out");
    ex(&wrapped, &out, Collision::Number, Layout::Folder);
    assert!(out
        .join("Bo3game (GOD)/415608C3/00007000/AAAA0001")
        .is_file());
    assert!(no_staging_left(&prof) && no_staging_left(&out));
}

fn names_in_dir(d: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(d)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n != FOLDER_LOCK)
        .collect();
    v.sort();
    v
}

/// Content mode + ISOs: the extraction folder holding the ISO goes next to the Content folder,
/// never inside Content (or its profile folder), so nothing but GOD trees ends up in Content.
#[test]
fn content_profile_iso_extraction_folder_goes_outside_content() {
    let d = tmp("content-iso-out");
    let usb = d.join("usb");
    let prof = usb.join("Content").join("0000000000000000");
    fs::create_dir_all(&prof).unwrap();
    let zip = d.join("Bo3game.zip");
    write_zip(&zip, &[("Bo3game/game.iso", b"not really an iso", 0o644)]);
    for dest in [prof.clone(), usb.join("Content")] {
        let o = ExtractOptions {
            archive: &zip,
            dest_parent: &dest,
            password: None,
            collision: Collision::Number,
            layout: Layout::Folder,
            skip_junk: true,
            nested: false,
        };
        let r = extract(&exe(), &o, &Jobs::default(), "ci", &mut |_, _| {}).unwrap();
        let out = Path::new(&r.output);
        assert_eq!(out.parent().unwrap(), usb, "{dest:?} -> {out:?}");
        assert!(out.join("game.iso").is_file());
        assert!(names_in_dir(&prof).is_empty(), "{:?}", names_in_dir(&prof));
        assert_eq!(names_in_dir(&usb.join("Content")), vec!["0000000000000000"]);
        assert!(r.warnings.iter().any(|w| w.contains("next to the Content folder, not inside it")), "{:?}", r.warnings);
        assert!(no_staging_left(&prof) && no_staging_left(&usb.join("Content")));
    }
    // R23 L6: Skip with the folder already there says it was skipped, not "the GOD game goes".
    let o = ExtractOptions {
        archive: &zip,
        dest_parent: &prof,
        password: None,
        collision: Collision::Skip,
        layout: Layout::Folder,
        skip_junk: true,
        nested: false,
    };
    let r = extract(&exe(), &o, &Jobs::default(), "ci", &mut |_, _| {}).unwrap();
    assert!(r.skipped);
    assert!(r.warnings.iter().any(|w| w.contains("was skipped")), "{:?}", r.warnings);
    assert!(!r.warnings.iter().any(|w| w.contains("GOD game goes")), "{:?}", r.warnings);
}

/// R23 M2/L3: in a Content folder a GOD package (header + .data) moves as one unit; half
/// packages on either side are never mixed, and stray files are left out with a warning.
#[test]
fn r23_content_packages_move_as_a_unit() {
    let d = tmp("r23-unit");
    let ex = |zip: &Path, prof: &Path, c: Collision| {
        let o = ExtractOptions {
            archive: zip,
            dest_parent: prof,
            password: None,
            collision: c,
            layout: Layout::Folder,
            skip_junk: true,
            nested: false,
        };
        extract(&exe(), &o, &Jobs::default(), "un", &mut |_, _| {}).unwrap()
    };
    let full = d.join("full.zip");
    write_zip(
        &full,
        &[
            ("415608C3/00007000/AAAA0001", b"NEWHDR", 0o644),
            ("415608C3/00007000/AAAA0001.data/Data0000", b"NEWDATA", 0o644),
            ("415608C3/README.txt", b"r", 0o644),
            ("415608C3/zzz/evil.bin", b"e", 0o644),
            ("415608C3/00007000/notes.txt", b"n", 0o644),
        ],
    );
    let pk = |prof: &Path| prof.join("415608C3/00007000");
    // target has only .data: nothing mixed, a clear warning (not "Nothing was changed")
    let p1 = d.join("a/Content/0000000000000000");
    fs::create_dir_all(pk(&p1).join("AAAA0001.data")).unwrap();
    fs::write(pk(&p1).join("AAAA0001.data/Data0000"), b"OLDDATA").unwrap();
    let r = ex(&full, &p1, Collision::Number);
    assert!(!pk(&p1).join("AAAA0001").exists(), "no new header over old data");
    assert_eq!(fs::read(pk(&p1).join("AAAA0001.data/Data0000")).unwrap(), b"OLDDATA");
    assert!(r.warnings.iter().any(|w| w.contains("incomplete") && w.contains("no header")), "{:?}", r.warnings);
    assert!(!r.warnings.iter().any(|w| w.contains("Nothing was changed")), "{:?}", r.warnings);
    // L3: stray files and folders stay out, with warnings
    assert_eq!(names_in_dir(&p1.join("415608C3")), vec!["00007000"]);
    assert_eq!(names_in_dir(&pk(&p1)), vec!["AAAA0001.data"]);
    assert!(r.warnings.iter().any(|w| w.contains("README.txt") && w.contains("left out")), "{:?}", r.warnings);
    assert!(r.warnings.iter().any(|w| w.contains("zzz") && w.contains("left out")), "{:?}", r.warnings);
    // target has only the header: old header not paired with new data
    let p2 = d.join("b/Content/0000000000000000");
    fs::create_dir_all(pk(&p2)).unwrap();
    fs::write(pk(&p2).join("AAAA0001"), b"OLDHDR").unwrap();
    let r = ex(&full, &p2, Collision::Number);
    assert!(!pk(&p2).join("AAAA0001.data").exists());
    assert_eq!(fs::read(pk(&p2).join("AAAA0001")).unwrap(), b"OLDHDR");
    assert!(r.warnings.iter().any(|w| w.contains("incomplete") && w.contains("no .data")), "{:?}", r.warnings);
    // Replace with the whole package fixes a half package: both parts are new
    let r = ex(&full, &p2, Collision::Replace);
    assert_eq!(fs::read(pk(&p2).join("AAAA0001")).unwrap(), b"NEWHDR");
    assert_eq!(fs::read(pk(&p2).join("AAAA0001.data/Data0000")).unwrap(), b"NEWDATA");
    assert!(!r.skipped);
    // Replace with a header-only archive: refused, the complete package stays as it is
    let half = d.join("half.zip");
    write_zip(&half, &[("415608C3/00007000/AAAA0001", b"HALFHDR", 0o644)]);
    let r = ex(&half, &p2, Collision::Replace);
    assert_eq!(fs::read(pk(&p2).join("AAAA0001")).unwrap(), b"NEWHDR");
    assert!(pk(&p2).join("AAAA0001.data/Data0000").is_file());
    assert!(r.warnings.iter().any(|w| w.contains("only half")), "{:?}", r.warnings);
}

/// R24 L2/L3/INFO: packages are matched by MediaID case-insensitively, the summary line is only
/// "already there" when it is, and Replace swaps the pair without losing the old one first.
#[test]
fn r24_content_units_case_summary_and_replace() {
    let td = crate::testutil::TmpDir::new("arch-r24-units");
    let run = |name: &str, setup: &dyn Fn(&Path, &Path), col: Collision| {
        let st = td.path().join(format!("{name}-st"));
        let prof = td.path().join(format!("{name}-prof"));
        let tree = st.join("4D5307E6");
        fs::create_dir_all(tree.join("00007000")).unwrap();
        fs::create_dir_all(&prof).unwrap();
        setup(&tree.join("00007000"), &prof.join("4D5307E6").join("00007000"));
        let r = place_title_trees(&st, &[tree], &prof, col, 1, 0, 0, vec![], &Jobs::default(), "t", &mut |_, _| {}).unwrap();
        (r, prof.join("4D5307E6").join("00007000"))
    };
    let pkg = |d: &Path, data: &str, body: &[u8]| {
        fs::write(d.join(&data[..8]), body).unwrap();
        fs::create_dir_all(d.join(data)).unwrap();
        fs::write(d.join(data).join("Data0000"), body).unwrap();
    };
    // L2: an upper-case .DATA folder is a whole package and is copied with its real name.
    let (r, dst) = run("upper", &|s, _| pkg(s, "abcd0001.DATA", b"N"), Collision::Skip);
    assert!(dst.join("abcd0001.DATA").join("Data0000").is_file(), "{:?}", r.warnings);
    assert!(!r.warnings.iter().any(|w| w.contains("only half")), "{:?}", r.warnings);
    // L2: already on the drive under another case -> skipped as present, nothing duplicated.
    let (r, dst) = run(
        "present",
        &|s, d| {
            pkg(s, "abcd0001.data", b"N");
            fs::create_dir_all(d).unwrap();
            pkg(d, "ABCD0001.DATA", b"O");
        },
        Collision::Skip,
    );
    assert!(r.warnings.iter().any(|w| w.contains("every package was already there")), "{:?}", r.warnings);
    assert_eq!(names_in_dir(&dst).len(), 2);
    // L3: half package in the archive -> no "already there" claim.
    let (r, _) = run("half", &|s, _| fs::write(s.join("abcd0001"), b"N").unwrap(), Collision::Skip);
    assert!(r.warnings.iter().any(|w| w.contains("only half")), "{:?}", r.warnings);
    assert!(!r.warnings.iter().any(|w| w.contains("already there")), "{:?}", r.warnings);
    // L3: incomplete on the drive -> no "already there" claim either.
    let (r, _) = run(
        "incomplete",
        &|s, d| {
            pkg(s, "abcd0001.data", b"N");
            fs::create_dir_all(d).unwrap();
            fs::write(d.join("abcd0001"), b"O").unwrap();
        },
        Collision::Skip,
    );
    assert!(r.warnings.iter().any(|w| w.contains("is incomplete")), "{:?}", r.warnings);
    assert!(!r.warnings.iter().any(|w| w.contains("already there")), "{:?}", r.warnings);
    // Replace: the new pair lands, the old (differently cased) pair and the aside copies are gone.
    let (r, dst) = run(
        "replace",
        &|s, d| {
            pkg(s, "abcd0001.data", b"N");
            fs::create_dir_all(d).unwrap();
            pkg(d, "ABCD0001.DATA", b"O");
        },
        Collision::Replace,
    );
    assert!(r.warnings.iter().any(|w| w.contains("replaced")), "{:?}", r.warnings);
    assert_eq!(names_in_dir(&dst), ["abcd0001", "abcd0001.data"]);
    assert_eq!(fs::read(dst.join("abcd0001.data").join("Data0000")).unwrap(), b"N");
}

/// R25/R27: a failed Replace puts the old pair back and leaves no manifest; asides without a
/// manifest are only reported (with correct grammar) and never moved or deleted; a stale aside
/// name is never reused.
#[test]
fn r25_replace_all_or_nothing_and_leftover_asides() {
    let td = crate::testutil::TmpDir::new("arch-r25-aside");
    let d = td.path().join("dst");
    fs::create_dir_all(d.join("ABCD0001.data")).unwrap();
    fs::write(d.join("ABCD0001"), b"OLD").unwrap();
    fs::write(d.join("ABCD0001.data").join("Data0000"), b"OLD").unwrap();
    let sh = td.path().join("abcd0001");
    fs::write(&sh, b"NEW").unwrap();
    let sd0 = td.path().join("x").join("abcd0001.data");
    fs::create_dir_all(&sd0).unwrap();
    let (dh, dd) = (d.join("abcd0001"), d.join("abcd0001.data"));
    // The new .data vanishes after the old pair is set aside: its move fails, undo runs.
    let _serial = R27_PAUSE_TEST.lock().unwrap_or_else(|e| e.into_inner());
    let blk = sd0.clone();
    *REPLACE_PAUSE.lock().unwrap() = Some(Box::new(move |n| {
        if std::thread::current().name() == Some("r25-undo") && n == 5 {
            fs::remove_dir_all(&blk).unwrap();
        }
    }));
    let (d2, sh2, dh2, sd2, dd2) = (d.clone(), sh.clone(), dh.clone(), sd0.clone(), dd.clone());
    let e = std::thread::Builder::new()
        .name("r25-undo".into())
        .spawn(move || {
            let lock = lock_type_folder(&d2, &|| false, &mut || {}).unwrap();
            replace_unit(&d2, &lock, [Some(d2.join("ABCD0001")), Some(d2.join("ABCD0001.data"))], [(&sh2, &dh2), (&sd2, &dd2)])
        })
        .unwrap()
        .join()
        .unwrap()
        .unwrap_err();
    *REPLACE_PAUSE.lock().unwrap() = None;

    assert!(e.contains("left as it was"), "{e}");
    assert_eq!(names_in_dir(&d), ["ABCD0001", "ABCD0001.data"], "old pair back, no manifest left");
    assert_eq!(fs::read(d.join("ABCD0001")).unwrap(), b"OLD");
    // One stray aside: reported in the singular, untouched.
    fs::write(d.join(".unpack-old.0.ABCD0001"), b"PRECIOUS").unwrap();
    let mut w = vec![];
    recover_asides_locked(&d, &mut w);
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(w[0].contains(" is left over") && w[0].contains("it was not touched"), "{w:?}");
    assert_eq!(fs::read(d.join(".unpack-old.0.ABCD0001")).unwrap(), b"PRECIOUS");
    // Two: plural.
    fs::create_dir_all(d.join(".unpack-old.1.ABCD0001.data")).unwrap();
    let mut w = vec![];
    recover_asides_locked(&d, &mut w);
    assert!(w[0].contains(" are left over") && w[0].contains("they were not touched"), "{w:?}");
    fs::remove_dir(d.join(".unpack-old.1.ABCD0001.data")).unwrap();
    // A successful Replace with a stale aside present uses a fresh tag and keeps the stale one.
    fs::write(&sh, b"NEW").unwrap();
    let sd = td.path().join("abcd0001.data");
    fs::create_dir_all(&sd).unwrap();
    fs::write(sd.join("Data0000"), b"NEW").unwrap();
    replace_unit_locked(&d, [Some(d.join("ABCD0001")), Some(d.join("ABCD0001.data"))], [(&sh, &dh), (&sd, &dd)]).unwrap();
    assert_eq!(fs::read(d.join(".unpack-old.0.ABCD0001")).unwrap(), b"PRECIOUS");
    assert_eq!(fs::read(dd.join("Data0000")).unwrap(), b"NEW");
    assert_eq!(names_in_dir(&d), [".unpack-old.0.ABCD0001", "abcd0001", "abcd0001.data"]);
    // A damaged manifest is reported, nothing moved.
    fs::write(d.join(".unpack-replace.7.json"), b"{not json").unwrap();
    let mut w = vec![];
    recover_asides_locked(&d, &mut w);
    assert!(w.iter().any(|x| x.contains("couldn't be read")), "{w:?}");
    assert!(d.join(".unpack-replace.7.json").exists());
}

/// R27 H: move_new never replaces a file or merges into an existing folder.
#[test]
fn r27_move_new_refuses_existing_destination() {
    let td = crate::testutil::TmpDir::new("arch-r27-movenew");
    let d = td.path();
    fs::create_dir_all(d.join("src.data")).unwrap();
    fs::write(d.join("src.data").join("a"), b"N").unwrap();
    fs::create_dir_all(d.join("dst.data")).unwrap();
    fs::write(d.join("dst.data").join("b"), b"O").unwrap();
    let e = move_new(&d.join("src.data"), &d.join("dst.data")).unwrap_err();
    assert_eq!(e.kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(names_in_dir(&d.join("dst.data")), ["b"], "no merge");
    assert!(d.join("src.data").join("a").exists());
    fs::write(d.join("f"), b"N").unwrap();
    fs::write(d.join("g"), b"O").unwrap();
    assert!(move_new(&d.join("f"), &d.join("g")).is_err());
    assert_eq!(fs::read(d.join("g")).unwrap(), b"O");
    move_new(&d.join("f"), &d.join("h")).unwrap();
    assert_eq!(fs::read(d.join("h")).unwrap(), b"N");
}

fn r27_setup(prof: &Path) -> PathBuf {
    let dst = prof.join("4D5307E6").join("00007000");
    fs::create_dir_all(dst.join("ABCD0001.data")).unwrap();
    fs::write(dst.join("ABCD0001"), b"OLD").unwrap();
    fs::write(dst.join("ABCD0001.data").join("Data0000"), b"OLD").unwrap();
    dst
}

fn r27_stage(root: &Path, tag: &str, body: &[u8]) -> (PathBuf, PathBuf) {
    let st = root.join(format!("st-{tag}"));
    let ct = st.join("4D5307E6").join("00007000");
    fs::create_dir_all(ct.join("abcd0001.data")).unwrap();
    fs::write(ct.join("abcd0001"), body).unwrap();
    fs::write(ct.join("abcd0001.data").join("Data0000"), body).unwrap();
    (st.clone(), st.join("4D5307E6"))
}

/// The whole folder's state: the live pair (must match), and every byte body found anywhere.
fn r27_check(dst: &Path, ctx: &str) -> Vec<u8> {
    let live = |n: &str| fs::read_dir(dst).unwrap().flatten().find(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case(n)).map(|e| e.path());
    let h = fs::read(live("abcd0001").unwrap_or_else(|| panic!("{ctx}: header {:?}", names_in_dir(dst)))).unwrap();
    let d = fs::read(live("abcd0001.data").unwrap_or_else(|| panic!("{ctx}: data {:?}", names_in_dir(dst))).join("Data0000")).unwrap();
    assert_eq!(h, d, "{ctx}: never a mixed package {:?}", names_in_dir(dst));
    assert!(!names_in_dir(dst).iter().any(|n| n.starts_with(".unpack-replace.")), "{ctx}: note and temp note gone {:?}", names_in_dir(dst));
    h
}

/// R27 M: a Replace killed right before each of its steps (1 manifest, 2-3 old halves aside,
/// 4-5 new halves in, 6 mark finishing, 7-8 delete asides, 9 delete manifest) is recovered on
/// the next run to a whole package (old before the new pair is complete, new after), with a
/// true message and nothing of the old copy lost.
#[test]
fn r27_killed_replace_recovers_whole_pair() {
    let td = crate::testutil::TmpDir::new("arch-r27-kill");
    for step in 1..=10 {
        let prof = td.path().join(format!("prof{step}"));
        let dst = r27_setup(&prof);
        let (st, tree) = r27_stage(td.path(), &format!("{step}a"), b"NEW");
        REPLACE_KILL_AT.with(|k| k.set(step));
        let r = place_title_trees(&st, &[tree], &prof, Collision::Replace, 1, 0, 0, vec![], &Jobs::default(), "t", &mut |_, _| {});
        REPLACE_KILL_AT.with(|k| k.set(0));
        assert_eq!(r.is_err(), step <= 9, "step {step}");
        let (st, tree) = r27_stage(td.path(), &format!("{step}b"), b"NEW");
        let r = place_title_trees(&st, &[tree], &prof, Collision::Skip, 1, 0, 0, vec![], &Jobs::default(), "t", &mut |_, _| {}).unwrap();
        let ctx = format!("step {step}: {:?}", r.warnings);
        let h = r27_check(&dst, &ctx);
        let finished = step >= 6;
        assert_eq!(h, if finished { b"NEW" } else { b"OLD" }, "{ctx}");
        let put_back = r.warnings.iter().any(|w| w.contains("put the old package back"));
        let cleaned = r.warnings.iter().any(|w| w.contains("had finished moving the new copy in"));
        assert_eq!(put_back, (3..=5).contains(&step), "{ctx}");
        assert_eq!(cleaned, (6..=8).contains(&step), "{ctx}");
        if step == 5 {
            assert!(r.warnings.iter().any(|w| w.contains("set aside as") && w.contains(".unpack-new.")), "{ctx}");
        }
        if !finished {
            assert!(!names_in_dir(&dst).iter().any(|n| n.starts_with(".unpack-old.")), "{ctx}: no old aside left");
        } else {
            assert_eq!(names_in_dir(&dst), ["abcd0001", "abcd0001.data"], "{ctx}");
        }
        assert!(r.warnings.iter().any(|w| w.contains("already there")), "{ctx}");
    }
}

/// R27 M: every double crash. The Replace is killed before step k, the first recovery is killed
/// before its write j, and a final recovery must still leave one whole package, the right one,
/// with no manifest left behind.
#[test]
fn r27_double_crash_every_case() {
    let td = crate::testutil::TmpDir::new("arch-r27-double");
    let mut cases = 0;
    for k in 2..=9 {
        for j in 1..=8 {
            let prof = td.path().join(format!("p{k}-{j}"));
            let dst = r27_setup(&prof);
            let (st, tree) = r27_stage(td.path(), &format!("{k}-{j}a"), b"NEW");
            REPLACE_KILL_AT.with(|x| x.set(k));
            assert!(place_title_trees(&st, &[tree], &prof, Collision::Replace, 1, 0, 0, vec![], &Jobs::default(), "t", &mut |_, _| {}).is_err());
            REPLACE_KILL_AT.with(|x| x.set(0));
            RECOVER_KILL_AT.with(|x| x.set(j));
            let mut w1 = vec![];
            recover_asides_locked(&dst, &mut w1);
            RECOVER_KILL_AT.with(|x| x.set(0));
            let mut w2 = vec![];
            recover_asides_locked(&dst, &mut w2);
            let ctx = format!("k={k} j={j}: {w1:?} / {w2:?} {:?}", names_in_dir(&dst));
            let h = r27_check(&dst, &ctx);
            assert_eq!(h, if k >= 6 { b"NEW" } else { b"OLD" }, "{ctx}");
            assert!(!names_in_dir(&dst).iter().any(|n| n.starts_with(".unpack-old.")), "{ctx}");
            // A third pass has nothing to do.
            let mut w3 = vec![];
            recover_asides_locked(&dst, &mut w3);
            assert!(w3.is_empty(), "{ctx}: {w3:?}");
            cases += 1;
        }
    }
    assert_eq!(cases, 64);
}

/// R27 H: two jobs on the same type folder. Job A pauses inside its Replace at each step in
/// turn; job B (a Replace of the same package, or an add of another) must wait for A's lock
/// and never repair or touch A's half-done Replace. The end state is whole and consistent.
#[test]
fn r27_concurrent_jobs_are_serialized_per_type_folder() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{mpsc, Arc};
    let _serial = R27_PAUSE_TEST.lock().unwrap_or_else(|e| e.into_inner());
    let td = crate::testutil::TmpDir::new("arch-r27-conc");
    for pause_at in 1..=9 {
        for b_adds in [false, true] {
            let prof = td.path().join(format!("c{pause_at}-{b_adds}"));
            let dst = r27_setup(&prof);
            let (sta, ta) = r27_stage(td.path(), &format!("c{pause_at}{b_adds}a"), b"AAA");
            let (stb, tb) = if b_adds {
                let st = td.path().join(format!("st-c{pause_at}{b_adds}b"));
                let ct = st.join("4D5307E6").join("00007000");
                fs::create_dir_all(ct.join("beef0002.data")).unwrap();
                fs::write(ct.join("beef0002"), b"BBB").unwrap();
                fs::write(ct.join("beef0002.data").join("Data0000"), b"BBB").unwrap();
                (st.clone(), st.join("4D5307E6"))
            } else {
                r27_stage(td.path(), &format!("c{pause_at}{b_adds}b"), b"BBB")
            };
            let (paused_tx, paused_rx) = mpsc::channel::<()>();
            let (go_tx, go_rx) = mpsc::channel::<()>();
            let hits = Arc::new(AtomicUsize::new(0));
            let go_rx = Mutex::new(go_rx);
            let paused_tx = Mutex::new(paused_tx);
            let h2 = hits.clone();
            *REPLACE_PAUSE.lock().unwrap() = Some(Box::new(move |n| {
                if std::thread::current().name() == Some("r27-a") && n == pause_at && h2.fetch_add(1, Ordering::SeqCst) == 0 {
                    paused_tx.lock().unwrap().send(()).unwrap();
                    go_rx.lock().unwrap().recv().unwrap();
                }
            }));
            let pa = prof.clone();
            let a = std::thread::Builder::new()
                .name("r27-a".into())
                .spawn(move || place_title_trees(&sta, &[ta], &pa, Collision::Replace, 1, 0, 0, vec![], &Jobs::default(), "a", &mut |_, _| {}))
                .unwrap();
            paused_rx.recv().unwrap();
            let b_done = Arc::new(AtomicUsize::new(0));
            let (pb, bd) = (prof.clone(), b_done.clone());
            let b = std::thread::spawn(move || {
                let r = place_title_trees(&stb, &[tb], &pb, Collision::Replace, 1, 0, 0, vec![], &Jobs::default(), "b", &mut |_, _| {});
                bd.store(1, Ordering::SeqCst);
                r
            });
            std::thread::sleep(std::time::Duration::from_millis(150));
            assert_eq!(b_done.load(Ordering::SeqCst), 0, "pause {pause_at}: B waited for A");
            go_tx.send(()).unwrap();
            let ra = a.join().unwrap().unwrap();
            let rb = b.join().unwrap().unwrap();
            *REPLACE_PAUSE.lock().unwrap() = None;
            let ctx = format!("pause {pause_at} add={b_adds}: {:?} / {:?}", ra.warnings, rb.warnings);
            assert!(!rb.warnings.iter().any(|w| w.contains("earlier Replace")), "{ctx}: B never repaired A");
            let h = r27_check(&dst, &ctx);
            assert_eq!(h, if b_adds { b"AAA" } else { b"BBB" }, "{ctx}");
            let mut want = vec!["abcd0001", "abcd0001.data"];
            if b_adds {
                want.extend(["beef0002", "beef0002.data"]);
            }
            assert_eq!(names_in_dir(&dst), want, "{ctx}");
        }
    }
}

static R27_PAUSE_TEST: Mutex<()> = Mutex::new(());

/// R26/R27 L: staging left by a killed run is removed only when its lock is a real, free file
/// with Unpack's signature; one in use is left alone silently; anything else is reported.
#[test]
fn r26_stale_staging_is_swept_or_reported() {
    let td = crate::testutil::TmpDir::new("arch-r26-sweep");
    let d = td.path();
    let mk = |n: &str| {
        fs::create_dir_all(d.join(n).join("sub")).unwrap();
        fs::write(d.join(n).join("sub").join("f"), b"x").unwrap();
    };
    // The lock is made before the folder.
    drop(lock_staging(&d.join(".unpack-tmp-dead")).expect("lock"));
    mk(".unpack-tmp-dead");
    let held = lock_staging(&d.join(".unpack-tmp-busy")).expect("lock");
    assert!(lock_staging(&d.join(".unpack-tmp-busy")).is_none(), "never reuses a lock file");
    mk(".unpack-tmp-busy");
    mk(".unpack-tmp-nolock");
    mk(".unpack-tmp-foreign");
    fs::write(d.join(".unpack-tmp-foreign.lock"), b"someone else").unwrap();
    mk(".unpack-tmp-own");
    // A lock with no folder (killed between the two) is tidied when it's ours and free.
    drop(lock_staging(&d.join(".unpack-tmp-orphan")).expect("lock"));
    #[cfg(unix)]
    {
        mk(".unpack-tmp-link");
        fs::write(d.join("target.lock"), b"unpack-staging v1").unwrap();
        std::os::unix::fs::symlink(d.join("target.lock"), d.join(".unpack-tmp-link.lock")).unwrap();
    }
    let mut w = vec![];
    sweep_stale_staging(d, &d.join(".unpack-tmp-own"), &mut w);
    assert!(!d.join(".unpack-tmp-dead").exists() && !d.join(".unpack-tmp-dead.lock").exists());
    assert!(!d.join(".unpack-tmp-orphan.lock").exists());
    assert!(d.join(".unpack-tmp-busy/sub/f").exists() && d.join(".unpack-tmp-busy.lock").exists(), "in use: untouched");
    assert!(d.join(".unpack-tmp-nolock/sub/f").exists() && d.join(".unpack-tmp-own/sub/f").exists());
    assert!(d.join(".unpack-tmp-foreign/sub/f").exists());
    assert!(w.iter().any(|x| x.contains(".unpack-tmp-nolock") && x.contains("no lock file")), "{w:?}");
    assert!(w.iter().any(|x| x.contains(".unpack-tmp-foreign") && x.contains("isn't one of mine")), "{w:?}");
    #[cfg(unix)]
    {
        assert!(d.join(".unpack-tmp-link/sub/f").exists() && d.join("target.lock").exists());
        assert!(w.iter().any(|x| x.contains(".unpack-tmp-link") && x.contains("is a link")), "{w:?}");
    }
    assert!(!w.iter().any(|x| x.contains("busy")), "{w:?}");
    drop(held);
}

/// R28 M: hand-made hostile notes are reported and nothing is moved or deleted.
#[test]
fn r28_hostile_notes_are_reported_not_acted_on() {
    let td = crate::testutil::TmpDir::new("arch-r28-hostile");
    let notes = [
        // The three Overseer triggers.
        r#"{"phase":"finishing","old":[["ABCD0001","ABCD0002.data",true]],"new":[]}"#,
        r#"{"phase":"finishing","old":[["X","ABCD0002",false]],"new":[]}"#,
        r#"{"phase":"undoing","old":[["ABCD0009","ABCD0002",false]],"new":[]}"#,
        // Aside names with another tag, or the right shape but someone else's package.
        r#"{"phase":"finishing","old":[["ABCD0002",".unpack-old.5.ABCD0002",false]],"new":[["abcd0002",".unpack-new.5.abcd0002",false,""],["abcd0002.data",".unpack-new.5.abcd0002.data",true,""]]}"#,
        // Mixed MediaIDs.
        r#"{"phase":"finishing","old":[["ABCD0002",".unpack-old.0.ABCD0002",false]],"new":[["abcd0001",".unpack-new.0.abcd0001",false,""],["abcd0001.data",".unpack-new.0.abcd0001.data",true,""]]}"#,
        // Unknown phase.
        r#"{"phase":"nuke","old":[["ABCD0002",".unpack-old.0.ABCD0002",false]],"new":[["ABCD0002",".unpack-new.0.ABCD0002",false,""],["ABCD0002.data",".unpack-new.0.ABCD0002.data",true,""]]}"#,
        // is_data flag lying.
        r#"{"phase":"finishing","old":[["ABCD0002.data",".unpack-old.0.ABCD0002.data",false]],"new":[["ABCD0002",".unpack-new.0.ABCD0002",false,""],["ABCD0002.data",".unpack-new.0.ABCD0002.data",true,""]]}"#,
        // Not hex.
        r#"{"phase":"finishing","old":[["ZZZZ0002",".unpack-old.0.ZZZZ0002",false]],"new":[["ZZZZ0002",".unpack-new.0.ZZZZ0002",false,""],["ZZZZ0002.data",".unpack-new.0.ZZZZ0002.data",true,""]]}"#,
    ];
    for (i, note) in notes.iter().enumerate() {
        let d = td.path().join(format!("n{i}"));
        fs::create_dir_all(d.join("ABCD0002.data")).unwrap();
        fs::write(d.join("ABCD0002"), b"DISC2").unwrap();
        fs::write(d.join("ABCD0002.data").join("Data0000"), b"DISC2").unwrap();
        fs::write(d.join(".unpack-old.0.ABCD0002"), b"KEEP").unwrap();
        fs::write(d.join(".unpack-replace.0.json"), note).unwrap();
        let before = names_in_dir(&d);
        let mut w = vec![];
        recover_asides_locked(&d, &mut w);
        assert!(w.iter().any(|x| x.contains("isn't one I can trust")), "note {i}: {w:?}");
        assert!(!w.iter().any(|x| x.contains("removed the old copy") || x.contains("put the old package back")), "note {i}: {w:?}");
        assert_eq!(names_in_dir(&d), before, "note {i}: nothing moved");
        assert_eq!(fs::read(d.join("ABCD0002")).unwrap(), b"DISC2");
        assert_eq!(fs::read(d.join("ABCD0002.data").join("Data0000")).unwrap(), b"DISC2");
        assert_eq!(fs::read(d.join(".unpack-old.0.ABCD0002")).unwrap(), b"KEEP");
    }
}

/// R28 H: undo removes only what still matches what it moved in, and never puts an old half
/// back over something; whatever it can't prove is reported and left.
#[test]
fn r28_undo_leaves_what_it_cannot_prove() {
    let _serial = R27_PAUSE_TEST.lock().unwrap_or_else(|e| e.into_inner());
    let td = crate::testutil::TmpDir::new("arch-r28-undo");
    let d = td.path().join("dst");
    fs::create_dir_all(d.join("ABCD0001.data")).unwrap();
    fs::write(d.join("ABCD0001"), b"OLD").unwrap();
    fs::write(d.join("ABCD0001.data").join("Data0000"), b"OLD").unwrap();
    let sh = td.path().join("abcd0001");
    fs::write(&sh, b"NEW").unwrap();
    let sd = td.path().join("abcd0001.data");
    fs::create_dir_all(&sd).unwrap();
    fs::write(sd.join("Data0000"), b"NEW").unwrap();
    let (dh, dd) = (d.join("abcd0001"), d.join("abcd0001.data"));
    let dc = d.clone();
    *REPLACE_PAUSE.lock().unwrap() = Some(Box::new(move |n| {
        if std::thread::current().name() == Some("r28-undo") && n == 5 {
            // Someone else swaps the header we just moved in and takes the .data's place
            // (and the old header's).
            fs::write(dc.join("abcd0001"), b"FOREIGN").unwrap();
            fs::create_dir_all(dc.join("abcd0001.data")).unwrap();
            #[cfg(not(windows))]
            fs::write(dc.join("ABCD0001"), b"THIRD").unwrap();
        }
    }));
    let e = std::thread::Builder::new()
        .name("r28-undo".into())
        .spawn(move || {
            let lock = lock_type_folder(&d, &|| false, &mut || {}).unwrap();
            replace_unit(&d, &lock, [Some(d.join("ABCD0001")), Some(d.join("ABCD0001.data"))], [(&sh, &dh), (&sd, &dd)])
        })
        .unwrap()
        .join()
        .unwrap()
        .unwrap_err();
    *REPLACE_PAUSE.lock().unwrap() = None;
    let d = td.path().join("dst");
    assert!(e.contains("no longer what I moved there") && e.contains("something else is in its place"), "{e}");
    assert_eq!(fs::read(d.join("abcd0001")).unwrap(), b"FOREIGN", "not deleted");
    #[cfg(not(windows))]
    assert_eq!(fs::read(d.join("ABCD0001")).unwrap(), b"THIRD", "not overwritten");
    let tag_old = names_in_dir(&d).into_iter().find(|n| n.starts_with(".unpack-old.") && n.ends_with("ABCD0001")).expect("old header kept aside");
    assert_eq!(fs::read(d.join(tag_old)).unwrap(), b"OLD");
    #[cfg(not(windows))]
    assert!(d.join("ABCD0001.data").join("Data0000").exists(), "old .data back");
    assert!(names_in_dir(&d).iter().any(|n| n.starts_with(".unpack-replace.") && n.ends_with(".json")), "note kept for the user");
}

/// R28 L: a temp note left by a kill mid-write is removed when it is provably ours, and
/// reported otherwise.
#[test]
fn r28_stale_temp_notes() {
    let td = crate::testutil::TmpDir::new("arch-r28-tmp");
    let d = td.path();
    let ours = r#"{"phase":"replacing","old":[["ABCD0001",".unpack-old.3.ABCD0001",false,"o"]],"new":[["abcd0001",".unpack-new.3.abcd0001",false,"x"],["abcd0001.data",".unpack-new.3.abcd0001.data",true,"y"]]}"#;
    fs::write(d.join(".unpack-replace.3.tmp"), ours).unwrap();
    fs::write(d.join(".unpack-replace.4.tmp"), b"{half wr").unwrap();
    let mut w = vec![];
    recover_asides_locked(d, &mut w);
    assert!(!d.join(".unpack-replace.3.tmp").exists());
    assert!(d.join(".unpack-replace.4.tmp").exists());
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(w[0].contains(".unpack-replace.4.tmp") && w[0].contains("not touched"), "{w:?}");
}

/// R28 H helper: one Unpack job in its own process. Driven by r28_two_processes_*; does
/// nothing when run on its own.
#[test]
#[ignore]
fn r28_child_job() {
    let Ok(prof) = std::env::var("R28_PROF") else { return };
    let st = PathBuf::from(std::env::var("R28_STAGE").unwrap());
    let ctl = PathBuf::from(std::env::var("R28_CTL").unwrap());
    let pause: usize = std::env::var("R28_PAUSE").ok().and_then(|x| x.parse().ok()).unwrap_or(0);
    if pause > 0 {
        let c = ctl.clone();
        *REPLACE_PAUSE.lock().unwrap() = Some(Box::new(move |n| {
            if n == pause {
                fs::write(c.join("paused"), b"").unwrap();
                while !c.join("go").exists() {
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
        }));
    }
    let col = if std::env::var("R28_SKIP").is_ok() { Collision::Skip } else { Collision::Replace };
    let r = place_title_trees(&st, &[st.join("4D5307E6")], Path::new(&prof), col, 1, 0, 0, vec![], &Jobs::default(), "c", &mut |_, m| {
        let _ = fs::write(ctl.join("waiting"), m);
    });
    fs::write(ctl.join(if pause > 0 { "a.out" } else { "b.out" }), format!("{:?}", r.map(|r| r.warnings))).unwrap();
}

/// R28 H: two real processes on one Content folder. Process A's Replace is held at each step in
/// turn; process B (Skip disc 2, Skip the same package, or Replace the same package) must wait
/// for A's lock and never repair A's Replace. A finishes cleanly; the end state is consistent.
#[test]
fn r28_two_processes_are_serialized() {
    let td = crate::testutil::TmpDir::new("arch-r28-proc");
    let exe = std::env::current_exe().unwrap();
    let spawn = |prof: &Path, st: &Path, ctl: &Path, pause: usize, skip: bool| {
        let mut c = Command::new(&exe);
        c.args(["archive::tests::r28_child_job", "--exact", "--ignored", "--nocapture", "--test-threads=1"])
            .env("R28_PROF", prof)
            .env("R28_STAGE", st)
            .env("R28_CTL", ctl)
            .env("R28_PAUSE", pause.to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if skip {
            c.env("R28_SKIP", "1");
        }
        c.spawn().unwrap()
    };
    let wait_for = |p: &Path| {
        let t = Instant::now();
        while !p.exists() {
            assert!(t.elapsed() < Duration::from_secs(60), "timed out waiting for {}", p.display());
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    for pause in [1, 3, 4, 5, 6, 8] {
        for variant in ["skip-disc2", "skip-same", "replace-same"] {
            let tag = format!("{pause}-{variant}");
            let prof = td.path().join(format!("p{tag}"));
            let dst = r27_setup(&prof);
            let (sta, _) = r27_stage(td.path(), &format!("{tag}a"), b"AAA");
            let (stb, _) = if variant == "skip-disc2" {
                let st = td.path().join(format!("st-{tag}b"));
                let ct = st.join("4D5307E6").join("00007000");
                fs::create_dir_all(ct.join("beef0002.data")).unwrap();
                fs::write(ct.join("beef0002"), b"BBB").unwrap();
                fs::write(ct.join("beef0002.data").join("Data0000"), b"BBB").unwrap();
                (st.clone(), st.join("4D5307E6"))
            } else {
                r27_stage(td.path(), &format!("{tag}b"), b"BBB")
            };
            let (ca, cb) = (td.path().join(format!("ctl-{tag}-a")), td.path().join(format!("ctl-{tag}-b")));
            fs::create_dir_all(&ca).unwrap();
            fs::create_dir_all(&cb).unwrap();
            let mut a = spawn(&prof, &sta, &ca, pause, false);
            wait_for(&ca.join("paused"));
            // R29: the Overseer attack: delete (k4) or replace (k5) the lock while A holds it.
            if pause == 4 {
                let _ = fs::remove_file(dst.join(FOLDER_LOCK));
            } else if pause == 5 {
                fs::write(dst.join(".x"), b"x").unwrap();
                let _ = fs::rename(dst.join(".x"), dst.join(FOLDER_LOCK));
                let _ = fs::remove_file(dst.join(".x"));
            }
            let mut b = spawn(&prof, &stb, &cb, 0, variant != "replace-same");
            wait_for(&cb.join("waiting"));
            std::thread::sleep(Duration::from_millis(300));
            assert!(b.try_wait().unwrap().is_none() && !cb.join("b.out").exists(), "{tag}: B waits for A");
            fs::write(ca.join("go"), b"").unwrap();
            assert!(a.wait().unwrap().success(), "{tag}");
            assert!(b.wait().unwrap().success(), "{tag}");
            let (ra, rb) = (fs::read_to_string(ca.join("a.out")).unwrap(), fs::read_to_string(cb.join("b.out")).unwrap());
            let ctx = format!("{tag}: A {ra} / B {rb}");
            assert!(ra.starts_with("Ok(") && ra.contains("replaced"), "{ctx}");
            assert!(rb.starts_with("Ok(") && !rb.contains("earlier Replace"), "{ctx}: B never repaired A");
            let h = r27_check(&dst, &ctx);
            assert_eq!(h, if variant == "replace-same" { b"BBB" } else { b"AAA" }, "{ctx}");
            let mut want = vec!["abcd0001", "abcd0001.data"];
            if variant == "skip-disc2" {
                want.extend(["beef0002", "beef0002.data"]);
            }
            assert_eq!(names_in_dir(&dst), want, "{ctx}");
        }
    }
}

/// R29 M: deleting or replacing the lock while it is held never lets a second job in.
#[test]
fn r29_lock_survives_delete_and_replace() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    let td = crate::testutil::TmpDir::new("arch-r29-lock");
    for attack in ["delete", "replace"] {
        let ct = td.path().join(attack).join("4D5307E6").join("00007000");
        let held = lock_type_folder(&ct, &|| false, &mut || {}).unwrap();
        let lp = ct.join(FOLDER_LOCK);
        let other = ct.join(".other");
        fs::write(&other, b"x").unwrap();
        let r = if attack == "delete" { fs::remove_file(&lp) } else { fs::rename(&other, &lp) };
        if cfg!(windows) {
            assert!(r.is_err(), "{attack}: the held lock file can't be deleted or replaced");
        }
        let got = Arc::new(AtomicBool::new(false));
        let (g, c) = (got.clone(), ct.clone());
        let b = std::thread::spawn(move || {
            let l = lock_type_folder(&c, &|| false, &mut || {}).unwrap();
            g.store(true, Ordering::SeqCst);
            drop(l);
        });
        std::thread::sleep(Duration::from_millis(400));
        assert!(!got.load(Ordering::SeqCst), "{attack}: second job must still wait");
        drop(held);
        b.join().unwrap();
        assert!(got.load(Ordering::SeqCst), "{attack}");
    }
}

/// R29 M: if the path is swapped between opening and locking, the lock is retaken on what the
/// path names now, so two jobs never hold "the" lock on different objects.
#[test]
#[cfg(unix)]
fn r29_lock_identity_is_rechecked() {
    use std::os::unix::fs::MetadataExt;
    let td = crate::testutil::TmpDir::new("arch-r29-ident");
    let ct = td.path().join("00007000");
    fs::create_dir_all(&ct).unwrap();
    let old = td.path().join("old");
    LOCK_RACE.with(|h| {
        *h.borrow_mut() = Some(Box::new(move |c: &Path| {
            fs::rename(c, &old).unwrap();
            fs::create_dir(c).unwrap();
        }))
    });
    let l = lock_type_folder(&ct, &|| false, &mut || {}).unwrap();
    assert_eq!(l.metadata().unwrap().ino(), fs::metadata(&ct).unwrap().ino(), "locked the folder now at the path");
    // The swapped-out folder is not locked by us.
    let o = fs::File::open(td.path().join("old")).unwrap();
    assert!(o.try_lock().is_ok());
}

/// R29 M: moves into place never overwrite.
#[test]
fn r29_rename_noreplace() {
    let td = crate::testutil::TmpDir::new("arch-r29-norepl");
    let d = td.path();
    fs::write(d.join("a"), b"A").unwrap();
    fs::write(d.join("b"), b"B").unwrap();
    let e = rename_noreplace(&d.join("a"), &d.join("b")).unwrap_err();
    assert_eq!(e.kind(), std::io::ErrorKind::AlreadyExists, "{e}");
    assert_eq!(fs::read(d.join("b")).unwrap(), b"B");
    fs::create_dir_all(d.join("da")).unwrap();
    fs::create_dir_all(d.join("db")).unwrap();
    assert!(rename_noreplace(&d.join("da"), &d.join("db")).is_err(), "not even onto an empty folder");
    rename_noreplace(&d.join("a"), &d.join("c")).unwrap();
    assert_eq!(fs::read(d.join("c")).unwrap(), b"A");
}

/// R29: notes larger than 64 KiB are never read, only reported.
#[test]
fn r29_oversized_note_is_refused() {
    let td = crate::testutil::TmpDir::new("arch-r29-big");
    let d = td.path();
    let mut big = br#"{"phase":"finishing","old":[],"new":[]}"#.to_vec();
    big.resize(70 * 1024, b' ');
    fs::write(d.join(".unpack-replace.0.json"), &big).unwrap();
    fs::write(d.join(".unpack-replace.1.tmp"), &big).unwrap();
    let mut w = vec![];
    recover_asides_locked(d, &mut w);
    assert_eq!(w.len(), 2, "{w:?}");
    assert!(d.join(".unpack-replace.0.json").exists() && d.join(".unpack-replace.1.tmp").exists());
}

/// R29: the .data fingerprint sees a same-size change past the first MiB (tail hash).
#[test]
fn r29_fingerprint_sees_tail_change() {
    let td = crate::testutil::TmpDir::new("arch-r29-fp");
    let d = td.path().join("x.data");
    fs::create_dir_all(&d).unwrap();
    let mut body = vec![7u8; 5 << 20];
    fs::write(d.join("Data0000"), &body).unwrap();
    let a = fingerprint(&d).unwrap();
    let n = body.len();
    body[n - 10] = 8;
    fs::write(d.join("Data0000"), &body).unwrap();
    assert_ne!(a, fingerprint(&d).unwrap());
}

/// 1.7.3: extras are recognised by type; game files never are.
#[test]
fn extras_are_only_non_game_types() {
    for n in ["Vimm's Lair.txt", "x.NFO", "site.url", "cover.jpg", "Thumbs.db", "a.sfv"] {
        assert!(is_extra(n), "{n}");
    }
    for n in ["game.iso", "default.xex", "4D5307E6", "Data0000", "a.ini", "x.bin", "readme"] {
        assert!(!is_extra(n), "{n}");
    }
}

/// 1.7.3: the lock file is gone once the job is done (Windows), a stale one from a dead job is
/// taken over and removed, and a waiter still gets in after the holder deleted it.
#[test]
fn lock_file_removed_after_work_and_stale_one_cleaned() {
    let td = crate::testutil::TmpDir::new("cs32-lockfile");
    let ct = td.path().join("4D5307E6").join("00007000");
    let lp = ct.join(FOLDER_LOCK);
    let l = lock_type_folder(&ct, &|| false, &mut || {}).unwrap();
    assert_eq!(lp.exists(), cfg!(windows));
    drop(l);
    assert!(!lp.exists(), "no lock file left after the work");
    // Stale lock from a crashed run (pid 7296): tolerated, then removed.
    fs::write(&lp, b"unpack-folder-lock v1 pid=7296\n").unwrap();
    let l = lock_type_folder(&ct, &|| false, &mut || {}).unwrap();
    drop(l);
    assert!(!lp.exists() || cfg!(unix), "stale lock removed");
    let _ = fs::remove_file(&lp);
    // Holder deletes while a waiter is queued: the waiter still gets exactly one lock.
    let held = lock_type_folder(&ct, &|| false, &mut || {}).unwrap();
    let c = ct.clone();
    let b = std::thread::spawn(move || drop(lock_type_folder(&c, &|| false, &mut || {}).unwrap()));
    std::thread::sleep(Duration::from_millis(300));
    assert!(!b.is_finished(), "waiter waits");
    drop(held);
    b.join().unwrap();
    assert!(!lp.exists());
    assert_eq!(names_in_dir(&ct), Vec::<String>::new(), "nothing of ours left in the type folder");
}

/// 1.7.3: a normal (unstaged) extract aimed at a Content profile drops readme/NFO/picture
/// extras and says so; the ISO and other game files are placed as before.
#[test]
fn content_profile_extract_skips_extras() {
    let d = tmp("cs32-content-extras");
    let usb = d.join("usb");
    let prof = usb.join("Content").join("0000000000000000");
    fs::create_dir_all(&prof).unwrap();
    let zip = d.join("Bully.zip");
    write_zip(
        &zip,
        &[("Bully/game.iso", b"not really an iso", 0o644), ("Bully/Vimm's Lair.txt", b"x", 0o644), ("Bully/Game/default.xex", b"XEX2", 0o644), ("Bully/Game/notes.txt", b"n", 0o644)],
    );
    let o = ExtractOptions {
        archive: &zip,
        dest_parent: &prof,
        password: None,
        collision: Collision::Number,
        layout: Layout::Folder,
        skip_junk: true,
        nested: false,
    };
    let r = extract(&exe(), &o, &Jobs::default(), "ci", &mut |_, _| {}).unwrap();
    let out = Path::new(&r.output);
    assert_eq!(names_in_dir(out), vec!["Game", "game.iso"]);
    assert_eq!(names_in_dir(&out.join("Game")), vec!["default.xex", "notes.txt"], "R33: a game folder is left whole");
    assert!(r.warnings.iter().any(|w| w.contains("Skipped 1 extra file") && w.contains("Vimm's Lair.txt")), "{:?}", r.warnings);
    assert!(names_in_dir(&prof).is_empty());
}

/// R33 M1 (Overseer's repro): only extras beside the ISO go; game and app folders keep every
/// file, including pictures, text files, web UI and an empty folder from the archive.
#[test]
fn r33_extras_never_touch_game_or_app_folders() {
    let d = tmp("cs33-extras");
    let usb = d.join("usb");
    let prof = usb.join("Content").join("0000000000000000");
    fs::create_dir_all(&prof).unwrap();
    let zip = d.join("Bully.zip");
    write_zip(
        &zip,
        &[
            ("Bully/Bully (USA).iso", b"not really an iso", 0o644),
            ("Bully/Vimm's Lair.TXT", b"x", 0o644),
            ("Bully/XexGame/default.xex", b"XEX2", 0o644),
            ("Bully/XexGame/media/ui.png", b"png", 0o644),
            ("Bully/XexGame/data/strings.txt", b"str", 0o644),
            ("Bully/XexGame/config.xml", b"<x/>", 0o644),
            ("Bully/XexGame/cache/", b"", 0o40755),
            ("Bully/Aurora/Aurora.xex", b"XEX2", 0o644),
            ("Bully/Aurora/Skins/Default/bg.png", b"png", 0o644),
            ("Bully/Aurora/web/index.html", b"<html>", 0o644),
            ("Bully/Scans/cover.jpg", b"jpg", 0o644),
            ("Bully/Empty/", b"", 0o40755),
        ],
    );
    let o = ExtractOptions {
        archive: &zip,
        dest_parent: &prof,
        password: None,
        collision: Collision::Number,
        layout: Layout::Folder,
        skip_junk: true,
        nested: false,
    };
    let r = extract(&exe(), &o, &Jobs::default(), "ci", &mut |_, _| {}).unwrap();
    let out = Path::new(&r.output);
    for f in ["XexGame/default.xex", "XexGame/media/ui.png", "XexGame/data/strings.txt", "XexGame/config.xml", "Aurora/Aurora.xex", "Aurora/Skins/Default/bg.png", "Aurora/web/index.html"] {
        assert!(out.join(f).is_file(), "{f} must survive");
    }
    assert!(out.join("XexGame/cache").is_dir(), "empty folder from the archive stays");
    assert!(out.join("Empty").is_dir(), "empty folder from the archive stays");
    assert!(!out.join("Vimm's Lair.TXT").exists() && !out.join("Scans").exists(), "extras beside the ISO go");
    assert!(out.join("Bully (USA).iso").is_file());
    let w: Vec<_> = r.warnings.iter().filter(|w| w.contains("extra file")).collect();
    assert!(w.len() == 1 && w[0].contains("Skipped 2 extra files") && w[0].contains("Vimm's Lair.TXT") && w[0].contains("cover.jpg"), "{:?}", r.warnings);
}

/// R33 L4: Skip with the folder already there says only "skipped", not also "Skipped N extras".
#[test]
fn r33_skip_does_not_mention_extras() {
    let d = tmp("cs33-skip");
    let usb = d.join("usb");
    let prof = usb.join("Content").join("0000000000000000");
    fs::create_dir_all(&prof).unwrap();
    fs::create_dir_all(usb.join("Bully")).unwrap();
    let zip = d.join("Bully.zip");
    write_zip(&zip, &[("Bully/game.iso", b"iso", 0o644), ("Bully/readme.txt", b"x", 0o644)]);
    let o = ExtractOptions {
        archive: &zip,
        dest_parent: &prof,
        password: None,
        collision: Collision::Skip,
        layout: Layout::Folder,
        skip_junk: true,
        nested: false,
    };
    let r = extract(&exe(), &o, &Jobs::default(), "ci", &mut |_, _| {}).unwrap();
    assert!(r.skipped);
    assert!(r.warnings.iter().any(|w| w.contains("nothing was extracted")), "{:?}", r.warnings);
    assert!(!r.warnings.iter().any(|w| w.contains("extra file")), "{:?}", r.warnings);
}

/// R33 L2: a read-only lock file is a real error, reported at once, not "waiting for another job".
#[test]
#[cfg(windows)]
fn r33_read_only_lock_file_is_an_error_not_a_wait() {
    let td = crate::testutil::TmpDir::new("cs33-ro");
    let ct = td.path().join("00007000");
    fs::create_dir_all(&ct).unwrap();
    let lp = ct.join(FOLDER_LOCK);
    fs::write(&lp, b"").unwrap();
    let mut perm = fs::metadata(&lp).unwrap().permissions();
    perm.set_readonly(true);
    fs::set_permissions(&lp, perm.clone()).unwrap();
    let t = std::time::Instant::now();
    let mut waited = false;
    let r = lock_type_folder(&ct, &|| false, &mut || waited = true);
    assert!(r.is_err() && !waited && t.elapsed() < Duration::from_secs(3), "{:?}", r.err());
    #[allow(clippy::permissions_set_readonly_false)]
    perm.set_readonly(false);
    fs::set_permissions(&lp, perm).unwrap();
}

/// R34 L5: a folder is judged by its whole subtree, so Game/readme.txt and Game/Art/box.png
/// stay when Game/Disc1/default.xex sits below them; extras beside the ISO still go.
#[test]
fn r34_l5_drop_extras_keeps_folder_with_xex_deeper_down() {
    let td = crate::testutil::TmpDir::new("arch-r34-l5");
    let root = td.join("iso");
    fs::create_dir_all(root.join("Game/Art")).unwrap();
    fs::create_dir_all(root.join("Game/Disc1")).unwrap();
    fs::write(root.join("Game/readme.txt"), b"r").unwrap();
    fs::write(root.join("Game/Art/box.png"), b"p").unwrap();
    fs::write(root.join("Game/Disc1/default.xex"), b"x").unwrap();
    fs::write(root.join("game.iso"), b"i").unwrap();
    fs::write(root.join("info.nfo"), b"n").unwrap();
    let only = td.join("only");
    fs::create_dir_all(only.join("Scans")).unwrap();
    fs::write(only.join("info.nfo"), b"n").unwrap();
    fs::write(only.join("Scans/cover.jpg"), b"j").unwrap();
    let note = drop_extras(&root).unwrap();
    assert!(note.contains("Skipped 1 extra file") && note.contains("info.nfo"), "{note}");
    assert!(root.join("game.iso").is_file());
    assert!(root.join("Game/readme.txt").is_file());
    assert!(root.join("Game/Art/box.png").is_file());
    assert!(root.join("Game/Disc1/default.xex").is_file());
    assert!(drop_extras(&only).is_some());
    assert!(!only.join("info.nfo").exists() && !only.join("Scans").exists());
}

/// R35 L5 (perf): drop_extras does one bottom-up pass, so a deep extras-only tree is quick, and a
/// deep chain with a .xex at the bottom is still kept whole. (30 s bound: Wine and busy boxes are slow.)
#[test]
fn r35_l5_drop_extras_deep_tree_single_pass() {
    let td = crate::testutil::TmpDir::new("arch-r35-l5");
    let root = td.join("iso");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("game.iso"), b"i").unwrap();
    let mut d = root.join("Scans");
    let mut keep = root.join("Game");
    for lvl in 0..50 {
        fs::create_dir_all(&d).unwrap();
        fs::create_dir_all(&keep).unwrap();
        for i in 0..40 {
            fs::write(d.join(format!("p{lvl}_{i}.jpg")), b"j").unwrap();
        }
        fs::write(keep.join("notes.txt"), b"n").unwrap();
        d = d.join("d");
        keep = keep.join("d");
    }
    fs::create_dir_all(&keep).unwrap();
    fs::write(keep.join("default.xex"), b"x").unwrap();
    let t = std::time::Instant::now();
    let note = drop_extras(&root).unwrap();
    assert!(t.elapsed() < std::time::Duration::from_secs(30), "{:?}", t.elapsed());
    assert!(note.contains("Skipped 2000 extra files"), "{}", &note[..80]);
    assert!(!root.join("Scans").exists());
    assert!(keep.join("default.xex").is_file() && root.join("Game/notes.txt").is_file());
}

/// A-1: after both halves are live, corrupt live `.data` before repair must not delete `.unpack-old.*`.
#[test]
fn a1_corrupt_live_data_after_step6_kill_keeps_old_aside() {
    let td = crate::testutil::TmpDir::new("arch-a1-corrupt");
    let prof = td.path().join("prof");
    let dst = r27_setup(&prof);
    let (st, tree) = r27_stage(td.path(), "a1", b"NEW");
    REPLACE_KILL_AT.with(|k| k.set(6));
    assert!(place_title_trees(&st, &[tree], &prof, Collision::Replace, 1, 0, 0, vec![], &Jobs::default(), "t", &mut |_, _| {}).is_err());
    REPLACE_KILL_AT.with(|k| k.set(0));
    let data_live = fs::read_dir(&dst)
        .unwrap()
        .flatten()
        .find(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case("abcd0001.data"))
        .map(|e| e.path().join("Data0000"))
        .expect("live .data");
    fs::write(&data_live, b"CORRUPT").unwrap();
    let mut w = vec![];
    recover_asides_locked(&dst, &mut w);
    assert!(
        w.iter().any(|x| x.contains("no longer matches what finished moving")),
        "{w:?}"
    );
    assert!(
        names_in_dir(&dst).iter().any(|n| n.starts_with(".unpack-old.")),
        "old aside kept: {:?}",
        names_in_dir(&dst)
    );
    assert!(
        !names_in_dir(&dst).iter().any(|n| n.starts_with(".unpack-replace.")),
        "manifest cleared: {:?}",
        names_in_dir(&dst)
    );
    let hdr = fs::read_dir(&dst)
        .unwrap()
        .flatten()
        .find(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case("abcd0001"))
        .map(|e| fs::read(e.path()).unwrap());
    assert_eq!(hdr.as_deref(), Some(b"NEW" as &[u8]));
    assert_eq!(fs::read(&data_live).unwrap(), b"CORRUPT");
    let aside_old = names_in_dir(&dst)
        .iter()
        .find(|n| n.starts_with(".unpack-old.") && !n.contains(".data"))
        .map(|n| fs::read(dst.join(n)).unwrap());
    assert_eq!(aside_old.as_deref(), Some(b"OLD" as &[u8]), "OLD header still in aside");
}

/// K1: killed Replace, user edits the new .data before repair — never pair the old header with edited data.
#[test]
fn k1_edited_data_after_kill_never_mixes_package() {
    let td = crate::testutil::TmpDir::new("arch-k1-edit");
    for step in 1..=9 {
        let prof = td.path().join(format!("p{step}"));
        let dst = r27_setup(&prof);
        let (st, tree) = r27_stage(td.path(), &format!("k1{step}a"), b"NEW");
        REPLACE_KILL_AT.with(|k| k.set(step));
        assert!(place_title_trees(&st, &[tree], &prof, Collision::Replace, 1, 0, 0, vec![], &Jobs::default(), "t", &mut |_, _| {}).is_err());
        REPLACE_KILL_AT.with(|k| k.set(0));
        let (st2, tree2) = r27_stage(td.path(), &format!("k1{step}b"), b"NEW");
        if step >= 6 {
            let data_live = fs::read_dir(&dst)
                .unwrap()
                .flatten()
                .find(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case("abcd0001.data"))
                .map(|e| e.path().join("Data0000"));
            if let Some(p) = data_live {
                fs::write(&p, b"USER-EDIT").unwrap();
            }
        }
        let r = place_title_trees(&st2, &[tree2], &prof, Collision::Skip, 1, 0, 0, vec![], &Jobs::default(), "t", &mut |_, _| {}).unwrap();
        let _ = r;
        let live_hdr = fs::read_dir(&dst)
            .unwrap()
            .flatten()
            .find(|e| {
                let n = e.file_name();
                let n = n.to_string_lossy();
                n.len() == 8 && n.eq_ignore_ascii_case("abcd0001")
            })
            .map(|e| fs::read(e.path()).unwrap());
        let live_data = fs::read_dir(&dst)
            .unwrap()
            .flatten()
            .find(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case("abcd0001.data"))
            .map(|e| fs::read(e.path().join("Data0000")).unwrap());
        let ctx = format!("step {step}: {:?} {:?}", live_hdr, live_data);
        if step >= 6 {
            assert_eq!(live_hdr.as_deref(), Some(b"NEW" as &[u8]), "{ctx}: new header kept, not the old one");
            assert_eq!(live_data.as_deref(), Some(b"USER-EDIT" as &[u8]), "{ctx}: user's edit kept");
            assert_ne!(live_hdr.as_deref(), Some(b"OLD" as &[u8]), "{ctx}: never the old header beside edited data");
        }
    }
}

/// K2: renaming the type folder during Replace stops the job and rolls back what is provably ours.
#[test]
#[cfg(unix)]
fn k2_rename_during_replace_stops_and_rolls_back() {
    let _serial = R27_PAUSE_TEST.lock().unwrap_or_else(|e| e.into_inner());
    let td = crate::testutil::TmpDir::new("arch-k2-rename");
    let prof = td.path().join("prof");
    let dst = r27_setup(&prof);
    let sh = td.path().join("abcd0001");
    fs::write(&sh, b"NEW").unwrap();
    let sd = td.path().join("abcd0001.data");
    fs::create_dir_all(&sd).unwrap();
    fs::write(sd.join("Data0000"), b"NEW").unwrap();
    let (dh, dd) = (dst.join("abcd0001"), dst.join("abcd0001.data"));
    let dst_c = dst.clone();
    let away = td.path().join("away");
    let away_c = away.clone();
    *REPLACE_PAUSE.lock().unwrap() = Some(Box::new(move |n| {
        if n == 4 {
            fs::rename(&dst_c, &away_c).unwrap();
            fs::create_dir_all(&dst_c).unwrap();
        }
    }));
    let e = std::thread::Builder::new()
        .name("k2-rename".into())
        .spawn(move || replace_unit_locked(&dst, [Some(dst.join("ABCD0001")), Some(dst.join("ABCD0001.data"))], [(&sh, &dh), (&sd, &dd)]))
        .unwrap()
        .join()
        .unwrap()
        .unwrap_err();
    *REPLACE_PAUSE.lock().unwrap() = None;
    assert!(e.contains("moved or renamed"), "{e}");
    let old_hdr = fs::read_dir(&away)
        .unwrap()
        .flatten()
        .find(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case("ABCD0001"))
        .map(|e| e.path());
    let old_hdr = old_hdr.or_else(|| {
        fs::read_dir(&away)
            .unwrap()
            .flatten()
            .find(|e| e.file_name().to_string_lossy().starts_with(".unpack-old.") && e.file_name().to_string_lossy().ends_with("ABCD0001"))
            .map(|e| e.path())
    });
    assert_eq!(fs::read(old_hdr.expect("old header in away")).unwrap(), b"OLD");
    assert!(!away.join("abcd0001").exists());
}

/// H1: a truncated live `.data` from an interrupted cross-volume copy must not finish the Replace
/// or delete the only good copy in `.unpack-old.*`.
#[test]
fn h1_partial_live_data_restores_old_aside() {
    let td = crate::testutil::TmpDir::new("arch-h1-partial");
    let prof = td.path().join("prof");
    let dst = r27_setup(&prof);
    let (st, tree) = r27_stage(td.path(), "h1a", b"NEW");
    REPLACE_KILL_AT.with(|k| k.set(5));
    assert!(place_title_trees(&st, &[tree], &prof, Collision::Replace, 1, 0, 0, vec![], &Jobs::default(), "t", &mut |_, _| {}).is_err());
    REPLACE_KILL_AT.with(|k| k.set(0));
    let note = fs::read_dir(&dst)
        .unwrap()
        .flatten()
        .find(|e| e.file_name().to_string_lossy().starts_with(".unpack-replace.") && e.file_name().to_string_lossy().ends_with(".json"))
        .map(|e| e.path())
        .expect("manifest");
    let mut m: ReplaceManifest = serde_json::from_slice(&fs::read(&note).unwrap()).unwrap();
    m.moved_live.retain(|n| n.eq_ignore_ascii_case("abcd0001"));
    fs::write(&note, serde_json::to_vec(&m).unwrap()).unwrap();
    fs::create_dir_all(dst.join("abcd0001.data")).unwrap();
    fs::write(dst.join("abcd0001.data/trunc"), b"partial").unwrap();
    let mut w = vec![];
    recover_asides_locked(&dst, &mut w);
    assert_eq!(r27_check(&dst, "h1-partial"), b"OLD");
    assert!(w.iter().any(|x| x.contains("put the old package back")), "{w:?}");
}

/// M1: notes written by 1.7.3 (old halves without fingerprints) still repair after upgrade.
#[test]
fn m1_legacy_replace_note_repairs_after_upgrade() {
    let td = crate::testutil::TmpDir::new("arch-m1-legacy");
    let prof = td.path().join("prof");
    let dst = r27_setup(&prof);
    let (st, _tree) = r27_stage(td.path(), "leg", b"NEW");
    let hdr = st.join("4D5307E6/00007000/abcd0001");
    let data = st.join("4D5307E6/00007000/abcd0001.data");
    let hfp = fingerprint(&hdr).unwrap();
    let dfp = fingerprint(&data).unwrap();
    let tag = "3";
    fs::rename(dst.join("ABCD0001"), dst.join(format!(".unpack-old.{tag}.abcd0001"))).unwrap();
    fs::rename(dst.join("ABCD0001.data"), dst.join(format!(".unpack-old.{tag}.abcd0001.data"))).unwrap();
    let m = ReplaceManifest {
        phase: "replacing".into(),
        old: vec![
            ("abcd0001".into(), format!(".unpack-old.{tag}.abcd0001"), false, String::new()),
            ("abcd0001.data".into(), format!(".unpack-old.{tag}.abcd0001.data"), true, String::new()),
        ],
        new: vec![
            ("abcd0001".into(), format!(".unpack-new.{tag}.abcd0001"), false, hfp),
            ("abcd0001.data".into(), format!(".unpack-new.{tag}.abcd0001.data"), true, dfp),
        ],
        moved_live: vec![],
    };
    let note = dst.join(format!(".unpack-replace.{tag}.json"));
    fs::write(&note, serde_json::to_vec(&m).unwrap()).unwrap();
    let mut w = vec![];
    recover_asides_locked(&dst, &mut w);
    assert_eq!(r27_check(&dst, "m1-legacy"), b"OLD");
    assert!(w.iter().any(|x| x.contains("put the old package back")), "{w:?}");
}

/// K2 (logic): folder identity check fails when the path no longer names the locked directory.
#[test]
fn k2_folder_identity_mismatch_is_detected() {
    let td = crate::testutil::TmpDir::new("arch-k2-ident");
    let ct = td.path().join("00007000");
    fs::create_dir_all(&ct).unwrap();
    let lock = lock_type_folder(&ct, &|| false, &mut || {}).unwrap();
    assert!(lock.ensure_same_folder().is_ok());
    let old = td.path().join("old");
    fs::rename(&ct, &old).unwrap();
    fs::create_dir_all(&ct).unwrap();
    let err = lock.ensure_same_folder().unwrap_err();
    assert!(err.contains("moved or renamed"), "{err}");
}
