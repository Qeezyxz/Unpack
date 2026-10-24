//! Library view + "Copy to USB". Offline only: game names come from the GOD/STFS headers
//! already on disk (and iso2god's built-in title list), never from the network.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::dlc;

#[derive(Debug, Serialize, Clone, PartialEq, serde::Deserialize)]
pub struct Game {
    pub title_id: String,
    pub name: String,
    pub path: String,
    /// "god" (TitleID/00007000) or "extracted" (folder with default.xex).
    pub kind: String,
    pub game_size: u64,
    pub dlc_count: usize,
    pub dlc_size: u64,
    pub tu_count: usize,
}

fn dir_size(p: &Path) -> (u64, usize) {
    let (mut size, mut files) = (0, 0);
    let mut st = vec![p.to_path_buf()];
    while let Some(d) = st.pop() {
        let Ok(rd) = fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let Ok(m) = fs::symlink_metadata(e.path()) else {
                continue;
            };
            if m.is_dir() {
                st.push(e.path());
            } else if m.is_file() {
                size += m.len();
                files += 1;
            }
        }
    }
    (size, files)
}

fn count_files(p: &Path) -> (u64, usize) {
    let Ok(rd) = fs::read_dir(p) else {
        return (0, 0);
    };
    rd.flatten()
        .filter_map(|e| fs::symlink_metadata(e.path()).ok())
        .filter(|m| m.is_file())
        .fold((0, 0), |(s, n), m| (s + m.len(), n + 1))
}

fn is_tid(name: &str) -> bool {
    name.len() == 8 && name.chars().all(|c| c.is_ascii_hexdigit())
}

/// Name from the first STFS header file in the game's content folder, else the built-in list.
fn game_name(tid_dir: &Path, tid: &str) -> String {
    for ct in ["00007000", "00004000"] {
        if let Ok(rd) = fs::read_dir(tid_dir.join(ct)) {
            for e in rd.flatten() {
                if e.file_type().map(|t| t.is_file()).unwrap_or(false) {
                    if let Ok(i) = dlc::parse_stfs(&e.path()) {
                        if !i.name.is_empty() {
                            return i.name;
                        }
                    }
                }
            }
        }
    }
    u32::from_str_radix(tid, 16)
        .ok()
        .and_then(iso2god::game_list::find_title_by_id)
        .unwrap_or_else(|| format!("Unknown game {tid}"))
}

/// Games in the library folders (same places `dlc::find_game` looks), de-duplicated by path.
pub fn scan(libraries: &[PathBuf]) -> Vec<Game> {
    let mut out: Vec<Game> = vec![];
    let mut seen = std::collections::HashSet::new();
    for lib in libraries {
        let mut parents = vec![
            lib.clone(),
            lib.join("Games"),
            lib.join("0000000000000000"),
            lib.join("Content").join("0000000000000000"),
        ];
        if let Ok(rd) = fs::read_dir(lib) {
            parents.extend(
                rd.flatten()
                    .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                    .map(|e| e.path()),
            );
        }
        for parent in parents {
            let Ok(rd) = fs::read_dir(&parent) else {
                continue;
            };
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                let p = e.path();
                if !e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    continue;
                }
                if is_tid(&name) {
                    let has_game = ["00007000", "00004000"].iter().any(|c| p.join(c).is_dir());
                    if has_game && seen.insert(fs::canonicalize(&p).unwrap_or(p.clone())) {
                        let tid = name.to_ascii_uppercase();
                        let game_size =
                            dir_size(&p.join("00007000")).0 + dir_size(&p.join("00004000")).0;
                        let (dlc_size, dlc_count) = count_files(&p.join("00000002"));
                        let (_, tu_count) = count_files(&p.join("000B0000"));
                        out.push(Game {
                            name: game_name(&p, &tid),
                            title_id: tid,
                            path: p.display().to_string(),
                            kind: "god".into(),
                            game_size,
                            dlc_count,
                            dlc_size,
                            tu_count,
                        });
                    }
                    continue;
                }
                // Extracted XEX folders: default.xex / default.xbe at the top of the folder.
                if let Some((tid, gname)) = crate::iso_extract::folder_title_id(&p) {
                    if seen.insert(fs::canonicalize(&p).unwrap_or(p.clone())) {
                        let (game_size, _) = dir_size(&p);
                        let disp = gname.unwrap_or(name);
                        out.push(Game {
                            name: disp,
                            title_id: tid,
                            path: p.display().to_string(),
                            kind: "extracted".into(),
                            game_size,
                            dlc_count: 0,
                            dlc_size: 0,
                            tu_count: 0,
                        });
                    }
                }
            }
        }
    }
    out.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then(a.path.cmp(&b.path))
    });
    out
}

