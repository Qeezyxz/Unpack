//! No real game ISO is available on the build box, so these tests build a tiny but
//! structurally valid XDVDFS image: volume descriptor, root directory with
//! default.xex (minimal XEX2 header carrying an execution-info block) and a large
//! sparse "pad.bin" so the output spans several GOD parts.
use super::*;
use crate::archive::Jobs;
use std::io::{Seek, SeekFrom, Write};

const SECTOR: u64 = 0x800;

fn make_iso(path: &Path, title_id: u32, pad_bytes: u64) {
    let mut f = File::create(path).unwrap();
    // volume descriptor at sector 32 (XSF layout, root offset 0)
    f.seek(SeekFrom::Start(32 * SECTOR)).unwrap();
    f.write_all(b"MICROSOFT*XBOX*MEDIA").unwrap();
    f.write_all(&33u32.to_le_bytes()).unwrap(); // root dir sector
    f.write_all(&(SECTOR as u32).to_le_bytes()).unwrap(); // root dir size
    f.write_all(&[0u8; 8]).unwrap();
    f.seek(SeekFrom::Start(32 * SECTOR + 0x7ec)).unwrap();
    f.write_all(b"MICROSOFT*XBOX*MEDIA").unwrap();
    // root directory table at sector 33
    let mut dir = Vec::new();
    let entry = |dir: &mut Vec<u8>, right: u16, sector: u32, size: u32, name: &str| {
        dir.extend_from_slice(&0u16.to_le_bytes());
        dir.extend_from_slice(&right.to_le_bytes());
        dir.extend_from_slice(&sector.to_le_bytes());
        dir.extend_from_slice(&size.to_le_bytes());
        dir.push(0x80);
        dir.push(name.len() as u8);
        dir.extend_from_slice(name.as_bytes());
        while dir.len() % 4 != 0 { dir.push(0xff); }
    };
    let xex_size = 0x200u32;
    entry(&mut dir, 0, 34, xex_size, "default.xex");
    if pad_bytes > 0 {
        entry(&mut dir, 0, 40, pad_bytes as u32, "pad.bin");
    }
    dir.extend_from_slice(&[0xff; 4]);
    f.seek(SeekFrom::Start(33 * SECTOR)).unwrap();
    f.write_all(&dir).unwrap();
    // default.xex at sector 34
    let mut x = Vec::new();
    x.extend_from_slice(b"XEX2");
    x.extend_from_slice(&0u32.to_be_bytes()); // module flags
    x.extend_from_slice(&0x1000u32.to_be_bytes()); // code offset
    x.extend_from_slice(&0u32.to_be_bytes());
    x.extend_from_slice(&0u32.to_be_bytes()); // certificate offset
    x.extend_from_slice(&1u32.to_be_bytes()); // field count
    x.extend_from_slice(&0x0004_0006u32.to_be_bytes()); // ExecutionId
    x.extend_from_slice(&0x100u32.to_be_bytes());
    x.resize(0x100, 0);
    x.extend_from_slice(&0x1234_5678u32.to_be_bytes()); // media id
    x.extend_from_slice(&1u32.to_be_bytes()); // version
    x.extend_from_slice(&0u32.to_be_bytes()); // base version
    x.extend_from_slice(&title_id.to_be_bytes());
    x.extend_from_slice(&[0, 0, 1, 1]); // platform, exe type, disc 1 of 1
    x.resize(xex_size as usize, 0);
    f.seek(SeekFrom::Start(34 * SECTOR)).unwrap();
    f.write_all(&x).unwrap();
    // sparse pad: write a marker at the very end so the file has its full length
    if pad_bytes > 0 {
        f.seek(SeekFrom::Start(40 * SECTOR + pad_bytes - 4)).unwrap();
        f.write_all(b"END!").unwrap();
    }
}

fn tmp(name: &str) -> crate::testutil::TmpDir {
    crate::testutil::TmpDir::new(&format!("god-{name}"))
}

fn utf16be(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(|c| c.to_be_bytes()).collect()
}

#[test]
fn info_reads_title_and_name() {
    let d = tmp("info");
    let iso = d.join("halo.iso");
    make_iso(&iso, 0x4D53_07E6, 0);
    let i = iso_info(&iso).unwrap();
    assert_eq!(i.title_id, "4D5307E6");
    assert_eq!(i.name.as_deref(), Some("Halo 3"));
    assert_eq!(i.kind, "Xbox 360");
    assert_eq!(i.parts, 1);
    fs::remove_dir_all(&d).unwrap();
}

