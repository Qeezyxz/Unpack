//! "Mod Menus": a local file manager for mod files the user already has (XEX plugins,
//! STFS/CON packages, folders). Identifies type and target game from headers, and copies to a
//! drive with a free-space check and no overwrite. Nothing is bundled, downloaded or linked,
//! and nothing here talks to any network service.

use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::dlc;

#[derive(Debug, Serialize, Clone, PartialEq)]
pub struct ModInfo {
    pub path: String,
    /// xex | stfs | folder | file
    pub kind: String,
    pub label: String,
    pub name: String,
    pub title_id: Option<String>,
    pub game: Option<String>,
    pub content_type: Option<String>,
    pub size: u64,
    pub files: usize,
    /// When the header has no TitleID: best guesses from file/folder/archive names.
    pub guesses: Vec<crate::gamedb::Guess>,
    pub confident: bool,
}

fn be32(b: &[u8], o: usize) -> Option<u32> {
    b.get(o..o + 4)
        .map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

/// XEX2 header: optional-header table at 0x18 (count at 0x14). Key 0x00040006 = execution info,
/// whose TitleID is at +0x0C. Plugins usually have none -> None.
pub fn xex_title_id(path: &Path) -> Result<Option<u32>, String> {
    let mut f = File::open(path).map_err(|e| e.to_string())?;
    let mut b = vec![0u8; 0x1000];
    let n = f.read(&mut b).map_err(|e| e.to_string())?;
    b.truncate(n);
    if b.get(0..4) != Some(b"XEX2") {
        return Err("not a XEX".into());
    }
    let count = be32(&b, 0x14).unwrap_or(0).min(64) as usize;
    for i in 0..count {
        let (Some(key), Some(val)) = (be32(&b, 0x18 + i * 8), be32(&b, 0x1C + i * 8)) else {
            break;
        };
        if key == 0x0004_0006 {
            // read the execution-info block wherever it is
            let mut e = [0u8; 0x18];
            let mut f = File::open(path).map_err(|e| e.to_string())?;
            use std::io::{Seek, SeekFrom};
            f.seek(SeekFrom::Start(val as u64))
                .map_err(|e| e.to_string())?;
            f.read_exact(&mut e)
                .map_err(|_| "XEX execution info is cut off".to_string())?;
            let tid = u32::from_be_bytes([e[12], e[13], e[14], e[15]]);
            return Ok((tid != 0).then_some(tid));
        }
    }
    Ok(None)
}

fn tree_size(p: &Path) -> (u64, usize) {
    let (mut s, mut n) = (0, 0);
    let mut st = vec![p.to_path_buf()];
    while let Some(d) = st.pop() {
        let Ok(rd) = fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let Ok(m) = fs::symlink_metadata(e.path()) else {
                continue;
            };
            if m.is_dir() {
                st.push(e.path())
            } else if m.is_file() {
                s += m.len();
                n += 1
            }
        }
    }
    (s, n)
}

fn game_name(tid: u32) -> Option<String> {
    crate::gamedb::name(tid)
}

pub fn identify(path: &Path) -> Result<ModInfo, String> {
    identify_with(path, None)
}

/// `hint`: e.g. the archive the file came from. Header TitleID always wins over names.
pub fn identify_with(path: &Path, hint: Option<&str>) -> Result<ModInfo, String> {
    identify_learned(path, hint, &[])
}

/// Like identify_with, but the user's learned mod names are checked before the generic guess.
pub fn identify_learned(
    path: &Path,
    hint: Option<&str>,
    learned: &[crate::modnames::Learned],
) -> Result<ModInfo, String> {
    let mut m = identify_raw(path)?;
    if m.title_id.is_some() {
        m.confident = true;
    } else {
        let file = path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let stem = file
            .rsplit_once('.')
            .map(|x| x.0.to_string())
            .unwrap_or(file);
        let parent = path
            .parent()
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut t = vec![stem.as_str(), parent.as_str()];
        if let Some(h) = hint {
            t.push(h);
        }
        m.guesses = crate::gamedb::guess(&t, 5);
        m.confident = crate::gamedb::confident(&m.guesses);
        if let Some(l) = crate::modnames::lookup(learned, &t) {
            let tid = l.tid.to_ascii_uppercase();
            // The file name itself confidently names a different game: don't auto-confirm, ask.
            let conflict = m.confident && m.guesses.first().is_some_and(|g| g.title_id != tid);
            m.guesses.retain(|g| g.title_id != tid);
            let name = crate::modnames::parse_tid(&tid)
                .and_then(game_name)
                .unwrap_or_default();
            let learned_guess = crate::gamedb::Guess {
                title_id: tid,
                name,
                confidence: if conflict { 80 } else { 99 },
                reason: if conflict {
                    format!(
                        "your known mod name “{}” — but the name points at another game",
                        l.key
                    )
                } else {
                    format!("your known mod name “{}”", l.key)
                },
            };
            if conflict {
                m.guesses.insert(1.min(m.guesses.len()), learned_guess);
            } else {
                m.guesses.insert(0, learned_guess);
            }
            m.guesses.truncate(5);
            m.confident = !conflict;
        }
    }
    Ok(m)
}