#[derive(Debug, Serialize, Clone, serde::Deserialize)]
pub struct LibraryIndex {
    pub scanned_at: u64,
    pub stale: bool,
    pub games: Vec<Game>,
}

/// Scan libraries, optionally reuse a recent on-disk cache, and warm cover art cache.
pub fn index_cached(
    libraries: &[PathBuf],
    cache_dir: &Path,
    allow_cover_network: bool,
    max_age_secs: u64,
) -> Result<LibraryIndex, String> {
    let cache_file = cache_dir.join("index.json");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    if max_age_secs > 0 && cache_file.is_file() {
        if let Ok(bytes) = fs::read(&cache_file) {
            if let Ok(idx) = serde_json::from_slice::<LibraryIndex>(&bytes) {
                if now.saturating_sub(idx.scanned_at) <= max_age_secs {
                    return Ok(LibraryIndex {
                        stale: false,
                        ..idx
                    });
                }
                let games = scan(libraries);
                return Ok(LibraryIndex {
                    scanned_at: idx.scanned_at,
                    stale: true,
                    games,
                });
            }
        }
    }
    let games = scan(libraries);
    let covers = cache_dir.join("covers");
    for g in &games {
        if crate::covers::cache_local(Path::new(&g.path), &g.title_id, &covers).is_none() {
            crate::covers::fetch_opt_in(&g.title_id, &covers, allow_cover_network);
        }
    }
    let idx = LibraryIndex {
        scanned_at: now,
        stale: false,
        games,
    };
    fs::create_dir_all(cache_dir).map_err(|e| e.to_string())?;
    let _ = fs::write(&cache_file, serde_json::to_vec(&idx).unwrap_or_default());
    Ok(idx)
}

#[derive(Debug, Serialize)]
pub struct CopyResult {
    pub dest: String,
    pub copied: usize,
    pub identical: usize,
    pub bytes: u64,
}

/// Copy a game's whole `<TitleID>` folder (GOD + DLC + TU) to `<target>/Content/0000000000000000/<TitleID>`.
/// Free-space checked first. Never overwrites: identical files are skipped; a different file
/// already at the same place stops the copy before anything is written.
pub fn copy_to_usb(
    game_dir: &Path,
    target_root: &Path,
    progress: &mut dyn FnMut(u32, &str),
    cancelled: &dyn Fn() -> bool,
) -> Result<CopyResult, String> {
    if crate::iso_extract::folder_title_id(game_dir).is_some() {
        return copy_extracted_to_usb(game_dir, target_root, progress, cancelled);
    }
    let tid = game_dir
        .file_name()
        .map(|s| s.to_string_lossy().to_ascii_uppercase())
        .unwrap_or_default();
    if !is_tid(&tid)
        || !["00007000", "00004000"]
            .iter()
            .any(|c| game_dir.join(c).is_dir())
    {
        return Err("That folder isn't a game (TitleID folder with 00007000, or extracted folder with default.xex).".into());
    }
    let dest = target_root
        .join("Content")
        .join("0000000000000000")
        .join(&tid);
    if let (Ok(a), Ok(b)) = (fs::canonicalize(game_dir), fs::canonicalize(&dest)) {
        if a == b {
            return Err("That's the same folder the game is already in.".into());
        }
    }
    // Plan: every file, relative path, and whether it already exists identically.
    let mut files = vec![];
    let mut st = vec![PathBuf::new()];
    while let Some(rel) = st.pop() {
        let rd = fs::read_dir(game_dir.join(&rel)).map_err(|e| e.to_string())?;
        for e in rd.flatten() {
            let m = fs::symlink_metadata(e.path()).map_err(|e| e.to_string())?;
            let r = rel.join(e.file_name());
            if m.is_dir() {
                st.push(r);
            } else if m.is_file() {
                files.push((r, m.len()));
            }
        }
    }
    let mut need = 0u64;
    let mut todo = vec![];
    let mut identical = 0;
    for (r, len) in &files {
        let d = dest.join(r);
        match fs::symlink_metadata(&d) {
            Ok(m) if m.is_file() && m.len() == *len && same_content(&game_dir.join(r), &d) => {
                identical += 1
            }
            Ok(_) => {
                return Err(format!(
                    "{} already exists on the target with different content. Nothing was copied.",
                    d.display()
                ))
            }
            Err(_) => {
                need += len;
                todo.push((r.clone(), *len));
            }
        }
    }
    crate::safecopy::ensure_dir_under(target_root, Path::new("Content"))?;
    crate::safecopy::fat_check(target_root, todo.iter().map(|(r, l)| (r.clone(), *l)))?;
    if let Some(free) = crate::god::free_space(target_root) {
        if free < need + 16 * 1024 * 1024 {
            return Err(format!(
                "Not enough free space on the target: needs {} MB, {} MB free.",
                need / 1_048_576,
                free / 1_048_576
            ));
        }
    }
    let total = need.max(1);
    let mut done = 0u64;
    let mut copied = 0;
    let base = Path::new("Content").join("0000000000000000").join(&tid);
    for (r, _len) in todo {
        if cancelled() {
            return Err(crate::archive::ERR_CANCELLED.into());
        }
        let label = r.display().to_string();
        let wrote = crate::safecopy::copy_file_safe(
            &game_dir.join(&r),
            target_root,
            &base.join(&r),
            &mut |k| {
                done += k;
                progress(((done * 100) / total).min(100) as u32, &label);
            },
        )
        .map_err(|e| format!("Copy failed for {}: {e}", r.display()))?;
        if wrote {
            copied += 1
        } else {
            identical += 1
        }
    }
    Ok(CopyResult {
        dest: dest.display().to_string(),
        copied,
        identical,
        bytes: need,
    })
}