#[test]
fn rejects_non_iso() {
    let d = tmp("bad");
    let p = d.join("x.iso");
    fs::write(&p, vec![0u8; 100_000]).unwrap();
    assert!(iso_info(&p).unwrap_err().contains("Not an Xbox ISO"));
    let e = convert(&p, &d.join("out"), None, &Jobs::default(), "x", &mut |_, _| {}).unwrap_err();
    assert!(e.contains("Not an Xbox ISO"), "{e}");
    fs::remove_dir_all(&d).unwrap();
}

#[test]
fn converts_multi_part_with_progress_and_title() {
    let d = tmp("conv");
    let iso = d.join("game.iso");
    // ~400 MB of (sparse) data -> 3 parts of ~166 MB
    make_iso(&iso, 0x4D53_07E6, 400 * 1024 * 1024);
    let out = d.join("out");
    let mut seen = Vec::new();
    let r = convert(&iso, &out, Some("My Test Game"), &Jobs::default(), "g", &mut |p, l| seen.push((p, l.to_string()))).unwrap();
    assert_eq!(r.parts, 3);
    assert_eq!(r.title_id, "4D5307E6");
    let base = out.join("4D5307E6").join("00007000");
    let header = fs::read(base.join("12345678")).unwrap();
    assert_eq!(&header[..4], b"LIVE", "CON/LIVE header magic");
    assert!(header.windows(utf16be("My Test Game").len()).any(|w| w == utf16be("My Test Game").as_slice()), "title in header");
    for i in 0..3 {
        assert!(base.join("12345678.data").join(format!("Data{i:04}")).is_file());
    }
    assert!(!base.join("12345678.data").join("Data0003").exists());
    let pcts: Vec<u32> = seen.iter().map(|s| s.0).collect();
    assert!(pcts.windows(2).all(|w| w[0] <= w[1]), "{pcts:?}");
    assert_eq!(*pcts.last().unwrap(), 100);
    // converting again replaces the parts cleanly
    convert(&iso, &out, None, &Jobs::default(), "g2", &mut |_, _| {}).unwrap();
    let header2 = fs::read(base.join("12345678")).unwrap();
    assert!(header2.windows(utf16be("Halo 3").len()).any(|w| w == utf16be("Halo 3").as_slice()), "title from built-in list");
    fs::remove_dir_all(&d).unwrap();
}

#[test]
fn cancel_cleans_up() {
    let d = tmp("cancel");
    let iso = d.join("game.iso");
    make_iso(&iso, 0x4D53_07E6, 400 * 1024 * 1024);
    let out = d.join("out");
    let jobs = Jobs::default();
    jobs.register("c");
    let j2 = jobs.clone();
    let e = convert(&iso, &out, None, &jobs, "c", &mut |p, _| { if p > 0 { j2.cancel("c"); } }).unwrap_err();
    assert_eq!(e, ERR_CANCELLED);
    assert!(!out.join("4D5307E6").exists(), "partial output removed");
    assert_eq!(fs::read_dir(&out).unwrap().count(), 0, "no temp folder left");
    fs::remove_dir_all(&d).unwrap();
}

#[test]
fn long_titles_are_clamped_and_header_stays_intact() {
    assert_eq!(clamp_title(&"x".repeat(200)).encode_utf16().count(), 63);
    assert_eq!(clamp_title(&"😀".repeat(40)).encode_utf16().count(), 62); // no split surrogate
    let d = tmp("long");
    let iso = d.join("g.iso");
    make_iso(&iso, 0x0ABC_DEF1, 0);
    let out = d.join("out");
    convert(&iso, &out, Some(&"Z".repeat(300)), &Jobs::default(), "l", &mut |_, _| {}).unwrap();
    let h = fs::read(out.join("0ABCDEF1/00007000/12345678")).unwrap();
    let z = utf16be(&"Z".repeat(63));
    let pos = h.windows(z.len()).position(|w| w == z.as_slice()).unwrap();
    assert_eq!(&h[pos + z.len()..pos + z.len() + 2], &[0, 0], "terminated, not overflowing");
    fs::remove_dir_all(&d).unwrap();
}

