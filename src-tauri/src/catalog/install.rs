//! Catalog installs: preflight, download (mirrors, backoff, capped concurrency), staging and
//! verification, moving into place, DashLaunch launch.ini edits, and the install journal.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::ini::{self, Ini};
use super::target::{sha256_file, walk_stage, Target};
use super::{dest_segments, file_name_of, is_archive_name, CFile, Entry, Loaded, Store};

pub const LAUNCH_INI: &str = "launch.ini";

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct JFile {
    /// Path on the target drive, "/"-joined, relative to the drive root.
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct IniChange {
    pub key: String,
    pub value: String,
    pub previous: Option<String>,
    /// Backup written on the target before the edit.
    pub backup: String,
    /// Journaled before the launch.ini write and cleared once it is read back (R15 L2). Still true
    /// means the write's outcome is unknown: uninstall/update look for the line in the live file.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unconfirmed: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Installed {
    pub entry_id: String,
    pub version: String,
    pub name: String,
    pub title_id: String,
    pub target: String,
    pub target_label: String,
    pub files: Vec<JFile>,
    pub backups: Vec<String>,
    pub ini: Vec<IniChange>,
    /// false while installing or after a failure part-way (Uninstall still cleans up).
    pub complete: bool,
    pub installed_at: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Journal {
    pub installs: Vec<Installed>,
}

impl Journal {
    pub fn find(&self, target: &str, id: &str) -> Option<&Installed> {
        self.installs.iter().find(|i| i.target == target && i.entry_id == id)
    }
    fn put(&mut self, rec: Installed) {
        self.installs.retain(|i| !(i.target == rec.target && i.entry_id == rec.entry_id));
        self.installs.push(rec);
    }
    fn remove(&mut self, target: &str, id: &str) {
        self.installs.retain(|i| !(i.target == target && i.entry_id == id));
    }
}

#[derive(Debug, Clone)]
pub struct Tuning {
    pub concurrency: usize,
    /// Wait before each retry of the same URL; its length is the retry count per URL.
    pub backoff: Vec<Duration>,
}

impl Default for Tuning {
    fn default() -> Self {
        Tuning { concurrency: 2, backoff: vec![Duration::from_secs(1), Duration::from_secs(4)] }
    }
}

pub struct Ctx {
    pub store: Store,
    /// Bundled 7-Zip, for archive downloads.
    pub exe: Option<PathBuf>,
    pub tuning: Tuning,
}

/// A backup-name stamp that never repeats in this process: UTC second, milliseconds and a counter
/// (R12 LOW-3). Example: 20261020-215300Z-042-3.
pub fn unique_stamp() -> String {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let d = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    format!("{}-{:03}-{}", stamp(d.as_secs()), d.subsec_millis(), N.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// UTC timestamp for backup names: 20261020-215300Z.
pub fn stamp(secs: u64) -> String {
    let days = (secs / 86400) as i64;
    let (h, m, s) = (secs % 86400 / 3600, secs % 3600 / 60, secs % 60);
    // civil-from-days (Howard Hinnant)
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if mo <= 2 { 1 } else { 0 };
    format!("{y:04}{mo:02}{d:02}-{h:02}{m:02}{s:02}Z")
}

pub(crate) fn key_hash(k: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(k.as_bytes()).iter().take(8).map(|b| format!("{b:02x}")).collect()
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Preflight {
    pub ok: bool,
    /// "new" | "installed" | "update" | "partial"
    pub state: String,
    pub installed_version: Option<String>,
    pub blockers: Vec<String>,
    /// Need the user's OK before installing.
    pub warnings: Vec<String>,
    /// Plain information.
    pub info: Vec<String>,
    pub detected_tus: Vec<u32>,
    pub need: u64,
    pub free: Option<u64>,
}

fn entry_name(l: &Loaded, id: &str) -> String {
    l.entries.iter().find(|e| e.id == id).map(|e| format!("{} ({id})", e.mod_.name)).unwrap_or_else(|| id.to_string())
}

/// TU number from a title update package's display name ("Title Update 18", "TU18").
pub fn tu_number(name: &str) -> Option<u32> {
    let w = name.to_ascii_lowercase();
    let toks: Vec<&str> = w.split(|c: char| !c.is_ascii_alphanumeric()).filter(|s| !s.is_empty()).collect();
    for (i, t) in toks.iter().enumerate() {
        if let Some(n) = t.strip_prefix("tu").and_then(|n| n.parse().ok()) { return Some(n); }
        if *t == "tu" || (*t == "update" && i > 0 && toks[i - 1] == "title") {
            if let Some(n) = toks.get(i + 1).and_then(|n| n.parse().ok()) { return Some(n); }
        }
    }
    None
}

/// TUs installed for `tid` on the target: (known numbers, packages whose number is unclear).
pub fn detect_tus(t: &Target, tid: &str) -> Result<(Vec<u32>, usize), String> {
    let rel: Vec<String> = vec!["Content".into(), "0000000000000000".into(), tid.into(), "000B0000".into()];
    let Some(list) = t.list(&rel)? else { return Ok((vec![], 0)) };
    let (mut known, mut unknown) = (vec![], 0);
    for (name, size, is_dir) in list.into_iter().filter(|e| !e.2).take(20) {
        let _ = (size, is_dir);
        let mut r = rel.clone();
        r.push(name);
        let Some(head) = t.read(&r, crate::dlc::STFS_HEADER_LEN, false)? else { continue };
        match crate::dlc::parse_stfs_bytes(&head) {
            Ok(info) if info.kind == "title_update" && info.title_id == tid => match tu_number(&info.name) {
                Some(n) => { if !known.contains(&n) { known.push(n) } }
                None => unknown += 1,
            },
            _ => {}
        }
    }
    known.sort();
    Ok((known, unknown))
}

fn plugin_file(e: &Entry) -> Option<&CFile> {
    e.files.iter().find(|f| f.install.handler == "plugin" && f.install.dashlaunch.is_some())
}

pub fn preflight(store: &Store, l: &Loaded, e: &Entry, t: &Target) -> Preflight {
    let mut p = Preflight { state: "new".into(), ..Default::default() };
    let j = match store.journal() { Ok(j) => j, Err(e) => { p.blockers.push(e); Journal::default() } };
    let key = t.key();
    if e.paid {
        p.blockers.push("This is a paid mod. Get it from its source page.".into());
    } else if !e.installable() {
        p.blockers.push("This mod can't be downloaded by Unpack (not redistributable). Open its source page instead.".into());
    }
    if let Some(r) = j.find(&key, &e.id) {
        p.installed_version = Some(r.version.clone());
        if !r.complete {
            p.state = "partial".into();
            p.warnings.push("An earlier install of this mod stopped part-way. Installing again replaces it.".into());
        } else if r.version == e.mod_.version {
            p.state = "installed".into();
            p.blockers.push(format!("Version {} is already installed on {}.", r.version, t.label()));
        } else {
            p.state = "update".into();
            p.info.push(format!("Updates version {} to {} (the old files are removed first).", r.version, e.mod_.version));
        }
    }
    let here: Vec<&Installed> = j.installs.iter().filter(|i| i.target == key && i.entry_id != e.id).collect();
    for r in &e.requires {
        if !here.iter().any(|i| &i.entry_id == r && i.complete) {
            p.blockers.push(format!("Needs {} installed on this target first.", entry_name(l, r)));
        }
    }
    for i in &here {
        let theirs = l.entries.iter().find(|x| x.id == i.entry_id).map_or(false, |x| x.conflicts.contains(&e.id));
        if e.conflicts.contains(&i.entry_id) || theirs {
            p.blockers.push(format!("Conflicts with {} (installed). Uninstall it first.", entry_name(l, &i.entry_id)));
        }
    }
    p.need = e.files.iter().map(|f| f.size).sum();
    p.free = t.free();
    match p.free {
        Some(f) if f < p.need.saturating_add(1 << 20) => p.blockers.push(format!("Not enough free space on {}: needs {} MB, {} MB free.", t.label(), p.need / 1_048_576 + 1, f / 1_048_576)),
        Some(_) => {}
        None if t.is_console() => p.info.push("Free space on the console can't be read over FTP; make sure there's room.".into()),
        None => p.warnings.push("Couldn't read the free space of the target.".into()),
    }
    if t.is_fat() && e.files.iter().any(|f| f.size > crate::safecopy::FAT_MAX) {
        p.blockers.push("A file is larger than 4 GB, which FAT32 / the Xbox drive can't store.".into());
    }
    if !e.compat.tu.is_empty() && e.installable() {
        match detect_tus(t, &e.game.title_id) {
            Ok((known, unknown)) => {
                p.detected_tus = known.clone();
                let want = e.compat.tu.iter().map(|n| if *n == 0 { "no TU".to_string() } else { format!("TU{n}") }).collect::<Vec<_>>().join(", ");
                if known.is_empty() && unknown == 0 {
                    if !e.compat.tu.contains(&0) {
                        p.blockers.push(format!("Needs {want}; no title update for {} was found on {}.", e.game.name, t.label()));
                    }
                } else if !known.is_empty() && !known.iter().any(|k| e.compat.tu.contains(k)) && unknown == 0 {
                    p.blockers.push(format!("Needs {want}; {} has TU{}.", t.label(), known.iter().map(|k| k.to_string()).collect::<Vec<_>>().join(", TU")));
                } else if !known.iter().any(|k| e.compat.tu.contains(k)) {
                    p.warnings.push(format!("Needs {want}; couldn't tell which title update is installed."));
                }
            }
            Err(err) => p.warnings.push(format!("Couldn't check the installed title update: {err}")),
        }
    }
    if let Some(d) = &e.compat.dash_min {
        p.info.push(format!("Needs DashLaunch {d} or newer (Unpack can't check the version)."));
    }
    for f in &e.files {
        match (f.install.dest.as_deref(), t.is_console()) {
            (Some("usb"), true) if !t.device().starts_with("Usb") => p.warnings.push("The catalog says this goes on a USB drive, but the target is the hard drive.".into()),
            (Some("hdd"), false) => p.warnings.push("The catalog says this goes on the console's hard drive, but the target is a local drive.".into()),
            _ => {}
        }
    }
    // An interrupted launch.ini swap is repaired before anything looks at launch.ini (R13 MEDIUM-1).
    match repair_ini(store, t) {
        Ok(n) => p.info.extend(n),
        Err(err) => p.blockers.push(err),
    }
    if plugin_file(e).is_some() && e.installable() {
        match ini_name(t).and_then(|n| t.read(&[n], ini::INI_MAX, true)) {
            Ok(Some(b)) => if let Err(err) = Ini::parse(&b) { p.blockers.push(err) },
            Ok(None) => p.warnings.push("No launch.ini on this drive: the plugin is copied but not added to DashLaunch.".into()),
            Err(err) => p.blockers.push(format!("Couldn't read launch.ini: {err}")),
        }
    }
    p.warnings.dedup();
    p.ok = p.blockers.is_empty();
    p
}

// ---- download ----

enum DlMsg { Pct(usize, u32), Done(usize, Result<PathBuf, String>) }

/// Errors that another try of the same URL won't fix. URLs are removed from the text first, so a
/// port or path that happens to contain "404" or "blocked" doesn't count (R14 LOW-4: a random test
/// port 40407 made a 503 look permanent).
pub(crate) fn is_permanent(e: &str) -> bool {
    let t: String = e.split_whitespace().filter(|w| !w.contains("://")).collect::<Vec<_>>().join(" ").to_ascii_lowercase();
    ["sha-256", "sha256", "larger than", "blocked", "not allowed", "web page", "status code 404", "status code 410", "http 404", "http 410"].iter().any(|k| t.contains(k))
}

fn fetch_one(f: &CFile, dir: &Path, tuning: &Tuning, stop: &std::sync::atomic::AtomicBool, tx: &std::sync::mpsc::Sender<DlMsg>, k: usize) -> Result<PathBuf, String> {
    use std::sync::atomic::Ordering::Relaxed;
    let mut errors = vec![];
    for u in std::iter::once(&f.url).chain(f.mirrors.iter()) {
        let host = url::Url::parse(u).ok().and_then(|x| x.host_str().map(String::from)).unwrap_or_default();
        for attempt in 0..=tuning.backoff.len() {
            if stop.load(Relaxed) { return Err(crate::archive::ERR_CANCELLED.into()); }
            let d = dir.join(format!("try{attempt}-{}", errors.len()));
            let opts = crate::download::DownloadOpts { max_bytes: f.size, expected_sha256: Some(f.sha256.clone()) };
            match crate::download::download_with(u, &d, &opts, &mut |p, _| { let _ = tx.send(DlMsg::Pct(k, p)); }, &|| stop.load(Relaxed)) {
                Ok(r) if r.bytes == f.size && r.sha256.eq_ignore_ascii_case(&f.sha256) => return Ok(PathBuf::from(r.path)),
                Ok(r) => { errors.push(format!("{host}: got {} bytes, expected {}", r.bytes, f.size)); let _ = std::fs::remove_dir_all(&d); break; }
                Err(e) if e == crate::archive::ERR_CANCELLED => return Err(e),
                Err(e) => {
                    let _ = std::fs::remove_dir_all(&d);
                    let permanent = is_permanent(&e);
                    errors.push(format!("{host}: {e}"));
                    if permanent { break; }
                    if let Some(w) = tuning.backoff.get(attempt) {
                        let until = std::time::Instant::now() + *w;
                        while std::time::Instant::now() < until {
                            if stop.load(Relaxed) { return Err(crate::archive::ERR_CANCELLED.into()); }
                            std::thread::sleep(Duration::from_millis(50));
                        }
                    }
                }
            }
        }
    }
    Err(format!("Couldn't download {}: {}", file_name_of(&f.url).unwrap_or_default(), errors.join("; ")))
}

/// Download every file of `e` into `dir`, at most `tuning.concurrency` at once.
pub fn download_all(e: &Entry, dir: &Path, tuning: &Tuning, progress: &mut dyn FnMut(u32, &str), cancelled: &dyn Fn() -> bool) -> Result<Vec<PathBuf>, String> {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed};
    let n = e.files.len();
    let stop = AtomicBool::new(false);
    let next = AtomicUsize::new(0);
    let (tx, rx) = std::sync::mpsc::channel::<DlMsg>();
    let mut out: Vec<Option<PathBuf>> = vec![None; n];
    let mut pct = vec![0u32; n];
    let mut first_err: Option<String> = None;
    let total: u64 = e.files.iter().map(|f| f.size).sum::<u64>().max(1);
    std::thread::scope(|sc| {
        for _ in 0..tuning.concurrency.clamp(1, 4).min(n.max(1)) {
            let (tx, stop, next) = (tx.clone(), &stop, &next);
            sc.spawn(move || loop {
                let k = next.fetch_add(1, Relaxed);
                let Some(f) = e.files.get(k) else { break };
                if stop.load(Relaxed) { break; }
                let r = fetch_one(f, &dir.join(format!("f{k}")), tuning, stop, &tx, k);
                if r.is_err() { stop.store(true, Relaxed); }
                let _ = tx.send(DlMsg::Done(k, r));
            });
        }
        drop(tx);
        loop {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(DlMsg::Pct(k, p)) => pct[k] = p.min(100),
                Ok(DlMsg::Done(k, Ok(p))) => { pct[k] = 100; out[k] = Some(p); }
                Ok(DlMsg::Done(_, Err(err))) => { if first_err.is_none() || first_err.as_deref() == Some(crate::archive::ERR_CANCELLED) { first_err = Some(err); } }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
            if cancelled() { stop.store(true, Relaxed); }
            let done: u64 = e.files.iter().zip(&pct).map(|(f, p)| f.size * *p as u64 / 100).sum();
            progress((done * 100 / total) as u32, "Downloading");
        }
    });
    if let Some(e) = first_err { return Err(e); }
    if cancelled() { return Err(crate::archive::ERR_CANCELLED.into()); }
    Ok(out.into_iter().map(|p| p.unwrap()).collect())
}

// ---- staging ----

/// One staged file set: the folder holding the files and where they go on the target.
pub struct Staged {
    pub dir: PathBuf,
    pub dest: Vec<String>,
    /// (relative segments inside dir, size, sha256)
    pub files: Vec<(Vec<String>, u64, String)>,
}

fn is_xex(p: &Path) -> bool {
    let mut b = [0u8; 4];
    crate::ftp::open_no_follow(p).ok().map_or(false, |mut f| std::io::Read::read_exact(&mut f, &mut b).is_ok() && &b == b"XEX2")
}

/// Largest unpacked total allowed for one catalog file: 4× its declared size (at least 256 MB),
/// never more than 20 GB (R12 LOW-1).
pub const UNPACK_ABS_MAX: u64 = 20 << 30;
pub const STAGE_RESERVE: u64 = 64 << 20;
/// Two sizes for one message; exact bytes when they'd round to the same text (R13 LOW-3).
#[cfg(test)]
pub fn two_sizes_for_test(a: u64, b: u64) -> (String, String) { two_sizes(a, b) }
fn two_sizes(a: u64, b: u64) -> (String, String) {
    let (x, y) = (crate::download::human_size(a), crate::download::human_size(b));
    if x == y { (format!("{a} bytes"), format!("{b} bytes")) } else { (x, y) }
}

pub fn unpack_cap(declared: u64) -> u64 {
    declared.saturating_mul(4).max(256 << 20).min(UNPACK_ABS_MAX)
}

pub fn stage(ctx: &Ctx, e: &Entry, downloads: &[PathBuf], out: &Path, progress: &mut dyn FnMut(u32, &str)) -> Result<Vec<Staged>, String> {
    let mut v = vec![];
    for (k, (f, dl)) in e.files.iter().zip(downloads).enumerate() {
        let dir = out.join(format!("s{k}"));
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        // Re-verify what's on disk now (it sat in staging while others downloaded).
        let (sha, size) = sha256_file(dl)?;
        if sha != f.sha256 || size != f.size {
            return Err(format!("{} changed after download (hash check failed).", file_name_of(&f.url)?));
        }
        let name = file_name_of(&f.url)?;
        let cap = unpack_cap(f.size);
        if is_archive_name(&name) {
            let exe = ctx.exe.as_deref().ok_or("7-Zip isn't available to unpack the download.")?;
            // Zip-bomb guard (R12 LOW-1): the listed total must fit the cap and the staging drive.
            let listed: u64 = crate::archive::list(exe, dl, None).map_err(|err| format!("Couldn't read {name}: {err}"))?.entries.iter().map(|x| x.size).fold(0u64, |a, b| a.saturating_add(b));
            if listed > cap {
                let (a, b) = two_sizes(listed, cap);
                return Err(format!("{name} would unpack to {a}, more than the {b} allowed for a {} download. Not unpacked.", crate::download::human_size(f.size)));
            }
            if let Some(free) = crate::god::free_space(&dir) {
                if free < listed.saturating_add(STAGE_RESERVE) {
                    return Err(format!("Not enough free space to unpack {name} in Unpack's staging folder ({} needed, {} free).", crate::download::human_size(listed.saturating_add(STAGE_RESERVE)), crate::download::human_size(free)));
                }
            }
            let opts = crate::archive::ExtractOptions { archive: dl, dest_parent: &dir, password: None, collision: crate::archive::Collision::Number, layout: crate::archive::Layout::Here, skip_junk: true, nested: false };
            crate::archive::extract(exe, &opts, &crate::archive::Jobs::default(), &format!("catalog-{k}"), progress).map_err(|err| format!("Couldn't unpack {name}: {err}"))?;
        } else {
            std::fs::rename(dl, dir.join(&name)).map_err(|e| e.to_string())?;
        }
        // Links, special files, escapes, bad names: refused here (zip-slip guard).
        let files = walk_stage(&dir)?;
        if files.is_empty() { return Err(format!("{name} contained no files.")); }
        let mut list = vec![];
        let mut total = 0u64;
        for (p, rel) in &files {
            let (h, n) = sha256_file(p)?;
            total = total.saturating_add(n);
            list.push((rel.clone(), n, h));
        }
        // The listing can understate; the real unpacked total is checked too.
        if total > cap {
            let (a, b) = two_sizes(total, cap);
            return Err(format!("{name} unpacked to {a}, more than the {b} allowed. Nothing was installed."));
        }
        check_handler(e, f, &dir, &files)?;
        let dest = dest_segments(&f.install.handler, f.install.dest_path.as_deref(), &e.game.title_id, &e.mod_.name)?;
        v.push(Staged { dir, dest, files: list });
    }
    Ok(v)
}

fn check_handler(e: &Entry, f: &CFile, dir: &Path, files: &[(PathBuf, Vec<String>)]) -> Result<(), String> {
    let tid = &e.game.title_id;
    match f.install.handler.as_str() {
        "xex" => if !files.iter().any(|(p, _)| is_xex(p)) { return Err("The game/mod download has no .xex file.".into()); },
        "plugin" => {
            let p = plugin_rel(f, files)?;
            if !is_xex(&dir.join(p.iter().collect::<PathBuf>())) { return Err("The plugin isn't an Xbox executable (XEX2).".into()); }
        }
        "tu" | "dlc" => {
            let want = if f.install.handler == "tu" { "title_update" } else { "dlc" };
            for (p, rel) in files {
                let info = crate::dlc::parse_stfs(p).map_err(|m| format!("{}: {m}", rel.join("/")))?;
                if info.kind != want || &info.title_id != tid {
                    return Err(format!("{} is a {} package for {}, not a {} for {tid}.", rel.join("/"), info.kind.replace('_', " "), info.title_id, want.replace('_', " ")));
                }
            }
        }
        _ => {}
    }
    Ok(())
}

/// The plugin file inside the staged set: dashlaunch.file, or the single staged file.
fn plugin_rel(f: &CFile, files: &[(PathBuf, Vec<String>)]) -> Result<Vec<String>, String> {
    if let Some(p) = f.install.dashlaunch.as_ref().and_then(|d| d.file.as_deref()) {
        let segs = crate::ftp::split_remote(&p.replace('\\', "/"))?;
        if files.iter().any(|(_, r)| r.iter().map(|s| s.to_ascii_lowercase()).eq(segs.iter().map(|s| s.to_ascii_lowercase()))) { return Ok(segs); }
        return Err(format!("The plugin file {p} isn't in the download."));
    }
    match files {
        [(_, r)] => Ok(r.clone()),
        _ => Err("The plugin download has several files; the catalog must name the plugin (dashlaunch.file).".into()),
    }
}

// ---- install / uninstall ----

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct InstallOpts {
    pub accept_warnings: bool,
    /// plugin slot the user agreed to overwrite
    pub confirm_slot: Option<u8>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Report {
    pub files: usize,
    pub bytes: u64,
    pub ini: Option<String>,
    pub notes: Vec<String>,
    pub updated_from: Option<String>,
}

fn ini_value(t: &Target, dest: &[String], rel: &[String]) -> String {
    format!("{}\\{}", t.device(), dest.iter().chain(rel).cloned().collect::<Vec<_>>().join("\\"))
}

/// The name launch.ini actually has on this drive (the Xbox file system and FAT ignore case, so
/// `LAUNCH.INI` is the same file; R14 MEDIUM-1). Defaults to `launch.ini` when there is none.
pub fn ini_name(t: &Target) -> Result<String, String> {
    Ok(t.list(&[])?.unwrap_or_default().into_iter().find(|(n, _, d)| !d && n.eq_ignore_ascii_case(LAUNCH_INI)).map(|(n, _, _)| n).unwrap_or_else(|| LAUNCH_INI.into()))
}

/// Why an `.old` copy may not be put back automatically (R14 LOW-1): it must be non-empty, at most
/// 64 KiB, UTF-8 text that parses as an ini, and — when Unpack holds backups for this target — byte-
/// identical to one of them.
fn old_problem(b: &[u8], backups: &[(String, Vec<u8>)]) -> Option<&'static str> {
    if b.is_empty() { return Some("is empty"); }
    if b.len() > ini::INI_MAX { return Some("is larger than 64 KB"); }
    if Ini::parse(b).is_err() { return Some("isn't a readable launch.ini (not plain text)"); }
    if !backups.is_empty() && !backups.iter().any(|(_, x)| x == b) { return Some("doesn't match any launch.ini Unpack backed up for this drive"); }
    None
}

/// Run before every launch.ini read/write and in preflight (R13 MEDIUM-1, LOW-2; R14):
/// - names match case-insensitively (`LAUNCH.INI`, `Launch.ini.OLD-2` …);
/// - launch.ini missing but an `.old` / `.old-N` present → the newest copy that passes `old_problem`
///   is put back (one identical to the latest app-side backup preferred); if none passes, nothing is
///   touched and the error names the file and the reason;
/// - stray `.new.part` / `.unpack-new` removed;
/// - with launch.ini present and parseable, `.old*` copies byte-identical to one of Unpack's backups
///   for this target (or to the current launch.ini) are deleted; anything else is left alone.
/// Holds the launch.ini lock so it can't interleave with a local `replace`.
pub fn repair_ini(store: &Store, t: &Target) -> Result<Vec<String>, String> {
    let _g = super::target::ini_lock();
    let mut notes = vec![];
    let Some(list) = t.list(&[])? else { return Ok(notes) };
    let names: Vec<String> = list.iter().filter(|(_, _, d)| !d).map(|(n, _, _)| n.clone()).collect();
    let present = names.iter().find(|x| x.eq_ignore_ascii_case(LAUNCH_INI)).cloned();
    let lower = |n: &str| n.to_ascii_lowercase();
    let strays = [lower(&crate::ftp::part_name(&format!("{LAUNCH_INI}.new"))), lower(&format!("{LAUNCH_INI}.unpack-new"))];
    for n in names.iter().filter(|n| strays.contains(&lower(n))) {
        if let Some(b) = t.read(&[n.clone()], 1 << 20, true).ok().flatten() {
            let _ = t.delete_if(&[n.clone()], b.len() as u64, &super::target::sha256_bytes(&b));
        }
    }
    let mut olds: Vec<String> = names.iter().filter(|n| super::target::is_old_name(n, LAUNCH_INI)).cloned().collect();
    // Recency: .old < .old-2 < … < .old-99 < time-based .old-<unix seconds>.
    olds.sort_by_key(|n| n.rsplit('-').next().and_then(|d| d.parse::<u64>().ok()).unwrap_or(1));
    if olds.is_empty() { return Ok(notes); }
    let kh = key_hash(&t.key());
    let mut backups: Vec<(String, Vec<u8>)> = std::fs::read_dir(store.dir.join("backups")).map(|rd| rd.flatten().filter_map(|e| {
        let n = e.file_name().to_string_lossy().into_owned();
        if !n.starts_with(&format!("{kh}-")) { return None; }
        std::fs::read(e.path()).ok().map(|b| (n, b))
    }).collect()).unwrap_or_default();
    backups.sort_by(|a, b| a.0.cmp(&b.0));
    let read_old = |o: &str| -> Result<Vec<u8>, &'static str> {
        match t.stat(&[o.to_string()]) {
            Ok(Some(n)) if n > ini::INI_MAX as u64 => return Err("is larger than 64 KB"),
            Ok(Some(_)) => {}
            _ => return Err("can't be read"),
        }
        t.read(&[o.to_string()], ini::INI_MAX, true).ok().flatten().ok_or("can't be read")
    };
    let ini_now = match &present {
        Some(p) => p.clone(),
        None => {
            let latest = backups.last().map(|b| b.1.clone());
            let mut checked: Vec<(String, Result<Vec<u8>, &'static str>)> = olds.iter().rev().map(|o| (o.clone(), read_old(o))).collect();
            for c in checked.iter_mut() { if let Ok(b) = &c.1 { if let Some(p) = old_problem(b, &backups) { c.1 = Err(p); } } }
            let pick = checked.iter().find(|(_, r)| matches!(r, Ok(b) if Some(b) == latest.as_ref())).or_else(|| checked.iter().find(|(_, r)| r.is_ok())).map(|(o, _)| o.clone());
            let Some(o) = pick else {
                let why = checked.iter().map(|(o, r)| format!("{o} {}", r.as_ref().err().copied().unwrap_or("?"))).collect::<Vec<_>>().join("; ");
                return Err(format!("launch.ini is missing on this drive. Unpack found {why}, so it won't put that back automatically. Check the file, rename the right one to launch.ini yourself, then try again."));
            };
            t.rename_new(&[o.clone()], &[LAUNCH_INI.into()]).map_err(|e| format!("launch.ini is missing on this drive and Unpack couldn't put back your previous one from {o} ({}). Rename {o} to launch.ini yourself, then try again.", e.trim_end_matches('.')))?;
            notes.push(format!("launch.ini was missing; Unpack put back your previous one from {o}."));
            olds.retain(|x| x != &o);
            LAUNCH_INI.to_string()
        }
    };
    // Prune Unpack's own leftovers once launch.ini is present and parses.
    if let Some(b) = t.read(&[ini_now], ini::INI_MAX, true).ok().flatten() {
        if Ini::parse(&b).is_ok() {
            for o in &olds {
                if let Ok(ob) = read_old(o) {
                    if backups.iter().any(|(_, bb)| *bb == ob) || ob == b {
                        let _ = t.delete_if(&[o.clone()], ob.len() as u64, &super::target::sha256_bytes(&ob));
                    }
                }
            }
        }
    }
    Ok(notes)
}

/// Write `after` over launch.ini after a backup, validated, verified, last-known-good kept.
fn write_ini(ctx: &Ctx, t: &Target, before_bytes: &[u8], before: &Ini, after: &Ini, key: &str, value: Option<&str>) -> Result<String, String> {
    let out = after.serialize();
    ini::validate_change(before, &out, key, value)?;
    // Target name stays within FATX's 42 characters (uniqueness by -2, -3… suffix there);
    // the app-side copy uses a millisecond + counter stamp and is created, never replaced.
    let ts = stamp(now());
    let mut backup = format!("launch.ini.unpack-{ts}.bak");
    let mut n = 1;
    while t.stat(&[backup.clone()])?.is_some() { n += 1; backup = format!("launch.ini.unpack-{ts}-{n}.bak"); }
    t.write_new(&[backup.clone()], before_bytes)?;
    let kh = key_hash(&t.key());
    ctx.store.write_new_file(&format!("backups/{kh}-{}-launch.ini", unique_stamp()), before_bytes)?;
    // `replace` never leaves the target without launch.ini and restores the original itself on error.
    let name = ini_name(t)?;
    t.replace(&[name.clone()], &out).map_err(|e| format!("Writing launch.ini failed: {} (backup: {backup}).", e.trim_end_matches('.')))?;
    let back = t.read(&[name.clone()], ini::INI_MAX, true)?;
    if back.as_deref() != Some(&out[..]) {
        let _ = t.replace(&[name.clone()], before_bytes);
        return Err(format!("launch.ini didn't read back as written; the original was put back (backup: {backup})."));
    }
    ctx.store.write_atomic(&format!("lkg/{kh}-launch.ini"), &out)?;
    Ok(backup)
}

pub fn install(ctx: &Ctx, l: &Loaded, id: &str, t: &Target, opts: &InstallOpts, progress: &mut dyn FnMut(u32, &str), cancelled: &dyn Fn() -> bool) -> Result<Report, String> {
    let e = l.entries.iter().find(|e| e.id == id).ok_or("That entry isn't in the catalog.")?;
    let pf = preflight(&ctx.store, l, e, t);
    if !pf.blockers.is_empty() {
        return Err(pf.blockers.join(" "));
    }
    if !pf.warnings.is_empty() && !opts.accept_warnings {
        return Err(format!("CONFIRM_WARNINGS:{}", pf.warnings.join(" ")));
    }
    // DashLaunch slot is decided (and the user asked) before anything is downloaded.
    let pfile = plugin_file(e);
    let mut early_notes: Vec<String> = pf.info.iter().filter(|i| i.starts_with("launch.ini was missing")).cloned().collect();
    let ini_now = match pfile { Some(_) => t.read(&[ini_name(t)?], ini::INI_MAX, true)?, None => None };
    if let (Some(f), Some(b)) = (pfile, &ini_now) {
        let ini = Ini::parse(b)?;
        let dest = dest_segments("plugin", f.install.dest_path.as_deref(), &e.game.title_id, &e.mod_.name)?;
        // For a single-file plugin the name is known now; archives are checked again after staging.
        let rel = vec![file_name_of(&f.url)?];
        let mine = if is_archive_name(&rel[0]) { f.install.dashlaunch.as_ref().and_then(|d| d.file.clone()).map(|p| ini_value(t, &dest, &crate::ftp::split_remote(&p.replace('\\', "/")).unwrap_or_default())) } else { Some(ini_value(t, &dest, &rel)) };
        let old = ctx.store.journal()?.find(&t.key(), &e.id).map(|r| r.ini.iter().map(|c| c.value.clone()).collect::<Vec<_>>()).unwrap_or_default();
        // An update reuses its own slot.
        let own = ini.plugin_slots().iter().position(|v| v.as_ref().map_or(false, |v| old.iter().any(|o| o.eq_ignore_ascii_case(v)))).map(|n| n as u8 + 1);
        if own.is_none() {
            ini::choose_slot(&ini, mine.as_deref().unwrap_or(""), f.install.dashlaunch.as_ref().and_then(|d| d.slot), opts.confirm_slot)?;
        }
    }
    let stage_root = ctx.store.stage_dir(&e.id)?;
    struct Cleanup(PathBuf);
    impl Drop for Cleanup { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }
    let _cleanup = Cleanup(stage_root.clone());
    let need: u64 = e.files.iter().map(|f| f.size).fold(0u64, |a, b| a.saturating_add(b)).saturating_add(STAGE_RESERVE);
    if let Some(free) = crate::god::free_space(&stage_root) {
        if free < need { return Err(format!("Not enough free space for Unpack's staging folder: {} needed, {} free.", crate::download::human_size(need), crate::download::human_size(free))); }
    }
    let dls = download_all(e, &stage_root.join("dl"), &ctx.tuning, &mut |p, _| progress(p * 60 / 100, "Downloading"), cancelled)?;
    progress(60, "Checking files");
    let staged = stage(ctx, e, &dls, &stage_root.join("out"), &mut |_, _| {})?;
    let bytes: u64 = staged.iter().flat_map(|s| s.files.iter().map(|f| f.1)).sum();
    if t.is_fat() && staged.iter().flat_map(|s| &s.files).any(|f| f.1 > crate::safecopy::FAT_MAX) {
        return Err("A file in the download is larger than 4 GB, which FAT32 / the Xbox drive can't store.".into());
    }
    if let Some(free) = t.free() {
        if free < bytes.saturating_add(1 << 20) { return Err(format!("Not enough free space: needs {} MB.", bytes / 1_048_576 + 1)); }
    }
    let mut report = Report { bytes, ..Default::default() };
    // Update: remove the old version's files (and its launch.ini line) first.
    if pf.state == "update" || pf.state == "partial" {
        let u = uninstall(ctx, None, id, t, true)?;
        report.updated_from = pf.installed_version.clone();
        if !u.kept.is_empty() { report.notes.push(format!("Kept {} old file(s) that were changed since: {}", u.kept.len(), u.kept.join(", "))); }
    }
    let mut rec = Installed { entry_id: e.id.clone(), version: e.mod_.version.clone(), name: e.mod_.name.clone(), title_id: e.game.title_id.clone(), target: t.key(), target_label: t.label(), installed_at: now(), ..Default::default() };
    for s in &staged {
        for (rel, size, sha) in &s.files {
            rec.files.push(JFile { path: s.dest.iter().chain(rel).cloned().collect::<Vec<_>>().join("/"), size: *size, sha256: sha.clone() });
        }
    }
    let mut j = ctx.store.journal()?;
    j.put(rec.clone());
    ctx.store.save_journal(&j)?;
    let n = staged.len().max(1) as u32;
    for (k, s) in staged.iter().enumerate() {
        let base = 60 + 35 * k as u32 / n;
        t.put_dir(&s.dir, &s.dest, &mut |p, f| progress(base + p * 35 / 100 / n, f), cancelled).map_err(|err| format!("{err} (Partly installed: use Uninstall to remove what was copied.)"))?;
    }
    report.files = rec.files.len();
    // DashLaunch
    if let Some(f) = pfile {
        let k = e.files.iter().position(|x| std::ptr::eq(x, f)).unwrap();
        let s = &staged[k];
        let files: Vec<(PathBuf, Vec<String>)> = s.files.iter().map(|(r, _, _)| (s.dir.join(r.iter().collect::<PathBuf>()), r.clone())).collect();
        let value = ini_value(t, &s.dest, &plugin_rel(f, &files)?);
        early_notes.extend(repair_ini(&ctx.store, t)?);
        match t.read(&[ini_name(t)?], ini::INI_MAX, true)? {
            None => report.notes.push("No launch.ini on this drive: add the plugin to DashLaunch yourself.".into()),
            Some(b) => {
                let before = Ini::parse(&b)?;
                let mut after = before.clone();
                let old: Vec<IniChange> = ctx.store.journal()?.installs.iter().filter(|i| i.target == t.key() && i.entry_id == e.id).flat_map(|i| i.ini.clone()).collect();
                let (slot, prev) = ini::choose_slot(&before, &value, f.install.dashlaunch.as_ref().and_then(|d| d.slot), opts.confirm_slot)?;
                let key = format!("plugin{slot}");
                if before.get(ini::PLUGIN_SECTION, &key).as_deref().map_or(false, |v| v.eq_ignore_ascii_case(&value)) {
                    report.ini = Some(format!("{key} already points at the plugin"));
                } else {
                    after.set(ini::PLUGIN_SECTION, &key, &value);
                    // Journal the intended line first, so an install whose write can't be confirmed
                    // (connection lost after the swap) still lets Uninstall take the line out.
                    let previous = prev.filter(|p| !old.iter().any(|o| o.value.eq_ignore_ascii_case(p)));
                    rec.ini.push(IniChange { key: key.clone(), value: value.clone(), previous, backup: String::new(), unconfirmed: true });
                    let mut j = ctx.store.journal()?;
                    j.put(rec.clone());
                    ctx.store.save_journal(&j)?;
                    let backup = write_ini(ctx, t, &b, &before, &after, &key, Some(&value))?;
                    rec.backups.push(backup.clone());
                    if let Some(c) = rec.ini.last_mut() { c.backup = backup; c.unconfirmed = false; }
                    report.ini = Some(format!("{key} = {value}"));
                }
            }
        }
    }
    report.notes.splice(0..0, early_notes);
    rec.complete = true;
    let mut j = ctx.store.journal()?;
    j.put(rec);
    ctx.store.save_journal(&j)?;
    progress(100, "Installed");
    Ok(report)
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct UninstallReport {
    pub removed: usize,
    /// Files left because they changed since install (or are already gone).
    pub kept: Vec<String>,
    pub ini: Option<String>,
    pub notes: Vec<String>,
}

/// Remove what the journal says was installed. Files changed since are kept. `for_update`
/// skips the dependents check (the entry is being replaced by a newer version).
pub fn uninstall(ctx: &Ctx, l: Option<&Loaded>, id: &str, t: &Target, for_update: bool) -> Result<UninstallReport, String> {
    let mut j = ctx.store.journal()?;
    let key = t.key();
    let rec = j.find(&key, id).cloned().ok_or("Unpack has no record of installing this on that target.")?;
    if !for_update {
        if let Some(l) = l {
            let deps: Vec<String> = j.installs.iter().filter(|i| i.target == key && i.entry_id != id && l.entries.iter().find(|e| e.id == i.entry_id).map_or(false, |e| e.requires.iter().any(|r| r == id))).map(|i| i.name.clone()).collect();
            if !deps.is_empty() {
                return Err(format!("{} needs this mod. Uninstall it first.", deps.join(", ")));
            }
        }
    }
    let mut r = UninstallReport::default();
    // Files and launch.ini lines another installed mod on this target also lists stay (R12 MEDIUM-1).
    // FATX names are case-insensitive.
    let others: Vec<&Installed> = j.installs.iter().filter(|i| i.target == key && i.entry_id != id).collect();
    let user_of = |path: &str| others.iter().find(|i| i.files.iter().any(|f| f.path.eq_ignore_ascii_case(path))).map(|i| i.name.clone());
    for f in &rec.files {
        if let Some(by) = user_of(&f.path) {
            r.kept.push(format!("{} (used by {by})", f.path));
            continue;
        }
        let segs: Vec<String> = f.path.split('/').map(String::from).collect();
        if t.delete_if(&segs, f.size, &f.sha256)? { r.removed += 1 } else { r.kept.push(f.path.clone()) }
    }
    let ini_user = |value: &str| others.iter().find(|i| i.ini.iter().any(|c| c.value.eq_ignore_ascii_case(value)) || i.files.iter().any(|f| {
        let segs: Vec<String> = f.path.split('/').map(String::from).collect();
        ini_value(t, &[], &segs).eq_ignore_ascii_case(value)
    })).map(|i| (i.entry_id.clone(), i.name.clone()));
    // Lines kept for another mod pass to it, so the last user's uninstall removes them (R13 LOW-1).
    let mut handover: Vec<(String, IniChange)> = vec![];
    if !rec.ini.is_empty() {
        r.notes.extend(repair_ini(&ctx.store, t)?);
        if let Some(b) = t.read(&[ini_name(t)?], ini::INI_MAX, true)? {
            let before = Ini::parse(&b)?;
            let mut after = before.clone();
            let mut changed = vec![];
            for c in rec.ini.iter().rev() {
                if let Some((by_id, by)) = ini_user(&c.value) {
                    r.ini = Some(format!("{} kept (used by {by})", c.key));
                    handover.push((by_id, c.clone()));
                    continue;
                }
                if after.get(ini::PLUGIN_SECTION, &c.key).as_deref().map_or(false, |v| v.eq_ignore_ascii_case(&c.value)) {
                    match &c.previous { Some(p) => after.set(ini::PLUGIN_SECTION, &c.key, p), None => after.remove(ini::PLUGIN_SECTION, &c.key) }
                    changed.push(c);
                } else if c.unconfirmed {
                    r.ini = Some(format!("{} was never added; nothing to remove", c.key));
                } else {
                    r.ini = Some(format!("{} was changed since; left as is", c.key));
                }
            }
            if let [c] = changed[..] {
                write_ini(ctx, t, &b, &before, &after, &c.key, c.previous.as_deref())?;
                r.ini = Some(match &c.previous { Some(p) => format!("{} restored to {p}", c.key), None => format!("{} removed", c.key) });
            } else if !changed.is_empty() {
                return Err("launch.ini has several Unpack changes for this mod; restore it from the backup.".into());
            }
        }
    }
    j.remove(&key, id);
    for (to, c) in handover {
        if let Some(rec) = j.installs.iter_mut().find(|i| i.target == key && i.entry_id == to) {
            if !rec.ini.iter().any(|x| x.key == c.key && x.value.eq_ignore_ascii_case(&c.value)) { rec.ini.push(c); }
        }
    }
    ctx.store.save_journal(&j)?;
    Ok(r)
}

#[derive(Debug, Clone, Serialize)]
pub struct EntryState {
    pub id: String,
    /// "new" | "installed" | "update" | "partial"
    pub state: String,
    pub installed_version: Option<String>,
}

pub fn states(store: &Store, l: &Loaded, target_key: &str) -> Result<Vec<EntryState>, String> {
    let j = store.journal()?;
    Ok(l.entries.iter().map(|e| {
        let r = j.find(target_key, &e.id);
        EntryState {
            id: e.id.clone(),
            state: match r { None => "new", Some(r) if !r.complete => "partial", Some(r) if r.version == e.mod_.version => "installed", _ => "update" }.into(),
            installed_version: r.map(|r| r.version.clone()),
        }
    }).collect())
}
