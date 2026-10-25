//! Xbox 360 / original Xbox ISO → extracted folder (XDVDFS/XGD filesystem).
//! Writes `Game Name\` with default.xex (or default.xbe) and every file from the image.
//! Same safety rules as GOD: atomic new folder, truncation check, verify file count/sizes,
//! delete-ISO only after verify, never with Replace.

use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use iso2god::executable::TitleInfo;
use iso2god::game_list;
use iso2god::iso::{self, DirectoryTable, SECTOR_SIZE};
use serde::Serialize;

use crate::archive::{Jobs, ERR_CANCELLED};
use crate::god::{self, clamp_title, free_space, iso_info};

#[derive(Debug, Serialize, Clone)]
pub struct ExtractResult {
    pub output: String,
    pub title_id: String,
    pub name: Option<String>,
    pub files: usize,
    pub bytes: u64,
    pub disc_number: u8,
    pub disc_count: u8,
    /// This disc was overlaid onto a folder that already holds another disc of the game.
    pub merged: bool,
    /// Files that were left alone because they are the playable executable of an earlier disc.
    pub kept: usize,
    /// Set when the TitleID is not in the game list.
    pub warning: Option<String>,
}

/// How a later disc is laid over an existing game folder.
#[derive(Clone, Copy)]
struct Merge {
    /// May default.xex / default.xbe be replaced? Only by a lower-numbered disc (Disc 1 over a Disc 2 that arrived first).
    replace_exe: bool,
    /// This disc's number, used to name a file that differs from the earlier disc's copy.
    disc: u8,
}

/// A later disc never replaces the playable executable of an earlier one.
pub fn is_playable_exe(rel: &Path) -> bool {
    rel.file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.eq_ignore_ascii_case("default.xex") || n.eq_ignore_ascii_case("default.xbe"))
        .unwrap_or(false)
}

#[derive(Clone)]
struct PlannedFile {
    rel: PathBuf,
    sector: u32,
    size: u64,
}

fn collect_files(dir: &DirectoryTable, prefix: &Path, out: &mut Vec<PlannedFile>) {
    for e in &dir.entries {
        let name = crate::archive::safe_segment(&e.name);
        if name.is_empty() {
            continue;
        }
        let rel = prefix.join(&name);
        if let Some(sub) = &e.subdirectory {
            collect_files(sub, &rel, out);
        } else if e.size > 0 {
            out.push(PlannedFile {
                rel,
                sector: e.sector,
                size: e.size as u64,
            });
        }
    }
}

fn verify_tree(root: &Path, planned: &[PlannedFile]) -> Result<(), String> {
    let mut found = 0usize;
    for p in planned {
        let path = root.join(&p.rel);
        let meta = fs::symlink_metadata(&path)
            .map_err(|_| format!("Missing extracted file: {}", p.rel.display()))?;
        if crate::safecopy::is_link(&meta) || !meta.is_file() {
            return Err(format!(
                "{} is not a regular file after extract.",
                p.rel.display()
            ));
        }
        if meta.len() != p.size {
            return Err(format!(
                "Size mismatch for {}: got {} bytes, ISO says {}.",
                p.rel.display(),
                meta.len(),
                p.size
            ));
        }
        found += 1;
    }
    if found != planned.len() {
        return Err(format!(
            "File count mismatch: wrote {found}, expected {}.",
            planned.len()
        ));
    }
    let mut walk = 0usize;
    let mut st = vec![root.to_path_buf()];
    while let Some(d) = st.pop() {
        let Ok(rd) = fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let Ok(m) = fs::symlink_metadata(e.path()) else {
                continue;
            };
            if m.is_dir() {
                st.push(e.path());
            } else if m.is_file() {
                walk += 1;
            }
        }
    }
    if walk != planned.len() {
        return Err(format!(
            "Extra or missing files on disk: found {walk} files, expected {}.",
            planned.len()
        ));
    }
    Ok(())
}

/// Byte-for-byte compare each extracted file with the corresponding ISO extent (for delete-ISO).
fn verify_content_vs_iso(
    iso_path: &Path,
    root_offset: u64,
    sector_size: u64,
    root: &Path,
    planned: &[PlannedFile],
    exact: bool,
) -> Result<(), String> {
    if exact {
        verify_tree(root, planned)?;
    }
    let mut iso = File::open(iso_path).map_err(|e| format!("Cannot open ISO: {e}"))?;
    let mut buf_a = vec![0u8; 1 << 16];
    let mut buf_b = vec![0u8; 1 << 16];
    for p in planned {
        let pos = root_offset + (p.sector as u64) * sector_size;
        iso.seek(SeekFrom::Start(pos))
            .map_err(|e| format!("Seek ISO: {e}"))?;
        let mut file = File::open(root.join(&p.rel)).map_err(|e| e.to_string())?;
        let mut left = p.size;
        while left > 0 {
            let n = (left as usize).min(buf_a.len());
            iso.read_exact(&mut buf_a[..n])
                .map_err(|e| format!("ISO read failed for {}: {e}", p.rel.display()))?;
            file.read_exact(&mut buf_b[..n])
                .map_err(|e| format!("File read failed for {}: {e}", p.rel.display()))?;
            if buf_a[..n] != buf_b[..n] {
                return Err(format!(
                    "Content mismatch for {} vs the ISO image.",
                    p.rel.display()
                ));
            }
            left -= n as u64;
        }
    }
    Ok(())
}

/// Extract XDVDFS contents of `iso_path` into `dest_dir` (must exist / be empty claim target parent already set).
/// Writes into a temp folder under dest_dir, verifies, then moves children into dest_dir.
pub fn extract(
    iso_path: &Path,
    dest_dir: &Path,
    title_override: Option<&str>,
    jobs: &Jobs,
    job_id: &str,
    progress: &mut dyn FnMut(u32, &str),
) -> Result<ExtractResult, String> {
    extract_inner(
        iso_path,
        dest_dir,
        title_override,
        jobs,
        job_id,
        progress,
        None,
    )
}

