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
    /// True when `game_size` stopped counting at the file cap (shown as "about").
    #[serde(default)]
    pub size_approx: bool,
}

/// On-disk library index format generation (bump when scan semantics change).
pub const INDEX_CACHE_VERSION: u32 = 3;

/// Hard cap so a mis-pointed library root cannot walk forever.
const SCAN_MAX_FOLDERS: usize = 50_000;

/// An extracted game's size stops counting after this many files (then shown as approximate).
const EXTRACTED_SIZE_MAX_FILES: usize = 20_000;

fn dir_size_bounded(p: &Path, max_entries: usize) -> (u64, usize) {
    let (mut size, mut files) = (0, 0);
    let mut st = vec![p.to_path_buf()];
    while let Some(d) = st.pop() {
        if files >= max_entries {
            break;
        }
        let Ok(rd) = fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            if files >= max_entries {
                break;
            }
            let path = e.path();
            if is_reparse_point(&path) {
                continue;
            }
            let Ok(m) = fs::symlink_metadata(&path) else {
                continue;
            };
            if m.is_dir() {
                st.push(path);
            } else if m.is_file() {
                size += m.len();
                files += 1;
            }
        }
    }
    (size, files)
}

/// GOD `00007000` / `00004000`: headers plus one level inside sibling `*.data` folders only.
fn god_content_size(ct_dir: &Path) -> u64 {
    let Ok(rd) = fs::read_dir(ct_dir) else {
        return 0;
    };
    let mut size = 0u64;
    for e in rd.flatten() {
        let path = e.path();
        if is_reparse_point(&path) {
            continue;
        }
        let Ok(m) = fs::symlink_metadata(&path) else {
            continue;
        };
        if m.is_file() {
            size += m.len();
        } else if m.is_dir() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.ends_with(".data") {
                size += count_files(&path).0;
            }
        }
    }
    size
}

fn god_game_size(tid_dir: &Path) -> u64 {
    god_content_size(&tid_dir.join("00007000")) + god_content_size(&tid_dir.join("00004000"))
}

fn is_reparse_point(path: &Path) -> bool {
    match (fs::symlink_metadata(path), fs::metadata(path)) {
        (Ok(a), Ok(b)) => a.file_type() != b.file_type() || a.file_type().is_symlink(),
        _ => false,
    }
}

/// `fs::canonicalize` without Windows' `\\?\` prefix (so paths still look like `D:\Content`).
pub(crate) fn canonical_root(p: &Path) -> PathBuf {
    let Ok(c) = fs::canonicalize(p) else {
        return p.to_path_buf();
    };
    strip_verbatim(c)
}

fn strip_verbatim(c: PathBuf) -> PathBuf {
    let s = c.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        c
    }
}

fn is_profile_name(name: &str) -> bool {
    name.len() == 16 && name.chars().all(|c| c.is_ascii_hexdigit())
}

fn is_listable_dir(path: &Path) -> bool {
    let Ok(m) = fs::symlink_metadata(path) else {
        return false;
    };
    m.is_dir() && !m.file_type().is_symlink() && !is_reparse_point(path)
}

fn has_game_packages(tid_dir: &Path) -> bool {
    ["00007000", "00004000"]
        .iter()
        .any(|c| is_listable_dir(&tid_dir.join(c)))
}