fn identify_raw(path: &Path) -> Result<ModInfo, String> {
    let meta =
        fs::symlink_metadata(path).map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
    let fname = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut m = ModInfo {
        path: path.display().to_string(),
        kind: "file".into(),
        label: "File".into(),
        name: fname.clone(),
        title_id: None,
        game: None,
        content_type: None,
        size: meta.len(),
        files: 1,
        guesses: vec![],
        confident: false,
    };
    if meta.file_type().is_symlink() {
        return Err("Links aren't supported; add the real file or folder.".into());
    }
    if meta.is_dir() {
        let (s, n) = tree_size(path);
        m.kind = "folder".into();
        m.label = "Folder".into();
        m.size = s;
        m.files = n;
        // A folder holding exactly one game's packages gets that game.
        let tids: std::collections::BTreeSet<String> = dlc::stfs_rel_paths(path)
            .iter()
            .filter_map(|r| dlc::parse_stfs(&path.join(r)).ok())
            .map(|i| i.title_id)
            .collect();
        if tids.len() == 1 {
            let t = tids.into_iter().next().unwrap();
            m.game = u32::from_str_radix(&t, 16).ok().and_then(game_name);
            m.title_id = Some(t);
        }
        return Ok(m);
    }
    if let Ok(tid) = xex_title_id(path) {
        m.kind = "xex".into();
        m.label = if tid.is_some() {
            "XEX executable".into()
        } else {
            "XEX plugin".into()
        };
        m.title_id = tid.map(|t| format!("{t:08X}"));
        m.game = tid.and_then(game_name);
        return Ok(m);
    }
    if let Ok(i) = dlc::parse_stfs(path) {
        m.kind = "stfs".into();
        m.label = format!("{} package", i.magic);
        if !i.name.is_empty() {
            m.name = format!("{} ({fname})", i.name);
        }
        m.content_type = Some(format!("{:08X}", i.content_type));
        m.game = u32::from_str_radix(&i.title_id, 16)
            .ok()
            .and_then(game_name);
        m.title_id = Some(i.title_id);
        return Ok(m);
    }
    Ok(m)
}

#[derive(Debug, Serialize)]
pub struct ModCopyResult {
    pub dest: String,
    pub copied: usize,
    pub identical: usize,
    pub bytes: u64,
}

/// Where a mod goes on the target drive/folder:
/// - "plugins": `<target>/Plugins/<name>` (a folder keeps its name, a file goes inside Plugins)
/// - "content": STFS only: `<target>/Content/0000000000000000/<TitleID>/<ContentType>/<file>`
/// - "as_is":   `<target>/<name>`
pub fn destination(info: &ModInfo, target: &Path, mode: &str) -> Result<PathBuf, String> {
    let src = Path::new(&info.path);
    let name = src.file_name().ok_or("bad name")?;
    Ok(match mode {
        "plugins" => target.join("Plugins").join(name),
        "content" => {
            let (Some(t), Some(ct)) = (&info.title_id, &info.content_type) else {
                return Err(
                    "Only Xbox packages (CON/LIVE/PIRS) know their game's Content folder.".into(),
                );
            };
            if t == "00000000" || t.eq_ignore_ascii_case("FFFFFFFF") {
                return Err(format!(
                    "TitleID {t} is not a real game; refusing Content placement."
                ));
            }
            target
                .join("Content")
                .join("0000000000000000")
                .join(t)
                .join(ct)
                .join(name)
        }
        "as_is" => target.join(name),
        _ => return Err("Unknown destination".into()),
    })
}

