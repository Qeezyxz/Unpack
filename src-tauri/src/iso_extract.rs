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
        let meta = fs::symlink_metadata(&path).map_err(|_| {
            format!("Missing extracted file: {}", p.rel.display())
        })?;
        if crate::safecopy::is_link(&meta) || !meta.is_file() {
            return Err(format!("{} is not a regular file after extract.", p.rel.display()));
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
        return Err(format!("File count mismatch: wrote {found}, expected {}.", planned.len()));
    }
    let mut walk = 0usize;
    let mut st = vec![root.to_path_buf()];
    while let Some(d) = st.pop() {
        let Ok(rd) = fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let Ok(m) = fs::symlink_metadata(e.path()) else { continue };
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
) -> Result<(), String> {
    verify_tree(root, planned)?;
    let mut iso = File::open(iso_path).map_err(|e| format!("Cannot open ISO: {e}"))?;
    let mut buf_a = vec![0u8; 1 << 16];
    let mut buf_b = vec![0u8; 1 << 16];
    for p in planned {
        let pos = root_offset + (p.sector as u64) * sector_size;
        iso.seek(SeekFrom::Start(pos)).map_err(|e| format!("Seek ISO: {e}"))?;
        let mut file = File::open(root.join(&p.rel)).map_err(|e| e.to_string())?;
        let mut left = p.size;
        while left > 0 {
            let n = (left as usize).min(buf_a.len());
            iso.read_exact(&mut buf_a[..n]).map_err(|e| {
                format!("ISO read failed for {}: {e}", p.rel.display())
            })?;
            file.read_exact(&mut buf_b[..n]).map_err(|e| {
                format!("File read failed for {}: {e}", p.rel.display())
            })?;
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
    let ioerr = |what: &str| {
        let w = what.to_string();
        move |e: std::io::Error| format!("{w}: {e}")
    };
    let f = File::open(iso_path).map_err(ioerr("Cannot open ISO"))?;
    let iso_len = f.metadata().map_err(ioerr("Cannot read ISO"))?.len();
    let mut source = iso::IsoReader::read(f).map_err(|e| format!("Not an Xbox ISO ({e})"))?;
    let title = TitleInfo::from_image(&mut source).map_err(|e| format!("Couldn't read the game's executable: {e:#}"))?;
    let exe_info = title.execution_info;
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
    if let Some(free) = free_space(dest_dir) {
        if free < need + 64 * 1024 * 1024 {
            return Err(format!(
                "Not enough free space: extracting needs about {} MB but the drive has {} MB free.",
                need / 1_048_576,
                free / 1_048_576
            ));
        }
    }

    let tmp = dest_dir.join(format!(".unpack-xex-tmp-{}", crate::archive::safe_segment(job_id)));
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
        src.seek(SeekFrom::Start(pos)).map_err(ioerr("Seek failed"))?;
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
                format!("Read failed for {}: {e} (ISO may be truncated)", p.rel.display())
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
    // Move children into dest_dir (already claimed empty).
    for e in fs::read_dir(&tmp).map_err(ioerr("Cannot read temp"))? {
        let e = e.map_err(ioerr("Cannot read temp"))?;
        let to = dest_dir.join(e.file_name());
        move_entry(&e.path(), &to).map_err(|err| format!("Could not place {}: {err}", e.file_name().to_string_lossy()))?;
    }
    verify_tree(dest_dir, &planned)?;
    if jobs.is_cancelled(job_id) {
        return Err(ERR_CANCELLED.into());
    }
    progress(100, "Done");
    let name = title_override
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .map(|t| clamp_title(&t))
        .or_else(|| game_list::find_title_by_id(exe_info.title_id));
    Ok(ExtractResult {
        output: dest_dir.display().to_string(),
        title_id: format!("{:08X}", exe_info.title_id),
        name,
        files: planned.len(),
        bytes: wrote_bytes,
    })
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

/// Batch / named: brand-new "<Game>" folder (then -2, …). Never reuses an existing folder.
pub fn extract_named(
    iso_path: &Path,
    parent: &Path,
    title_override: Option<&str>,
    jobs: &Jobs,
    job_id: &str,
    progress: &mut dyn FnMut(u32, &str),
) -> Result<ExtractResult, String> {
    let info = iso_info(iso_path)?;
    let stem = iso_path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let base = title_override
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .or(info.name.clone())
        .unwrap_or(if stem.trim().is_empty() {
            info.title_id.clone()
        } else {
            stem
        });
    let mut seg = crate::archive::safe_segment(&base.chars().take(80).collect::<String>());
    if seg.is_empty() {
        seg = info.title_id.clone();
    }
    let dir = crate::archive::claim_dir(parent, &seg).map_err(|e| format!("Cannot create output folder: {e}"))?;
    let r = extract(iso_path, &dir, title_override, jobs, job_id, progress);
    if r.is_err() {
        let _ = fs::remove_dir_all(&dir);
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
}

/// Same token registry as GOD auto-convert. Mode is chosen by the caller (page setting).
pub fn extract_registered(
    token: &str,
    want_delete: bool,
    jobs: &Jobs,
    job_id: &str,
    progress: &mut dyn FnMut(u32, &str),
) -> Result<ExtractedXex, String> {
    let (path, root, delete_allowed, ident, sealed) = god::take_extracted_token(token)?;
    let changed = "This ISO was replaced or changed after it was extracted (another archive in the batch may have overwritten it), so it was not converted.";
    if god::identity(&path)? != ident {
        return Err(changed.into());
    }
    let out_parent = root
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .to_path_buf();
    let r = extract_named(&path, &out_parent, None, jobs, job_id, progress)?;
    if god::identity(&path)? != ident {
        return Err(format!("{changed} Output in {} may not match; delete it.", r.output));
    }
    let mut out = ExtractedXex {
        output: r.output.clone(),
        files: r.files,
        bytes: r.bytes,
        title_id: r.title_id.clone(),
        name: r.name.clone(),
        iso_deleted: false,
        kept_reason: None,
    };
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
    progress(0, "Re-checking extracted folder before deleting the ISO");
    let f = File::open(&path).map_err(|e| format!("Cannot open ISO: {e}"))?;
    let source = iso::IsoReader::read(f).map_err(|e| format!("Not an Xbox ISO ({e})"))?;
    let root_offset = source.volume_descriptor.root_offset;
    let sector_size = source.volume_descriptor.sector_size.max(SECTOR_SIZE);
    let mut planned = Vec::new();
    collect_files(&source.directory_table, Path::new(""), &mut planned);
    verify_content_vs_iso(&path, root_offset, sector_size, Path::new(&r.output), &planned)
        .map_err(|e| format!("{e}. The ISO was kept."))?;
    if jobs.is_cancelled(job_id) {
        return keep("ISO kept: cancelled.", out);
    }
    let inside = match (fs::canonicalize(&path), fs::canonicalize(&root)) {
        (Ok(i), Ok(rt)) => i.starts_with(&rt) && i != rt,
        _ => false,
    };
    if !inside {
        return keep("ISO kept: it is no longer inside the extraction folder.", out);
    }
    if god::identity(&path)? != ident {
        return keep("ISO kept: it changed during the check.", out);
    }
    match sealed {
        None => return keep("ISO kept: it wasn't fingerprinted when extracted.", out),
        Some(h) => {
            progress(100, "Verifying the ISO is unchanged");
            if god::full_hash(&path)? != h {
                return keep("ISO kept: its contents changed after it was extracted.", out);
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
        write_entry(&mut img, table, 0, xex_sector, xex.len() as u32, 0x20, b"default.xex");
        let e2 = table + entry_len(b"default.xex");
        write_entry(&mut img, e2, 0, media_sector, SECTOR_SIZE as u32, 0x10, b"media");
        // pad rest of sector with 0xff end marker feel — zeros fine; size 0 terminates? size==0 ends. Fill remaining with left/right 0xffff
        let end_mark = e2 + entry_len(b"media");
        if end_mark + 4 <= table + SECTOR_SIZE as usize {
            img[end_mark..end_mark + 2].copy_from_slice(&0xffffu16.to_le_bytes());
            img[end_mark + 2..end_mark + 4].copy_from_slice(&0xffffu16.to_le_bytes());
        }

        // media dir: readme.txt
        let mt = (media_sector as u64 * SECTOR_SIZE) as usize;
        write_entry(&mut img, mt, 0, readme_sector, readme.len() as u32, 0x20, b"readme.txt");
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

    fn write_entry(img: &mut [u8], at: usize, _subtree: u16, sector: u32, size: u32, attr: u8, name: &[u8]) {
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
        let r = extract(&iso_path, &out, Some("Halo Test"), &jobs, "t1", &mut |_, _| {}).unwrap();
        assert_eq!(r.title_id, "41560817");
        assert_eq!(r.files, 2);
        assert!(out.join("default.xex").is_file());
        assert_eq!(fs::read(out.join("media/readme.txt")).unwrap(), b"hello from iso");
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
        let r1 = extract_named(&iso_path, &dir, Some("My Game"), &jobs, "a", &mut |_, _| {}).unwrap();
        jobs.register("b");
        let r2 = extract_named(&iso_path, &dir, Some("My Game"), &jobs, "b", &mut |_, _| {}).unwrap();
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
        assert!(e.contains("incomplete") || e.contains("truncated") || e.contains("Not an Xbox"), "{e}");
        let _ = fs::remove_dir_all(&dir);
    }
}
