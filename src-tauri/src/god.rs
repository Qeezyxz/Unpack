//! Xbox 360 / original Xbox ISO -> Games on Demand (GOD), using the MIT-licensed
//! iso2god-rs library (vendored in vendor/iso2god). This mirrors iso2god-rs's own
//! CLI (src/bin/iso2god.rs upstream) but runs parts sequentially so it can report
//! progress and stop between parts when cancelled.

use std::fs::{self, File};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use iso2god::executable::TitleInfo;
use iso2god::god::{self, ContentType};
use iso2god::{game_list, iso};
use serde::Serialize;

use crate::archive::{Jobs, ERR_CANCELLED};

#[derive(Debug, Serialize, Clone)]
pub struct IsoInfo {
    pub title_id: String,
    pub name: Option<String>,
    pub kind: String,
    pub size: u64,
    /// Size after trimming unused space, i.e. roughly the GOD output size.
    pub data_size: u64,
    pub parts: u64,
}

pub fn iso_info(path: &Path) -> Result<IsoInfo, String> {
    let f = File::open(path).map_err(|e| format!("Cannot open ISO: {e}"))?;
    let size = f.metadata().map(|m| m.len()).unwrap_or(0);
    let mut reader = iso::IsoReader::read(f).map_err(|e| format!("Not an Xbox ISO ({e})"))?;
    let info = TitleInfo::from_image(&mut reader).map_err(|e| format!("Couldn't read the game's executable: {e:#}"))?;
    let data_size = reader.get_max_used_prefix_size();
    check_complete(size, reader.volume_descriptor.root_offset, data_size)?;
    let blocks = data_size.div_ceil(god::BLOCK_SIZE);
    Ok(IsoInfo {
        title_id: format!("{:08X}", info.execution_info.title_id),
        name: game_list::find_title_by_id(info.execution_info.title_id),
        kind: match info.content_type {
            ContentType::GamesOnDemand => "Xbox 360".into(),
            ContentType::XboxOriginal => "Original Xbox".into(),
        },
        size,
        data_size,
        parts: blocks.div_ceil(god::BLOCKS_PER_PART),
    })
}

/// H1: refuse an ISO that is shorter than its own file system says (half-downloaded,
/// cut-off copy). Converting it would produce a GOD that doesn't boot.
pub fn check_complete_pub(file_len: u64, root_offset: u64, data_size: u64) -> Result<(), String> {
    check_complete(file_len, root_offset, data_size)
}

fn check_complete(file_len: u64, root_offset: u64, data_size: u64) -> Result<(), String> {
    let need = root_offset.saturating_add(data_size);
    if file_len < need {
        return Err(format!(
            "The ISO is incomplete or truncated: it is {file_len} bytes but its file system needs at least {need} bytes. Re-download or re-extract it."
        ));
    }
    Ok(())
}

/// The CON header iso2god writes, rebuilt from its inputs (deterministic). Used to write the
/// header and, before deleting an ISO, to check the header on disk byte for byte.
fn build_header(exe_info: &iso2god::executable::TitleExecutionInfo, content_type: ContentType, block_count: u64, part_count: u64, last_part_size: u64, mht: &[u8; 20], title: Option<&str>) -> Vec<u8> {
    let mut header = god::ConHeaderBuilder::new()
        .with_execution_info(exe_info)
        .with_block_counts(block_count as u32, 0)
        .with_data_parts_info(part_count as u32, last_part_size + (part_count - 1) * god::BLOCK_SIZE * 0xa290)
        .with_content_type(content_type)
        .with_mht_hash(mht);
    if let Some(t) = title {
        header = header.with_game_title(t);
    }
    header.finalize().to_vec()
}