fn dedupe_key(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ScanProgress {
    #[serde(default)]
    pub id: String,
    pub folders_scanned: usize,
    pub games_found: usize,
    pub message: String,
    pub warnings: Vec<String>,
    pub done: bool,
    pub error: Option<String>,
}

struct ScanCtx {
    out: Vec<Game>,
    seen: std::collections::HashSet<PathBuf>,
    folders_scanned: usize,
    warnings: Vec<String>,
    cancelled: bool,
}

fn emit_scan_progress(
    ctx: &ScanCtx,
    on_progress: &mut Option<&mut dyn FnMut(ScanProgress)>,
    message: &str,
    done: bool,
    error: Option<String>,
) {
    if let Some(cb) = on_progress.as_mut() {
        cb(ScanProgress {
            id: String::new(),
            folders_scanned: ctx.folders_scanned,
            games_found: ctx.out.len(),
            message: message.into(),
            warnings: ctx.warnings.clone(),
            done,
            error,
        });
    }
}

impl ScanCtx {
    fn tick(&self, on_progress: &mut Option<&mut dyn FnMut(ScanProgress)>, message: &str) {
        emit_scan_progress(self, on_progress, message, false, None);
    }

    fn note_warn(&mut self, msg: String) {
        if self.warnings.len() < 32 {
            self.warnings.push(msg);
        }
    }

    fn push_god(
        &mut self,
        tid_dir: &Path,
        tid: &str,
        on_progress: &mut Option<&mut dyn FnMut(ScanProgress)>,
    ) {
        let key = dedupe_key(tid_dir);
        if !self.seen.insert(key) {
            return;
        }
        let tid = tid.to_ascii_uppercase();
        let (dlc_size, dlc_count) = count_files(&tid_dir.join("00000002"));
        let (_, tu_count) = count_files(&tid_dir.join("000B0000"));
        self.out.push(Game {
            name: game_name(tid_dir, &tid),
            title_id: tid,
            path: tid_dir.display().to_string(),
            kind: "god".into(),
            game_size: god_game_size(tid_dir),
            dlc_count,
            dlc_size,
            tu_count,
            size_approx: false,
        });
        self.tick(on_progress, "Found games");
    }

    fn push_extracted(
        &mut self,
        dir: &Path,
        tid: String,
        name: String,
        on_progress: &mut Option<&mut dyn FnMut(ScanProgress)>,
    ) {
        let key = dedupe_key(dir);
        if !self.seen.insert(key) {
            return;
        }
        let (game_size, files) = dir_size_bounded(dir, EXTRACTED_SIZE_MAX_FILES);
        self.out.push(Game {
            name,
            title_id: tid,
            path: dir.display().to_string(),
            kind: "extracted".into(),
            game_size,
            dlc_count: 0,
            dlc_size: 0,
            tu_count: 0,
            size_approx: files >= EXTRACTED_SIZE_MAX_FILES,
        });
        self.tick(on_progress, "Found games");
    }

    fn scan_profile(
        &mut self,
        profile: &Path,
        cancelled: &dyn Fn() -> bool,
        on_progress: &mut Option<&mut dyn FnMut(ScanProgress)>,
    ) -> bool {
        if !is_listable_dir(profile) {
            return true;
        }
        self.folders_scanned += 1;
        if self.folders_scanned.is_multiple_of(8) {
            self.tick(on_progress, "Scanning library folders");
        }
        if self.folders_scanned > SCAN_MAX_FOLDERS {
            self.note_warn(format!(
                "Stopped after scanning {} folders (limit reached).",
                SCAN_MAX_FOLDERS
            ));
            return false;
        }
        let rd = match fs::read_dir(profile) {
            Ok(r) => r,
            Err(e) => {
                self.note_warn(format!("{}: {}", profile.display(), e));
                return true;
            }
        };
        for e in rd.flatten() {
            if cancelled() {
                self.cancelled = true;
                return false;
            }
            let name = e.file_name().to_string_lossy().into_owned();
            let p = e.path();
            if !is_listable_dir(&p) {
                continue;
            }
            if is_tid(&name) && has_game_packages(&p) {
                self.push_god(&p, &name, on_progress);
            } else if let Some((tid, gname)) = crate::iso_extract::folder_title_id(&p) {
                self.push_extracted(&p, tid, gname.unwrap_or(name), on_progress);
            }
        }
        true
    }

    fn scan_unpack_wrappers(
        &mut self,
        lib: &Path,
        cancelled: &dyn Fn() -> bool,
        on_progress: &mut Option<&mut dyn FnMut(ScanProgress)>,
    ) -> bool {
        let rd = match fs::read_dir(lib) {
            Ok(r) => r,
            Err(_) => return true,
        };
        for e in rd.flatten() {
            if cancelled() {
                self.cancelled = true;
                return false;
            }
            let wrap = e.path();
            if !is_listable_dir(&wrap) {
                continue;
            }
            let wname = e.file_name().to_string_lossy().into_owned();
            if is_tid(&wname) || is_profile_name(&wname) || wname.eq_ignore_ascii_case("Content") {
                continue;
            }
            self.folders_scanned += 1;
            if self.folders_scanned > SCAN_MAX_FOLDERS {
                self.note_warn(format!(
                    "Stopped after scanning {} folders (limit reached).",
                    SCAN_MAX_FOLDERS
                ));
                return false;
            }
            let inner = match fs::read_dir(&wrap) {
                Ok(r) => r,
                Err(e) => {
                    self.note_warn(format!("{}: {}", wrap.display(), e));
                    continue;
                }
            };
            for e2 in inner.flatten() {
                if cancelled() {
                    self.cancelled = true;
                    return false;
                }
                let n2 = e2.file_name().to_string_lossy().into_owned();
                let p2 = e2.path();
                if !is_listable_dir(&p2) {
                    continue;
                }
                if is_tid(&n2) && has_game_packages(&p2) {
                    self.push_god(&p2, &n2, on_progress);
                } else if let Some((tid, gname)) = crate::iso_extract::folder_title_id(&p2) {
                    self.push_extracted(&p2, tid, gname.unwrap_or(n2), on_progress);
                }
            }
        }
        true
    }
}

fn push_profile_candidate(profiles: &mut Vec<PathBuf>, p: PathBuf) {
    if is_listable_dir(&p) {
        profiles.push(p);
    }
}

fn profile_dirs_for_lib(lib: &Path) -> Vec<PathBuf> {
    let mut profiles = Vec::new();
    // 1.7.0 also listed TitleID folders straight under the library folder and under
    // <lib>\Games (a plain "GOD games" folder without a profile name); keep doing that.
    push_profile_candidate(&mut profiles, lib.to_path_buf());
    push_profile_candidate(&mut profiles, lib.join("Games"));
    // Every profile folder (16 hex digits), not just the shared 0000000000000000 one,
    // directly under the root and under <root>\Content. One read_dir each, no deeper.
    for parent in [lib.to_path_buf(), lib.join("Content")] {
        push_profile_candidate(&mut profiles, parent.join("0000000000000000"));
        if let Ok(rd) = fs::read_dir(&parent) {
            let mut others: Vec<PathBuf> = rd
                .flatten()
                .filter(|e| is_profile_name(&e.file_name().to_string_lossy()))
                .map(|e| e.path())
                .collect();
            others.sort();
            for p in others {
                push_profile_candidate(&mut profiles, p);
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    profiles
        .into_iter()
        .filter(|p| seen.insert(p.display().to_string().to_ascii_lowercase()))
        .collect()
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
    let mut progress = None;
    scan_with(libraries, &mut progress, &|| false).games
}

/// Scan with optional progress callbacks and cancellation.
pub fn scan_with(
    libraries: &[PathBuf],
    on_progress: &mut Option<&mut dyn FnMut(ScanProgress)>,
    cancelled: &dyn Fn() -> bool,
) -> ScanResult {
    let mut ctx = ScanCtx {
        out: vec![],
        seen: std::collections::HashSet::new(),
        folders_scanned: 0,
        warnings: vec![],
        cancelled: false,
    };
    for lib in libraries {
        if cancelled() {
            ctx.cancelled = true;
            break;
        }
        // The root itself may be a symlink/junction the user picked on purpose: resolve it once,
        // and keep refusing links below it.
        let lib = canonical_root(lib);
        let lib = &lib;
        if let Some(name) = lib.file_name() {
            let n = name.to_string_lossy();
            if is_tid(&n) && has_game_packages(lib) {
                ctx.push_god(lib, &n, on_progress);
                continue;
            }
        }
        for profile in profile_dirs_for_lib(lib) {
            if !ctx.scan_profile(&profile, cancelled, on_progress) {
                break;
            }
        }
        if ctx.cancelled {
            break;
        }
        // Profile folders (incl. the root and <root>\Games) already listed GOD and extracted
        // games in one pass each; wrappers do the same one level down.
        if !ctx.scan_unpack_wrappers(lib, cancelled, on_progress) {
            break;
        }
    }
    ctx.out.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then(a.path.cmp(&b.path))
    });
    let msg = if ctx.cancelled {
        "Scan cancelled".into()
    } else {
        format!(
            "Scanned {} folders, found {} games",
            ctx.folders_scanned,
            ctx.out.len()
        )
    };
    emit_scan_progress(
        &ctx,
        on_progress,
        &msg,
        true,
        if ctx.cancelled {
            Some("cancelled".into())
        } else {
            None
        },
    );
    ScanResult {
        games: ctx.out,
        folders_scanned: ctx.folders_scanned,
        warnings: ctx.warnings,
        cancelled: ctx.cancelled,
    }
}

#[derive(Debug, Clone)]
pub struct ScanResult {
    pub games: Vec<Game>,
    pub folders_scanned: usize,
    pub warnings: Vec<String>,
    pub cancelled: bool,
}

#[derive(Debug, Serialize, Clone, serde::Deserialize)]
pub struct LibraryIndex {
    pub scanned_at: u64,
    pub stale: bool,
    pub games: Vec<Game>,
    /// Library folders this index was built from; a cache for other folders is ignored.
    #[serde(default)]
    pub libraries: Vec<String>,
    /// True when served from the on-disk cache (the UI then refreshes in the background).
    #[serde(default)]
    pub cached: bool,
    #[serde(default)]
    pub cache_version: u32,
    #[serde(default)]
    pub scan_warnings: Vec<String>,
}

fn index_cache_valid(idx: &LibraryIndex, keys: &[String], now: u64, max_age_secs: u64) -> bool {
    if idx.cache_version != INDEX_CACHE_VERSION {
        return false;
    }
    if idx.libraries != keys {
        return false;
    }
    // A scanned_at in the future (clock change or a hand-edited file) forces a rescan.
    if idx.scanned_at > now || now - idx.scanned_at > max_age_secs {
        return false;
    }
    // Every cached game must live under one of the library roots (as typed or resolved).
    let roots: Vec<PathBuf> = keys
        .iter()
        .flat_map(|k| [PathBuf::from(k), canonical_root(Path::new(k))])
        .collect();
    idx.games.iter().all(|g| {
        g.title_id.len() == 8
            && g.title_id.chars().all(|c| c.is_ascii_hexdigit())
            && !g.path.is_empty()
            && roots.iter().any(|r| Path::new(&g.path).starts_with(r))
    })
}

/// Scan libraries, optionally reuse a recent on-disk cache, and warm cover art cache.
pub fn index_cached(
    libraries: &[PathBuf],
    cache_dir: &Path,
    allow_cover_network: bool,
    max_age_secs: u64,
) -> Result<LibraryIndex, String> {
    let mut progress = None;
    index_cached_with_progress(
        libraries,
        cache_dir,
        allow_cover_network,
        max_age_secs,
        &mut progress,
        &|| false,
    )
}

pub fn index_cached_with_progress(
    libraries: &[PathBuf],
    cache_dir: &Path,
    allow_cover_network: bool,
    max_age_secs: u64,
    on_progress: &mut Option<&mut dyn FnMut(ScanProgress)>,
    cancelled: &dyn Fn() -> bool,
) -> Result<LibraryIndex, String> {
    let cache_file = cache_dir.join("index.json");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let keys: Vec<String> = libraries.iter().map(|p| p.display().to_string()).collect();
    if max_age_secs > 0 {
        if let Some(idx) = fs::read(&cache_file)
            .ok()
            .and_then(|b| serde_json::from_slice::<LibraryIndex>(&b).ok())
        {
            if index_cache_valid(&idx, &keys, now, max_age_secs) {
                return Ok(LibraryIndex {
                    stale: false,
                    cached: true,
                    ..idx
                });
            }
        }
    }
    let scan = scan_with(libraries, on_progress, cancelled);
    if scan.cancelled {
        return Err(crate::archive::ERR_CANCELLED.into());
    }
    let games = scan.games;
    let covers = cache_dir.join("covers");
    for g in &games {
        if cancelled() {
            return Err(crate::archive::ERR_CANCELLED.into());
        }
        if crate::covers::cache_local(Path::new(&g.path), &g.title_id, &covers).is_none() {
            crate::covers::fetch_opt_in(&g.title_id, &covers, allow_cover_network);
        }
    }
    let idx = LibraryIndex {
        scanned_at: now,
        stale: false,
        games,
        libraries: keys,
        cached: false,
        cache_version: INDEX_CACHE_VERSION,
        scan_warnings: scan.warnings,
    };
    fs::create_dir_all(cache_dir).map_err(|e| e.to_string())?;
    // Write next to it and rename, so a reader never sees a half-written index.json.
    let tmp = cache_dir.join(format!(
        "index.json.{}-{:?}.tmp",
        std::process::id(),
        std::thread::current().id()
    ));
    if fs::write(&tmp, serde_json::to_vec(&idx).unwrap_or_default()).is_err()
        || fs::rename(&tmp, &cache_file).is_err()
    {
        let _ = fs::remove_file(&tmp);
    }
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
    fn index_cache_is_keyed_on_library_folders() {
        let t = tmp("idxkey");
        let (a, b, cache) = (t.join("a"), t.join("b"), t.join("cache"));
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        let first = index_cached(std::slice::from_ref(&a), &cache, false, 3600).unwrap();
        assert!(!first.cached);
        assert_eq!(first.cache_version, INDEX_CACHE_VERSION);
        assert!(
            index_cached(std::slice::from_ref(&a), &cache, false, 3600)
                .unwrap()
                .cached
        );
        // A different folder list must not reuse the cache.
        assert!(
            !index_cached(std::slice::from_ref(&b), &cache, false, 3600)
                .unwrap()
                .cached
        );
        // max_age 0 always rescans.
        assert!(!index_cached(&[b], &cache, false, 0).unwrap().cached);
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
        let v = scan(std::slice::from_ref(&d));
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

#[cfg(test)]
mod justin_drive_tests {
    use super::*;
    use crate::dlc::tests::make_stfs;

    /// D:\ like Justin's: Content\0000000000000000\5454081A\00007000\<hdr> + <hdr>.data\, plus junk.
    fn justin_drive(n: &str) -> crate::testutil::TmpDir {
        let d = crate::testutil::TmpDir::new(&format!("lib-justin-{n}"));
        let g = d.join("Content/0000000000000000/5454081A/00007000");
        fs::create_dir_all(g.join("0867AD86.data")).unwrap();
        make_stfs(&g.join("0867AD86"), b"LIVE", 0x7000, 0x5454_081A, "Bully", b"");
        for i in 0..3 {
            fs::write(g.join(format!("0867AD86.data/Data000{i}")), vec![0u8; 4096]).unwrap();
        }
        fs::create_dir_all(d.join(".Ignore")).unwrap();
        fs::create_dir_all(d.join("Emulators/RetroArch/cores")).unwrap();
        fs::write(d.join("name.txt"), b"x").unwrap();
        d
    }
    #[test]
    fn finds_game_from_drive_content_and_profile_roots() {
        let d = justin_drive("roots");
        for root in [
            d.to_path_buf(),
            d.join("Content"),
            d.join("Content/0000000000000000"),
            d.join("Content/0000000000000000/5454081A"),
        ] {
            let v = scan(std::slice::from_ref(&root));
            assert_eq!(v.len(), 1, "{root:?}: {v:?}");
            assert_eq!((v[0].title_id.as_str(), v[0].name.as_str()), ("5454081A", "Bully"));
        }
    }

    #[test]
    fn finds_games_in_other_profile_folders() {
        let d = justin_drive("profiles");
        let g = d.join("Content/E00001234ABCDEF0/4D5307E6/00007000");
        fs::create_dir_all(&g).unwrap();
        make_stfs(&g.join("ABCDEF01"), b"LIVE", 0x7000, 0x4D53_07E6, "Halo 3", b"");
        for root in [d.to_path_buf(), d.join("Content")] {
            let v = scan(std::slice::from_ref(&root));
            assert_eq!(v.len(), 2, "{root:?}: {v:?}");
        }
    }

    #[test]
    fn finds_god_games_in_plain_folder_and_games_subfolder() {
        let d = crate::testutil::TmpDir::new("lib-plain");
        for g in [d.join("4D5307E6/00007000"), d.join("Games/5454081A/00007000")] {
            fs::create_dir_all(&g).unwrap();
        }
        let v = scan(std::slice::from_ref(&d.to_path_buf()));
        let mut t: Vec<_> = v.iter().map(|g| g.title_id.as_str()).collect();
        t.sort();
        assert_eq!(t, ["4D5307E6", "5454081A"], "{v:?}");
    }

    #[test]
    fn god_size_ignores_deep_data_tree() {
        let d = justin_drive("deepdata");
        let data = d.join("Content/0000000000000000/5454081A/00007000/0867AD86.data");
        for i in 0..200 {
            let sub = data.join(format!("nested/{i}"));
            fs::create_dir_all(&sub).unwrap();
            fs::write(sub.join("chunk"), vec![0u8; 8192]).unwrap();
        }
        let start = std::time::Instant::now();
        let v = scan(std::slice::from_ref(&d.join("Content")));
        assert_eq!(v.len(), 1);
        assert!(
            start.elapsed() < std::time::Duration::from_secs(3),
            "scan took {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn ignores_corrupt_and_legacy_cache() {
        let t = crate::testutil::TmpDir::new("lib-badcache");
        let cache = t.join("cache");
        fs::create_dir_all(&cache).unwrap();
        fs::write(cache.join("index.json"), b"{not json").unwrap();
        let a = index_cached(std::slice::from_ref(&t.join("x")), &cache, false, 3600).unwrap();
        assert!(!a.cached);
        let legacy = LibraryIndex {
            scanned_at: 9_999_999_999,
            stale: false,
            games: vec![],
            libraries: vec![t.join("x").display().to_string()],
            cached: false,
            cache_version: 0,
            scan_warnings: vec![],
        };
        fs::write(
            cache.join("index.json"),
            serde_json::to_vec(&legacy).unwrap(),
        )
        .unwrap();
        let b = index_cached(std::slice::from_ref(&t.join("x")), &cache, false, 3600).unwrap();
        assert!(!b.cached, "legacy empty cache must not hide a rescan");
    }

    #[cfg(unix)]
    #[test]
    fn scan_finishes_with_symlink_loop() {
        let d = justin_drive("loop");
        let loopdir = d.join("Emulators/loop");
        fs::create_dir_all(&loopdir).unwrap();
        std::os::unix::fs::symlink(&loopdir, loopdir.join("self")).unwrap();
        let start = std::time::Instant::now();
        let v = scan(std::slice::from_ref(&d));
        assert_eq!(v.len(), 1);
        assert!(start.elapsed() < std::time::Duration::from_secs(3));
    }

    #[test]
    fn cancelled_scan_returns_error() {
        let d = justin_drive("cancel");
        let flag = std::sync::atomic::AtomicBool::new(true);
        let err = index_cached_with_progress(
            std::slice::from_ref(&d),
            &d.join("cache"),
            false,
            0,
            &mut None,
            &|| flag.load(std::sync::atomic::Ordering::Relaxed),
        )
        .unwrap_err();
        assert!(err.to_lowercase().contains("cancel"));
    }

    fn xex_game(dir: &Path, tid: u32) {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join("default.xex"), crate::iso_extract::tests::minimal_xex(tid)).unwrap();
    }

    /// R35 M1 (Overseer's tree): extracted games in wrappers and profile folders are listed again.
    #[test]
    fn r35_m1_extracted_games_everywhere_8efb381_found_them() {
        let d = crate::testutil::TmpDir::new("lib-r35-m1");
        for (i, rel) in ["Wrap/G", "Content/0000000000000000/G", "0000000000000000/G", "Games/G", "G"]
            .iter()
            .enumerate()
        {
            xex_game(&d.join(rel), 0x4156_0810 + i as u32);
        }
        fs::create_dir_all(d.join("Wrap/4D5307E6/00007000")).unwrap();
        let v = scan(std::slice::from_ref(&d.to_path_buf()));
        assert_eq!(v.len(), 6, "{v:?}");
        assert_eq!(v.iter().filter(|g| g.kind == "god").count(), 1);
    }

    /// R35 M2: a library root that is itself a symlink still lists what's under it.
    #[cfg(unix)]
    #[test]
    fn r35_m2_symlinked_root_is_followed() {
        let d = crate::testutil::TmpDir::new("lib-r35-m2");
        let real = d.join("real");
        fs::create_dir_all(real.join("4D5307E6/00007000")).unwrap();
        xex_game(&real.join("XexGame"), 0x4156_0817);
        let link = d.join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert_eq!(scan(std::slice::from_ref(&real)).len(), 2);
        assert_eq!(scan(std::slice::from_ref(&link)).len(), 2);
        // and the cache built from the link root is reused (its games resolve under the root)
        let cache = d.join("cache");
        assert!(!index_cached(std::slice::from_ref(&link), &cache, false, 3600).unwrap().cached);
        assert!(index_cached(std::slice::from_ref(&link), &cache, false, 3600).unwrap().cached);
    }

    #[test]
    fn r35_strip_verbatim_prefix() {
        assert_eq!(strip_verbatim(PathBuf::from(r"\\?\D:\Content")), PathBuf::from(r"D:\Content"));
        assert_eq!(strip_verbatim(PathBuf::from(r"\\?\UNC\nas\share")), PathBuf::from(r"\\nas\share"));
        assert_eq!(strip_verbatim(PathBuf::from("/mnt/x")), PathBuf::from("/mnt/x"));
    }

    /// R35 L4: a cache naming a game outside the roots, or scanned in the future, is rescanned.
    #[test]
    fn r35_l4_cache_outside_roots_or_future_is_rejected() {
        let d = justin_drive("r35-l4");
        let cache = d.join("cache");
        let root = d.join("Content");
        let first = index_cached(std::slice::from_ref(&root), &cache, false, 3600).unwrap();
        assert_eq!(first.games.len(), 1);
        let write = |idx: &LibraryIndex| fs::write(cache.join("index.json"), serde_json::to_vec(idx).unwrap()).unwrap();
        let mut bad = first.clone();
        bad.games[0].path = "/etc".into();
        write(&bad);
        let r = index_cached(std::slice::from_ref(&root), &cache, false, 3600).unwrap();
        assert!(!r.cached && r.games[0].path != "/etc");
        let mut future = first.clone();
        future.scanned_at = u64::MAX / 2;
        write(&future);
        assert!(!index_cached(std::slice::from_ref(&root), &cache, false, 3600).unwrap().cached);
        // L3: the atomic write leaves no temp files behind
        let left: Vec<_> = fs::read_dir(&cache).unwrap().flatten().map(|e| e.file_name()).filter(|n| n.to_string_lossy().ends_with(".tmp")).collect();
        assert!(left.is_empty(), "{left:?}");
    }

    /// R35 L6: an extracted game over the file cap is marked approximate.
    #[test]
    fn r35_l6_capped_size_is_marked_approximate() {
        let d = crate::testutil::TmpDir::new("lib-r35-l6");
        let g = d.join("Big");
        xex_game(&g, 0x4156_0817);
        fs::create_dir_all(g.join("f")).unwrap();
        for i in 0..EXTRACTED_SIZE_MAX_FILES {
            fs::write(g.join(format!("f/{i}")), b"").unwrap();
        }
        let small = d.join("Small");
        xex_game(&small, 0x4156_0818);
        let v = scan(std::slice::from_ref(&d.to_path_buf()));
        let big = v.iter().find(|x| x.path.ends_with("Big")).unwrap();
        let sm = v.iter().find(|x| x.path.ends_with("Small")).unwrap();
        assert!(big.size_approx && !sm.size_approx);
    }
}