/// Extracted game → `<USB>/Games/<folder name>` (Aurora-style). Same no-overwrite rules.
fn copy_extracted_to_usb(
    game_dir: &Path,
    target_root: &Path,
    progress: &mut dyn FnMut(u32, &str),
    cancelled: &dyn Fn() -> bool,
) -> Result<CopyResult, String> {
    let name = game_dir
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Game".into());
    let seg = crate::archive::safe_segment(&name);
    let dest = target_root.join("Games").join(&seg);
    if let (Ok(a), Ok(b)) = (fs::canonicalize(game_dir), fs::canonicalize(&dest)) {
        if a == b {
            return Err("That's the same folder the game is already in.".into());
        }
    }
    let mut files = vec![];
    let mut st = vec![PathBuf::new()];
    while let Some(rel) = st.pop() {
        let rd = fs::read_dir(game_dir.join(&rel)).map_err(|e| e.to_string())?;
        for e in rd.flatten() {
            let m = fs::symlink_metadata(e.path()).map_err(|e| e.to_string())?;
            let r = rel.join(e.file_name());
            if m.is_dir() {
                st.push(r);
            } else if m.is_file() {
                files.push((r, m.len()));
            }
        }
    }
    let mut need = 0u64;
    let mut todo = vec![];
    let mut identical = 0;
    for (r, len) in &files {
        let d = dest.join(r);
        match fs::symlink_metadata(&d) {
            Ok(m) if m.is_file() && m.len() == *len && same_content(&game_dir.join(r), &d) => {
                identical += 1
            }
            Ok(_) => {
                return Err(format!(
                    "{} already exists on the target with different content. Nothing was copied.",
                    d.display()
                ))
            }
            Err(_) => {
                need += len;
                todo.push((r.clone(), *len));
            }
        }
    }
    crate::safecopy::ensure_dir_under(target_root, Path::new("Games"))?;
    crate::safecopy::fat_check(target_root, todo.iter().map(|(r, l)| (r.clone(), *l)))?;
    if let Some(free) = crate::god::free_space(target_root) {
        if free < need + 16 * 1024 * 1024 {
            return Err(format!(
                "Not enough free space on the target: needs {} MB, {} MB free.",
                need / 1_048_576,
                free / 1_048_576
            ));
        }
    }
    let total = need.max(1);
    let mut done = 0u64;
    let mut copied = 0;
    let base = Path::new("Games").join(&seg);
    for (r, _len) in todo {
        if cancelled() {
            return Err(crate::archive::ERR_CANCELLED.into());
        }
        let label = r.display().to_string();
        let wrote = crate::safecopy::copy_file_safe(
            &game_dir.join(&r),
            target_root,
            &base.join(&r),
            &mut |k| {
                done += k;
                progress(((done * 100) / total).min(100) as u32, &label);
            },
        )
        .map_err(|e| format!("Copy failed for {}: {e}", r.display()))?;
        if wrote {
            copied += 1
        } else {
            identical += 1
        }
    }
    Ok(CopyResult {
        dest: dest.display().to_string(),
        copied,
        identical,
        bytes: need,
    })
}

