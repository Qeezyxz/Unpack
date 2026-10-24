//! Xbox 360 DLC (STFS packages: CON / LIVE / PIRS) detection and placement into a game's
//! Content folder. A GOD game lives at `<lib>/…/<TitleID>/00007000/…`; DLC goes to
//! `<TitleID>/00000002/<file>`. Never overwrites: identical files are skipped, a different
//! file with the same name is saved under a new name (`name-2`).

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::Serialize;
use sha1::{Digest, Sha1};

pub const CT_DLC: u32 = 0x0000_0002;
pub const CT_TITLE_UPDATE: u32 = 0x000B_0000;
const HEADER_LEN: usize = 0x411 + 0x80;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct StfsInfo {
    pub magic: String,
    pub title_id: String,
    pub content_type: u32,
    /// "dlc", "title_update" or "other"
    pub kind: String,
    pub name: String,
}

fn be32(b: &[u8], off: usize) -> u32 {
    u32::from_be_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

/// Parse an STFS header. Err if the file isn't an STFS package.
pub fn parse_stfs(path: &Path) -> Result<StfsInfo, String> {
    let mut f = File::open(path).map_err(|e| format!("Cannot open: {e}"))?;
    let mut b = vec![0u8; HEADER_LEN];
    f.read_exact(&mut b)
        .map_err(|_| "Not an Xbox 360 package (too small).".to_string())?;
    parse_stfs_bytes(&b)
}

/// Bytes needed by `parse_stfs_bytes` (start of the file).
pub const STFS_HEADER_LEN: usize = HEADER_LEN;

/// Parse an STFS header from the first `STFS_HEADER_LEN` bytes of a file (local or remote).
pub fn parse_stfs_bytes(b: &[u8]) -> Result<StfsInfo, String> {
    if b.len() < HEADER_LEN {
        return Err("Not an Xbox 360 package (too small).".into());
    }
    let magic = match &b[0..4] {
        b"CON " => "CON",
        b"LIVE" => "LIVE",
        b"PIRS" => "PIRS",
        _ => return Err("Not an Xbox 360 package.".into()),
    };
    let content_type = be32(&b, 0x344);
    let tid = be32(&b, 0x360);
    let units: Vec<u16> = b[0x411..0x411 + 0x80]
        .chunks(2)
        .map(|c| u16::from_be_bytes([c[0], c[1]]))
        .take_while(|&u| u != 0)
        .collect();
    let name: String = String::from_utf16_lossy(&units)
        .chars()
        .filter(|c| !c.is_control())
        .collect();
    Ok(StfsInfo {
        magic: magic.into(),
        title_id: format!("{tid:08X}"),
        content_type,
        kind: match content_type {
            CT_DLC => "dlc",
            CT_TITLE_UPDATE => "title_update",
            _ => "other",
        }
        .into(),
        name: name.trim().to_string(),
    })
}

/// Files under `root` (relative paths) that start with an STFS magic. Links never followed.
pub fn stfs_rel_paths(root: &Path) -> Vec<PathBuf> {
    let mut out = vec![];
    let mut stack = vec![PathBuf::new()];
    while let Some(rel) = stack.pop() {
        let Ok(rd) = fs::read_dir(root.join(&rel)) else {
            continue;
        };
        for e in rd.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            let r = rel.join(e.file_name());
            if ft.is_dir() {
                stack.push(r);
            } else if ft.is_file() {
                let mut m = [0u8; 4];
                if File::open(e.path())
                    .and_then(|mut f| f.read_exact(&mut m))
                    .is_ok()
                    && matches!(&m, b"CON " | b"LIVE" | b"PIRS")
                {
                    out.push(r);
                }
            }
        }
    }
    out.sort();
    out
}

fn is_tid_dir(p: &Path) -> bool {
    fs::symlink_metadata(p).map(|m| m.is_dir()).unwrap_or(false)
}