/// Copy a mod (file or folder). Plans first: any existing file with different content stops the
/// copy before anything is written; identical files are skipped. Free-space checked.
pub fn copy_mod(
    src: &Path,
    target: &Path,
    mode: &str,
    progress: &mut dyn FnMut(u32, &str),
    cancelled: &dyn Fn() -> bool,
) -> Result<ModCopyResult, String> {
    let info = identify_raw(src)?;
    let dest = destination(&info, target, mode)?;
    if let (Ok(a), Ok(b)) = (fs::canonicalize(src), fs::canonicalize(&dest)) {
        if a == b {
            return Err("That's the same place the mod already is.".into());
        }
    }
    if let (Ok(a), Ok(b)) = (fs::canonicalize(src), fs::canonicalize(target)) {
        if b.starts_with(&a) {
            return Err("The target is inside the mod folder.".into());
        }
    }
    let mut files = vec![];
    if info.kind == "folder" {
        let mut st = vec![PathBuf::new()];
        while let Some(rel) = st.pop() {
            for e in fs::read_dir(src.join(&rel))
                .map_err(|e| e.to_string())?
                .flatten()
            {
                let m = fs::symlink_metadata(e.path()).map_err(|e| e.to_string())?;
                let r = rel.join(e.file_name());
                if m.is_dir() {
                    st.push(r)
                } else if m.is_file() {
                    files.push((src.join(&r), dest.join(&r), m.len()))
                }
            }
        }
    } else {
        files.push((src.to_path_buf(), dest.clone(), info.size));
    }
    let (mut todo, mut identical, mut need) = (vec![], 0usize, 0u64);
    for (s, d, len) in files {
        match fs::symlink_metadata(&d) {
            Ok(m) if m.is_file() && m.len() == len && same(&s, &d) => identical += 1,
            Ok(_) => {
                return Err(format!(
                    "{} already exists with different content. Nothing was copied.",
                    d.display()
                ))
            }
            Err(_) => {
                need += len;
                todo.push((s, d, len));
            }
        }
    }
    let croot =
        fs::canonicalize(target).map_err(|e| format!("Cannot open {}: {e}", target.display()))?;
    crate::safecopy::fat_check(&croot, todo.iter().map(|(s, _, l)| (s.clone(), *l)))?;
    if let Some(free) = crate::god::free_space(&croot) {
        if free < need + 8 * 1024 * 1024 {
            return Err(format!(
                "Not enough free space: needs {} MB, {} MB free.",
                need / 1_048_576,
                free / 1_048_576
            ));
        }
    }
    let (total, mut done, mut copied) = (need.max(1), 0u64, 0);
    for (s, d, _len) in todo {
        if cancelled() {
            return Err(crate::archive::ERR_CANCELLED.into());
        }
        let rel = d
            .strip_prefix(target)
            .map_err(|_| "Bad destination".to_string())?
            .to_path_buf();
        let label = d
            .file_name()
            .map(|x| x.to_string_lossy().into_owned())
            .unwrap_or_default();
        let wrote = crate::safecopy::copy_file_safe(&s, &croot, &rel, &mut |k| {
            done += k;
            progress(((done * 100) / total).min(100) as u32, &label);
        })?;
        if wrote {
            copied += 1
        } else {
            identical += 1
        }
    }
    Ok(ModCopyResult {
        dest: dest.display().to_string(),
        copied,
        identical,
        bytes: need,
    })
}

fn same(a: &Path, b: &Path) -> bool {
    let (Ok(mut fa), Ok(mut fb)) = (File::open(a), File::open(b)) else {
        return false;
    };
    let (mut x, mut y) = (vec![0u8; 1 << 16], vec![0u8; 1 << 16]);
    loop {
        let (Ok(n), Ok(m)) = (fa.read(&mut x), fb.read(&mut y)) else {
            return false;
        };
        if n != m || x[..n] != y[..m] {
            return false;
        }
        if n == 0 {
            return true;
        }
    }
}