/// Free bytes on the drive holding `dir` (None if unknown: the check is then skipped).
#[cfg(windows)]
pub fn free_space(dir: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    extern "system" {
        fn GetDiskFreeSpaceExW(path: *const u16, avail: *mut u64, total: *mut u64, free: *mut u64) -> i32;
    }
    let w: Vec<u16> = dir.as_os_str().encode_wide().chain(Some(0)).collect();
    let (mut a, mut t, mut f) = (0u64, 0u64, 0u64);
    // SAFETY: NUL-terminated wide string and valid out-pointers.
    let ok = unsafe { GetDiskFreeSpaceExW(w.as_ptr(), &mut a, &mut t, &mut f) };
    (ok != 0).then_some(a)
}
#[cfg(not(windows))]
pub fn free_space(dir: &Path) -> Option<u64> {
    let o = std::process::Command::new("df").arg("-Pk").arg(dir).output().ok()?;
    let s = String::from_utf8_lossy(&o.stdout);
    s.lines().nth(1)?.split_whitespace().nth(3)?.parse::<u64>().ok().map(|k| k * 1024)
}

/// Bytes a conversion will write (all parts + header), from the ISO length.
pub fn estimated_output(iso_len: u64, root_offset: u64, part_count: u64) -> u64 {
    let avail = iso_len.saturating_sub(root_offset);
    (0..part_count).map(|i| expected_part_size(avail, i)).sum::<u64>() + 0xb000
}

/// Expected size of GOD part `i`: master hash block + one hash block per sub-part + data.
fn expected_part_size(avail: u64, i: u64) -> u64 {
    let start = i * god::BLOCKS_PER_PART * god::BLOCK_SIZE;
    let d = (god::BLOCKS_PER_PART * god::BLOCK_SIZE).min(avail.saturating_sub(start));
    god::BLOCK_SIZE + d.div_ceil(god::SUBPART_SIZE) * god::BLOCK_SIZE + d
}

/// Re-check a finished package on disk before calling it a success: every DataNNNN part
/// exists with exactly the expected size, each part's master hash list is chained into the
/// previous one, and the header starts with the `LIVE` magic and carries part 0's hash.
pub fn verify_package(layout: &god::FileLayout, part_count: u64, iso_len: u64, root_offset: u64) -> Result<(), String> {
    let bad = |m: String| Err(format!("GOD package check failed: {m}"));
    let avail = iso_len.saturating_sub(root_offset);
    let mut digests = vec![];
    for i in 0..part_count {
        let p = layout.part_file_path(i);
        let len = match fs::metadata(&p) { Ok(m) => m.len(), Err(e) => return bad(format!("Data{i:04} missing ({e})")) };
        let want = expected_part_size(avail, i);
        if len != want {
            return bad(format!("Data{i:04} is {len} bytes, expected {want}"));
        }
        let f = File::open(&p).map_err(|e| e.to_string())?;
        let mht = god::HashList::read(f).map_err(|e| e.to_string())?;
        digests.push((mht.digest(), *mht.bytes()));
    }
    for i in 0..part_count.saturating_sub(1) as usize {
        let next = digests[i + 1].0;
        if !digests[i].1.windows(20).any(|w| w == next) {
            return bad(format!("hash chain broken between Data{i:04} and Data{:04}", i + 1));
        }
    }
    let hdr = fs::read(layout.con_header_file_path()).map_err(|e| format!("GOD package check failed: header missing ({e})"))?;
    if !hdr.starts_with(b"LIVE") {
        return bad("header has no LIVE magic".into());
    }
    if let Some((d0, _)) = digests.first() {
        if !hdr.windows(20).any(|w| w == d0) {
            return bad("header hash doesn't match Data0000".into());
        }
    }
    Ok(())
}

/// The CON header's display-name slot is 0x80 bytes (64 UTF-16 units, NUL-terminated);
/// iso2god writes the string without a length check, so a long title would spill into
/// the neighbouring fields. Keep it to 63 units, without splitting a surrogate pair.
pub fn clamp_title(t: &str) -> String {
    let mut out = String::new();
    let mut units = 0;
    for c in t.chars().filter(|c| !c.is_control()) {
        let n = c.len_utf16();
        if units + n > 63 {
            break;
        }
        units += n;
        out.push(c);
    }
    out
}