/// Find the game's `<TitleID>` folder in the library folders. Checks, per library root L:
/// `L/<TID>`, `L/0000000000000000/<TID>`, `L/Content/0000000000000000/<TID>`, and one level of
/// subfolders `L/*/<TID>` (how Unpack lays out "Game (GOD)/<TID>"). A folder counts as the
/// game only if it holds a game (`00007000` GOD or `00004000` original Xbox) or DLC already.
/// Every matching game folder across libraries (deterministic: libraries order, then path order).
pub fn find_games(libraries: &[PathBuf], title_id: &str) -> Vec<PathBuf> {
    let mut out = vec![];
    let mut seen = std::collections::HashSet::new();
    for lib in libraries {
        if let Some(g) = find_game_in(lib, title_id) {
            let key = crate::safecopy::simplify(std::fs::canonicalize(&g).unwrap_or(g.clone()));
            if seen.insert(key) {
                out.push(g);
            }
        }
    }
    out
}

pub fn find_game(libraries: &[PathBuf], title_id: &str) -> Option<PathBuf> {
    find_games(libraries, title_id).into_iter().next()
}

fn find_game_in(lib: &Path, title_id: &str) -> Option<PathBuf> {
    find_game_one(&[lib.to_path_buf()], title_id)
}

fn find_game_one(libraries: &[PathBuf], title_id: &str) -> Option<PathBuf> {
    let tid = title_id.to_ascii_uppercase();
    if tid.len() != 8 || !tid.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let has_game = |d: &Path| {
        ["00007000", "00004000"]
            .iter()
            .any(|c| is_tid_dir(&d.join(c)))
    };
    for lib in libraries {
        let mut cands = vec![
            lib.join(&tid),
            lib.join("0000000000000000").join(&tid),
            lib.join("Content").join("0000000000000000").join(&tid),
        ];
        if let Ok(rd) = fs::read_dir(lib) {
            let mut subs: Vec<_> = rd
                .flatten()
                .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                .map(|e| e.path().join(&tid))
                .collect();
            subs.sort();
            cands.extend(subs);
        }
        if let Some(c) = cands.into_iter().find(|c| is_tid_dir(c) && has_game(c)) {
            return Some(c);
        }
    }
    None
}

/// Content roots on fixed/removable drives: only `<drive>:\Content\0000000000000000` and a few
/// common top-level paths. Never crawls a whole drive.
pub fn scan_content_roots() -> Vec<String> {
    #[allow(unused_mut)]
    let mut drives = vec![];
    #[cfg(windows)]
    for c in b'A'..=b'Z' {
        drives.push(PathBuf::from(format!("{}:\\", c as char)));
    }
    scan_roots(&drives)
}

pub fn scan_roots(drives: &[PathBuf]) -> Vec<String> {
    let mut out = vec![];
    for d in drives {
        for sub in [
            "Content",
            "Xbox360/Content",
            "XBOX/Content",
            "Games/Content",
            "GOD/Content",
        ] {
            let p = d.join(sub).join("0000000000000000");
            if is_tid_dir(&p) {
                out.push(p.display().to_string());
            }
        }
    }
    out
}

fn sha1_file(p: &Path) -> std::io::Result<([u8; 20], u64)> {
    let mut f = File::open(p)?;
    let mut h = Sha1::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut n = 0u64;
    loop {
        let k = f.read(&mut buf)?;
        if k == 0 {
            break;
        }
        n += k as u64;
        h.update(&buf[..k]);
    }
    Ok((h.finalize().into(), n))
}

#[derive(Debug, Serialize, PartialEq)]
pub struct DlcResult {
    /// installed | identical | not_found | title_update | not_dlc
    pub status: String,
    pub title_id: String,
    pub name: String,
    pub dest: String,
    pub message: String,
    pub moved: bool,
}