/// M2: a re-convert that is cancelled or fails must leave the previous GOD package intact.
#[test]
fn m2_failed_reconvert_keeps_previous_output() {
    let d = tmp("m2");
    let iso = d.join("game.iso");
    make_iso(&iso, 0x4D53_07E6, 400 * 1024 * 1024);
    let out = d.join("out");
    convert(&iso, &out, Some("First"), &Jobs::default(), "a", &mut |_, _| {}).unwrap();
    let base = out.join("4D5307E6/00007000");
    let snapshot = |b: &Path| -> Vec<(String, Vec<u8>)> {
        let mut v = vec![("hdr".into(), fs::read(b.join("12345678")).unwrap())];
        for i in 0..3 { v.push((format!("{i}"), fs::read(b.join(format!("12345678.data/Data{i:04}"))).unwrap())); }
        v
    };
    let before = snapshot(&base);
    // cancelled at various points (after part 1, after part 2, at the hashing step)
    for stop_at in [1u32, 50, 75] {
        let jobs = Jobs::default();
        jobs.register("c");
        let j2 = jobs.clone();
        let e = convert(&iso, &out, Some("Second"), &jobs, "c", &mut |p, _| { if p >= stop_at { j2.cancel("c"); } }).unwrap_err();
        assert_eq!(e, ERR_CANCELLED, "stop at {stop_at}");
        assert!(snapshot(&base) == before, "previous output changed (stop at {stop_at})");
    }
    // failure mid-way (the work folder vanishes, like an unplugged drive) -> error, old output intact
    let work = out.join(".unpack-god-tmp-f");
    let r = convert(&iso, &out, None, &Jobs::default(), "f", &mut |p, _| { if p > 0 { let _ = fs::remove_dir_all(&work); } });
    assert!(r.is_err(), "write failure should be an error");
    assert!(snapshot(&base) == before, "previous output changed after error");
    let leftovers: Vec<_> = fs::read_dir(&out).unwrap().flatten().map(|e| e.file_name()).collect();
    assert_eq!(leftovers, vec![std::ffi::OsString::from("4D5307E6")], "temp folders left: {leftovers:?}");
    let in_base: Vec<_> = fs::read_dir(&base).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    assert!(in_base.iter().all(|n| !n.starts_with(".unpack")), "{in_base:?}");
    // and a successful re-convert replaces it
    convert(&iso, &out, Some("Third"), &Jobs::default(), "b", &mut |_, _| {}).unwrap();
    let h = fs::read(base.join("12345678")).unwrap();
    assert!(h.windows(utf16be("Third").len()).any(|w| w == utf16be("Third").as_slice()));
    fs::remove_dir_all(&d).unwrap();
}