#[derive(Debug, Serialize)]
pub struct GodResult {
    /// The <TitleID> folder that should be copied to the console (Content/0000000000000000/).
    pub output: String,
    pub title_id: String,
    pub name: Option<String>,
    pub parts: u64,
}

/// Convert. `progress(percent, label)` is called after each part.
pub fn convert(
    iso_path: &Path,
    dest_dir: &Path,
    title_override: Option<&str>,
    jobs: &Jobs,
    job_id: &str,
    progress: &mut dyn FnMut(u32, &str),
) -> Result<GodResult, String> {
    let ioerr = |what: &str| { let w = what.to_string(); move |e: std::io::Error| format!("{w}: {e}") };
    let f = File::open(iso_path).map_err(ioerr("Cannot open ISO"))?;
    let mut source = iso::IsoReader::read(f).map_err(|e| format!("Not an Xbox ISO ({e})"))?;
    let title = TitleInfo::from_image(&mut source).map_err(|e| format!("Couldn't read the game's executable: {e:#}"))?;
    let exe_info = title.execution_info;
    let content_type = title.content_type;
    let root_offset = source.volume_descriptor.root_offset;

    let data_size = source.get_max_used_prefix_size(); // "trim from end", the iso2god default
    let block_count = data_size.div_ceil(god::BLOCK_SIZE);
    let part_count = block_count.div_ceil(god::BLOCKS_PER_PART);
    if part_count == 0 {
        return Err("The ISO looks empty.".into());
    }
    let iso_len = fs::metadata(iso_path).map_err(ioerr("Cannot read ISO"))?.len();
    check_complete(iso_len, root_offset, data_size)?;

    fs::create_dir_all(dest_dir).map_err(ioerr("Cannot create output folder"))?;
    let need = estimated_output(iso_len, root_offset, part_count);
    if let Some(free) = free_space(dest_dir) {
        if free < need + 64 * 1024 * 1024 {
            return Err(format!(
                "Not enough free space: the GOD copy needs about {} MB but the drive has {} MB free.",
                need / 1_048_576,
                free / 1_048_576
            ));
        }
    }
    // Build the whole package in a private temp folder inside dest_dir (same drive, so the
    // final step is a rename). A cancelled or failed re-convert never touches the previous output.
    let tmp_base = dest_dir.join(format!(".unpack-god-tmp-{}", crate::archive::safe_segment(job_id)));
    if tmp_base.exists() {
        let _ = fs::remove_dir_all(&tmp_base);
    }
    struct TmpGuard(std::path::PathBuf);
    impl Drop for TmpGuard {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    let _tmp_guard = TmpGuard(tmp_base.clone());
    let layout = god::FileLayout::new(&tmp_base, &exe_info, content_type);
    let final_layout = god::FileLayout::new(dest_dir, &exe_info, content_type);
    let data_dir = layout.data_dir_path();
    let title_dir = dest_dir.join(format!("{:08X}", exe_info.title_id));
    fs::create_dir_all(&data_dir).map_err(ioerr("Cannot create output folder"))?;
    let cleanup = || {}; // the temp guard removes everything
    let total_steps = part_count + 1; // parts + hashing/header
    progress(0, "Starting");

    for part_index in 0..part_count {
        if jobs.is_cancelled(job_id) {
            cleanup();
            return Err(ERR_CANCELLED.into());
        }
        let res = (|| -> Result<(), String> {
            let mut vol = File::open(iso_path).map_err(ioerr("Cannot open ISO"))?;
            vol.seek(SeekFrom::Start(root_offset)).map_err(ioerr("Seek failed"))?;
            let part = File::options().write(true).create(true).truncate(true).open(layout.part_file_path(part_index)).map_err(ioerr("Cannot create part file"))?;
            god::write_part(vol, part_index, part).map_err(|e| format!("Error writing part {part_index}: {e:#}"))
        })();
        if let Err(e) = res {
            cleanup();
            return Err(e);
        }
        let pct = (((part_index + 1) * 100) / total_steps) as u32;
        progress(pct, &format!("Part {}/{}", part_index + 1, part_count));
    }
    if jobs.is_cancelled(job_id) {
        cleanup();
        return Err(ERR_CANCELLED.into());
    }

    let finish = (|| -> Result<(), String> {
        progress((part_count * 100 / total_steps) as u32, "Hashing");
        let read_mht = |i: u64| -> Result<god::HashList, String> {
            let mut f = File::open(layout.part_file_path(i)).map_err(|e| e.to_string())?;
            god::HashList::read(&mut f).map_err(|e| e.to_string())
        };
        let mut mht = read_mht(part_count - 1)?;
        for prev in (0..part_count - 1).rev() {
            let mut prev_mht = read_mht(prev)?;
            prev_mht.add_hash(&mht.digest());
            let mut f = File::options().write(true).open(layout.part_file_path(prev)).map_err(|e| e.to_string())?;
            prev_mht.write(&mut f).map_err(|e| e.to_string())?;
            mht = prev_mht;
        }
        let last_part_size = fs::metadata(layout.part_file_path(part_count - 1)).map_err(|e| e.to_string())?.len();
        let title = title_override
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .or_else(|| game_list::find_title_by_id(exe_info.title_id))
            .map(|t| clamp_title(&t));
        let bytes = build_header(&exe_info, content_type, block_count, part_count, last_part_size, &mht.digest(), title.as_deref());
        let mut f = File::options().write(true).create(true).truncate(true).open(layout.con_header_file_path()).map_err(|e| e.to_string())?;
        f.write_all(&bytes).map_err(|e| e.to_string())
    })();
    if let Err(e) = finish {
        cleanup();
        return Err(format!("Error finishing GOD package: {e}"));
    }
    if jobs.is_cancelled(job_id) {
        return Err(ERR_CANCELLED.into());
    }
    // Verify in the temp folder first (never swap a bad package in), and make sure the ISO
    // didn't change while we were reading it.
    if fs::metadata(iso_path).map(|m| m.len()).ok() != Some(iso_len) {
        return Err("The ISO changed while it was being converted. Try again.".into());
    }
    verify_package(&layout, part_count, iso_len, root_offset)?;
    install(&layout, &final_layout, job_id).map_err(|e| format!("Could not move the GOD package into place: {e}"))?;
    verify_package(&final_layout, part_count, iso_len, root_offset)?;
    // L1: a cancel that arrives after the package was placed keeps the package but reports
    // the cancel (so nothing downstream, e.g. delete-ISO, treats it as a plain success).
    if jobs.is_cancelled(job_id) {
        return Err(ERR_CANCELLED.into());
    }
    progress(100, "Done");
    Ok(GodResult {
        output: title_dir.display().to_string(),
        title_id: format!("{:08X}", exe_info.title_id),
        name: title_override.map(|s| clamp_title(s.trim())).filter(|s| !s.is_empty()).or_else(|| game_list::find_title_by_id(exe_info.title_id)),
        parts: part_count,
    })
}

/// Batch auto mode: convert into a brand-new "<Game> (GOD)" folder (then "-2", ...) inside
/// `parent`. The ISO is checked first, so a non-Xbox ISO fails before anything is created.
/// The folder is claimed atomically and never reused; on failure/cancel the (then empty)
/// folder is removed again.
pub fn convert_named(
    iso_path: &Path,
    parent: &Path,
    title_override: Option<&str>,
    jobs: &Jobs,
    job_id: &str,
    progress: &mut dyn FnMut(u32, &str),
) -> Result<GodResult, String> {
    let info = iso_info(iso_path)?;
    let stem = iso_path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let base = title_override
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .or(info.name.clone())
        .unwrap_or(if stem.trim().is_empty() { info.title_id.clone() } else { stem });
    let mut seg = crate::archive::safe_segment(&format!("{} (GOD)", base.chars().take(80).collect::<String>()));
    if seg.is_empty() {
        seg = format!("{} (GOD)", info.title_id);
    }
    let dir = crate::archive::claim_dir(parent, &seg).map_err(|e| format!("Cannot create output folder: {e}"))?;
    let r = convert(iso_path, &dir, title_override, jobs, job_id, progress);
    if r.is_err() {
        let _ = fs::remove_dir(&dir); // only succeeds if empty: never deletes real output
    }
    r
}

/// Identity of an extracted ISO, taken right after extraction: size, modification time and a
/// SHA-1 over three 1 MiB samples (start, middle, end). Re-checked before converting and right
/// before deleting, so a file swapped in by a later merge is never converted or deleted.
#[derive(Clone, Debug, PartialEq)]
pub struct IsoIdentity {
    len: u64,
    mtime: Option<std::time::SystemTime>,
    /// Unix: (device, inode, ctime ns) — ctime changes on any write or set_modified.
    /// Windows: (volume serial, file index, creation time).
    file_id: (u64, u64, i128),
    sample: [u8; 20],
}

#[cfg(unix)]
fn file_id(_p: &Path, m: &fs::Metadata) -> (u64, u64, i128) {
    use std::os::unix::fs::MetadataExt;
    (m.dev(), m.ino(), m.ctime() as i128 * 1_000_000_000 + m.ctime_nsec() as i128)
}
#[cfg(windows)]
fn file_id(p: &Path, m: &fs::Metadata) -> (u64, u64, i128) {
    use std::os::windows::fs::MetadataExt;
    use std::os::windows::io::AsRawHandle;
    #[repr(C)]
    #[derive(Default)]
    struct Info { attrs: u32, c: [u32; 2], a: [u32; 2], w: [u32; 2], serial: u32, size_hi: u32, size_lo: u32, links: u32, idx_hi: u32, idx_lo: u32 }
    #[link(name = "kernel32")]
    extern "system" { fn GetFileInformationByHandle(h: *mut core::ffi::c_void, info: *mut Info) -> i32; }
    let mut info = Info::default();
    let ok = File::open(p).map(|f| unsafe { GetFileInformationByHandle(f.as_raw_handle() as _, &mut info) } != 0).unwrap_or(false);
    let ct = m.creation_time() as i128;
    if ok { (info.serial as u64, ((info.idx_hi as u64) << 32) | info.idx_lo as u64, ct) } else { (0, 0, ct) }
}
#[cfg(not(any(unix, windows)))]
fn file_id(_p: &Path, _m: &fs::Metadata) -> (u64, u64, i128) { (0, 0, 0) }

/// Full SHA-1 of a file (used to seal ISOs when delete-after-convert is on).
pub fn full_hash(p: &Path) -> Result<[u8; 20], String> {
    use sha1::{Digest, Sha1};
    use std::io::Read;
    let mut f = File::open(p).map_err(|e| format!("Cannot open ISO: {e}"))?;
    let mut h = Sha1::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 { break; }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().into())
}