fn extract_inner(
    iso_path: &Path,
    dest_dir: &Path,
    title_override: Option<&str>,
    jobs: &Jobs,
    job_id: &str,
    progress: &mut dyn FnMut(u32, &str),
    merge: Option<Merge>,
) -> Result<ExtractResult, String> {
    let ioerr = |what: &str| {
        let w = what.to_string();
        move |e: std::io::Error| format!("{w}: {e}")
    };
    let f = File::open(iso_path).map_err(ioerr("Cannot open ISO"))?;
    let iso_len = f.metadata().map_err(ioerr("Cannot read ISO"))?.len();
    let mut source = iso::IsoReader::read(f).map_err(|e| format!("Not an Xbox ISO ({e})"))?;
    // A later disc that carries no default.xex can still be overlaid onto its game's folder.
    let title = match TitleInfo::from_image(&mut source) {
        Ok(t) => Some(t),
        Err(e) if merge.is_none() => {
            return Err(god::missing_executable_message(iso_path, &format!("{e:#}")))
        }
        Err(_) => None,
    };
    let exe_info = title.map(|t| t.execution_info);
    let (disc_number, disc_count) = match &exe_info {
        Some(e) => god::disc_numbers(iso_path, e),
        None => (crate::discs::disc_hint_from_name(iso_path).unwrap_or(1), 2),
    };
    let root_offset = source.volume_descriptor.root_offset;
    let sector_size = source.volume_descriptor.sector_size.max(SECTOR_SIZE);
    let data_size = source.get_max_used_prefix_size();
    god::check_complete_pub(iso_len, root_offset, data_size)?;

    let mut planned = Vec::new();
    collect_files(&source.directory_table, Path::new(""), &mut planned);
    if planned.is_empty() {
        return Err("The ISO file system has no files.".into());
    }
    let need: u64 = planned.iter().map(|p| p.size).sum();
    fs::create_dir_all(dest_dir).map_err(ioerr("Cannot create output folder"))?;
    let _dest_lock = crate::destlock::acquire(
        dest_dir,
        || progress(0, crate::destlock::WAIT_MESSAGE),
        &|| jobs.is_cancelled(job_id),
    )?;
    crate::safecopy::fat_check(dest_dir, planned.iter().map(|p| (p.rel.clone(), p.size)))?;
    if let Some(free) = free_space(dest_dir) {
        if free < need + 64 * 1024 * 1024 {
            return Err(format!(
                "Not enough free space: extracting needs about {} MB but the drive has {} MB free.",
                need / 1_048_576,
                free / 1_048_576
            ));
        }
    }

    let tmp = dest_dir.join(format!(
        ".unpack-xex-tmp-{}",
        crate::archive::safe_segment(job_id)
    ));
    if tmp.exists() {
        let _ = fs::remove_dir_all(&tmp);
    }
    struct TmpGuard(PathBuf);
    impl Drop for TmpGuard {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    let _guard = TmpGuard(tmp.clone());
    fs::create_dir_all(&tmp).map_err(ioerr("Cannot create temp folder"))?;

    let total = planned.len().max(1) as u64;
    let mut wrote_bytes = 0u64;
    for (i, p) in planned.iter().enumerate() {
        if jobs.is_cancelled(job_id) {
            return Err(ERR_CANCELLED.into());
        }
        let out_path = tmp.join(&p.rel);
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent).map_err(ioerr("Cannot create folder"))?;
        }
        let pos = root_offset + (p.sector as u64) * sector_size;
        let mut src = File::open(iso_path).map_err(ioerr("Cannot open ISO"))?;
        src.seek(SeekFrom::Start(pos))
            .map_err(ioerr("Seek failed"))?;
        let mut dest = File::options()
            .write(true)
            .create_new(true)
            .open(&out_path)
            .map_err(ioerr("Cannot create file"))?;
        let mut left = p.size;
        let mut buf = vec![0u8; 1024 * 1024];
        while left > 0 {
            if jobs.is_cancelled(job_id) {
                return Err(ERR_CANCELLED.into());
            }
            let n = (left as usize).min(buf.len());
            src.read_exact(&mut buf[..n]).map_err(|e| {
                format!(
                    "Read failed for {}: {e} (ISO may be truncated)",
                    p.rel.display()
                )
            })?;
            dest.write_all(&buf[..n]).map_err(ioerr("Write failed"))?;
            left -= n as u64;
            wrote_bytes += n as u64;
        }
        dest.sync_all().ok();
        let pct = (((i as u64 + 1) * 100) / total) as u32;
        progress(pct.min(99), &p.rel.display().to_string());
    }
    if fs::metadata(iso_path).map(|m| m.len()).ok() != Some(iso_len) {
        return Err("The ISO changed while it was being extracted. Try again.".into());
    }
    verify_tree(&tmp, &planned)?;
    if jobs.is_cancelled(job_id) {
        return Err(ERR_CANCELLED.into());
    }
    let mut kept_files: Vec<PathBuf> = Vec::new();
    let mut renamed: Vec<(PathBuf, PathBuf)> = Vec::new();
    match merge {
        None => {
            // Move children into dest_dir (already claimed empty).
            for e in fs::read_dir(&tmp).map_err(ioerr("Cannot read temp"))? {
                let e = e.map_err(ioerr("Cannot read temp"))?;
                let to = dest_dir.join(e.file_name());
                move_entry(&e.path(), &to).map_err(|err| {
                    format!("Could not place {}: {err}", e.file_name().to_string_lossy())
                })?;
            }
            verify_tree(dest_dir, &planned)?;
        }
        Some(m) => {
            overlay(
                &tmp,
                dest_dir,
                Path::new(""),
                m,
                job_id,
                &mut kept_files,
                &mut renamed,
            )?;
            for p in &planned {
                if kept_files.contains(&p.rel) {
                    continue;
                }
                let at = renamed
                    .iter()
                    .find(|(a, _)| *a == p.rel)
                    .map(|(_, b)| b.clone())
                    .unwrap_or_else(|| p.rel.clone());
                let meta = fs::symlink_metadata(dest_dir.join(&at))
                    .map_err(|_| format!("Missing merged file: {}", p.rel.display()))?;
                if crate::safecopy::is_link(&meta) || !meta.is_file() || meta.len() != p.size {
                    return Err(format!(
                        "Merged file {} does not match the ISO.",
                        p.rel.display()
                    ));
                }
            }
        }
    }
    if jobs.is_cancelled(job_id) {
        return Err(ERR_CANCELLED.into());
    }
    let kept = kept_files.len();
    progress(100, "Done");
    let catalog = exe_info
        .as_ref()
        .and_then(|e| game_list::find_title_by_id(e.title_id))
        .filter(|n| !n.trim().is_empty());
    let title_hex = exe_info
        .as_ref()
        .map(|e| format!("{:08X}", e.title_id))
        .unwrap_or_default();
    let typed = title_override
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty());
    let mut warning = (exe_info.is_some() && catalog.is_none() && typed.is_none())
        .then(|| god::unknown_title_warning(&title_hex));
    if !renamed.is_empty() {
        // R15 M3: never replace a different file from another disc without saying so.
        let list: Vec<String> = renamed
            .iter()
            .take(5)
            .map(|(a, b)| format!("{} → {}", a.display(), b.display()))
            .collect();
        let more = if renamed.len() > 5 {
            format!(" and {} more", renamed.len() - 5)
        } else {
            String::new()
        };
        let w = format!("{} file(s) on this disc differ from the copy already in the folder. The earlier copy was kept and this disc's was saved next to it: {}{more}.", renamed.len(), list.join(", "));
        warning = Some(match warning {
            Some(x) => format!("{x} {w}"),
            None => w,
        });
    }
    let name = typed.map(|t| clamp_title(&t)).or(catalog);
    Ok(ExtractResult {
        output: dest_dir.display().to_string(),
        title_id: title_hex,
        name,
        files: planned.len() - kept,
        bytes: wrote_bytes,
        disc_number,
        disc_count,
        merged: merge.is_some(),
        kept,
        warning,
    })
}