/// R22 M2: a mod path the page asks to identify must be a folder/file picked in Unpack (saved
/// mod folders are persisted grants, built-in menus session grants) and never a network share
/// or device path.
pub fn check_mod_path(grants: &crate::safecopy::Grants, p: &str) -> Result<PathBuf, String> {
    let t = p.trim_start();
    let lower = t.to_ascii_lowercase().replace('/', "\\");
    if lower.starts_with("\\\\.\\") || (lower.starts_with("\\\\?\\") && !lower.starts_with("\\\\?\\unc\\")
        && lower.as_bytes().get(5).is_none_or(|c| *c != b':'))
    {
        return Err(format!("{} is a device path; pick a normal folder.", crate::safecopy::plain(p)));
    }
    if lower.starts_with("\\\\") {
        return Err(format!(
            "{} is on a network share; Unpack only reads mods from this PC's drives.",
            crate::safecopy::plain(p)
        ));
    }
    let c = grants.check(p)?;
    if c.display().to_string().starts_with("\\\\") {
        return Err(format!(
            "{} is on a network share; Unpack only reads mods from this PC's drives.",
            crate::safecopy::plain(p)
        ));
    }
    Ok(c)
}

#[cfg(test)]
mod tests {
    /// R22 M2: mod_identify only reads folders picked in Unpack (or saved mod folders, which
    /// are persisted grants) and never a network share.
    #[test]
    fn r22_m2_identify_needs_a_grant_and_no_unc() {
        let td = crate::testutil::TmpDir::new("mods-r22");
        let picked = td.path().join("picked");
        let other = td.path().join("other");
        std::fs::create_dir_all(&picked).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        let g = crate::safecopy::Grants::default();
        g.grant_session(&picked);
        assert!(check_mod_path(&g, &picked.display().to_string()).is_ok());
        assert!(check_mod_path(&g, &other.display().to_string()).is_err());
        for unc in ["\\\\server\\share\\mod", "//server/share/mod", "\\\\?\\UNC\\server\\share", "\\\\.\\C:\\x"] {
            let e = check_mod_path(&g, unc).unwrap_err();
            assert!(e.contains("network") || e.contains("device"), "{unc}: {e}");
        }
    }