/// Batch auto mode via the Rust registry: new "<Game> (GOD)" folder next to the extraction
/// folder, never reused, non-Xbox ISO creates nothing, cancel cleans up, delete only when allowed.
#[test]
fn auto_named_claims_new_folder_and_rejects_non_xbox() {
    let d = tmp("auto");
    let ext = d.join("archive");
    fs::create_dir_all(ext.join("sub")).unwrap();
    let iso = ext.join("sub/halo.iso");
    make_iso(&iso, 0x4D53_07E6, 0);
    let reg = |p: &Path, del: bool| register_extracted(&[p.display().to_string()], &ext, del).remove(0);
    let names = |d: &Path| { let mut v: Vec<String> = fs::read_dir(d).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect(); v.sort(); v };
    let first = convert_registered(&reg(&iso, true), false, &Jobs::default(), "a1", &mut |_, _| {}).unwrap();
    assert!(!first.iso_deleted && iso.exists() && first.output.contains("(GOD)"));
    let god_dir = names(&d).into_iter().find(|x| x.ends_with("(GOD)")).expect("named folder");
    let marker = d.join(&god_dir).join("keep.txt");
    fs::write(&marker, b"x").unwrap();
    // delete wanted but not allowed (Replace) -> kept with a reason
    let second = convert_registered(&reg(&iso, false), true, &Jobs::default(), "a2", &mut |_, _| {}).unwrap();
    assert!(second.output.contains("(GOD)-2"), "{}", second.output);
    assert!(!second.iso_deleted && iso.exists() && second.kept_reason.unwrap().contains("Replace"));
    assert_eq!(fs::read(&marker).unwrap(), b"x");
    // non-Xbox ISO: error, nothing created, ISO kept
    let bad = ext.join("linux.iso");
    fs::write(&bad, vec![0u8; 200_000]).unwrap();
    let before = names(&d);
    assert!(convert_registered(&reg(&bad, true), true, &Jobs::default(), "b", &mut |_, _| {}).is_err());
    assert_eq!(names(&d), before);
    assert!(bad.exists());
    // cancelled: claimed folder removed, ISO kept
    let jobs = Jobs::default(); jobs.register("c"); let j2 = jobs.clone();
    let e = convert_registered(&reg(&iso, true), true, &jobs, "c", &mut |_, _| { j2.cancel("c"); }).unwrap_err();
    assert_eq!(e, ERR_CANCELLED);
    assert_eq!(names(&d), before);
    assert!(iso.exists());
    // unknown token (M2: the page can't name arbitrary files)
    assert!(convert_registered("iso-forged", true, &Jobs::default(), "u", &mut |_, _| {}).is_err());
    // allowed + verified -> extracted ISO deleted
    // not sealed (no full fingerprint) -> kept
    let r = convert_registered(&reg(&iso, true), true, &Jobs::default(), "y", &mut |_, _| {}).unwrap();
    assert!(!r.iso_deleted && iso.exists() && r.kept_reason.unwrap().contains("fingerprint"));
    let t = reg(&iso, true);
    seal_tokens(&[t.clone()]);
    let r = convert_registered(&t, true, &Jobs::default(), "z", &mut |_, _| {}).unwrap();
    assert!(r.iso_deleted && !iso.exists());
    // single use
    assert!(convert_registered(&t, false, &Jobs::default(), "z2", &mut |_, _| {}).unwrap_err().contains("already used"));
    fs::remove_dir_all(&d).unwrap();
}

/// Overseer round-3 H1: archive A registers Game/game.iso, archive B (Replace) overwrites the
/// same path with a different game before A's conversion runs. A must refuse to convert or
/// delete B's file.
#[test]
fn r3h1_swapped_iso_is_not_converted_or_deleted() {
    let d = tmp("swap");
    let ext = d.join("Game");
    fs::create_dir_all(&ext).unwrap();
    let iso = ext.join("game.iso");
    make_iso(&iso, 0x4D53_07E6, 0);
    let ta = register_extracted(&[iso.display().to_string()], &ext, true).remove(0);
    std::thread::sleep(std::time::Duration::from_millis(20));
    make_iso(&iso, 0x0ABC_DEF1, 0); // same size, different content
    let tb = register_extracted(&[iso.display().to_string()], &ext, false).remove(0);
    let e = convert_registered(&ta, true, &Jobs::default(), "a", &mut |_, _| {}).unwrap_err();
    assert!(e.contains("replaced or changed"), "{e}");
    assert!(iso.exists());
    assert!(!fs::read_dir(&d).unwrap().flatten().any(|e| e.file_name().to_string_lossy().contains("(GOD)")));
    let r = convert_registered(&tb, true, &Jobs::default(), "b", &mut |_, _| {}).unwrap();
    assert!(!r.iso_deleted && iso.exists(), "B was merged: never deleted");
    fs::remove_dir_all(&d).unwrap();
}

/// Overseer round-3 L1: a cancel that arrives after the package is placed keeps the ISO.
#[test]
fn r3l1_late_cancel_keeps_iso() {
    let d = tmp("late");
    let ext = d.join("x");
    fs::create_dir_all(&ext).unwrap();
    let iso = ext.join("g.iso");
    make_iso(&iso, 0x4D53_07E6, 0);
    let t = register_extracted(&[iso.display().to_string()], &ext, true).remove(0);
    seal_tokens(&[t.clone()]);
    let jobs = Jobs::default(); jobs.register("l"); let j2 = jobs.clone();
    let r = convert_registered(&t, true, &jobs, "l", &mut |_, f| { if f.starts_with("Verifying") { j2.cancel("l"); } }).unwrap();
    assert!(!r.iso_deleted && iso.exists() && r.kept_reason.unwrap().contains("cancel"));
    fs::remove_dir_all(&d).unwrap();
}

