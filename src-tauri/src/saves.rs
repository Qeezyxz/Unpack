//! Save backup/restore over FTP (Content/<profile>/<TitleID>/00000001) and Aurora screenshots.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::ftp::{self, Console, Ftp};

const IMG_EXT: &[&str] = &[".png", ".jpg", ".jpeg", ".bmp"];

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SaveBackupResult {
    pub files: usize,
    pub bytes: u64,
    pub dest: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SaveRestoreResult {
    pub uploaded: usize,
    pub bytes: u64,
    pub backed_up: usize,
    pub backup_remote: String,
    pub remote: String,
}

#[derive(Debug, Clone, Serialize, serde::Deserialize, PartialEq)]
pub struct ScreenshotFile {
    pub name: String,
    pub remote_path: String,
    pub size: u64,
}

fn ts_tag() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
        .to_string()
}

pub fn profile_save_remote(drive: &str, profile: &str, title_id: &str) -> Result<Vec<String>, String> {
    let tid = title_id.to_ascii_uppercase();
    if tid.len() != 8 || !tid.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("Title ID must be 8 hex characters.".into());
    }
    let prof = profile.trim();
    if prof.is_empty() || prof.contains('/') || prof.contains('\\') {
        return Err("Profile folder must be a single name like 0000000000000000.".into());
    }
    Ok(vec![
        drive.to_string(),
        "Content".into(),
        prof.into(),
        tid,
        "00000001".into(),
    ])
}

fn is_image(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    IMG_EXT.iter().any(|e| n.ends_with(e))
}

/// Candidate Aurora screenshot folders on a drive.
pub fn screenshot_folder_candidates(drive: &str) -> Vec<Vec<String>> {
    [
        vec![drive, "Aurora", "Screenshots"],
        vec![drive, "Screenshots"],
        vec![drive, "GameData", "Screenshots"],
    ]
    .into_iter()
    .map(|v| v.into_iter().map(String::from).collect())
    .collect()
}