    use super::*;
    use crate::dlc::tests::make_stfs;
    fn tmp(n: &str) -> crate::testutil::TmpDir {
        crate::testutil::TmpDir::new(&format!("mods-{n}"))
    }
    fn make_xex(p: &Path, tid: Option<u32>) {
        let mut b = vec![0u8; 0x400];
        b[0..4].copy_from_slice(b"XEX2");
        let entries: Vec<(u32, u32)> = match tid {
            Some(_) => vec![(0x0001_00FF, 0), (0x0004_0006, 0x200)],
            None => vec![(0x0001_00FF, 0)],
        };
        b[0x14..0x18].copy_from_slice(&(entries.len() as u32).to_be_bytes());
        for (i, (k, v)) in entries.iter().enumerate() {
            b[0x18 + i * 8..0x1C + i * 8].copy_from_slice(&k.to_be_bytes());
            b[0x1C + i * 8..0x20 + i * 8].copy_from_slice(&v.to_be_bytes());
        }
        if let Some(t) = tid {
            b[0x20C..0x210].copy_from_slice(&t.to_be_bytes());
        }
        fs::write(p, b).unwrap();
    }
    #[test]
    fn identifies_xex_stfs_folder_and_other() {
        let d = tmp("id");
        make_xex(&d.join("menu.xex"), None);
        make_xex(&d.join("default.xex"), Some(0x4D53_07E6));
        make_stfs(&d.join("pkg"), b"CON ", 0x2, 0x4D53_07E6, "Map Pack", b"");
        fs::write(d.join("readme.txt"), b"hi").unwrap();
        fs::write(d.join("cut.xex"), b"XEX2").unwrap();
        let a = identify(&d.join("menu.xex")).unwrap();
        assert_eq!(
            (a.kind.as_str(), a.label.as_str(), a.title_id.clone()),
            ("xex", "XEX plugin", None)
        );
        let b = identify(&d.join("default.xex")).unwrap();
        assert_eq!(b.title_id.as_deref(), Some("4D5307E6"));
        assert_eq!(b.game.as_deref(), Some("Halo 3"));
        let c = identify(&d.join("pkg")).unwrap();
        assert_eq!(
            (
                c.kind.as_str(),
                c.content_type.as_deref(),
                c.title_id.as_deref()
            ),
            ("stfs", Some("00000002"), Some("4D5307E6"))
        );
        assert!(c.name.contains("Map Pack"));
        assert_eq!(identify(&d.join("readme.txt")).unwrap().kind, "file");
        assert_eq!(identify(&d.join("cut.xex")).unwrap().kind, "xex"); // header too short for exec info: plugin
        let f = identify(&d).unwrap();
        assert_eq!(
            (f.kind.as_str(), f.files, f.title_id.as_deref()),
            ("folder", 5, Some("4D5307E6"))
        );
        // no header TitleID: guessed from names; header always wins
        fs::create_dir_all(d.join("GTA V Stuff")).unwrap();
        make_xex(&d.join("GTA V Stuff/trainer.xex"), None);
        let g = identify_with(&d.join("GTA V Stuff/trainer.xex"), None).unwrap();
        assert!(g.confident && g.guesses[0].title_id == "545408A7");
        let g = identify_with(&d.join("menu.xex"), Some("BO2 Menus Pack.rar")).unwrap();
        assert!(g.confident && g.guesses[0].title_id == "415608C3");
        assert!(!identify_with(&d.join("menu.xex"), None).unwrap().confident);
        let h = identify_with(&d.join("default.xex"), Some("MW2.zip")).unwrap();
        assert!(h.confident && h.guesses.is_empty() && h.title_id.as_deref() == Some("4D5307E6"));
        let learned = vec![crate::modnames::Learned {
            key: "jiggy".into(),
            tid: "415608C3".into(),
        }];
        make_xex(&d.join("Jiggy Menu v5.0.xex"), None);
        let j = identify_learned(
            &d.join("Jiggy Menu v5.0.xex"),
            Some("downloads.zip"),
            &learned,
        )
        .unwrap();
        assert!(
            j.confident && j.guesses[0].title_id == "415608C3" && j.guesses[0].confidence == 99,
            "learned beats weak guess"
        );
        // R8: Overseer's three cases with Halo 3 keys; none may auto-file to Halo 3.
        let halo = vec![
            crate::modnames::Learned {
                key: "black ops".into(),
                tid: "4D5307E6".into(),
            },
            crate::modnames::Learned {
                key: "jiggy".into(),
                tid: "4D5307E6".into(),
            },
            crate::modnames::Learned {
                key: "aimbot".into(),
                tid: "4D5307E6".into(),
            },
        ];
        for (f, want) in [
            ("Black Ops II Menu v3.xex", "415608C3"),
            ("BO2 Jiggy Menu v5.xex", "415608C3"),
            ("MW2 Aimbot.xex", "41560817"),
        ] {
            make_xex(&d.join(f), None);
            let r = identify_learned(&d.join(f), None, &halo).unwrap();
            let top_halo_confident = r.confident
                && r.guesses
                    .first()
                    .is_some_and(|g| g.title_id == "4D5307E6");
            assert!(
                !top_halo_confident,
                "{f}: must not auto-confirm Halo 3: {:?}",
                r.guesses
            );
            assert_eq!(r.guesses[0].title_id, want, "{f}");
            if f.contains("Jiggy") {
                assert!(!r.confident, "conflict → Which game?");
                assert!(
                    r.guesses.iter().any(|g| g.title_id == "4D5307E6"),
                    "learned offered as a choice"
                );
            }
        }
        let hj = identify_learned(&d.join("default.xex"), Some("Jiggy.zip"), &learned).unwrap();
        assert_eq!(hj.title_id.as_deref(), Some("4D5307E6"), "header wins");
        assert!(hj.guesses.is_empty());
        make_stfs(&d.join("nullpkg"), b"CON ", 0x2, 0, "Bad", b"");
        let n = identify(&d.join("nullpkg")).unwrap();
        assert_eq!(n.title_id.as_deref(), Some("00000000"));
        assert!(destination(&n, &d.join("usb"), "content")
            .unwrap_err()
            .contains("not a real game"));
        fs::remove_dir_all(&d).unwrap();
    }
    #[test]
    fn copies_to_plugins_and_content_without_overwrite() {
        let d = tmp("cp");
        let usb = d.join("usb");
        fs::create_dir_all(&usb).unwrap();
        make_xex(&d.join("menu.xex"), None);
        let r = copy_mod(
            &d.join("menu.xex"),
            &usb,
            "plugins",
            &mut |_, _| {},
            &|| false,
        )
        .unwrap();
        assert_eq!(PathBuf::from(&r.dest), usb.join("Plugins/menu.xex"));
        assert_eq!(
            copy_mod(
                &d.join("menu.xex"),
                &usb,
                "plugins",
                &mut |_, _| {},
                &|| false
            )
            .unwrap()
            .identical,
            1
        );
        fs::write(usb.join("Plugins/menu.xex"), b"OTHER").unwrap();
        assert!(copy_mod(
            &d.join("menu.xex"),
            &usb,
            "plugins",
            &mut |_, _| {},
            &|| false
        )
        .unwrap_err()
        .contains("different content"));
        assert_eq!(fs::read(usb.join("Plugins/menu.xex")).unwrap(), b"OTHER");
        make_stfs(&d.join("pkg"), b"LIVE", 0x2, 0x4D53_07E6, "X", b"");
        let r = copy_mod(&d.join("pkg"), &usb, "content", &mut |_, _| {}, &|| false).unwrap();
        assert!(Path::new(&r.dest).ends_with("Content/0000000000000000/4D5307E6/00000002/pkg"));
        assert!(
            copy_mod(
                &d.join("menu.xex"),
                &usb,
                "content",
                &mut |_, _| {},
                &|| false
            )
            .is_err(),
            "xex has no content folder"
        );
        // folder: conflict anywhere stops before writing anything
        let m = d.join("MyMod");
        fs::create_dir_all(m.join("sub")).unwrap();
        fs::write(m.join("a.ini"), b"a").unwrap();
        fs::write(m.join("sub/b.xex"), b"b").unwrap();
        fs::create_dir_all(usb.join("MyMod/sub")).unwrap();
        fs::write(usb.join("MyMod/sub/b.xex"), b"DIFFERENT").unwrap();
        assert!(copy_mod(&m, &usb, "as_is", &mut |_, _| {}, &|| false).is_err());
        assert!(!usb.join("MyMod/a.ini").exists());
        fs::remove_file(usb.join("MyMod/sub/b.xex")).unwrap();
        let r = copy_mod(&m, &usb, "as_is", &mut |_, _| {}, &|| false).unwrap();
        assert_eq!(r.copied, 2);
        assert!(
            copy_mod(&m, &m.join("sub"), "as_is", &mut |_, _| {}, &|| false).is_err(),
            "target inside source"
        );
        fs::remove_dir_all(&d).unwrap();
    }
}