/// Install one DLC package. `move_file`: delete the source after a verified copy (off by default
/// in the UI). `game_path`: when several library copies share a TitleID, the UI picks one.
/// Never overwrites anything.
pub fn install_dlc(
    src: &Path,
    libraries: &[PathBuf],
    move_file: bool,
    game_path: Option<&Path>,
) -> Result<DlcResult, String> {
    let info = parse_stfs(src)?;
    let mut r = DlcResult {
        status: String::new(),
        title_id: info.title_id.clone(),
        name: info.name.clone(),
        dest: String::new(),
        message: String::new(),
        moved: false,
    };
    let label = if info.name.is_empty() {
        src.file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    } else {
        info.name.clone()
    };
    match info.kind.as_str() {
        "dlc" => {}
        "title_update" => {}
        _ => {
            r.status = "not_dlc".into();
            r.message = format!(
                "{label}: Xbox package type {:08X} isn't DLC, left where it is.",
                info.content_type
            );
            return Ok(r);
        }
    }
    if info.title_id == "00000000" || info.title_id.eq_ignore_ascii_case("FFFFFFFF") {
        r.status = "not_dlc".into();
        r.message = format!(
            "{label}: TitleID {} isn't a real game, left where it is.",
            info.title_id
        );
        return Ok(r);
    }
    let matches = find_games(libraries, &info.title_id);
    let game = if let Some(g) = game_path {
        let key = crate::safecopy::simplify(std::fs::canonicalize(g).unwrap_or(g.to_path_buf()));
        matches
            .into_iter()
            .find(|m| {
                crate::safecopy::simplify(std::fs::canonicalize(m).unwrap_or(m.clone())) == key
            })
            .ok_or_else(|| format!("That game folder doesn't match TitleID {}.", info.title_id))?
    } else if matches.is_empty() {
        r.status = "not_found".into();
        r.message = format!("Game not found (TitleID {}). Add the folder with the game to Settings → Game library folders.", info.title_id);
        return Ok(r);
    } else if matches.len() > 1 {
        r.status = "ambiguous".into();
        r.message = format!(
            "Several library games use TitleID {}. Pick which one should get this package.",
            info.title_id
        );
        return Ok(r);
    } else {
        matches[0].clone()
    };
    let multi = String::new();
    // DLC -> 00000002, title update -> 000B0000 (sibling of the game's 00007000 folder).
    let is_tu = info.kind == "title_update";
    let dir = crate::safecopy::ensure_dir_under(
        &game,
        Path::new(if is_tu { "000B0000" } else { "00000002" }),
    )?;
    let fname = src
        .file_name()
        .ok_or("bad file name")?
        .to_string_lossy()
        .into_owned();
    let (src_hash, src_len) = sha1_file(src).map_err(|e| format!("Cannot read DLC: {e}"))?;
    let mut n = 1u32;
    let target = loop {
        let t = if n == 1 {
            dir.join(&fname)
        } else {
            dir.join(format!("{fname}-{n}"))
        };
        match fs::OpenOptions::new().write(true).create_new(true).open(&t) {
            Ok(mut out) => {
                // We own this new file: copy, flush, verify. On any failure remove our partial file.
                let res = (|| -> std::io::Result<bool> {
                    let mut f = File::open(src)?;
                    std::io::copy(&mut f, &mut out)?;
                    out.flush()?;
                    out.sync_all()?;
                    drop(out);
                    Ok(sha1_file(&t)? == (src_hash, src_len))
                })();
                match res {
                    Ok(true) => break t,
                    Ok(false) => {
                        let _ = fs::remove_file(&t);
                        return Err(
                            "Copy check failed (the copy doesn't match). Nothing was installed."
                                .into(),
                        );
                    }
                    Err(e) => {
                        let _ = fs::remove_file(&t);
                        return Err(format!("Copy failed: {e}"));
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                if sha1_file(&t)
                    .map(|h| h == (src_hash, src_len))
                    .unwrap_or(false)
                {
                    r.status = "identical".into();
                    r.dest = t.display().to_string();
                    r.message = format!("{label}: already installed (identical file), skipped.");
                    return Ok(r);
                }
                n += 1;
                if n > 999 {
                    return Err("Too many files with this name.".into());
                }
            }
            Err(e) => return Err(format!("Cannot write {}: {e}", t.display())),
        }
    };
    r.status = if is_tu {
        "tu_installed".into()
    } else {
        "installed".into()
    };
    r.dest = target.display().to_string();
    let renamed = target
        .file_name()
        .map(|s| s.to_string_lossy() != fname.as_str())
        .unwrap_or(false);
    r.message = format!(
        "{label} → {}{}{}",
        crate::safecopy::plain(&game.display().to_string()),
        if renamed {
            " (a different file had the same name, saved under a new name)"
        } else {
            ""
        },
        multi
    );
    if move_file {
        r.moved = fs::remove_file(src).is_ok();
    }
    Ok(r)
}

#[cfg(test)]
pub(crate) mod tests;