pub fn identity(p: &Path) -> Result<IsoIdentity, String> {
    use sha1::{Digest, Sha1};
    use std::io::Read;
    let meta = fs::symlink_metadata(p).map_err(|e| format!("Cannot read ISO: {e}"))?;
    if !meta.file_type().is_file() {
        return Err("The ISO is no longer a regular file.".into());
    }
    let len = meta.len();
    let mut f = File::open(p).map_err(|e| format!("Cannot open ISO: {e}"))?;
    let mut h = Sha1::new();
    h.update(len.to_le_bytes());
    const S: u64 = 1 << 20;
    let mut buf = vec![0u8; S as usize];
    for off in [0, (len / 2).saturating_sub(S / 2), len.saturating_sub(S)] {
        f.seek(SeekFrom::Start(off)).map_err(|e| e.to_string())?;
        let n = (&mut f).take(S).read(&mut buf).map_err(|e| e.to_string())?;
        h.update(&buf[..n]);
    }
    Ok(IsoIdentity { len, mtime: meta.modified().ok(), file_id: file_id(p, &meta), sample: h.finalize().into() })
}

struct ExtractedIso {
    path: std::path::PathBuf,
    root: std::path::PathBuf,
    delete_allowed: bool,
    ident: IsoIdentity,
    /// Full-content SHA-1 taken right after extraction (`seal_tokens`). Without it the ISO is
    /// never deleted.
    sealed: Option<[u8; 20]>,
    born: std::time::Instant,
}