// ---- Built-in menus: resources/menus/menus.json (ships EMPTY; see HANDOFF "Adding built-in menus") ----

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BuiltinEntry {
    name: String,
    file: String,
    title_id: String,
    version: String,
    #[serde(default)]
    sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct BuiltinMenu {
    pub name: String,
    /// Absolute, canonical path of the file inside the menus folder.
    pub path: String,
    pub title_id: String,
    pub game: String,
    pub version: String,
    pub sha256: Option<String>,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct BuiltinSet {
    pub menus: Vec<BuiltinMenu>,
    /// Skipped entries and why (shown in the log / tests).
    pub problems: Vec<String>,
    /// Learned-name keys derived from built-in names (seed for auto-detect).
    #[serde(skip)]
    pub learned: Vec<crate::modnames::Learned>,
}

pub const BUILTIN_MAX: usize = 500;
const BUILTIN_JSON_MAX: u64 = 1 << 20;

fn sha256_file(p: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    let mut f = File::open(p).map_err(|e| e.to_string())?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

fn check_entry(root: &Path, croot: &Path, e: &BuiltinEntry) -> Result<BuiltinMenu, String> {
    let name: String = e
        .name
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(120)
        .collect();
    if name.is_empty() {
        return Err("empty name".into());
    }
    let version: String = e
        .version
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(40)
        .collect();
    // Confined path: relative, only normal components, every component a real (non-link) entry.
    let rel = Path::new(&e.file);
    if e.file.is_empty()
        || e.file.len() > 260
        || rel.is_absolute()
        || e.file.contains(':')
        || e.file.starts_with(['\\', '/'])
    {
        return Err(format!(
            "file \"{}\" must be a relative path inside the menus folder",
            e.file
        ));
    }
    let mut cur = root.to_path_buf();
    for c in Path::new(&e.file.replace('\\', "/")).components() {
        let std::path::Component::Normal(n) = c else {
            return Err(format!("file \"{}\" leaves the menus folder", e.file));
        };
        cur.push(n);
        let m = fs::symlink_metadata(&cur).map_err(|_| format!("file \"{}\" not found", e.file))?;
        if crate::safecopy::is_link(&m) {
            return Err(format!("file \"{}\" goes through a link", e.file));
        }
    }
    let m = fs::symlink_metadata(&cur).map_err(|e| e.to_string())?;
    if !m.is_file() {
        return Err(format!("file \"{}\" isn't a regular file", e.file));
    }
    let canon = crate::safecopy::simplify(fs::canonicalize(&cur).map_err(|e| e.to_string())?);
    if !canon.starts_with(croot) {
        return Err(format!("file \"{}\" leaves the menus folder", e.file));
    }
    let t = e.title_id.trim().to_ascii_uppercase();
    if t.len() != 8 || !t.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!("titleId \"{}\" isn't 8 hex digits", e.title_id));
    }
    let game = crate::gamedb::name(u32::from_str_radix(&t, 16).unwrap())
        .ok_or_else(|| format!("titleId {t} isn't a known game"))?;
    let sha = match &e.sha256 {
        None => None,
        Some(s) => {
            let s = s.trim().to_ascii_lowercase();
            if s.len() != 64 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err("sha256 must be 64 hex digits".into());
            }
            let got = sha256_file(&canon)?;
            if got != s {
                return Err(format!("sha256 mismatch for \"{}\"", e.file));
            }
            Some(s)
        }
    };
    Ok(BuiltinMenu {
        name,
        path: canon.display().to_string(),
        title_id: t,
        game,
        version,
        sha256: sha,
    })
}