/// Lay the finished temp tree over `dest` (a folder Unpack made earlier in this run). Existing files
/// are replaced by set-aside-and-rename, never written through; links in the way are refused.
/// Playable executables that were left alone are listed in `kept`.
fn overlay(
    src: &Path,
    dest: &Path,
    rel: &Path,
    m: Merge,
    job_id: &str,
    kept: &mut Vec<PathBuf>,
    renamed: &mut Vec<(PathBuf, PathBuf)>,
) -> Result<(), String> {
    for e in fs::read_dir(src).map_err(|e| format!("Cannot read temp: {e}"))? {
        let e = e.map_err(|e| format!("Cannot read temp: {e}"))?;
        let name = e.file_name();
        let to = dest.join(&name);
        let rel2 = rel.join(&name);
        let from_meta = fs::symlink_metadata(e.path()).map_err(|e| e.to_string())?;
        let to_meta = fs::symlink_metadata(&to).ok();
        if let Some(t) = &to_meta {
            if crate::safecopy::is_link(t) {
                return Err(format!(
                    "{} is a link; refusing to write through it.",
                    rel2.display()
                ));
            }
        }
        if from_meta.is_dir() {
            match &to_meta {
                Some(t) if !t.is_dir() => {
                    return Err(format!(
                        "{} is a file where the disc has a folder.",
                        rel2.display()
                    ))
                }
                Some(_) => {}
                None => fs::create_dir(&to)
                    .map_err(|e| format!("Cannot create {}: {e}", rel2.display()))?,
            }
            overlay(&e.path(), &to, &rel2, m, job_id, kept, renamed)?;
            continue;
        }
        match &to_meta {
            Some(_) if is_playable_exe(&rel2) && !m.replace_exe => {
                kept.push(rel2);
                continue;
            }
            Some(t) if t.is_dir() => {
                return Err(format!(
                    "{} is a folder where the disc has a file.",
                    rel2.display()
                ))
            }
            Some(_) if !is_playable_exe(&rel2) => {
                // Same bytes: nothing to do. Different: keep the earlier disc's file and save this one beside it.
                if same_content(&e.path(), &to) {
                    let _ = fs::remove_file(e.path());
                    continue;
                }
                let name_s = name.to_string_lossy();
                let (stem, ext) = match name_s.rfind('.') {
                    Some(i) if i > 0 => (&name_s[..i], &name_s[i..]),
                    _ => (&name_s[..], ""),
                };
                let mut n = 1;
                let alt = loop {
                    let cand = if n == 1 {
                        format!("{stem} (Disc {}){ext}", m.disc)
                    } else {
                        format!("{stem} (Disc {})-{n}{ext}", m.disc)
                    };
                    if fs::symlink_metadata(dest.join(&cand)).is_err() {
                        break cand;
                    }
                    n += 1;
                };
                move_entry(&e.path(), &dest.join(&alt))
                    .map_err(|err| format!("Could not place {}: {err}", rel2.display()))?;
                renamed.push((rel2.clone(), rel.join(&alt)));
            }
            Some(_) => {
                let old = to.with_file_name(format!(
                    ".unpack-old-{}-{}",
                    crate::archive::safe_segment(job_id),
                    name.to_string_lossy()
                ));
                let _ = fs::remove_file(&old);
                fs::rename(&to, &old)
                    .map_err(|e| format!("Cannot set aside {}: {e}", rel2.display()))?;
                if let Err(err) = move_entry(&e.path(), &to) {
                    let _ = fs::remove_file(&to);
                    let _ = fs::rename(&old, &to);
                    return Err(format!("Could not place {}: {err}", rel2.display()));
                }
                let _ = fs::remove_file(&old);
            }
            None => move_entry(&e.path(), &to)
                .map_err(|err| format!("Could not place {}: {err}", rel2.display()))?,
        }
    }
    Ok(())
}

/// Byte-for-byte equal regular files?
fn same_content(a: &Path, b: &Path) -> bool {
    use std::io::Read;
    let (Ok(ma), Ok(mb)) = (fs::metadata(a), fs::metadata(b)) else {
        return false;
    };
    if ma.len() != mb.len() {
        return false;
    }
    let (Ok(mut fa), Ok(mut fb)) = (fs::File::open(a), fs::File::open(b)) else {
        return false;
    };
    let (mut ba, mut bb) = (vec![0u8; 1 << 16], vec![0u8; 1 << 16]);
    loop {
        let na = match fa.read(&mut ba) {
            Ok(n) => n,
            Err(_) => return false,
        };
        if na == 0 {
            return true;
        }
        if fb.read_exact(&mut bb[..na]).is_err() || ba[..na] != bb[..na] {
            return false;
        }
    }
}

fn move_entry(src: &Path, dst: &Path) -> std::io::Result<()> {
    match fs::rename(src, dst) {
        Ok(()) => Ok(()),
        Err(_) if src.is_dir() => {
            copy_dir_all(src, dst)?;
            fs::remove_dir_all(src)
        }
        Err(_) => {
            if let Some(p) = dst.parent() {
                fs::create_dir_all(p)?;
            }
            fs::copy(src, dst)?;
            fs::remove_file(src)?;
            Ok(())
        }
    }
}

fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for e in fs::read_dir(src)? {
        let e = e?;
        let t = dst.join(e.file_name());
        if e.file_type()?.is_dir() {
            copy_dir_all(&e.path(), &t)?;
        } else {
            fs::copy(e.path(), t)?;
        }
    }
    Ok(())
}

/// Batch / named: brand-new "<Game>" folder (then -2, …); never reuses an existing folder.
///
/// Multi-disc games: when an earlier disc of the same game was extracted into `parent` during this
/// run, this disc is overlaid onto that folder. The playable executable (default.xex / default.xbe)
/// of the lowest-numbered disc wins; a later disc never replaces it. A disc 2 without a default.xex
/// of its own is accepted this way as long as its file name matches an earlier disc.
pub fn extract_named(
    iso_path: &Path,
    parent: &Path,
    title_override: Option<&str>,
    jobs: &Jobs,
    job_id: &str,
    progress: &mut dyn FnMut(u32, &str),
) -> Result<ExtractResult, String> {
    use crate::discs::{self, Kind};
    let info = iso_info(iso_path);
    let hint = discs::disc_hint_from_name(iso_path);
    let (multi, disc, tid, game_name) = match &info {
        Ok(i) => (
            discs::looks_multi_disc(iso_path, i.disc_number, i.disc_count),
            i.disc_number,
            Some(i.title_id.clone()),
            i.name.clone(),
        ),
        // No default.xex: only a "(Disc N)" file name can attach it to a game that is already in `parent`.
        Err(_) => (
            hint.map(|n| n > 1).unwrap_or(false),
            hint.unwrap_or(1),
            None,
            None,
        ),
    };
    if let (Err(e), false) = (&info, multi) {
        return Err(e.clone());
    }
    let keys = discs::keys_for(tid.as_deref(), iso_path);
    let stem = iso_path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let title_hex = tid.clone().unwrap_or_default();
    let base = title_override
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .or(game_name)
        .unwrap_or(if stem.trim().is_empty() {
            title_hex.clone()
        } else {
            stem
        });
    let mut seg = crate::archive::safe_segment(&base.chars().take(80).collect::<String>());
    if seg.is_empty() {
        seg = if title_hex.is_empty() {
            "Game".into()
        } else {
            title_hex
        };
    }
    let make = || {
        crate::archive::claim_dir(parent, &seg)
            .map_err(|e| format!("Cannot create output folder: {e}"))
    };
    let claim = if multi {
        discs::claim(parent, Kind::Xex, &keys, disc, make)?
    } else {
        discs::Claim {
            dir: make()?,
            joined: false,
        }
    };
    if info.is_err() && !claim.joined {
        // Disc 2 with no executable and nothing to join: say why, leave nothing behind.
        discs::release(parent, Kind::Xex, &keys, disc);
        let _ = fs::remove_dir(&claim.dir);
        return Err(info.err().unwrap());
    }
    let merge = claim.joined.then(|| {
        let have = discs::exe_disc(parent, Kind::Xex, &keys);
        Merge {
            replace_exe: have == 0 || disc < have,
            disc,
        }
    });
    let r = extract_inner(
        iso_path,
        &claim.dir,
        title_override,
        jobs,
        job_id,
        progress,
        merge,
    );
    match &r {
        Ok(ok) => {
            if multi && ok.kept == 0 && !ok.title_id.is_empty() {
                // this disc's executable is now in the folder
                discs::set_exe_disc(parent, Kind::Xex, &keys, disc);
            }
        }
        Err(_) => {
            if multi {
                discs::release(parent, Kind::Xex, &keys, disc);
            }
            if !claim.joined {
                let _ = fs::remove_dir_all(&claim.dir);
            }
        }
    }
    r
}

#[derive(Debug, Serialize)]
pub struct ExtractedXex {
    pub output: String,
    pub files: usize,
    pub bytes: u64,
    pub title_id: String,
    pub name: Option<String>,
    pub iso_deleted: bool,
    pub kept_reason: Option<String>,
    pub disc_number: u8,
    pub disc_count: u8,
    pub merged: bool,
    pub warning: Option<String>,
}

/// Same token registry as GOD auto-convert. Mode is chosen by the caller (page setting).
pub fn extract_registered(
    token: &str,
    want_delete: bool,
    jobs: &Jobs,
    job_id: &str,
    progress: &mut dyn FnMut(u32, &str),
) -> Result<ExtractedXex, String> {
    let (path, root, delete_allowed, ident, sealed, stage) = god::take_extracted_token(token)?;
    let changed = "This ISO was replaced or changed after it was extracted (another archive in the batch may have overwritten it), so it was not converted.";
    if god::identity(&path)? != ident {
        return Err(changed.into());
    }
    let out_parent = god::out_parent_for(&root, &stage);
    let r = extract_named(&path, &out_parent, None, jobs, job_id, progress)?;
    if god::identity(&path)? != ident {
        return Err(format!(
            "{changed} Output in {} may not match; delete it.",
            r.output
        ));
    }
    let mut out = ExtractedXex {
        output: r.output.clone(),
        files: r.files,
        bytes: r.bytes,
        title_id: r.title_id.clone(),
        name: r.name.clone(),
        iso_deleted: false,
        kept_reason: None,
        disc_number: r.disc_number,
        disc_count: r.disc_count,
        merged: r.merged,
        warning: r.warning.clone(),
    };
    if stage.is_some() {
        out.kept_reason = Some("the temporary ISO copy on your PC's drive was removed".into());
        return Ok(out);
    }
    if !want_delete {
        return Ok(out);
    }
    let keep = |m: &str, mut o: ExtractedXex| {
        o.kept_reason = Some(m.into());
        Ok(o)
    };
    if !delete_allowed {
        return keep(
            "ISO kept: deleting is off when “If it already exists” is Replace files.",
            out,
        );
    }
    if r.kept > 0 {
        return keep("ISO kept: its default.xex was not copied because the folder already holds another disc's.", out);
    }
    progress(0, "Re-checking extracted folder before deleting the ISO");
    let f = File::open(&path).map_err(|e| format!("Cannot open ISO: {e}"))?;
    let source = iso::IsoReader::read(f).map_err(|e| format!("Not an Xbox ISO ({e})"))?;
    let root_offset = source.volume_descriptor.root_offset;
    let sector_size = source.volume_descriptor.sector_size.max(SECTOR_SIZE);
    let mut planned = Vec::new();
    collect_files(&source.directory_table, Path::new(""), &mut planned);
    verify_content_vs_iso(
        &path,
        root_offset,
        sector_size,
        Path::new(&r.output),
        &planned,
        !r.merged,
    )
    .map_err(|e| format!("{e}. The ISO was kept."))?;
    if jobs.is_cancelled(job_id) {
        return keep("ISO kept: cancelled.", out);
    }
    let inside = match (fs::canonicalize(&path), fs::canonicalize(&root)) {
        (Ok(i), Ok(rt)) => i.starts_with(&rt) && i != rt,
        _ => false,
    };
    if !inside {
        return keep(
            "ISO kept: it is no longer inside the extraction folder.",
            out,
        );
    }
    if god::identity(&path)? != ident {
        return keep("ISO kept: it changed during the check.", out);
    }
    match sealed {
        None => return keep("ISO kept: it wasn't fingerprinted when extracted.", out),
        Some(h) => {
            progress(100, "Verifying the ISO is unchanged");
            if god::full_hash(&path)? != h {
                return keep(
                    "ISO kept: its contents changed after it was extracted.",
                    out,
                );
            }
        }
    }
    match fs::remove_file(&path) {
        Ok(()) => {
            out.iso_deleted = true;
            Ok(out)
        }
        Err(e) => keep(&format!("ISO kept: couldn't delete it ({e})."), out),
    }
}