const TOKEN_TTL: std::time::Duration = std::time::Duration::from_secs(12 * 3600);

fn random_token() -> String {
    use std::hash::{BuildHasher, Hasher};
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let part = |salt: u64| {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(n ^ salt);
        h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
        h.finish()
    };
    format!("iso-{:016x}{:016x}", part(0x9e37_79b9), part(0x7f4a_7c15))
}

/// When delete-after-convert is on, the page asks Rust to seal the ISOs it just extracted:
/// a full-content hash taken now is compared again right before deleting (Overseer R4 M1).
pub fn seal_tokens(tokens: &[String]) {
    for t in tokens {
        let path = registry().lock().unwrap_or_else(|p| p.into_inner()).get(t).map(|e| e.path.clone());
        if let Some(p) = path {
            let h = full_hash(&p).ok();
            if let Some(e) = registry().lock().unwrap_or_else(|p| p.into_inner()).get_mut(t) {
                e.sealed = h;
            }
        }
    }
}

fn registry() -> &'static std::sync::Mutex<std::collections::HashMap<String, ExtractedIso>> {
    static R: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, ExtractedIso>>> = std::sync::OnceLock::new();
    R.get_or_init(Default::default)
}

/// Called by `archive::extract` for the ISOs it just placed. Returns one token per path
/// ("" if the file couldn't be fingerprinted; such an ISO is never auto-converted).
pub fn register_extracted(paths: &[String], root: &Path, delete_allowed: bool) -> Vec<String> {
    let mut reg = registry().lock().unwrap_or_else(|p| p.into_inner());
    reg.retain(|_, e| e.born.elapsed() < TOKEN_TTL);
    paths
        .iter()
        .map(|p| {
            let path = std::path::PathBuf::from(p);
            match identity(&path) {
                Ok(ident) => {
                    let t = random_token();
                    reg.insert(t.clone(), ExtractedIso { path, root: root.to_path_buf(), delete_allowed, ident, sealed: None, born: std::time::Instant::now() });
                    t
                }
                Err(_) => String::new(),
            }
        })
        .collect()
}