/// Read and validate `<dir>/menus.json`. A missing folder/file is an empty set, not an error.
pub fn load_builtin(dir: &Path) -> BuiltinSet {
    let mut set = BuiltinSet::default();
    let json = dir.join("menus.json");
    let Ok(m) = fs::symlink_metadata(&json) else {
        return set;
    };
    if crate::safecopy::is_link(&m) || !m.is_file() {
        set.problems.push("menus.json is not a regular file".into());
        return set;
    }
    if m.len() > BUILTIN_JSON_MAX {
        set.problems.push("menus.json is larger than 1 MB".into());
        return set;
    }
    let Ok(croot) = fs::canonicalize(dir).map(crate::safecopy::simplify) else {
        return set;
    };
    let text = match fs::read_to_string(&json) {
        Ok(t) => t,
        Err(e) => {
            set.problems.push(format!("menus.json: {e}"));
            return set;
        }
    };
    let raw: Vec<serde_json::Value> =
        match serde_json::from_str(text.trim_start_matches('\u{feff}')) {
            Ok(v) => v,
            Err(e) => {
                set.problems
                    .push(format!("menus.json isn't a JSON array: {e}"));
                return set;
            }
        };
    let mut seen = std::collections::HashSet::new();
    for (i, v) in raw.into_iter().enumerate() {
        if set.menus.len() >= BUILTIN_MAX {
            set.problems.push(format!(
                "more than {BUILTIN_MAX} entries; the rest are ignored"
            ));
            break;
        }
        let e: BuiltinEntry = match serde_json::from_value(v) {
            Ok(e) => e,
            Err(err) => {
                set.problems.push(format!("entry {i}: {err}"));
                continue;
            }
        };
        match check_entry(dir, &croot, &e) {
            Ok(b) if !seen.insert(b.path.to_lowercase()) => set
                .problems
                .push(format!("entry {i}: duplicate file {}", e.file)),
            Ok(b) => {
                if let Ok((key, tid, _)) = crate::modnames::learn_key(&b.name, &b.title_id) {
                    if !set.learned.iter().any(|l| l.key == key) {
                        set.learned.push(crate::modnames::Learned { key, tid });
                    }
                }
                set.menus.push(b);
            }
            Err(err) => set.problems.push(format!("entry {i}: {err}")),
        }
    }
    set
}

/// User-learned names first; built-in keys only fill in keys the user doesn't have.
pub fn merge_learned(
    user: Vec<crate::modnames::Learned>,
    builtin: &[crate::modnames::Learned],
) -> Vec<crate::modnames::Learned> {
    let have: std::collections::HashSet<String> = user.iter().map(|l| l.key.clone()).collect();
    let mut out = user;
    out.extend(builtin.iter().filter(|l| !have.contains(&l.key)).cloned());
    out
}

#[cfg(test)]
mod builtin_tests;