/// TitleID from an extracted game folder (default.xex / default.xbe).
pub fn folder_title_id(dir: &Path) -> Option<(String, Option<String>)> {
    let xex = dir.join("default.xex");
    if xex.is_file() {
        if let Ok(Some(tid)) = crate::mods::xex_title_id(&xex) {
            let s = format!("{tid:08X}");
            let name = game_list::find_title_by_id(tid);
            return Some((s, name));
        }
    }
    let xbe = dir.join("default.xbe");
    if xbe.is_file() {
        if let Ok(mut f) = File::open(&xbe) {
            if let Ok(h) = iso2god::executable::xbe::XbeHeader::read(&mut f) {
                if let Some(exe) = h.fields.execution_info {
                    let s = format!("{:08X}", exe.title_id);
                    let name = game_list::find_title_by_id(exe.title_id);
                    return Some((s, name));
                }
            }
        }
    }
    None
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::archive::Jobs;
    use std::io::Cursor;

    /// Minimal XEX2 with ExecutionId pointing at title_id 0x41560817.
    pub fn minimal_xex(title_id: u32) -> Vec<u8> {
        let mut b = vec![0u8; 0x80];
        b[0..4].copy_from_slice(b"XEX2");
        // code_offset / cert unused
        b[8..12].copy_from_slice(&0x80u32.to_be_bytes());
        b[16..20].copy_from_slice(&0x80u32.to_be_bytes());
        b[20..24].copy_from_slice(&1u32.to_be_bytes()); // field_count
        b[24..28].copy_from_slice(&0x0004_0006u32.to_be_bytes()); // ExecutionId
        b[28..32].copy_from_slice(&0x40u32.to_be_bytes()); // offset of exec info
                                                           // exec info at 0x40
        let exec = 0x40usize;
        // media, version, base
        for i in 0..3 {
            b[exec + i * 4..exec + i * 4 + 4].copy_from_slice(&0u32.to_be_bytes());
        }
        b[exec + 12..exec + 16].copy_from_slice(&title_id.to_be_bytes());
        b[exec + 16] = 0;
        b[exec + 17] = 0;
        b[exec + 18] = 1;
        b[exec + 19] = 1;
        b
    }

    /// Xsf (root_offset 0) image with default.xex + media/readme.txt
    pub fn synthetic_xdvdfs() -> Vec<u8> {
        let xex = minimal_xex(0x4156_0817);
        let readme = b"hello from iso";
        // Layout: sector 0x20 = volume desc; sector 33 = root table; 34 = xex; 35 = readme; 36 = media dir table
        let root_table_sector = 33u32;
        let xex_sector = 34u32;
        let readme_sector = 35u32;
        let media_sector = 36u32;
        let end = 38usize;
        let mut img = vec![0u8; end * SECTOR_SIZE as usize];

        let desc = (0x20 * SECTOR_SIZE) as usize;
        img[desc..desc + 20].copy_from_slice(b"MICROSOFT*XBOX*MEDIA");
        img[desc + 20..desc + 24].copy_from_slice(&root_table_sector.to_le_bytes());
        img[desc + 24..desc + 28].copy_from_slice(&(SECTOR_SIZE as u32).to_le_bytes());

        // Root table: default.xex + media (dir)
        let table = (root_table_sector as u64 * SECTOR_SIZE) as usize;
        write_entry(
            &mut img,
            table,
            0,
            xex_sector,
            xex.len() as u32,
            0x20,
            b"default.xex",
        );
        let e2 = table + entry_len(b"default.xex");
        write_entry(
            &mut img,
            e2,
            0,
            media_sector,
            SECTOR_SIZE as u32,
            0x10,
            b"media",
        );
        // pad rest of sector with 0xff end marker feel — zeros fine; size 0 terminates? size==0 ends. Fill remaining with left/right 0xffff
        let end_mark = e2 + entry_len(b"media");
        if end_mark + 4 <= table + SECTOR_SIZE as usize {
            img[end_mark..end_mark + 2].copy_from_slice(&0xffffu16.to_le_bytes());
            img[end_mark + 2..end_mark + 4].copy_from_slice(&0xffffu16.to_le_bytes());
        }

        // media dir: readme.txt
        let mt = (media_sector as u64 * SECTOR_SIZE) as usize;
        write_entry(
            &mut img,
            mt,
            0,
            readme_sector,
            readme.len() as u32,
            0x20,
            b"readme.txt",
        );
        let mend = mt + entry_len(b"readme.txt");
        img[mend..mend + 2].copy_from_slice(&0xffffu16.to_le_bytes());
        img[mend + 2..mend + 4].copy_from_slice(&0xffffu16.to_le_bytes());

        let xs = (xex_sector as u64 * SECTOR_SIZE) as usize;
        img[xs..xs + xex.len()].copy_from_slice(&xex);
        let rs = (readme_sector as u64 * SECTOR_SIZE) as usize;
        img[rs..rs + readme.len()].copy_from_slice(readme);
        img
    }

    fn entry_len(name: &[u8]) -> usize {
        let raw = 14 + name.len();
        (raw + 3) & !3
    }

    fn write_entry(
        img: &mut [u8],
        at: usize,
        _subtree: u16,
        sector: u32,
        size: u32,
        attr: u8,
        name: &[u8],
    ) {
        img[at..at + 2].copy_from_slice(&0u16.to_le_bytes());
        img[at + 2..at + 4].copy_from_slice(&0u16.to_le_bytes());
        img[at + 4..at + 8].copy_from_slice(&sector.to_le_bytes());
        img[at + 8..at + 12].copy_from_slice(&size.to_le_bytes());
        img[at + 12] = attr;
        img[at + 13] = name.len() as u8;
        img[at + 14..at + 14 + name.len()].copy_from_slice(name);
    }

    #[test]
    fn synthetic_image_parses_and_lists() {
        let img = synthetic_xdvdfs();
        let iso = iso::IsoReader::read(Cursor::new(img)).unwrap();
        let mut files = Vec::new();
        collect_files(&iso.directory_table, Path::new(""), &mut files);
        assert_eq!(files.len(), 2);
        assert!(files.iter().any(|f| f.rel == Path::new("default.xex")));
        assert!(files.iter().any(|f| f.rel == Path::new("media/readme.txt")));
    }

    #[test]
    fn extract_synthetic_to_folder() {
        let dir = crate::testutil::TmpDir::new("xex-ext");
        let iso_path = dir.join("game.iso");
        fs::write(&iso_path, synthetic_xdvdfs()).unwrap();
        let out = dir.join("out");
        fs::create_dir(&out).unwrap();
        let jobs = Jobs::default();
        jobs.register("t1");
        let r = extract(
            &iso_path,
            &out,
            Some("Halo Test"),
            &jobs,
            "t1",
            &mut |_, _| {},
        )
        .unwrap();
        assert_eq!(r.title_id, "41560817");
        assert_eq!(r.files, 2);
        assert!(out.join("default.xex").is_file());
        assert_eq!(
            fs::read(out.join("media/readme.txt")).unwrap(),
            b"hello from iso"
        );
        let (tid, _) = folder_title_id(&out).unwrap();
        assert_eq!(tid, "41560817");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn extract_named_never_reuses() {
        let dir = crate::testutil::TmpDir::new("xex-named");
        let iso_path = dir.join("game.iso");
        fs::write(&iso_path, synthetic_xdvdfs()).unwrap();
        let jobs = Jobs::default();
        jobs.register("a");
        let r1 =
            extract_named(&iso_path, &dir, Some("My Game"), &jobs, "a", &mut |_, _| {}).unwrap();
        jobs.register("b");
        let r2 =
            extract_named(&iso_path, &dir, Some("My Game"), &jobs, "b", &mut |_, _| {}).unwrap();
        assert_ne!(r1.output, r2.output);
        assert!(Path::new(&r1.output).join("default.xex").is_file());
        assert!(Path::new(&r2.output).join("default.xex").is_file());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn directory_cycle_errors_not_stack_overflow() {
        // Self-referential directory table: sector points at itself as a subdirectory.
        let mut img = vec![0u8; 40 * SECTOR_SIZE as usize];
        let desc = (0x20 * SECTOR_SIZE) as usize;
        img[desc..desc + 20].copy_from_slice(b"MICROSOFT*XBOX*MEDIA");
        let root_sec = 33u32;
        img[desc + 20..desc + 24].copy_from_slice(&root_sec.to_le_bytes());
        img[desc + 24..desc + 28].copy_from_slice(&(SECTOR_SIZE as u32).to_le_bytes());
        let table = (root_sec as u64 * SECTOR_SIZE) as usize;
        // directory entry pointing back to sector 33
        img[table..table + 2].copy_from_slice(&0u16.to_le_bytes());
        img[table + 2..table + 4].copy_from_slice(&0u16.to_le_bytes());
        img[table + 4..table + 8].copy_from_slice(&root_sec.to_le_bytes());
        img[table + 8..table + 12].copy_from_slice(&(SECTOR_SIZE as u32).to_le_bytes());
        img[table + 12] = 0x10; // DIRECTORY
        img[table + 13] = 4;
        img[table + 14..table + 18].copy_from_slice(b"loop");
        let end = table + 20;
        img[end..end + 2].copy_from_slice(&0xffffu16.to_le_bytes());
        img[end + 2..end + 4].copy_from_slice(&0xffffu16.to_le_bytes());
        use std::io::Cursor;
        let err = match iso::IsoReader::read(Cursor::new(img)) {
            Ok(_) => panic!("expected cycle error"),
            Err(e) => e.to_string(),
        };
        assert!(
            err.contains("cycle") || err.contains("deep") || err.contains("nesting"),
            "expected cycle/depth error, got {err}"
        );
    }

    #[test]
    fn truncated_iso_refused() {
        let dir = crate::testutil::TmpDir::new("xex-trunc");
        let mut img = synthetic_xdvdfs();
        img.truncate(img.len() / 2);
        let iso_path = dir.join("bad.iso");
        fs::write(&iso_path, &img).unwrap();
        let out = dir.join("out");
        fs::create_dir(&out).unwrap();
        let jobs = Jobs::default();
        jobs.register("t");
        let e = extract(&iso_path, &out, None, &jobs, "t", &mut |_, _| {}).unwrap_err();
        assert!(
            e.contains("incomplete") || e.contains("truncated") || e.contains("Not an Xbox"),
            "{e}"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    // ---------- multi-disc overlay, unknown TitleID, safer replace ----------

    use crate::god::tests::{make_iso_files, tmp};

    fn xex_disc(dir: &Path, name: &str, media: u32, n: u8, extra: &[(&str, &[u8])]) -> PathBuf {
        let p = dir.join(name);
        let mut files: Vec<(&str, &[u8])> =
            vec![("default.xex", format!("disc{n}").leak().as_bytes())];
        files.extend_from_slice(extra);
        make_iso_files(&p, 0x4D53_07E6, media, (n, 2), &files);
        p
    }

    fn read_xex_marker(folder: &Path) -> String {
        let b = fs::read(folder.join("default.xex")).unwrap();
        String::from_utf8_lossy(&b[b.len() - 5..])
            .trim_end_matches('\0')
            .to_string()
    }

    fn xex_names(d: &Path) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(d)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    #[test]
    fn xex_disc_two_is_overlaid_and_never_replaces_disc_ones_executable() {
        let d = tmp("xex-merge");
        let out = d.join("out");
        fs::create_dir_all(&out).unwrap();
        let one = xex_disc(&d, "Halo (Disc 1).iso", 1, 1, &[("one.bin", b"ONE")]);
        let two = xex_disc(&d, "Halo (Disc 2).iso", 2, 2, &[("two.bin", b"TWO")]);
        let a = extract_named(&one, &out, None, &Jobs::default(), "x1", &mut |_, _| {}).unwrap();
        let b = extract_named(&two, &out, None, &Jobs::default(), "x2", &mut |_, _| {}).unwrap();
        assert!(!a.merged && b.merged, "disc 2 joins disc 1's folder");
        assert_eq!(a.output, b.output);
        assert_eq!(xex_names(&out).len(), 1, "{:?}", xex_names(&out));
        let game = Path::new(&a.output);
        assert_eq!(read_xex_marker(game), "disc1", "disc 1's default.xex stays");
        assert_eq!(fs::read(game.join("one.bin")).unwrap(), b"ONE");
        assert_eq!(fs::read(game.join("two.bin")).unwrap(), b"TWO");
        assert_eq!(b.kept, 1, "disc 2's own default.xex was not copied over");
        assert!(
            xex_names(game).iter().all(|n| !n.starts_with('.')),
            "{:?}",
            xex_names(game)
        );
    }

    #[test]
    fn xex_disc_one_arriving_second_takes_over_the_executable() {
        let d = tmp("xex-merge-rev");
        let out = d.join("out");
        fs::create_dir_all(&out).unwrap();
        let one = xex_disc(&d, "Halo (Disc 1).iso", 1, 1, &[("one.bin", b"ONE")]);
        let two = xex_disc(&d, "Halo (Disc 2).iso", 2, 2, &[("two.bin", b"TWO")]);
        let b = extract_named(&two, &out, None, &Jobs::default(), "y2", &mut |_, _| {}).unwrap();
        assert_eq!(read_xex_marker(Path::new(&b.output)), "disc2");
        let a = extract_named(&one, &out, None, &Jobs::default(), "y1", &mut |_, _| {}).unwrap();
        assert!(a.merged);
        let game = Path::new(&a.output);
        assert_eq!(
            read_xex_marker(game),
            "disc1",
            "the lower disc number owns the executable"
        );
        assert!(game.join("one.bin").is_file() && game.join("two.bin").is_file());
        assert_eq!(xex_names(&out).len(), 1);
    }

    #[test]
    fn xex_disc_without_an_executable_joins_by_name_or_explains() {
        let d = tmp("xex-noexe");
        let out = d.join("out");
        fs::create_dir_all(&out).unwrap();
        let lone = d.join("Zed (Disc 2).iso");
        make_iso_files(&lone, 0, 0, (1, 1), &[("data.bin", b"DATA")]);
        // nothing to join: plain message, nothing created
        let e =
            extract_named(&lone, &out, None, &Jobs::default(), "n0", &mut |_, _| {}).unwrap_err();
        assert!(
            e.contains("Disc 2") && e.contains("Convert Disc 1 first"),
            "{e}"
        );
        assert!(xex_names(&out).is_empty(), "{:?}", xex_names(&out));
        // with disc 1 already extracted from a matching file name it overlays
        let one = d.join("Zed (Disc 1).iso");
        make_iso_files(&one, 0x4D53_07E6, 1, (1, 2), &[("default.xex", b"disc1")]);
        let a = extract_named(&one, &out, None, &Jobs::default(), "n1", &mut |_, _| {}).unwrap();
        let b = extract_named(&lone, &out, None, &Jobs::default(), "n2", &mut |_, _| {}).unwrap();
        assert!(b.merged && a.output == b.output);
        assert_eq!(read_xex_marker(Path::new(&a.output)), "disc1");
        assert_eq!(
            fs::read(Path::new(&a.output).join("data.bin")).unwrap(),
            b"DATA"
        );
    }

    #[test]
    fn xex_merge_never_writes_through_a_link() {
        let d = tmp("xex-links");
        let out = d.join("out");
        fs::create_dir_all(&out).unwrap();
        let one = xex_disc(&d, "Q (Disc 1).iso", 1, 1, &[("a.bin", b"A1")]);
        let two = xex_disc(&d, "Q (Disc 2).iso", 2, 2, &[("a.bin", b"A2")]);
        let a = extract_named(&one, &out, None, &Jobs::default(), "l1", &mut |_, _| {}).unwrap();
        let game = PathBuf::from(&a.output);
        #[cfg(unix)]
        {
            let victim = d.join("victim.txt");
            fs::write(&victim, "victim").unwrap();
            fs::remove_file(game.join("a.bin")).unwrap();
            std::os::unix::fs::symlink(&victim, game.join("a.bin")).unwrap();
            let e = extract_named(&two, &out, None, &Jobs::default(), "l2", &mut |_, _| {})
                .unwrap_err();
            assert!(e.contains("link"), "{e}");
            assert_eq!(fs::read_to_string(&victim).unwrap(), "victim");
        }
        #[cfg(not(unix))]
        let _ = (game, two);
    }

    #[test]
    fn xex_replaced_files_are_swapped_not_overwritten_in_place() {
        let d = tmp("xex-swap");
        let out = d.join("out");
        fs::create_dir_all(&out).unwrap();
        let one = xex_disc(
            &d,
            "Swap (Disc 1).iso",
            1,
            1,
            &[("shared.bin", b"FROM-ONE")],
        );
        let two = xex_disc(
            &d,
            "Swap (Disc 2).iso",
            2,
            2,
            &[("shared.bin", b"FROM-TWO")],
        );
        let a = extract_named(&one, &out, None, &Jobs::default(), "s1", &mut |_, _| {}).unwrap();
        extract_named(&two, &out, None, &Jobs::default(), "s2", &mut |_, _| {}).unwrap();
        let game = Path::new(&a.output);
        // R15 M3: a differing file is never silently replaced; the later disc's copy sits beside it.
        assert_eq!(fs::read(game.join("shared.bin")).unwrap(), b"FROM-ONE");
        assert_eq!(
            fs::read(game.join("shared (Disc 2).bin")).unwrap(),
            b"FROM-TWO"
        );
        assert!(
            xex_names(game)
                .iter()
                .all(|n| !n.starts_with(".unpack-old")),
            "{:?}",
            xex_names(game)
        );
    }

    #[test]
    fn r15_m3_differing_pad_is_kept_and_warned_identical_is_quiet() {
        let d = tmp("xex-pad");
        let out = d.join("out");
        fs::create_dir_all(&out).unwrap();
        let one = xex_disc(
            &d,
            "Pad (Disc 1).iso",
            1,
            1,
            &[("pad.bin", &[1u8; 4096]), ("same.bin", b"SAME")],
        );
        let two = xex_disc(
            &d,
            "Pad (Disc 2).iso",
            2,
            2,
            &[("pad.bin", &[2u8; 8192]), ("same.bin", b"SAME")],
        );
        let a = extract_named(&one, &out, None, &Jobs::default(), "p1", &mut |_, _| {}).unwrap();
        let b = extract_named(&two, &out, None, &Jobs::default(), "p2", &mut |_, _| {}).unwrap();
        assert!(b.merged && a.output == b.output);
        let game = Path::new(&a.output);
        assert_eq!(
            fs::metadata(game.join("pad.bin")).unwrap().len(),
            4096,
            "disc 1's pad.bin kept"
        );
        assert_eq!(
            fs::metadata(game.join("pad (Disc 2).bin")).unwrap().len(),
            8192
        );
        assert!(
            !game.join("same (Disc 2).bin").exists(),
            "identical files are not duplicated"
        );
        let w = b.warning.expect("warning about the differing file");
        assert!(w.contains("pad.bin") && w.contains("differ"), "{w}");
    }

    #[test]
    fn r15_m3_halo3_and_quake4_with_one_name_are_not_merged() {
        let d = tmp("xex-mix");
        let out = d.join("out");
        fs::create_dir_all(&out).unwrap();
        let halo = d.join("Mix (Disc 1).iso");
        make_iso_files(
            &halo,
            0x4D53_07E6,
            11,
            (1, 2),
            &[("default.xex", b"HALO"), ("maps.bin", b"H")],
        );
        let quake = d.join("Mix (Disc 2).iso");
        make_iso_files(
            &quake,
            0x4156_07D2,
            12,
            (2, 2),
            &[("default.xex", b"QUAKE"), ("maps.bin", b"Q")],
        );
        let a = extract_named(&halo, &out, None, &Jobs::default(), "m1", &mut |_, _| {}).unwrap();
        let b = extract_named(&quake, &out, None, &Jobs::default(), "m2", &mut |_, _| {}).unwrap();
        assert!(!b.merged, "different TitleIDs must not merge");
        assert_ne!(a.output, b.output);
        assert_eq!(
            fs::read(Path::new(&a.output).join("maps.bin")).unwrap(),
            b"H"
        );
        assert_eq!(
            fs::read(Path::new(&b.output).join("maps.bin")).unwrap(),
            b"Q"
        );
        assert!(!crate::discs::companions(&halo).unwrap().contains(&quake));
    }

    #[test]
    fn xex_merged_disc_never_deletes_its_iso_when_its_executable_was_kept() {
        let d = tmp("xex-nodelete");
        let ext = d.join("archive");
        fs::create_dir_all(&ext).unwrap();
        let one = xex_disc(&ext, "Del (Disc 1).iso", 1, 1, &[]);
        let two = xex_disc(&ext, "Del (Disc 2).iso", 2, 2, &[("t.bin", b"T")]);
        let reg = |p: &Path| {
            let t = god::register_extracted(&[p.display().to_string()], &ext, true).remove(0);
            god::seal_tokens(std::slice::from_ref(&t));
            t
        };
        extract_registered(&reg(&one), false, &Jobs::default(), "d1", &mut |_, _| {}).unwrap();
        let r =
            extract_registered(&reg(&two), true, &Jobs::default(), "d2", &mut |_, _| {}).unwrap();
        assert!(r.merged && !r.iso_deleted && two.exists());
        assert!(r.kept_reason.unwrap().contains("default.xex"));
    }

    #[test]
    fn xex_unknown_title_extracts_with_a_warning() {
        let d = tmp("xex-unknown");
        let out = d.join("out");
        fs::create_dir_all(&out).unwrap();
        let iso = d.join("Mystery.iso");
        make_iso_files(
            &iso,
            0xDEAD_BEEF,
            9,
            (1, 1),
            &[("default.xex", b"M"), ("x.bin", b"X")],
        );
        let r = extract_named(&iso, &out, None, &Jobs::default(), "k", &mut |_, _| {}).unwrap();
        assert_eq!(r.title_id, "DEADBEEF");
        let w = r.warning.expect("warning for an unknown TitleID");
        assert!(w.contains("DEADBEEF"), "{w}");
        assert!(Path::new(&r.output).ends_with("Mystery"), "{}", r.output);
        assert!(Path::new(&r.output).join("x.bin").is_file());
    }

    #[test]
    fn companions_finds_the_other_discs_next_to_an_iso() {
        let d = tmp("companions");
        let one = xex_disc(&d, "Halo (Disc 1).iso", 1, 1, &[]);
        let two = xex_disc(&d, "Halo (Disc 2).iso", 2, 2, &[]);
        // same TitleID, no marker in the name: found through the header's "disc 2 of 2"
        let renamed = d.join("Halo second.iso");
        make_iso_files(&renamed, 0x4D53_07E6, 3, (2, 2), &[("default.xex", b"x")]);
        let other = d.join("Other Game.iso");
        make_iso_files(&other, 0x4156_0817, 4, (1, 1), &[("default.xex", b"o")]);
        fs::write(d.join("notes.txt"), "x").unwrap();
        let found = crate::discs::companions(&one).unwrap();
        assert_eq!(found, vec![two, renamed]);
        assert!(!found.contains(&other) && !found.contains(&one));
        assert!(crate::discs::companions(&other).unwrap().is_empty());
    }

    #[test]
    fn playable_exe_names_are_matched_case_insensitively() {
        assert!(is_playable_exe(Path::new("Default.XEX")));
        assert!(is_playable_exe(Path::new("sub/default.xbe")));
        assert!(!is_playable_exe(Path::new("default.xex.bak")));
        assert!(!is_playable_exe(Path::new("media/readme.txt")));
    }
}
