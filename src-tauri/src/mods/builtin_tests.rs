//! Built-in menus loader: dummy fixtures only (no real menu files or names).
use super::*;
use crate::modnames::Learned;

fn tmp(n: &str) -> crate::testutil::TmpDir {
    crate::testutil::TmpDir::new(&format!("builtin-{n}"))
}

const DUMMY_SHA: &str = "b5a2c96250612366ea272ffac6d9744aaf4b45aacd96aa7cfcb931ee3b558259"; // sha256("dummy")

#[test]
fn shipped_folder_entries_are_all_valid() {
    // Justin fills resources/menus himself: any number of entries is fine, but every one must
    // load without problems (empty is fine too).
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/menus");
    let s = load_builtin(&dir);
    assert!(
        s.problems.is_empty(),
        "invalid built-in menu entries: {:?}",
        s.problems
    );
}

#[test]
fn missing_folder_is_empty_not_error() {
    let s = load_builtin(Path::new("/nonexistent/unpack-menus"));
    assert!(s.menus.is_empty() && s.problems.is_empty());
}

#[test]
fn valid_entries_load_and_bad_ones_are_skipped() {
    let d = tmp("load");
    fs::create_dir_all(d.join("sub")).unwrap();
    fs::write(d.join("sub/dummy-a.bin"), b"dummy").unwrap();
    fs::write(d.join("dummy-b.bin"), b"other").unwrap();
    let outside = tmp("load-out");
    fs::write(outside.join("x.bin"), b"x").unwrap();
    let json = serde_json::json!([
        {"name": "Zorblax Fixture", "file": "sub/dummy-a.bin", "titleId": "4d5307e6", "version": "0.0.1", "sha256": DUMMY_SHA},
        {"name": "Quuxly Fixture", "file": "dummy-b.bin", "titleId": "4D5307E6", "version": "1"},
        {"name": "Bad hash", "file": "dummy-b.bin", "titleId": "4D5307E6", "version": "1", "sha256": DUMMY_SHA},
        {"name": "Short hash", "file": "dummy-b.bin", "titleId": "4D5307E6", "version": "1", "sha256": "abc"},
        {"name": "Escape", "file": "../x.bin", "titleId": "4D5307E6", "version": "1"},
        {"name": "Absolute", "file": outside.join("x.bin").display().to_string(), "titleId": "4D5307E6", "version": "1"},
        {"name": "Drive", "file": "C:x.bin", "titleId": "4D5307E6", "version": "1"},
        {"name": "Missing", "file": "nope.bin", "titleId": "4D5307E6", "version": "1"},
        {"name": "Folder", "file": "sub", "titleId": "4D5307E6", "version": "1"},
        {"name": "Unknown game", "file": "dummy-b.bin", "titleId": "12345678", "version": "1"},
        {"name": "Not hex", "file": "dummy-b.bin", "titleId": "XYZ", "version": "1"},
        {"name": "Extra field", "file": "dummy-b.bin", "titleId": "4D5307E6", "version": "1", "url": "http://x"},
        {"name": "No version", "file": "dummy-b.bin", "titleId": "4D5307E6"},
        {"name": "Dup", "file": "./sub/dummy-a.bin", "titleId": "4D5307E6", "version": "1"},
        "not an object"
    ]);
    fs::write(d.join("menus.json"), json.to_string()).unwrap();
    let s = load_builtin(&d);
    assert_eq!(
        s.menus.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(),
        vec!["Zorblax Fixture", "Quuxly Fixture"],
        "{:?}",
        s.problems
    );
    assert_eq!(s.problems.len(), 13, "{:#?}", s.problems);
    let a = &s.menus[0];
    assert_eq!(
        (a.title_id.as_str(), a.version.as_str(), a.sha256.as_deref()),
        ("4D5307E6", "0.0.1", Some(DUMMY_SHA))
    );
    assert!(a.game.contains("Halo"));
    assert!(
        Path::new(&a.path).starts_with(crate::safecopy::simplify(fs::canonicalize(&d).unwrap()))
    );
    assert!(s.problems.iter().any(|p| p.contains("sha256 mismatch")));
    assert!(s
        .problems
        .iter()
        .any(|p| p.contains("leaves the menus folder")));
    assert!(s.problems.iter().any(|p| p.contains("isn't a known game")));
    assert!(s.learned.iter().all(|l| l.tid == "4D5307E6") && !s.learned.is_empty());
}

#[cfg(unix)]
#[test]
fn links_inside_menus_folder_refused() {
    let d = tmp("link");
    let outside = tmp("link-out");
    fs::write(outside.join("secret.bin"), b"s").unwrap();
    std::os::unix::fs::symlink(outside.join("secret.bin"), d.join("l.bin")).unwrap();
    std::os::unix::fs::symlink(&outside, d.join("ldir")).unwrap();
    fs::write(d.join("menus.json"), r#"[{"name":"Zorblax Fixture","file":"l.bin","titleId":"4D5307E6","version":"1"},{"name":"Quuxly Fixture","file":"ldir/secret.bin","titleId":"4D5307E6","version":"1"}]"#).unwrap();
    let s = load_builtin(&d);
    assert!(s.menus.is_empty());
    assert!(
        s.problems.iter().all(|p| p.contains("link")),
        "{:?}",
        s.problems
    );
}

#[test]
fn bad_json_and_size_cap() {
    let d = tmp("json");
    fs::write(d.join("menus.json"), "{not json").unwrap();
    assert!(load_builtin(&d).problems[0].contains("JSON array"));
    fs::write(d.join("menus.json"), vec![b' '; (1 << 20) + 1]).unwrap();
    assert!(load_builtin(&d).problems[0].contains("1 MB"));
}

#[test]
fn builtin_seeds_never_override_user() {
    let user = vec![Learned {
        key: "zorblax".into(),
        tid: "415608C3".into(),
    }];
    let builtin = vec![
        Learned {
            key: "zorblax".into(),
            tid: "4D5307E6".into(),
        },
        Learned {
            key: "quuxly".into(),
            tid: "4D5307E6".into(),
        },
    ];
    let m = merge_learned(user, &builtin);
    assert_eq!(m.len(), 2);
    assert_eq!(
        m.iter().find(|l| l.key == "zorblax").unwrap().tid,
        "415608C3"
    );
    assert!(m.iter().any(|l| l.key == "quuxly"));
}
