//! Save backup/restore over FTP (Content/<profile>/<TitleID>/00000001).

use std::path::Path;

use serde::Serialize;

use crate::ftp::{self, Console, Ftp};

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SaveBackupResult {
    pub files: usize,
    pub bytes: u64,
    pub dest: String,
}

/// Pull `remote` folder to local `dest` (created). Uses one FTP session.
pub fn backup_folder(
    con: &Console,
    remote: &[String],
    dest: &Path,
    progress: &mut dyn FnMut(u32, &str),
) -> Result<SaveBackupResult, String> {
    std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    let mut ftp = Ftp::connect(con.addr, &con.user, &con.pass)?;
    let entries = ftp.list(remote)?;
    let mut files = 0usize;
    let mut bytes = 0u64;
    for e in entries {
        if e.is_dir {
            continue;
        }
        let segs = remote.iter().chain([&e.name]).cloned().collect::<Vec<_>>();
        let local = dest.join(&e.name);
        progress(0, &e.name);
        let cap = (e.size as usize).clamp(1, 64 << 20);
        let data = ftp.retr(&segs, cap, false)?;
        std::fs::write(&local, &data).map_err(|e| e.to_string())?;
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

/// Upload local files into `remote` (mkdirs). Backs up existing remote file to `.unpack-backup` first when present.
pub fn restore_folder(
    con: &Console,
    remote: &[String],
    src: &Path,
    progress: &mut dyn FnMut(u32, &str),
) -> Result<SaveBackupResult, String> {
    let mut ftp = Ftp::connect(con.addr, &con.user, &con.pass)?;
    ftp.mkdirs(remote, &mut std::collections::HashSet::new())?;
    let mut files = 0usize;
    let mut bytes = 0u64;
    for e in std::fs::read_dir(src).map_err(|e| e.to_string())?.flatten() {
        if !e.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let name = e.file_name().to_string_lossy().into_owned();
        let segs = remote.iter().chain([&name]).cloned().collect::<Vec<_>>();
        progress(0, &name);
        let len = e.metadata().map(|m| m.len()).unwrap_or(0);
        let mut sent = 0u64;
        ftp.stor(&e.path(), 0, &segs, &mut |n| sent += n, &|| false)?;
        bytes += len.max(sent);
        files += 1;
    }
    ftp.quit();
    Ok(SaveBackupResult {
        files,
        bytes,
        dest: ftp::join_remote(remote),
    })
}

pub fn profile_save_remote(
    drive: &str,
    profile: &str,
    title_id: &str,
) -> Result<Vec<String>, String> {
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