/// Overseer H1 regression: a truncated Xbox ISO is refused (info + convert + extracted/delete),
/// the ISO is kept, and nothing is created.
#[test]
fn h1_truncated_iso_is_refused_and_kept() {
    let d = tmp("h1");
    let iso = d.join("cut.iso");
    make_iso(&iso, 0x4D53_07E6, 400 * 1024 * 1024);
    let full = fs::metadata(&iso).unwrap().len();
    let f = File::options().write(true).open(&iso).unwrap();
    f.set_len(full / 2).unwrap();
    drop(f);
    assert!(iso_info(&iso).unwrap_err().contains("truncated"));
    assert!(register_extracted(&[iso.display().to_string()], &d, true).iter().all(|t| !t.is_empty()));
    let t = register_extracted(&[iso.display().to_string()], &d, true).remove(0);
    let e = convert_registered(&t, true, &Jobs::default(), "h", &mut |_, _| {}).unwrap_err();
    assert!(e.contains("truncated"), "{e}");
    assert!(iso.exists());
    let left: Vec<_> = fs::read_dir(&d).unwrap().flatten().map(|e| e.file_name()).collect();
    assert_eq!(left, vec![std::ffi::OsString::from("cut.iso")]);
    fs::remove_dir_all(&d).unwrap();
}

/// verify_package catches a damaged part (wrong size), a broken hash chain and a bad header.
#[test]
fn verify_package_detects_damage() {
    let d = tmp("vp");
    let iso = d.join("g.iso");
    make_iso(&iso, 0x4D53_07E6, 400 * 1024 * 1024);
    let out = d.join("out");
    convert(&iso, &out, None, &Jobs::default(), "v", &mut |_, _| {}).unwrap();
    let f = File::open(&iso).unwrap();
    let src = iso::IsoReader::read(f).unwrap();
    let root = src.volume_descriptor.root_offset;
    let mut img = File::open(&iso).unwrap();
    let ti = TitleInfo::from_image(&mut iso::IsoReader::read(File::open(&iso).unwrap()).unwrap()).unwrap();
    let _ = &mut img;
    let layout = god::FileLayout::new(&out, &ti.execution_info, ti.content_type);
    let len = fs::metadata(&iso).unwrap().len();
    verify_package(&layout, 3, len, root).unwrap();
    // header magic
    let hp = layout.con_header_file_path();
    let mut h = fs::read(&hp).unwrap();
    h[0] = b'X';
    fs::write(&hp, &h).unwrap();
    assert!(verify_package(&layout, 3, len, root).unwrap_err().contains("LIVE"));
    h[0] = b'L';
    fs::write(&hp, &h).unwrap();
    // hash chain: corrupt part 1's master hash list
    let p1 = layout.part_file_path(1);
    let mut b = fs::read(&p1).unwrap();
    b[0] ^= 0xff;
    fs::write(&p1, &b).unwrap();
    assert!(verify_package(&layout, 3, len, root).unwrap_err().contains("chain"));
    // M1 (round 3): data damage inside Data0001 passes the cheap check but fails deep_verify
    fs::write(&p1, &{ b[0] ^= 0xff; b.clone() }).unwrap();
    verify_package(&layout, 3, len, root).unwrap();
    deep_verify(&layout, 3, len, root).unwrap();
    let mut dmg = b.clone();
    let off = 4096 * 2 + 4096 * 100; // inside sub-part 0's data
    for x in &mut dmg[off..off + 4096] { *x ^= 0x5a; }
    fs::write(&p1, &dmg).unwrap();
    verify_package(&layout, 3, len, root).unwrap();
    assert!(deep_verify(&layout, 3, len, root).unwrap_err().contains("damaged"));
    fs::write(&p1, &b).unwrap();
    // size: truncate part 1
    let f = File::options().write(true).open(&p1).unwrap();
    f.set_len(b.len() as u64 - 4096).unwrap();
    assert!(verify_package(&layout, 3, len, root).unwrap_err().contains("expected"));
    fs::remove_dir_all(&d).unwrap();
}

