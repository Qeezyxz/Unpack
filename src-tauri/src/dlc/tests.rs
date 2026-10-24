use super::*;

fn tmp(n: &str) -> crate::testutil::TmpDir {
    crate::testutil::TmpDir::new(&format!("dlc-{n}"))
}
/// Synthetic STFS header: magic, content type @0x344, TitleID @0x360, UTF-16BE name @0x411.
pub fn make_stfs(path: &Path, magic: &[u8; 4], ct: u32, tid: u32, name: &str, extra: &[u8]) {
    let mut b = vec![0u8; 0xA000];
    b[0..4].copy_from_slice(magic);
    b[0x344..0x348].copy_from_slice(&ct.to_be_bytes());
    b[0x360..0x364].copy_from_slice(&tid.to_be_bytes());
    for (i, u) in name.encode_utf16().enumerate() {
        b[0x411 + i * 2..0x413 + i * 2].copy_from_slice(&u.to_be_bytes());
    }
    b.extend_from_slice(extra);
    fs::write(path, b).unwrap();
}
fn game(lib: &Path, sub: &str, tid: &str) -> PathBuf {
    let g = lib.join(sub).join(tid);
    fs::create_dir_all(g.join("00007000")).unwrap();
    g
}

#[test]
fn parses_headers_and_kinds() {
    let d = tmp("parse");
    make_stfs(
        &d.join("a"),
        b"LIVE",
        CT_DLC,
        0x4D53_07E6,
        "Mythic Map Pack",
        b"",
    );
    make_stfs(
        &d.join("b"),
        b"CON ",
        CT_TITLE_UPDATE,
        0x4D53_07E6,
        "TU",
        b"",
    );
    make_stfs(&d.join("c"), b"PIRS", 0x7000, 0x1234_ABCD, "", b"");
    fs::write(d.join("x"), b"LIVE but short").unwrap();
    let a = parse_stfs(&d.join("a")).unwrap();
    assert_eq!(
        (
            a.magic.as_str(),
            a.title_id.as_str(),
            a.kind.as_str(),
            a.name.as_str()
        ),
        ("LIVE", "4D5307E6", "dlc", "Mythic Map Pack")
    );
    assert_eq!(parse_stfs(&d.join("b")).unwrap().kind, "title_update");
    assert_eq!(parse_stfs(&d.join("c")).unwrap().kind, "other");
    assert!(parse_stfs(&d.join("x")).is_err());
    assert_eq!(stfs_rel_paths(&d).len(), 4); // magic-only scan; parse decides
    fs::remove_dir_all(&d).unwrap();
}

#[test]
fn installs_into_god_game_and_never_overwrites() {
    let d = tmp("inst");
    let lib = d.join("out");
    let g = game(&lib, "Halo 3 (GOD)", "4D5307E6");
    let src = d.join("dlc1");
    make_stfs(&src, b"LIVE", CT_DLC, 0x4D53_07E6, "Map Pack", b"one");
    let r = install_dlc(&src, std::slice::from_ref(&lib), false, None).unwrap();
    assert_eq!(r.status, "installed");
    assert_eq!(PathBuf::from(&r.dest), g.join("00000002/dlc1"));
    assert!(src.exists(), "copy, not move");
    // identical again -> skipped
    assert_eq!(
        install_dlc(&src, std::slice::from_ref(&lib), false, None)
            .unwrap()
            .status,
        "identical"
    );
    // different content, same name -> new name, original untouched
    let before = fs::read(g.join("00000002/dlc1")).unwrap();
    make_stfs(&src, b"LIVE", CT_DLC, 0x4D53_07E6, "Map Pack", b"two");
    let r = install_dlc(&src, std::slice::from_ref(&lib), false, None).unwrap();
    assert_eq!(PathBuf::from(&r.dest), g.join("00000002/dlc1-2"));
    assert_eq!(fs::read(g.join("00000002/dlc1")).unwrap(), before);
    // move: source removed only after verified copy
    let s2 = d.join("dlc2");
    make_stfs(&s2, b"PIRS", CT_DLC, 0x4D53_07E6, "Other", b"");
    let r = install_dlc(&s2, std::slice::from_ref(&lib), true, None).unwrap();
    assert!(r.moved && !s2.exists() && PathBuf::from(&r.dest).exists());
    fs::remove_dir_all(&d).unwrap();
}

#[test]
fn not_found_title_update_and_layouts() {
    let d = tmp("nf");
    let src = d.join("dlc");
    make_stfs(&src, b"LIVE", CT_DLC, 0x0ABC_DEF1, "X", b"");
    let lib = d.join("lib");
    fs::create_dir_all(&lib).unwrap();
    let r = install_dlc(&src, std::slice::from_ref(&lib), true, None).unwrap();
    assert_eq!(r.status, "not_found");
    assert!(r.message.contains("0ABCDEF1") && src.exists());
    // a bare TitleID folder without a game isn't a match
    fs::create_dir_all(lib.join("0ABCDEF1")).unwrap();
    assert_eq!(find_game(std::slice::from_ref(&lib), "0ABCDEF1"), None);
    // console layout Content/0000000000000000/<TID>/00007000
    let g = game(&lib, "Content/0000000000000000", "0ABCDEF1");
    assert_eq!(find_game(std::slice::from_ref(&lib), "0abcdef1"), Some(g));
    assert_eq!(find_game(std::slice::from_ref(&lib), "../../x"), None);
    let tu = d.join("tu");
    make_stfs(&tu, b"CON ", CT_TITLE_UPDATE, 0x0ABC_DEF1, "TU", b"");
    let r = install_dlc(&tu, std::slice::from_ref(&lib), false, None).unwrap();
    assert_eq!(r.status, "tu_installed");
    assert!(
        tu.exists()
            && lib
                .join("Content/0000000000000000/0ABCDEF1/000B0000/tu")
                .exists()
    );
    // drive scan only looks at fixed top-level paths
    assert_eq!(
        scan_roots(std::slice::from_ref(&lib)),
        vec![lib
            .join("Content")
            .join("0000000000000000")
            .display()
            .to_string()]
    );
    fs::remove_dir_all(&d).unwrap();
}

#[cfg(unix)]
#[test]
fn never_writes_through_linked_dlc_folder() {
    let d = tmp("link");
    let lib = d.join("lib");
    let g = game(&lib, "G (GOD)", "4D5307E6");
    let elsewhere = d.join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, g.join("00000002")).unwrap();
    let src = d.join("dlc");
    make_stfs(&src, b"LIVE", CT_DLC, 0x4D53_07E6, "X", b"");
    assert!(install_dlc(&src, &[lib], false, None).is_err());
    assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 0);
    fs::remove_dir_all(&d).unwrap();
}
