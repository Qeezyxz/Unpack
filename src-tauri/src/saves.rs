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
    /// Where each file was really saved (a taken name gets "-2" etc.), in input order (R21 L3).
    pub paths: Vec<String>,
    /// Screenshot pull only: a data: URL of each saved image (≤ 4 MB, PNG/JPEG/BMP by magic),
    /// else "". Built from the exact bytes written to `paths[i]` (R21 L3).
    #[serde(default)]
    pub thumbs: Vec<String>,
}

fn b64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut o = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                o.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                o.push('=');
            }
        }
    }
    o
}

fn thumb_url(data: &[u8]) -> String {
    let mime = if data.starts_with(&[0x89, b'P', b'N', b'G']) {
        "image/png"
    } else if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        "image/jpeg"
    } else if data.starts_with(b"BM") {
        "image/bmp"
    } else {
        return String::new();
    };
    if data.len() > 4 << 20 {
        return String::new();
    }
    format!("data:{mime};base64,{}", b64(data))
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SaveRestoreResult {
    pub uploaded: usize,
    pub bytes: u64,
    pub backed_up: usize,
    pub backup_remote: String,
    pub remote: String,
    /// Notes about an earlier interrupted restore that was repaired first (R22 L(a)).
    pub recovered: Vec<String>,
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
        .map(|d| d.as_millis())
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
/// Is `p` a symlink or (Windows) a junction / reparse point? Never follows it.
fn is_link(p: &Path) -> bool {
    fs::symlink_metadata(p).is_ok_and(|m| {
        if m.file_type().is_symlink() {
            return true;
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            m.file_attributes() & 0x400 != 0 // FILE_ATTRIBUTE_REPARSE_POINT
        }
        #[cfg(not(windows))]
        false
    })
}

/// The local folder files are written into: must be a real folder, not a link (R22 M1), and
/// (R23 L5) no existing folder on the way to it may be a link either, so create_dir_all never
/// follows one. Returns the folder's canonical path, re-checked before and after every write.
fn real_dest(dest: &Path) -> Result<PathBuf, String> {
    let linked = |p: &Path| {
        format!("{} is a link (shortcut to another folder); pick a real folder.", p.display())
    };
    for a in dest.ancestors().filter(|a| !a.as_os_str().is_empty()) {
        if is_link(a) {
            return Err(linked(a));
        }
    }
    fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    still_dest(dest, None)
}

/// R23 L5: the destination is still the same real folder (not swapped for a link mid-transfer).
fn still_dest(dest: &Path, canon: Option<&Path>) -> Result<PathBuf, String> {
    let swapped = || {
        format!(
            "{} was changed into a link (shortcut to another folder) during the transfer; stopped.",
            dest.display()
        )
    };
    for a in dest.ancestors().filter(|a| !a.as_os_str().is_empty()) {
        if is_link(a) {
            return Err(if canon.is_some() {
                swapped()
            } else {
                format!("{} is a link (shortcut to another folder); pick a real folder.", a.display())
            });
        }
    }
    if !fs::symlink_metadata(dest).is_ok_and(|m| m.is_dir()) {
        return Err(swapped());
    }
    let c = fs::canonicalize(dest).map_err(|e| e.to_string())?;
    if canon.is_some_and(|k| k != c) {
        return Err(swapped());
    }
    Ok(c)
}

/// Write `data` as a brand-new file at `local`, or `<stem>-2<ext>`, `-3`, … when anything (a
/// file, a folder or a link, even a dangling one) already has that name. Created with
/// create_new (and O_NOFOLLOW on Unix), so nothing is ever overwritten or written through a link.
fn write_new_unique(local: &Path, data: &[u8], canon: &Path) -> Result<PathBuf, String> {
    use std::io::Write;
    let dir = local.parent().ok_or("bad destination folder")?;
    still_dest(dir, Some(canon))?;
    let stem = local.file_stem().map(|x| x.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = local.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    for n in 1u32..=999 {
        let t = if n == 1 { local.to_path_buf() } else { dir.join(format!("{stem}-{n}{ext}")) };
        if fs::symlink_metadata(&t).is_ok() {
            continue;
        }
        let mut o = fs::OpenOptions::new();
        o.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            o.custom_flags(libc_nofollow());
        }
        match o.open(&t) {
            Ok(mut f) => {
                // R23 L5: if the folder was swapped between the check and the open, the new
                // file landed elsewhere: remove what was just created there and stop.
                let moved = still_dest(dir, Some(canon)).is_err()
                    || fs::canonicalize(&t).ok().as_deref().and_then(Path::parent) != Some(canon);
                if moved {
                    drop(f);
                    let _ = fs::remove_file(&t);
                    return Err(still_dest(dir, Some(canon)).err().unwrap_or_else(|| {
                        format!("{} changed during the transfer; stopped.", dir.display())
                    }));
                }
                f.write_all(data).map_err(|e| e.to_string())?;
                return Ok(t);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
    Err(format!("Too many files named like {} in that folder.", local.display()))
}

#[cfg(target_os = "linux")]
fn libc_nofollow() -> i32 {
    0o400000 // O_NOFOLLOW
}
#[cfg(all(unix, not(target_os = "linux")))]
fn libc_nofollow() -> i32 {
    0x0100 // O_NOFOLLOW (BSD/macOS)
}

pub fn pull_screenshots(
    con: &Console,
    shots: &[ScreenshotFile],
    dest: &Path,
    progress: &mut dyn FnMut(u32, &str),
) -> Result<SaveBackupResult, String> {
    let canon = real_dest(dest)?;
    let mut ftp = Ftp::connect(con.addr, &con.user, &con.pass)?;
    let mut files = 0usize;
    let mut bytes = 0u64;
    let mut paths = vec![];
    let mut thumbs = vec![];
    for s in shots {
        ftp::check_local_name(&s.name)?;
    }
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
        let local = ftp::local_child(dest, &s.name)?;
        let target = write_new_unique(&local, &data, &canon)?;
        paths.push(target.display().to_string());
        thumbs.push(thumb_url(&data));
        files += 1;
        bytes += data.len() as u64;
    }
    ftp.quit();
    Ok(SaveBackupResult {
        files,
        bytes,
        dest: dest.display().to_string(),
        paths,
        thumbs,
    })
}

/// Pull a save folder to local `dest`. Verifies download sizes when LIST reported them.
pub fn backup_folder(
    con: &Console,
    remote: &[String],
    dest: &Path,
    progress: &mut dyn FnMut(u32, &str),
) -> Result<SaveBackupResult, String> {
    let canon = real_dest(dest)?;
    let mut ftp = Ftp::connect(con.addr, &con.user, &con.pass)?;
    let entries = ftp.list(remote)?;
    // Check every name before writing anything (R21 H1).
    for e in entries.iter().filter(|e| !e.is_dir) {
        ftp::check_local_name(&e.name)?;
    }
    let mut files = 0usize;
    let mut bytes = 0u64;
    let mut paths = vec![];
    let nfiles = entries.iter().filter(|e| !e.is_dir).count().max(1);
    for (i, e) in entries.iter().filter(|e| !e.is_dir).enumerate() {
        let segs = remote.iter().chain([&e.name]).cloned().collect::<Vec<_>>();
        let local = ftp::local_child(dest, &e.name)?;
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
        let local = write_new_unique(&local, &data, &canon)?;
        paths.push(local.display().to_string());
        bytes += data.len() as u64;
        files += 1;
    }
    ftp.quit();
    Ok(SaveBackupResult {
        files,
        bytes,
        dest: dest.display().to_string(),
        paths,
        thumbs: vec![],
    })
}

/// R22 L(a): local record of a restore in progress, so a crash between "live save moved to the
/// backup folder" and "new save renamed in" is repaired on the next run.
#[derive(Serialize, serde::Deserialize)]
struct RestoreJournal {
    addr: String,
    remote: Vec<String>,
    backup: Vec<String>,
    names: Vec<String>,
    /// R23 M1: the names that already existed on the console before the restore started.
    /// Recovery undoes the whole restore: these get their backup back, the others are removed.
    #[serde(default)]
    had: Option<Vec<String>>,
    /// R24 M1: size and SHA-256 (hex) of what the restore uploads, per name. Recovery only
    /// removes or replaces a live file that still holds exactly that.
    #[serde(default)]
    uploaded: Option<Vec<(String, u64, String)>>,
}

fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect()
}

fn journal_path(con: &Console) -> Option<PathBuf> {
    let tag: String = con
        .addr
        .to_string()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    con.resume_dir.as_ref().map(|d| d.join(format!("save-restore-{tag}.json")))
}

fn write_journal(
    con: &Console,
    remote: &[String],
    backup: &[String],
    names: &[String],
    had: &[String],
    uploaded: &[(String, u64, String)],
) -> Result<(), String> {
    let Some(p) = journal_path(con) else { return Ok(()) };
    let j = RestoreJournal {
        addr: con.addr.to_string(),
        remote: remote.to_vec(),
        backup: backup.to_vec(),
        names: names.to_vec(),
        had: Some(had.to_vec()),
        uploaded: Some(uploaded.to_vec()),
    };
    let dir = p.parent().ok_or("bad journal folder")?;
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let tmp = p.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_vec(&j).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    fs::rename(&tmp, &p).map_err(|e| format!("Couldn't write the restore record: {e}"))
}

fn clear_journal(con: &Console) {
    if let Some(p) = journal_path(con) {
        let _ = fs::remove_file(p);
    }
}

/// Is a restore record waiting for this console? (No network.)
pub fn has_pending(con: &Console) -> bool {
    journal_path(con).is_some_and(|p| p.is_file())
}

/// Repair an interrupted restore on this console, if one is recorded (R23 M1: all or nothing,
/// like the in-process rollback). Every file that existed before the restore gets its original
/// back from the backup folder (the live copy, old or new, is replaced); every file the restore
/// added is removed; stray `.part` uploads are deleted. The record is removed only once all of
/// that worked. Returns what to tell the user (empty when there was nothing to undo).
pub fn recover_pending(con: &Console) -> Result<Vec<String>, String> {
    let Some(p) = journal_path(con) else { return Ok(vec![]) };
    let Ok(raw) = fs::read(&p) else { return Ok(vec![]) };
    let Ok(j) = serde_json::from_slice::<RestoreJournal>(&raw) else {
        let _ = fs::remove_file(&p);
        return Ok(vec![]);
    };
    let sane = |v: &[String]| v.len() >= 2 && v.iter().all(|x| ftp::check_local_name(x).is_ok());
    if j.addr != con.addr.to_string() || !sane(&j.remote) || !sane(&j.backup) {
        let _ = fs::remove_file(&p);
        return Ok(vec![]);
    }
    let mut ftp = Ftp::connect(con.addr, &con.user, &con.pass)?;
    let names: Vec<&String> = j.names.iter().filter(|n| ftp::check_local_name(n).is_ok()).collect();
    let seg = |base: &[String], n: &str| base.iter().cloned().chain([n.to_string()]).collect::<Vec<_>>();
    // R24 M1: does the live file still hold exactly what the restore uploaded?
    // None = unknown (journal from an older build).
    let is_ours = |ftp: &mut Ftp, name: &str| -> Result<Option<bool>, String> {
        let Some(up) = j.uploaded.as_ref() else { return Ok(None) };
        let Some((_, size, hash)) = up.iter().find(|(n, _, _)| n == name) else { return Ok(Some(false)) };
        let d = seg(&j.remote, name);
        match ftp.size_of(&d)? {
            Some(n) if n == *size => {
                let data = ftp.retr(&d, (*size as usize).clamp(1, 64 << 20), false)?;
                Ok(Some(data.len() as u64 == *size && sha256_hex(&data) == *hash))
            }
            _ => Ok(Some(false)),
        }
    };
    for name in &names {
        let part = seg(&j.remote, &ftp::part_name(name));
        if ftp.size_of(&part)?.is_some() {
            ftp.dele(&part)?;
        }
    }
    // R25 M1: the restore had finished (and the save was played since, which is why a journal
    // is still here) only when every file is in place: every name is live, and every file that
    // existed before was swapped (its original is in the backup).
    let mut state = vec![];
    for name in &names {
        let (d, b) = (seg(&j.remote, name), seg(&j.backup, name));
        state.push((ftp.size_of(&b)?.is_some(), ftp.size_of(&d)?.is_some()));
    }
    let had_it = |n: &String| j.had.as_ref().is_none_or(|h| h.contains(n));
    let all_in_place = names.iter().zip(&state).all(|(n, &(backed, live))| live && (backed || !had_it(n)));
    let mut changed = vec![];
    for (name, &(backed, live)) in names.iter().zip(&state) {
        if backed && live && is_ours(&mut ftp, name)? != Some(true) {
            changed.push((*name).clone());
        }
    }
    if all_in_place && !changed.is_empty() {
        ftp.quit();
        let _ = fs::remove_file(&p);
        return Ok(vec![format!(
            "A save restore to {} had finished, but Unpack closed before it could note that. {} changed since (you've played), so I kept your current save files as they are. The files from before that restore are still on the console in {}.",
            con.addr.ip(),
            changed.join(", "),
            ftp::join_remote(&j.backup)
        )]);
    }
    // Interrupted: file by file, put back what is missing, undo what still holds exactly our
    // upload, and keep anything that changed since (or, for a journal without hashes, anything
    // we can't prove we wrote).
    let (mut put_back, mut removed, mut kept) = (vec![], vec![], vec![]);
    for (name, &(backed, live)) in names.iter().zip(&state) {
        let (d, b) = (seg(&j.remote, name), seg(&j.backup, name));
        if backed && !live {
            ftp.rename(&b, &d)?;
            put_back.push((*name).clone());
        } else if backed {
            if is_ours(&mut ftp, name)? == Some(true) {
                ftp.dele(&d)?;
                ftp.rename(&b, &d)?;
                put_back.push((*name).clone());
            } else {
                kept.push((*name).clone());
            }
        } else if live {
            let new_by_restore = j.had.as_ref().is_some_and(|h| !h.contains(name));
            if new_by_restore {
                if is_ours(&mut ftp, name)? == Some(true) {
                    ftp.dele(&d)?;
                    removed.push((*name).clone());
                } else {
                    kept.push((*name).clone());
                }
            }
        }
    }
    ftp.quit();
    let _ = fs::remove_file(&p);
    if put_back.is_empty() && removed.is_empty() && kept.is_empty() {
        return Ok(vec![]);
    }
    let mut what = vec![];
    if !put_back.is_empty() {
        what.push(format!("put the original {} back", put_back.join(", ")));
    }
    if !removed.is_empty() {
        what.push(format!("removed {} (added by that restore)", removed.join(", ")));
    }
    let ip = con.addr.ip();
    let dir = ftp::join_remote(&j.remote);
    let msg = if kept.is_empty() {
        format!(
            "A save restore to {ip} was interrupted earlier, so Unpack undid all of it: it {} in {dir}. The save is exactly as it was before that restore; restore again if you still want the new one.",
            what.join(" and ")
        )
    } else {
        format!(
            "A save restore to {ip} was interrupted earlier and only partly done. {} changed after that (or can't be proven to be from that restore), so I kept {} as {}; {}. The save in {dir} may now be a mix of old and new files. The files from before that restore are in {}.",
            kept.join(", "),
            if kept.len() == 1 { "it" } else { "them" },
            if kept.len() == 1 { "it is" } else { "they are" },
            if what.is_empty() { "nothing else was changed".to_string() } else { format!("for the rest I {}", what.join(" and ")) },
            ftp::join_remote(&j.backup)
        )
    };
    Ok(vec![msg])
}

/// R22 L(b): FTP errors carry Send to Xbox wording (".part kept, the upload resumes"); a save
/// restore never resumes (it rolls back), so say that instead.
fn friendly_restore_err(e: String) -> String {
    e.replace(" (For Send to Xbox, the partial .part file is kept and the upload resumes.)", "")
        .replace("; the partial .part file is kept.", ".")
        .replace("send again", "restore again")
        .replace(".part", "")
}

/// Restore saves from a local folder: backs up existing remote files first, uploads, verifies SIZE.
pub fn restore_folder(
    con: &Console,
    remote: &[String],
    src: &Path,
    progress: &mut dyn FnMut(u32, &str),
) -> Result<SaveRestoreResult, String> {
    restore_inner(con, remote, src, progress).map_err(friendly_restore_err)
}

fn restore_inner(
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
    let recovered = recover_pending(con).map_err(|e| {
        format!("An earlier save restore on this console was interrupted and couldn't be repaired yet ({e}). Nothing new was changed; try again.")
    })?;
    let mut ftp = Ftp::connect(con.addr, &con.user, &con.pass)?;
    ftp.mkdirs(remote, &mut std::collections::HashSet::new())?;
    let parent = if remote.len() >= 2 {
        remote[..remote.len() - 1].to_vec()
    } else {
        remote.to_vec()
    };
    // R21 M2: a backup folder name that is new on the console (ms + random), never reused.
    let backup_segs = {
        let mut tries = 0;
        loop {
            let mut r = [0u8; 3];
            let _ = getrandom::getrandom(&mut r);
            let name = format!(
                ".unpack-save-backup-{}-{:02x}{:02x}{:02x}",
                ts_tag(),
                r[0],
                r[1],
                r[2]
            );
            let segs = parent.iter().chain([&name]).cloned().collect::<Vec<_>>();
            if !ftp.cwd(&segs)? {
                break segs;
            }
            tries += 1;
            if tries > 20 {
                return Err("Couldn't pick a new backup folder name on the console.".into());
            }
        }
    };
    ftp.mkdirs(&backup_segs, &mut std::collections::HashSet::new())?;
    let backup_remote = ftp::join_remote(&backup_segs);
    let names: Vec<String> = locals
        .iter()
        .filter_map(|(l, _)| l.file_name().map(|n| n.to_string_lossy().into_owned()))
        .collect();
    let mut had = vec![];
    for n in &names {
        let d = remote.iter().chain([n]).cloned().collect::<Vec<_>>();
        if ftp.size_of(&d)?.is_some() {
            had.push(n.clone());
        }
    }
    let mut uploaded = vec![];
    for (l, _) in &locals {
        let data = fs::read(l).map_err(|e| format!("Cannot read {}: {e}", l.display()))?;
        let n = l.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        uploaded.push((n, data.len() as u64, sha256_hex(&data)));
    }
    write_journal(con, remote, &backup_segs, &names, &had, &uploaded)?;
    let mut moved: Vec<String> = vec![];
    let mut created: Vec<String> = vec![];
    let res = (|| -> Result<(), String> {
        for (local, len) in &locals {
            let name = local.file_name().ok_or("bad local file name")?.to_string_lossy().into_owned();
            let dest_segs = remote.iter().chain([&name]).cloned().collect::<Vec<_>>();
            // R21 M3: upload to .part, verify, then rename into place.
            let part_segs = remote.iter().chain([&ftp::part_name(&name)]).cloned().collect::<Vec<_>>();
            if ftp.size_of(&part_segs)?.is_some() {
                ftp.dele(&part_segs)?;
            }
            progress(0, &name);
            let mut sent = 0u64;
            ftp.stor(local, 0, &part_segs, &mut |n| sent += n, &|| false)?;
            let remote_sz = ftp
                .size_of(&part_segs)?
                .ok_or_else(|| format!("{} didn't appear on the console after upload.", name))?;
            if remote_sz != *len {
                return Err(format!(
                    "Size check failed for {}: console has {} bytes, PC has {} bytes.",
                    name, remote_sz, len
                ));
            }
            // R22 L(a): the new file is fully on the console before the live one is touched, so
            // only the two renames below can be interrupted (the journal covers that).
            if ftp.size_of(&dest_segs)?.is_some() {
                // Back up every existing file (even empty ones) before it's replaced, and never
                // rename onto something already in the backup folder.
                let to = backup_segs.iter().chain([&name]).cloned().collect::<Vec<_>>();
                if ftp.size_of(&to)?.is_some() {
                    return Err(format!("{} already exists; not overwriting it.", ftp::join_remote(&to)));
                }
                ftp.rename(&dest_segs, &to)?;
                moved.push(name.clone());
            }
            if ftp.size_of(&dest_segs)?.is_some() {
                return Err(format!("{} appeared on the console during the restore; not replacing it.", name));
            }
            ftp.rename(&part_segs, &dest_segs)?;
            created.push(name.clone());
            match ftp.size_of(&dest_segs)? {
                Some(n) if n == *len => {}
                _ => return Err(format!("Size check failed for {} after the rename.", name)),
            }
        }
        Ok(())
    })();
    if let Err(e) = res {
        let e = friendly_restore_err(e);
        ftp.quit();
        // Put the originals back on a fresh connection (the old one may be dead).
        let rolled = (|| -> Result<(), String> {
            let mut f = Ftp::connect(con.addr, &con.user, &con.pass)?;
            for (local, _) in &locals {
                let name = local.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let part = remote.iter().chain([&ftp::part_name(&name)]).cloned().collect::<Vec<_>>();
                let _ = f.dele(&part);
            }
            for name in &created {
                let d = remote.iter().chain([name]).cloned().collect::<Vec<_>>();
                f.dele(&d)?;
            }
            for name in &moved {
                let d = remote.iter().chain([name]).cloned().collect::<Vec<_>>();
                let b = backup_segs.iter().chain([name]).cloned().collect::<Vec<_>>();
                if f.size_of(&d)?.is_some() {
                    f.dele(&d)?;
                }
                f.rename(&b, &d)?;
            }
            f.quit();
            Ok(())
        })();
        let rolled = rolled.map_err(friendly_restore_err);
        if rolled.is_ok() {
            clear_journal(con);
        }
        return Err(match rolled {
            Ok(()) => format!(
                "The save restore didn't finish ({e}). Your original save files were put back, so the console is as it was before."
            ),
            Err(e2) => format!(
                "The save restore didn't finish ({e}), and putting the originals back also failed ({e2}). Your original save files are safe on the console in {backup_remote}; copy them back into {} before playing.",
                ftp::join_remote(remote)
            ),
        });
    }
    // R24 M1: every file is renamed in and size-checked, so the restore is done: forget the
    // journal before anything else can fail (QUIT, a crash), so recovery never undoes it.
    clear_journal(con);
    ftp.quit();
    Ok(SaveRestoreResult {
        uploaded: locals.len(),
        bytes: locals.iter().map(|(_, n)| n).sum(),
        backed_up: moved.len(),
        backup_remote,
        remote: ftp::join_remote(remote),
        recovered,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ftp::tests::{con, fake, Fake, Fault};
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

    #[test]
    fn r21_h1_backup_refuses_backslash_and_drive_names() {
        let fx = fake(true, true);
        let save = "/Hdd1/Content/0000000000000000/4D5307E6/00000001";
        put_fake(&fx, &format!("{}/ok.sav", save), b"OK");
        put_fake(&fx, &format!("{}/..\\..\\esc.txt", save), b"EVIL");
        put_fake(&fx, &format!("{}/Z:\\tmp\\abs.txt", save), b"EVIL");
        put_fake(&fx, &format!("{}/Z:abs.txt", save), b"EVIL");
        let td = TmpDir::new("saves-h1");
        let dest = td.path().join("a").join("dest");
        let remote = profile_save_remote("Hdd1", "0000000000000000", "4D5307E6").unwrap();
        let r = backup_folder(&con(fx.port), &remote, &dest, &mut |_, _| {});
        assert!(r.is_err(), "{r:?}");
        let names: Vec<String> = fs::read_dir(&dest)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(names.iter().all(|n| !n.contains('\\') && !n.contains(':')), "{names:?}");
        assert!(!td.path().join("esc.txt").exists());
    }

    #[test]
    fn r21_h1_pull_screenshots_refuses_bad_local_name() {
        let fx = fake(true, true);
        put_fake(&fx, "/Hdd1/Aurora/Screenshots/shot1.png", &[0x89, 0x50, 0x4E, 0x47]);
        let c = con(fx.port);
        let out = TmpDir::new("shots-h1");
        let dest = out.path().join("g");
        for bad in ["..\\evil.png", "C:\\evil.png", "..", "CON.png", "a/b.png", "x.png."] {
            let shot = ScreenshotFile {
                name: bad.into(),
                remote_path: "/Hdd1/Aurora/Screenshots/shot1.png".into(),
                size: 4,
            };
            let r = pull_screenshots(&c, &[shot], &dest, &mut |_, _| {});
            assert!(r.is_err(), "{bad}: {r:?}");
        }
        assert_eq!(fs::read_dir(&dest).unwrap().count(), 0);
    }

    #[test]
    fn r21_m2_back_to_back_restores_keep_the_original() {
        let fx = fake(true, true);
        let save = "/Hdd1/Content/0000000000000000/4D5307E6/00000001";
        put_fake(&fx, &format!("{}/profile.sav", save), b"ORIGINAL");
        let td = TmpDir::new("saves-m2");
        let remote = profile_save_remote("Hdd1", "0000000000000000", "4D5307E6").unwrap();
        let c = con(fx.port);
        let mut backups = vec![];
        for body in [&b"V1"[..], &b"V2"[..]] {
            let local = td.path().join(String::from_utf8_lossy(body).to_string());
            fs::create_dir(&local).unwrap();
            fs::write(local.join("profile.sav"), body).unwrap();
            backups.push(restore_folder(&c, &remote, &local, &mut |_, _| {}).unwrap().backup_remote);
        }
        assert_ne!(backups[0], backups[1]);
        let f = fx.fs.lock().unwrap();
        assert_eq!(f.files.get(&format!("{}/profile.sav", save)).unwrap(), b"V2");
        assert_eq!(f.files.get(&format!("{}/profile.sav", backups[0])).unwrap(), b"ORIGINAL");
        assert_eq!(f.files.get(&format!("{}/profile.sav", backups[1])).unwrap(), b"V1");
    }

    #[test]
    fn r21_m3_interrupted_restore_puts_the_original_back() {
        let fx = fake(true, true);
        let save = "/Hdd1/Content/0000000000000000/4D5307E6/00000001";
        put_fake(&fx, &format!("{}/profile.sav", save), b"ORIGINAL");
        {
            let mut f = fx.fs.lock().unwrap();
            // Whichever name the upload goes to, the connection drops mid-STOR.
            f.faults.push(Fault::new("STOR", &format!("{}/profile.sav", save), true, 1));
            f.faults.push(Fault::new("STOR", &format!("{}/profile.sav.part", save), true, 1));
        }
        let td = TmpDir::new("saves-m3");
        let local = td.path().join("picked");
        fs::create_dir(&local).unwrap();
        fs::write(local.join("profile.sav"), b"NEW-DATA").unwrap();
        let remote = profile_save_remote("Hdd1", "0000000000000000", "4D5307E6").unwrap();
        let err = restore_folder(&con(fx.port), &remote, &local, &mut |_, _| {}).unwrap_err();
        assert!(!err.contains("resumes"), "{err}");
        assert!(err.contains("put back"), "{err}");
        let f = fx.fs.lock().unwrap();
        assert_eq!(f.files.get(&format!("{}/profile.sav", save)).map(|v| &v[..]), Some(&b"ORIGINAL"[..]));
    }

    #[test]
    fn r21_l3_pull_reports_the_real_saved_name() {
        let fx = fake(true, true);
        put_fake(&fx, "/Hdd1/Aurora/Screenshots/shot1.png", &[0x89, 0x50, 0x4E, 0x47, 9]);
        let c = con(fx.port);
        let list = list_screenshots(&c, "Hdd1").unwrap();
        let out = TmpDir::new("shots-l3");
        fs::write(out.path().join("shot1.png"), b"OLD").unwrap();
        let r = pull_screenshots(&c, &list, out.path(), &mut |_, _| {}).unwrap();
        assert_eq!(r.paths.len(), 1);
        assert!(r.paths[0].ends_with("shot1-2.png"), "{:?}", r.paths);
        assert_eq!(fs::read(&r.paths[0]).unwrap(), [0x89, 0x50, 0x4E, 0x47, 9]);
        assert_eq!(r.thumbs, vec![format!("data:image/png;base64,{}", b64(&[0x89, 0x50, 0x4E, 0x47, 9]))]);
        assert_eq!(b64(b"Man"), "TWFu");
        assert_eq!(b64(b"Ma"), "TWE=");
        assert_eq!(b64(b"M"), "TQ==");
    }

    /// R22 M1: a link (or any existing file) in the destination is never written through or
    /// overwritten; the file is saved under a new name, and a linked destination folder is refused.
    #[cfg(unix)]
    #[test]
    fn r22_m1_links_in_dest_are_never_followed() {
        use std::os::unix::fs::symlink;
        let fx = fake(true, true);
        let save = "/Hdd1/Content/0000000000000000/4D5307E6/00000001";
        put_fake(&fx, &format!("{}/ok.sav", save), b"EVIL");
        put_fake(&fx, "/Hdd1/Aurora/Screenshots/x.png", &[0x89, 0x50, 0x4E, 0x47]);
        let td = TmpDir::new("saves-r22m1");
        let victim = td.path().join("victim2");
        fs::write(&victim, b"SAFE").unwrap();
        let bk = td.path().join("bk");
        fs::create_dir(&bk).unwrap();
        symlink(&victim, bk.join("ok.sav")).unwrap();
        let c = con(fx.port);
        let remote = profile_save_remote("Hdd1", "0000000000000000", "4D5307E6").unwrap();
        let r = backup_folder(&c, &remote, &bk, &mut |_, _| {}).unwrap();
        assert_eq!(fs::read(&victim).unwrap(), b"SAFE");
        assert!(r.paths[0].ends_with("ok-2.sav"), "{:?}", r.paths);
        // an existing real file is not overwritten either
        fs::write(bk.join("ok-3.sav"), b"MINE").unwrap();
        let r = backup_folder(&c, &remote, &bk, &mut |_, _| {}).unwrap();
        assert_eq!(fs::read(bk.join("ok-3.sav")).unwrap(), b"MINE");
        assert!(r.paths[0].ends_with("ok-4.sav"), "{:?}", r.paths);
        // dangling link in the gallery folder: nothing is created outside it
        let sh = td.path().join("sh");
        fs::create_dir(&sh).unwrap();
        let outside = td.path().join("outside.png");
        symlink(&outside, sh.join("x.png")).unwrap();
        let list = list_screenshots(&c, "Hdd1").unwrap();
        let r = pull_screenshots(&c, &list, &sh, &mut |_, _| {}).unwrap();
        assert!(!outside.exists());
        assert!(r.paths[0].ends_with("x-2.png"), "{:?}", r.paths);
        // a destination folder that is itself a link is refused
        let ln = td.path().join("linkdir");
        symlink(&sh, &ln).unwrap();
        assert!(backup_folder(&c, &remote, &ln, &mut |_, _| {}).is_err());
        assert!(pull_screenshots(&c, &list, &ln, &mut |_, _| {}).is_err());
    }

    /// R23 L5: the destination swapped for a link during the transfer (here: from the progress
    /// callback, i.e. after the start check, before the file is written) is caught and nothing
    /// lands in the link target; a link in a parent folder is never followed either.
    #[cfg(unix)]
    #[test]
    fn r23_l5_dest_swapped_mid_transfer_or_linked_parent() {
        use std::os::unix::fs::symlink;
        let fx = fake(true, true);
        let save = "/Hdd1/Content/0000000000000000/4D5307E6/00000001";
        put_fake(&fx, &format!("{}/ok.sav", save), b"DATA");
        put_fake(&fx, "/Hdd1/Aurora/Screenshots/x.png", &[0x89, 0x50, 0x4E, 0x47]);
        let td = TmpDir::new("saves-r23l5");
        let c = con(fx.port);
        let remote = profile_save_remote("Hdd1", "0000000000000000", "4D5307E6").unwrap();
        let list = list_screenshots(&c, "Hdd1").unwrap();
        for (i, shots) in [false, true].into_iter().enumerate() {
            let dest = td.path().join(format!("dest{i}"));
            let out = td.path().join(format!("out{i}"));
            fs::create_dir_all(&out).unwrap();
            let mut swap = |_: u32, _: &str| {
                if fs::symlink_metadata(&dest).is_ok_and(|m| m.is_dir()) {
                    fs::rename(&dest, td.path().join(format!("moved{i}"))).unwrap();
                    symlink(&out, &dest).unwrap();
                }
            };
            let r = if shots {
                pull_screenshots(&c, &list, &dest, &mut swap)
            } else {
                backup_folder(&c, &remote, &dest, &mut swap)
            };
            let e = r.unwrap_err();
            assert!(e.contains("during the transfer"), "{e}");
            assert_eq!(fs::read_dir(&out).unwrap().count(), 0, "nothing written through the link");
        }
        // link in a parent folder: refused, and create_dir_all never ran through it
        let out = td.path().join("out6");
        fs::create_dir_all(&out).unwrap();
        fs::create_dir_all(td.path().join("d6")).unwrap();
        symlink(&out, td.path().join("d6/sub")).unwrap();
        let dest = td.path().join("d6/sub/x");
        assert!(backup_folder(&c, &remote, &dest, &mut |_, _| {}).unwrap_err().contains("is a link"));
        assert!(pull_screenshots(&c, &list, &dest, &mut |_, _| {}).unwrap_err().contains("is a link"));
        assert!(!out.join("x").exists());
    }

    /// R22 L(b): restore errors talk about the save restore, not Send to Xbox's .part resume.
    #[test]
    fn r22_lb_restore_error_text_is_specific() {
        let fx = fake(true, true);
        let save = "/Hdd1/Content/0000000000000000/4D5307E6/00000001";
        put_fake(&fx, &format!("{}/old.sav", save), b"OLD");
        fx.fs.lock().unwrap().disk_full = 1;
        let td = TmpDir::new("saves-r22lb");
        fs::write(td.path().join("old.sav"), b"NEW-DATA").unwrap();
        let remote = profile_save_remote("Hdd1", "0000000000000000", "4D5307E6").unwrap();
        let e = restore_folder(&con(fx.port), &remote, td.path(), &mut |_, _| {}).unwrap_err();
        assert!(e.contains("drive is full"), "{e}");
        assert!(!e.contains("Send to Xbox") && !e.contains("resumes") && !e.contains(".part"), "{e}");
        assert_eq!(fx.fs.lock().unwrap().files.get(&format!("{}/old.sav", save)).unwrap(), b"OLD");
        let d = super::friendly_restore_err("The connection to the console dropped. Check the Wi-Fi or cable and that the console is still on, then try again. (For Send to Xbox, the partial .part file is kept and the upload resumes.)".into());
        assert!(!d.contains("Send to Xbox") && d.contains("connection to the console dropped"), "{d}");
    }

    /// R22 L(a): a restore that died between moving the live save to the backup folder and
    /// renaming the new one in leaves a local journal; the next run (startup or the next
    /// restore) puts the original back, removes the stray .part and says so.
    #[test]
    fn r22_la_interrupted_restore_is_recovered() {
        let fx = fake(true, true);
        let save = "/Hdd1/Content/0000000000000000/4D5307E6/00000001";
        let bk = "/Hdd1/Content/0000000000000000/4D5307E6/.unpack-save-backup-1-aabbcc";
        put_fake(&fx, &format!("{}/old.sav", bk), b"OLD");
        put_fake(&fx, &format!("{}/old.sav.part", save), b"NE");
        put_fake(&fx, &format!("{}/done.sav", save), b"NEWDONE");
        put_fake(&fx, &format!("{}/done.sav", bk), b"OLDDONE");
        let jd = TmpDir::new("saves-r22la");
        let mut c = con(fx.port);
        c.resume_dir = Some(jd.path().to_path_buf());
        let remote = profile_save_remote("Hdd1", "0000000000000000", "4D5307E6").unwrap();
        let backup: Vec<String> = bk.trim_start_matches('/').split('/').map(String::from).collect();
        write_journal(&c, &remote, &backup, &["old.sav".into(), "done.sav".into()], &["old.sav".into(), "done.sav".into()], &up(&[("old.sav", b"NEW"), ("done.sav", b"NEWDONE")])).unwrap();
        let msgs = recover_pending(&c).unwrap();
        assert_eq!(msgs.len(), 1, "{msgs:?}");
        assert!(msgs[0].contains("old.sav") && msgs[0].contains("done.sav") && msgs[0].contains("undid all"), "{msgs:?}");
        {
            let f = fx.fs.lock().unwrap();
            assert_eq!(f.files.get(&format!("{}/old.sav", save)).unwrap(), b"OLD");
            assert!(!f.files.contains_key(&format!("{}/old.sav.part", save)));
            // R23 M1: all or nothing, the already swapped file gets its original back too.
            assert_eq!(f.files.get(&format!("{}/done.sav", save)).unwrap(), b"OLDDONE");
        }
        assert!(recover_pending(&c).unwrap().is_empty(), "journal is removed once recovered");
        // a normal restore leaves no journal behind
        let src = TmpDir::new("saves-r22la-src");
        fs::write(src.path().join("old.sav"), b"FRESH").unwrap();
        let r = restore_folder(&c, &remote, src.path(), &mut |_, _| {}).unwrap();
        assert!(r.recovered.is_empty());
        assert_eq!(fs::read_dir(jd.path()).unwrap().count(), 0);
    }

    fn up(v: &[(&str, &[u8])]) -> Vec<(String, u64, String)> {
        v.iter().map(|(n, b)| (n.to_string(), b.len() as u64, sha256_hex(b))).collect()
    }

    fn r24_setup(name: &str) -> (Fake, TmpDir, Console, Vec<String>, &'static str) {
        let fx = fake(true, true);
        let save = "/Hdd1/Content/0000000000000000/4D5307E6/00000001";
        put_fake(&fx, &format!("{save}/a.sav"), b"OLD-A");
        put_fake(&fx, &format!("{save}/b.sav"), b"OLD-B");
        let jd = TmpDir::new(name);
        let mut c = con(fx.port);
        c.resume_dir = Some(jd.path().to_path_buf());
        let remote = profile_save_remote("Hdd1", "0000000000000000", "4D5307E6").unwrap();
        (fx, jd, c, remote, save)
    }

    /// R24 M1: killed at QUIT after a finished restore, then the player plays: the next start
    /// must not undo the restore or lose the new progress.
    #[test]
    fn r24_m1_kill_at_quit_then_play_keeps_progress() {
        let (fx, jd, c, remote, save) = r24_setup("saves-r24-quit");
        let src = TmpDir::new("saves-r24-quit-src");
        for (n, b) in [("a.sav", "NEW-A"), ("b.sav", "NEW-B"), ("c.sav", "NEW-C")] {
            fs::write(src.path().join(n), b).unwrap();
        }
        fx.fs.lock().unwrap().faults.push(Fault::new("QUIT", "*", true, 1));
        restore_folder(&c, &remote, src.path(), &mut |_, _| {}).unwrap();
        assert_eq!(fs::read_dir(jd.path()).unwrap().count(), 0, "journal gone before QUIT");
        put_fake(&fx, &format!("{save}/a.sav"), b"PLAYED-PROGRESS");
        assert!(recover_pending(&c).unwrap().is_empty());
        let f = fx.fs.lock().unwrap();
        assert_eq!(f.files.get(&format!("{save}/a.sav")).unwrap(), b"PLAYED-PROGRESS");
        assert_eq!(f.files.get(&format!("{save}/c.sav")).unwrap(), b"NEW-C");
    }

    /// R24 M1: a journal still left for a finished restore (older build, or a crash right after
    /// the last rename) must keep a save that changed since, and say where the originals are.
    #[test]
    fn r24_m1_stale_journal_keeps_changed_save() {
        let (fx, jd, c, remote, save) = r24_setup("saves-r24-stale");
        let bk = "/Hdd1/Content/0000000000000000/4D5307E6/.unpack-save-backup-1-aabbcc";
        let backup: Vec<String> = bk.trim_start_matches('/').split('/').map(String::from).collect();
        put_fake(&fx, &format!("{bk}/a.sav"), b"OLD-A");
        put_fake(&fx, &format!("{bk}/b.sav"), b"OLD-B");
        put_fake(&fx, &format!("{save}/a.sav"), b"PLAYED-PROGRESS");
        put_fake(&fx, &format!("{save}/b.sav"), b"NEW-B");
        put_fake(&fx, &format!("{save}/c.sav"), b"NEW-C");
        let names: Vec<String> = ["a.sav", "b.sav", "c.sav"].map(String::from).to_vec();
        let ups = up(&[("a.sav", b"NEW-A"), ("b.sav", b"NEW-B"), ("c.sav", b"NEW-C")]);
        write_journal(&c, &remote, &backup, &names, &names[..2], &ups).unwrap();
        let msgs = recover_pending(&c).unwrap();
        assert!(msgs.len() == 1 && msgs[0].contains("kept your current save") && msgs[0].contains(bk), "{msgs:?}");
        let f = fx.fs.lock().unwrap();
        assert_eq!(f.files.get(&format!("{save}/a.sav")).unwrap(), b"PLAYED-PROGRESS");
        assert_eq!(f.files.get(&format!("{save}/c.sav")).unwrap(), b"NEW-C");
        assert_eq!(f.files.get(&format!("{bk}/a.sav")).unwrap(), b"OLD-A");
        assert_eq!(fs::read_dir(jd.path()).unwrap().count(), 0);
    }

    /// R25 M1: interrupted at RNFR:3 / RNFR:4 (a.sav swapped, b.sav not yet / half swapped),
    /// then a.sav played. Never "finished": b.sav must be live (original), a.sav kept, c.sav
    /// absent, and the message must say it was interrupted and where the backup is.
    fn r25_case(case: &str, b_moved: bool, uploaded: bool) {
        let (fx, jd, c, remote, save) = r24_setup(&format!("saves-r25-{case}"));
        let bk = "/Hdd1/Content/0000000000000000/4D5307E6/.unpack-save-backup-1-aabbcc";
        let backup: Vec<String> = bk.trim_start_matches('/').split('/').map(String::from).collect();
        put_fake(&fx, &format!("{bk}/a.sav"), b"OLD-A");
        put_fake(&fx, &format!("{save}/a.sav"), b"PLAYED");
        put_fake(&fx, &format!("{save}/b.sav.part"), b"NEW-B");
        if b_moved {
            fx.fs.lock().unwrap().files.remove(&format!("{save}/b.sav"));
            put_fake(&fx, &format!("{bk}/b.sav"), b"OLD-B");
        }
        let names: Vec<String> = ["a.sav", "b.sav", "c.sav"].map(String::from).to_vec();
        let ups = up(&[("a.sav", b"NEW-A"), ("b.sav", b"NEW-B"), ("c.sav", b"NEW-C")]);
        write_journal(&c, &remote, &backup, &names, &names[..2], if uploaded { &ups } else { &[] }).unwrap();
        if !uploaded {
            let jp = journal_path(&c).unwrap();
            let mut v: serde_json::Value = serde_json::from_slice(&fs::read(&jp).unwrap()).unwrap();
            v.as_object_mut().unwrap().remove("uploaded");
            fs::write(&jp, v.to_string()).unwrap();
        }
        let msgs = recover_pending(&c).unwrap();
        assert!(
            msgs.len() == 1 && msgs[0].contains("interrupted") && !msgs[0].contains("had finished") && msgs[0].contains("a.sav") && msgs[0].contains(bk),
            "{case}: {msgs:?}"
        );
        let f = fx.fs.lock().unwrap();
        assert_eq!(f.files.get(&format!("{save}/a.sav")).unwrap(), b"PLAYED", "{case}");
        assert_eq!(f.files.get(&format!("{save}/b.sav")).unwrap(), b"OLD-B", "{case}");
        assert!(!f.files.contains_key(&format!("{save}/c.sav")), "{case}");
        assert!(!f.files.keys().any(|k| k.ends_with(".part")), "{case}");
        assert_eq!(f.files.get(&format!("{bk}/a.sav")).unwrap(), b"OLD-A", "{case}: backup kept");
        drop(f);
        assert_eq!(fs::read_dir(jd.path()).unwrap().count(), 0, "{case}");
    }

    #[test]
    fn r25_m1_kill_at_rnfr3_then_play() {
        r25_case("rnfr3", false, true);
    }

    #[test]
    fn r25_m1_kill_at_rnfr4_then_play() {
        r25_case("rnfr4", true, true);
    }

    /// R25 L1: a journal without hashes never deletes a live file it can't prove it wrote.
    #[test]
    fn r25_l1_no_hashes_keeps_played_file() {
        r25_case("nohash", true, false);
    }

    /// R24 L1: an old-format journal (no `had`, no `uploaded`) never deletes a file without backup.
    #[test]
    fn r24_l1_journal_without_had_keeps_unbacked_originals() {
        let (fx, jd, c, remote, save) = r24_setup("saves-r24-l1");
        put_fake(&fx, &format!("{save}/a.sav.part"), b"NEW-A");
        let j = serde_json::json!({"addr": c.addr.to_string(), "remote": remote,
            "backup": ["Hdd1","Content","0000000000000000","4D5307E6",".unpack-save-backup-1-aabbcc"],
            "names": ["a.sav", "b.sav"]});
        fs::write(journal_path(&c).unwrap(), j.to_string()).unwrap();
        let _ = recover_pending(&c).unwrap();
        let f = fx.fs.lock().unwrap();
        assert_eq!(f.files.get(&format!("{save}/a.sav")).unwrap(), b"OLD-A");
        assert_eq!(f.files.get(&format!("{save}/b.sav")).unwrap(), b"OLD-B");
        assert!(!f.files.contains_key(&format!("{save}/a.sav.part")));
        drop(f);
        assert_eq!(fs::read_dir(jd.path()).unwrap().count(), 0);
    }

    /// R23 M1: state left by a kill at any point of a restore of a.sav (existed), b.sav (existed)
    /// and c.sav (new); recovery must always return the whole save to its pre-restore state.
    fn r23_crash_case(case: &str, setup: &dyn Fn(&Fake, &str, &str)) {
        let fx = fake(true, true);
        let save = "/Hdd1/Content/0000000000000000/4D5307E6/00000001";
        let bk = "/Hdd1/Content/0000000000000000/4D5307E6/.unpack-save-backup-1-aabbcc";
        setup(&fx, save, bk);
        let jd = TmpDir::new(&format!("saves-r23m1-{}", case.replace(' ', "-")));
        let mut c = con(fx.port);
        c.resume_dir = Some(jd.path().to_path_buf());
        let remote = profile_save_remote("Hdd1", "0000000000000000", "4D5307E6").unwrap();
        let backup: Vec<String> = bk.trim_start_matches('/').split('/').map(String::from).collect();
        let names: Vec<String> = ["a.sav", "b.sav", "c.sav"].map(String::from).to_vec();
        let ups = up(&[("a.sav", b"NEW-A"), ("b.sav", b"NEW-B"), ("c.sav", b"NEW-C")]);
        write_journal(&c, &remote, &backup, &names, &names[..2], &ups).unwrap();
        let msgs = recover_pending(&c).unwrap();
        let f = fx.fs.lock().unwrap();
        assert_eq!(f.files.get(&format!("{save}/a.sav")).map(|v| v.as_slice()), Some(&b"OLD-A"[..]), "{case}");
        assert_eq!(f.files.get(&format!("{save}/b.sav")).map(|v| v.as_slice()), Some(&b"OLD-B"[..]), "{case}");
        assert!(!f.files.contains_key(&format!("{save}/c.sav")), "{case}");
        assert!(!f.files.keys().any(|k| k.ends_with(".part")), "{case}");
        assert_eq!(msgs.len(), 1, "{case}: the user is always told ({msgs:?})");
        assert!(msgs[0].contains("undid all") && msgs[0].contains("a.sav"), "{case}: {msgs:?}");
        assert_eq!(fs::read_dir(jd.path()).unwrap().count(), 0, "{case}");
    }

    #[test]
    fn r23_m1_kill_between_files_rolls_everything_back() {
        // a.sav swapped (live NEW, original in backup); b.sav not touched yet; b.sav.part uploaded.
        r23_crash_case("between files", &|fx, save, bk| {
            put_fake(fx, &format!("{save}/a.sav"), b"NEW-A");
            put_fake(fx, &format!("{bk}/a.sav"), b"OLD-A");
            put_fake(fx, &format!("{save}/b.sav"), b"OLD-B");
            put_fake(fx, &format!("{save}/b.sav.part"), b"NEW-B");
        });
        // ...and after c.sav (new) was already renamed in.
        r23_crash_case("after new file", &|fx, save, bk| {
            put_fake(fx, &format!("{save}/a.sav"), b"NEW-A");
            put_fake(fx, &format!("{bk}/a.sav"), b"OLD-A");
            put_fake(fx, &format!("{save}/b.sav"), b"NEW-B");
            put_fake(fx, &format!("{bk}/b.sav"), b"OLD-B");
            put_fake(fx, &format!("{save}/c.sav"), b"NEW-C");
        });
    }

    #[test]
    fn r23_m1_kill_mid_file_rolls_everything_back() {
        // b.sav's live copy already moved to the backup, the new one not renamed in yet.
        r23_crash_case("mid file", &|fx, save, bk| {
            put_fake(fx, &format!("{save}/a.sav"), b"NEW-A");
            put_fake(fx, &format!("{bk}/a.sav"), b"OLD-A");
            put_fake(fx, &format!("{bk}/b.sav"), b"OLD-B");
            put_fake(fx, &format!("{save}/b.sav.part"), b"NEW-B");
        });
    }
}