/// L2: the free-space estimate equals what a conversion actually writes (minus header slack).
#[test]
fn free_space_estimate_matches_output() {
    let d = tmp("fs");
    let iso = d.join("g.iso");
    make_iso(&iso, 0x4D53_07E6, 400 * 1024 * 1024);
    assert!(free_space(&d).unwrap_or(0) > 0);
    let r = convert(&iso, &d.join("o"), None, &Jobs::default(), "f", &mut |_, _| {}).unwrap();
    let mut written = 0;
    let mut st = vec![std::path::PathBuf::from(&r.output)];
    while let Some(p) = st.pop() { for e in fs::read_dir(&p).unwrap().flatten() { if e.path().is_dir() { st.push(e.path()) } else { written += e.metadata().unwrap().len() } } }
    let ro = iso::IsoReader::read(File::open(&iso).unwrap()).unwrap().volume_descriptor.root_offset;
    let est = estimated_output(fs::metadata(&iso).unwrap().len(), ro, r.parts);
    assert!(est >= written && est - written <= 0xb000, "est {est} written {written}");
    fs::remove_dir_all(&d).unwrap();
}

/// Overseer R4 M1: same-size in-place change outside the 1 MiB samples with mtime restored.
/// The sealed full hash (and ctime on Unix) catches it; the ISO is kept.
#[test]
fn r4m1_restored_mtime_swap_is_caught() {
    let d = tmp("r4m1");
    let ext = d.join("x");
    fs::create_dir_all(&ext).unwrap();
    let iso = ext.join("g.iso");
    make_iso(&iso, 0x4D53_07E6, 400 * 1024 * 1024);
    let t = register_extracted(&[iso.display().to_string()], &ext, true).remove(0);
    seal_tokens(&[t.clone()]);
    let mtime = fs::metadata(&iso).unwrap().modified().unwrap();
    // change 12 bytes at 100 MiB during the conversion (after the pre-convert identity check)
    let mut done = false;
    let r = convert_registered(&t, true, &Jobs::default(), "m", &mut |_, f| {
        if !done && f.starts_with("Part 1") {
            done = true;
            let mut h = File::options().write(true).open(&iso).unwrap();
            h.seek(SeekFrom::Start(100 << 20)).unwrap();
            h.write_all(b"XXXXXXXXXXXX").unwrap();
            drop(h);
            File::options().write(true).open(&iso).unwrap().set_modified(mtime).unwrap();
        }
    });
    match r {
        Ok(g) => assert!(!g.iso_deleted && iso.exists(), "{:?}", g.kept_reason),
        Err(e) => assert!(iso.exists(), "{e}"),
    }
    fs::remove_dir_all(&d).unwrap();
}

/// Overseer R4 M2: any byte flip in the header fields fails the delete-time header check.
#[test]
fn r4m2_header_checked_byte_for_byte() {
    let d = tmp("r4m2");
    let iso = d.join("g.iso");
    make_iso(&iso, 0x4D53_07E6, 0);
    let out = d.join("o");
    let r = convert(&iso, &out, None, &Jobs::default(), "h", &mut |_, _| {}).unwrap();
    deep_verify_output(&r, &iso, &mut |_, _| {}).unwrap();
    let hp = fs::read_dir(Path::new(&r.output).join("00007000")).unwrap().flatten().find(|e| e.path().is_file()).unwrap().path();
    let orig = fs::read(&hp).unwrap();
    for off in [0x32cusize, 0x344, 0x354, 0x360, 0x392, 0x3a0, 0x411] {
        let mut h = orig.clone();
        h[off] ^= 0x01;
        fs::write(&hp, &h).unwrap();
        let e = deep_verify_output(&r, &iso, &mut |_, _| {}).unwrap_err();
        assert!(e.contains("header"), "0x{off:x}: {e}");
    }
    fs::write(&hp, &orig).unwrap();
    // progress shows a Verifying phase
    let mut seen = false;
    deep_verify_output(&r, &iso, &mut |_, f| { if f.starts_with("Verifying") { seen = true; } }).unwrap();
    assert!(seen);
    fs::remove_dir_all(&d).unwrap();
}

/// Overseer R4 L1: tokens are random (not sequential) and single-use.
#[test]
fn r4l1_tokens_random_single_use() {
    let d = tmp("tok");
    let p = d.join("a.iso");
    fs::write(&p, b"x").unwrap();
    let a = register_extracted(&[p.display().to_string(), p.display().to_string()], &d, false);
    assert_ne!(a[0], a[1]);
    assert!(a[0].starts_with("iso-") && a[0].len() == 36, "{}", a[0]);
    let _ = convert_registered(&a[0], false, &Jobs::default(), "t", &mut |_, _| {});
    assert!(convert_registered(&a[0], false, &Jobs::default(), "t", &mut |_, _| {}).unwrap_err().contains("already used"));
    fs::remove_dir_all(&d).unwrap();
}