#[derive(Debug, Serialize)]
pub struct ExtractedGod {
    pub output: String,
    pub parts: u64,
    pub title_id: String,
    pub name: Option<String>,
    pub iso_deleted: bool,
    /// Why the ISO was kept although the user asked to delete it (None if deleted / not asked).
    pub kept_reason: Option<String>,
}

/// Convert an ISO registered by `register_extracted`. Output goes into a new "<Game> (GOD)"
/// folder next to the extraction folder. Rust alone decides deletion: only if the user wants
/// it, the extraction allowed it (fresh folder, not Replace), the job wasn't cancelled, the
/// file still has its recorded identity, and the GOD package re-hashes cleanly.

/// Consume a one-shot extracted-ISO token (shared by GOD and XEX extract paths).
pub fn take_extracted_token(
    token: &str,
) -> Result<(PathBuf, PathBuf, bool, IsoIdentity, Option<[u8; 20]>), String> {
    let mut reg = registry().lock().unwrap_or_else(|p| p.into_inner());
    let e = reg
        .remove(token)
        .ok_or("Unknown or already used ISO token (it wasn't extracted in this session).")?;
    if e.born.elapsed() >= TOKEN_TTL {
        return Err("This extracted ISO's token expired; extract the archive again.".into());
    }
    Ok((e.path, e.root, e.delete_allowed, e.ident, e.sealed))
}