/// List image files in known Aurora screenshot folders (read-only LIST).
pub fn list_screenshots(con: &Console, drive: &str) -> Result<Vec<ScreenshotFile>, String> {
    let mut ftp = Ftp::connect(con.addr, &con.user, &con.pass)?;
    let mut out = vec![];
    for segs in screenshot_folder_candidates(drive) {
        let entries = match ftp.list(&segs) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for e in entries {
            if e.is_dir || !is_image(&e.name) {
                continue;
            }
            let path = ftp::join_remote(&segs.iter().chain([&e.name]).cloned().collect::<Vec<_>>());
            out.push(ScreenshotFile {
                name: e.name.clone(),
                remote_path: path,
                size: e.size,
            });
        }
    }
    ftp.quit();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Download screenshots into `dest` (created). Verifies each file size when LIST gave a size.
pub fn pull_screenshots(
    con: &Console,
    shots: &[ScreenshotFile],
    dest: &Path,
    progress: &mut dyn FnMut(u32, &str),
) -> Result<SaveBackupResult, String> {
    fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    let mut ftp = Ftp::connect(con.addr, &con.user, &con.pass)?;
    let mut files = 0usize;
    let mut bytes = 0u64;
    for (i, s) in shots.iter().enumerate() {
        progress(((i * 100) / shots.len().max(1)) as u32, &s.name);
        let segs = ftp::split_remote(&s.remote_path)?;
        let cap = (s.size as usize).clamp(1, 32 << 20);
        let data = ftp.retr(&segs, cap, false)?;
        if s.size > 0 && data.len() as u64 != s.size {
            return Err(format!(
                "Screenshot {} size mismatch (got {} bytes, expected {}).",
                s.name,
                data.len(),
                s.size
            ));
        }
        let local = dest.join(&s.name);
        let mut n = 1u32;
        let target = loop {
            let t = if n == 1 {
                local.clone()
            } else {
                dest.join(format!(
                    "{}-{}{}",
                    local.file_stem().map(|x| x.to_string_lossy().into_owned()).unwrap_or_default(),
                    n,
                    local.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default()
                ))
            };
            if !t.exists() {
                break t;
            }
            n += 1;
            if n > 999 {
                return Err(format!("Too many files named like {} in the gallery folder.", s.name));
            }
        };
        fs::write(&target, &data).map_err(|e| e.to_string())?;
        files += 1;
        bytes += data.len() as u64;
    }
    ftp.quit();
    Ok(SaveBackupResult {
        files,
        bytes,
        dest: dest.display().to_string(),
    })
}

/// Pull a save folder to local `dest`. Verifies download sizes when LIST reported them.
pub fn backup_folder(
    con: &Console,
    remote: &[String],
    dest: &Path,
    progress: &mut dyn FnMut(u32, &str),
) -> Result<SaveBackupResult, String> {
    fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    let mut ftp = Ftp::connect(con.addr, &con.user, &con.pass)?;
    let entries = ftp.list(remote)?;
    let mut files = 0usize;
    let mut bytes = 0u64;
    let nfiles = entries.iter().filter(|e| !e.is_dir).count().max(1);
    for (i, e) in entries.iter().filter(|e| !e.is_dir).enumerate() {
        let segs = remote.iter().chain([&e.name]).cloned().collect::<Vec<_>>();
        let local = dest.join(&e.name);
        progress(((i * 100) / nfiles) as u32, &e.name);
        let cap = (e.size as usize).clamp(1, 64 << 20);
        let data = ftp.retr(&segs, cap, false)?;
        if e.size > 0 && data.len() as u64 != e.size {
            return Err(format!(
                "Save file {} incomplete (got {} bytes, expected {}).",
                e.name,
                data.len(),
                e.size
            ));
        }
        fs::write(&local, &data).map_err(|e| e.to_string())?;
        bytes += data.len() as u64;
        files += 1;
    }
    ftp.quit();
    Ok(SaveBackupResult {
        files,
        bytes,
        dest: dest.display().to_string(),
    })
}

/// Restore saves from a local folder: backs up existing remote files first, uploads, verifies SIZE.
pub fn restore_folder(
    con: &Console,
    remote: &[String],
    src: &Path,
    progress: &mut dyn FnMut(u32, &str),
) -> Result<SaveRestoreResult, String> {
    if !src.is_dir() {
        return Err("Pick the folder that contains the save files to restore.".into());
    }
    let locals: Vec<(PathBuf, u64)> = fs::read_dir(src)
        .map_err(|e| e.to_string())?
        .flatten()
        .filter_map(|e| {
            e.file_type()
                .ok()
                .filter(|t| t.is_file())
                .map(|_| (e.path(), e.metadata().map(|m| m.len()).unwrap_or(0)))
        })
        .collect();
    if locals.is_empty() {
        return Err("That folder has no files to restore.".into());
    }
    let mut ftp = Ftp::connect(con.addr, &con.user, &con.pass)?;
    ftp.mkdirs(remote, &mut std::collections::HashSet::new())?;
    let parent = if remote.len() >= 2 {
        remote[..remote.len() - 1].to_vec()
    } else {
        remote.to_vec()
    };
    let backup_segs = {
        let tag = ts_tag();
        let name = format!(".unpack-save-backup-{}", tag);
        parent.iter().chain([&name]).cloned().collect::<Vec<_>>()
    };
    ftp.mkdirs(&backup_segs, &mut std::collections::HashSet::new())?;
    let backup_remote = ftp::join_remote(&backup_segs);
    let mut backed_up = 0usize;
    for (local, len) in &locals {
        let name = local.file_name().ok_or("bad local file name")?.to_string_lossy().into_owned();
        let dest_segs = remote.iter().chain([&name]).cloned().collect::<Vec<_>>();
        if let Some(sz) = ftp.size_of(&dest_segs)? {
            let from = dest_segs.clone();
            let to = backup_segs.iter().chain([&name]).cloned().collect::<Vec<_>>();
            if sz > 0 {
                ftp.rename(&from, &to)?;
                backed_up += 1;
            }
        }
        progress(0, &name);
        let mut sent = 0u64;
        ftp.stor(local, 0, &dest_segs, &mut |n| sent += n, &|| false)?;
        let remote_sz = ftp.size_of(&dest_segs)?.ok_or_else(|| format!("{} didn't appear on the console after upload.", name))?;
        if remote_sz != *len {
            return Err(format!(
                "Size check failed for {}: console has {} bytes, PC has {} bytes.",
                name,
                remote_sz,
                len
            ));
        }
    }
    ftp.quit();
    Ok(SaveRestoreResult {
        uploaded: locals.len(),
        bytes: locals.iter().map(|(_, n)| n).sum(),
        backed_up,
        backup_remote,
        remote: ftp::join_remote(remote),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ftp::tests::{con, fake, Fake};
    use crate::testutil::TmpDir;

    fn put_fake(fs: &Fake, path: &str, body: &[u8]) {
        let mut f = fs.fs.lock().unwrap();
        let p = path.to_string();
        if let Some(parent) = Path::new(path).parent() {
            let ps = parent.to_string_lossy();
            if ps != "/" && !ps.is_empty() {
                f.dirs.insert(ps.to_string());
            }
        }
        f.files.insert(p, body.to_vec());
    }

    #[test]
    fn backup_and_restore_with_console_backup() {
        let fx = fake(true, true);
        let save = "/Hdd1/Content/0000000000000000/4D5307E6/00000001";
        put_fake(&fx, &format!("{}/old.sav", save), b"OLD-SAVE-DATA");
        let td = TmpDir::new("saves-restore");
        let local = td.path().join("picked");
        fs::create_dir(&local).unwrap();
        fs::write(local.join("old.sav"), b"NEW-SAVE-DATA").unwrap();
        let remote = profile_save_remote("Hdd1", "0000000000000000", "4D5307E6").unwrap();
        let c = con(fx.port);
        let r = restore_folder(&c, &remote, &local, &mut |_, _| {}).unwrap();
        assert_eq!(r.uploaded, 1);
        assert_eq!(r.backed_up, 1);
        assert!(r.backup_remote.contains(".unpack-save-backup-"));
        let f = fx.fs.lock().unwrap();
        let new_on_console = f.files.get(&format!("{}/old.sav", save)).unwrap();
        assert_eq!(new_on_console, b"NEW-SAVE-DATA");
        let backed = f
            .files
            .keys()
            .find(|k| k.contains(".unpack-save-backup-") && k.ends_with("old.sav"))
            .cloned()
            .unwrap();
        assert_eq!(f.files.get(&backed).unwrap(), b"OLD-SAVE-DATA");
    }

    #[test]
    fn list_and_pull_screenshots() {
        let fx = fake(true, true);
        put_fake(&fx, "/Hdd1/Aurora/Screenshots/shot1.png", &[0x89, 0x50, 0x4E, 0x47, 1, 2, 3]);
        let c = con(fx.port);
        let list = list_screenshots(&c, "Hdd1").unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "shot1.png");
        let out = TmpDir::new("shots-pull");
        let r = pull_screenshots(&c, &list, out.path(), &mut |_, _| {}).unwrap();
        assert_eq!(r.files, 1);
        assert!(out.path().join("shot1.png").is_file());
    }
}