fn same_content(a: &Path, b: &Path) -> bool {
    use std::io::Read;
    let (Ok(mut fa), Ok(mut fb)) = (fs::File::open(a), fs::File::open(b)) else {
        return false;
    };
    let (mut ba, mut bb) = (vec![0u8; 1 << 16], vec![0u8; 1 << 16]);
    loop {
        let na = fa.read(&mut ba).unwrap_or(usize::MAX);
        let nb = fb.read(&mut bb).unwrap_or(usize::MAX - 1);
        if na != nb {
            return false;
        }
        if na == 0 {
            return true;
        }
        if ba[..na] != bb[..nb] {
            return false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dlc::tests::make_stfs;
    fn tmp(n: &str) -> crate::testutil::TmpDir {
        crate::testutil::TmpDir::new(&format!("lib-{n}"))
    }
    #[test]
    fn scans_games_with_counts_and_names() {
        let d = tmp("scan");
        let g = d.join("Halo 3 (GOD)/4D5307E6");
        fs::create_dir_all(g.join("00007000/ABCD.data")).unwrap();
        make_stfs(
            &g.join("00007000/ABCD"),
            b"LIVE",
            0x7000,
            0x4D53_07E6,
            "Halo 3",
            b"",
        );
        fs::write(g.join("00007000/ABCD.data/Data0000"), vec![1u8; 1000]).unwrap();
        fs::create_dir_all(g.join("00000002")).unwrap();
        fs::write(g.join("00000002/a"), b"12345").unwrap();
        fs::write(g.join("00000002/b"), b"1").unwrap();
        fs::create_dir_all(g.join("000B0000")).unwrap();
        fs::write(g.join("000B0000/tu"), b"1").unwrap();
        fs::create_dir_all(d.join("Content/0000000000000000/0ABCDEF1/00007000")).unwrap();
        fs::create_dir_all(d.join("Content/0000000000000000/FFFFFFFF")).unwrap(); // no game: ignored
        let v = scan(&[d.clone(), d.clone()]);
        assert_eq!(v.len(), 2, "{v:?}");
        let h = v.iter().find(|x| x.title_id == "4D5307E6").unwrap();
        assert_eq!(
            (h.name.as_str(), h.dlc_count, h.dlc_size, h.tu_count),
            ("Halo 3", 2, 6, 1)
        );
        assert!(h.game_size >= 1000);
        assert!(v
            .iter()
            .any(|x| x.title_id == "0ABCDEF1" && x.name.contains("0ABCDEF1")));
        fs::remove_dir_all(&d).unwrap();
    }
    #[test]
    fn scans_extracted_xex_folders_and_copies_to_games() {
        let d = tmp("xexlib");
        let game = d.join("My Halo");
        fs::create_dir_all(game.join("media")).unwrap();
        // Write a minimal default.xex with TitleID
        let xex = crate::iso_extract::tests::minimal_xex(0x4156_0817);
        fs::write(game.join("default.xex"), &xex).unwrap();
        fs::write(game.join("media/readme.txt"), b"hi").unwrap();
        let v = scan(&[d.clone()]);
        let g = v
            .iter()
            .find(|x| x.kind == "extracted")
            .expect("extracted game");
        assert_eq!(g.title_id, "41560817");
        let usb = d.join("usb");
        fs::create_dir_all(&usb).unwrap();
        let r = copy_to_usb(&game, &usb, &mut |_, _| {}, &|| false).unwrap();
        assert!(
            PathBuf::from(&r.dest).ends_with("Games/My Halo")
                || r.dest.replace('\\', "/").ends_with("Games/My Halo")
        );
        assert!(usb.join("Games/My Halo/default.xex").is_file());
        fs::remove_dir_all(&d).unwrap();
    }
    /// A DashLaunch `launch.ini` that is already on the drive is never replaced or removed by a copy.
    #[test]
    fn copy_to_usb_never_clobbers_an_existing_launch_ini() {
        let d = tmp("usb-ini");
        let game = d.join("My Halo");
        fs::create_dir_all(&game).unwrap();
        fs::write(
            game.join("default.xex"),
            crate::iso_extract::tests::minimal_xex(0x4156_0817),
        )
        .unwrap();
        fs::write(game.join("launch.ini"), b"new-ini").unwrap();
        let usb = d.join("usb");
        let dest = usb.join("Games/My Halo");
        fs::create_dir_all(&dest).unwrap();
        fs::write(dest.join("launch.ini"), b"keep-me").unwrap();
        let e = copy_to_usb(&game, &usb, &mut |_, _| {}, &|| false).unwrap_err();
        assert!(e.contains("different content"), "{e}");
        assert_eq!(fs::read(dest.join("launch.ini")).unwrap(), b"keep-me");
        assert!(
            !dest.join("default.xex").exists(),
            "nothing is copied when a file would be replaced"
        );
        fs::remove_dir_all(&d).unwrap();
    }
    #[test]
    fn copy_to_usb_never_overwrites_and_skips_identical() {
        let d = tmp("usb");
        let g = d.join("lib/4D5307E6");
        fs::create_dir_all(g.join("00007000")).unwrap();
        fs::write(g.join("00007000/h"), b"game").unwrap();
        fs::create_dir_all(g.join("00000002")).unwrap();
        fs::write(g.join("00000002/dlc"), b"dlc").unwrap();
        let usb = d.join("usb");
        fs::create_dir_all(&usb).unwrap();
        let r = copy_to_usb(&g, &usb, &mut |_, _| {}, &|| false).unwrap();
        assert_eq!((r.copied, r.identical), (2, 0));
        let dst = usb.join("Content/0000000000000000/4D5307E6");
        assert_eq!(fs::read(dst.join("00000002/dlc")).unwrap(), b"dlc");
        let r = copy_to_usb(&g, &usb, &mut |_, _| {}, &|| false).unwrap();
        assert_eq!((r.copied, r.identical), (0, 2));
        fs::write(dst.join("00000002/dlc"), b"DIFFERENT").unwrap();
        fs::write(g.join("00000002/new"), b"n").unwrap();
        assert!(copy_to_usb(&g, &usb, &mut |_, _| {}, &|| false)
            .unwrap_err()
            .contains("different content"));
        assert_eq!(fs::read(dst.join("00000002/dlc")).unwrap(), b"DIFFERENT");
        assert!(
            !dst.join("00000002/new").exists(),
            "nothing copied on conflict"
        );
        assert!(copy_to_usb(&d.join("lib"), &usb, &mut |_, _| {}, &|| false).is_err());
        assert!(copy_to_usb(&dst, &usb, &mut |_, _| {}, &|| false)
            .unwrap_err()
            .contains("same folder"));
        fs::remove_dir_all(&d).unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn r5_copy_to_usb_refuses_symlinked_content() {
        let d = tmp("r5link");
        let game = d.join("lib/4D5307E6");
        fs::create_dir_all(game.join("00007000")).unwrap();
        fs::write(game.join("00007000/HDR"), b"h").unwrap();
        let (usb, other) = (d.join("usb"), d.join("other"));
        fs::create_dir_all(&usb).unwrap();
        fs::create_dir_all(&other).unwrap();
        std::os::unix::fs::symlink(&other, usb.join("Content")).unwrap();
        let e = copy_to_usb(&game, &usb, &mut |_, _| {}, &|| false).unwrap_err();
        assert!(e.contains("link"), "{e}");
        assert!(fs::read_dir(&other).unwrap().next().is_none());
        fs::remove_file(usb.join("Content")).unwrap();
        let r = copy_to_usb(&game, &usb, &mut |_, _| {}, &|| false).unwrap();
        assert_eq!(r.copied, 1);
        assert!(
            fs::read_dir(usb.join("Content/0000000000000000/4D5307E6/00007000"))
                .unwrap()
                .all(|e| !e
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".partial"))
        );
        fs::remove_dir_all(&d).unwrap();
    }
}