pub fn convert_registered(
    token: &str,
    want_delete: bool,
    jobs: &Jobs,
    job_id: &str,
    progress: &mut dyn FnMut(u32, &str),
) -> Result<ExtractedGod, String> {
    // Single use: the token is consumed here, whatever happens next.
    let (path, root, delete_allowed, ident, sealed) = {
        let mut reg = registry().lock().unwrap_or_else(|p| p.into_inner());
        let e = reg.remove(token).ok_or("Unknown or already used ISO token (it wasn't extracted in this session).")?;
        if e.born.elapsed() >= TOKEN_TTL {
            return Err("This extracted ISO's token expired; extract the archive again.".into());
        }
        (e.path, e.root, e.delete_allowed, e.ident, e.sealed)
    };
    let changed = "This ISO was replaced or changed after it was extracted (another archive in the batch may have overwritten it), so it was not converted.";
    if identity(&path)? != ident {
        return Err(changed.into());
    }
    let out_parent = root.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new(".")).to_path_buf();
    let r = convert_named(&path, &out_parent, None, jobs, job_id, progress)?;
    if identity(&path)? != ident {
        return Err(format!("{changed} Output in {} may not match; delete it.", r.output));
    }
    let mut out = ExtractedGod { output: r.output.clone(), parts: r.parts, title_id: r.title_id.clone(), name: r.name.clone(), iso_deleted: false, kept_reason: None };
    if !want_delete {
        return Ok(out);
    }
    let keep = |m: &str, mut o: ExtractedGod| { o.kept_reason = Some(m.into()); Ok(o) };
    if !delete_allowed {
        return keep("ISO kept: deleting is off when “If it already exists” is Replace files.", out);
    }
    progress(0, "Verifying GOD data before deleting the ISO");
    if let Err(e) = deep_verify_output(&r, &path, progress) {
        return Err(format!("{e}. The ISO was kept."));
    }
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
    if identity(&path)? != ident {
        return keep("ISO kept: it changed during the check.", out);
    }
    match sealed {
        None => return keep("ISO kept: it wasn't fingerprinted when extracted.", out),
        Some(h) => {
            progress(100, "Verifying the ISO is unchanged");
            if full_hash(&path)? != h {
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

/// Re-hash every data block of the package in `r.output` against its sub-hash lists, and each
/// sub-hash list against the part's master hash list (one extra read of the output).
pub fn deep_verify_output(r: &GodResult, iso_path: &Path, progress: &mut dyn FnMut(u32, &str)) -> Result<(), String> {
    let f = File::open(iso_path).map_err(|e| format!("Cannot open ISO: {e}"))?;
    let iso_len = f.metadata().map_err(|e| e.to_string())?.len();
    let mut src = iso::IsoReader::read(f).map_err(|e| format!("Not an Xbox ISO ({e})"))?;
    let title = TitleInfo::from_image(&mut src).map_err(|e| format!("{e:#}"))?;
    let base = Path::new(&r.output).parent().ok_or("bad output path")?;
    let layout = god::FileLayout::new(base, &title.execution_info, title.content_type);
    let root_offset = src.volume_descriptor.root_offset;
    deep_verify_with(&layout, r.parts, iso_len, root_offset, progress)?;
    // Header, byte for byte against the one iso2god would write for these inputs
    // (title ID, title text, block/part counts, content type, media ID, header SHA-1, …).
    let block_count = src.get_max_used_prefix_size().div_ceil(god::BLOCK_SIZE);
    let last = fs::metadata(layout.part_file_path(r.parts - 1)).map_err(|e| e.to_string())?.len();
    let mht0 = god::HashList::read(File::open(layout.part_file_path(0)).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    let want = build_header(&title.execution_info, title.content_type, block_count, r.parts, last, &mht0.digest(), r.name.as_deref());
    let have = fs::read(layout.con_header_file_path()).map_err(|e| format!("GOD header missing: {e}"))?;
    if have != want {
        let at = have.iter().zip(&want).position(|(a, b)| a != b).unwrap_or(have.len().min(want.len()));
        return Err(format!("GOD header check failed: header differs from the expected one at byte 0x{at:X}"));
    }
    Ok(())
}

pub fn deep_verify(layout: &god::FileLayout, part_count: u64, iso_len: u64, root_offset: u64) -> Result<(), String> {
    deep_verify_with(layout, part_count, iso_len, root_offset, &mut |_, _| {})
}

fn deep_verify_with(layout: &god::FileLayout, part_count: u64, iso_len: u64, root_offset: u64, progress: &mut dyn FnMut(u32, &str)) -> Result<(), String> {
    use sha1::{Digest, Sha1};
    use std::io::Read;
    verify_package(layout, part_count, iso_len, root_offset)?;
    let bad = |m: String| Err(format!("GOD data check failed: {m}"));
    let avail = iso_len.saturating_sub(root_offset);
    let bs = god::BLOCK_SIZE as usize;
    let mut blk = vec![0u8; bs];
    for i in 0..part_count {
        progress(((i * 100) / part_count.max(1)) as u32, &format!("Verifying GOD data, part {}/{part_count}", i + 1));
        let mut f = std::io::BufReader::with_capacity(1 << 20, File::open(layout.part_file_path(i)).map_err(|e| e.to_string())?);
        let mut master = [0u8; 4096];
        f.read_exact(&mut master).map_err(|e| e.to_string())?;
        let mut d = (god::BLOCKS_PER_PART * god::BLOCK_SIZE).min(avail.saturating_sub(i * god::BLOCKS_PER_PART * god::BLOCK_SIZE));
        let mut j = 0usize;
        while d > 0 {
            let mut sub = [0u8; 4096];
            f.read_exact(&mut sub).map_err(|e| e.to_string())?;
            let sd: [u8; 20] = Sha1::digest(sub).into();
            if master[j * 20..j * 20 + 20] != sd {
                return bad(format!("Data{i:04} sub-hash list {j} doesn't match the master hash"));
            }
            let n = d.min(god::SUBPART_SIZE);
            let blocks = (n as usize).div_ceil(bs);
            for k in 0..blocks {
                let len = (n as usize - k * bs).min(bs);
                f.read_exact(&mut blk[..len]).map_err(|e| e.to_string())?;
                let h: [u8; 20] = Sha1::digest(&blk[..len]).into();
                if sub[k * 20..k * 20 + 20] != h {
                    return bad(format!("Data{i:04} block {} is damaged", j * god::BLOCKS_PER_SUBPART as usize + k));
                }
            }
            d -= n;
            j += 1;
        }
    }
    Ok(())
}

/// Swap the finished package in: old `.data` folder is renamed aside first and only deleted
/// once the new one is in place (restored if the move fails).
fn install(tmp: &god::FileLayout, fin: &god::FileLayout, job_id: &str) -> std::io::Result<()> {
    let (new_data, new_hdr) = (tmp.data_dir_path(), tmp.con_header_file_path());
    let (data, hdr) = (fin.data_dir_path(), fin.con_header_file_path());
    if let Some(p) = hdr.parent() {
        fs::create_dir_all(p)?;
    }
    let backup = data.with_file_name(format!(".unpack-god-old-{}", crate::archive::safe_segment(job_id)));
    let _ = fs::remove_dir_all(&backup);
    let had_old = data.exists();
    if had_old {
        fs::rename(&data, &backup)?;
    }
    if let Err(e) = fs::rename(&new_data, &data) {
        if had_old {
            let _ = fs::rename(&backup, &data);
        }
        return Err(e);
    }
    if hdr.exists() {
        fs::remove_file(&hdr)?;
    }
    fs::rename(&new_hdr, &hdr)?;
    if had_old {
        let _ = fs::remove_dir_all(&backup);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
