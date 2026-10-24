//! Native archive engine: drives the bundled 7-Zip command-line tool (7za).
//!
//! Everything here is plain Rust with no Tauri types so it can be unit-tested on Linux.

use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;

#[cfg(windows)]
const EXE_NAME: &str = "7za.exe";
#[cfg(not(windows))]
const EXE_NAME: &str = "7za";

/// Passed when the user gave no password, so 7-Zip never stops to ask on stdin.
const NO_PASSWORD: &str = "\u{1}unpack-no-password";

pub const ERR_PASSWORD_REQUIRED: &str = "PASSWORD_REQUIRED";
pub const ERR_WRONG_PASSWORD: &str = "WRONG_PASSWORD";
pub const ERR_CANCELLED: &str = "CANCELLED";

/// Find 7za: next to our own exe first (installed / bundled), then a few dev fallbacks.
/// File names the app treats as archives (same list as ARCHIVE_RE in native.js).
pub fn is_archive_name(path: &str) -> bool {
    let l = path.to_lowercase();
    [
        ".zip", ".rar", ".7z", ".tar", ".tgz", ".gz", ".bz2", ".xz", ".cab", ".arj", ".lzh", ".z",
        ".lzma", ".wim", ".zipx",
    ]
    .iter()
    .any(|e| l.ends_with(e))
}

pub fn find_7za() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("UNPACK_7ZA") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join(EXE_NAME);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    #[cfg(not(windows))]
    for p in [
        "/usr/lib/7zip/7za",
        "/usr/bin/7za",
        "/usr/bin/7zz",
        "/usr/bin/7z",
    ] {
        if Path::new(p).is_file() {
            return Some(PathBuf::from(p));
        }
    }
    None
}

fn base_command(exe: &Path) -> Command {
    let mut cmd = Command::new(exe);
    cmd.stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Add arguments to a 7-Zip command.
///
/// On Windows, 7-Zip splits its command line itself: a `"` only toggles quoting and
/// backslashes are never escapes. Rust's default quoting (MSVC rules: `\"`, doubled
/// trailing backslashes) would therefore corrupt passwords with spaces + backslashes.
/// So we quote args ourselves, 7-Zip style. `"` can't be expressed at all, which is why
/// passwords containing it are rejected up front (`check_password`).
fn add_args<I, S>(cmd: &mut Command, args: I)
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    for a in args {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            let s = a.as_ref().to_string_lossy();
            if s.is_empty() || s.contains(' ') || s.contains('\t') {
                cmd.raw_arg(format!("\"{s}\""));
            } else {
                cmd.raw_arg(s.as_ref());
            }
        }
        #[cfg(not(windows))]
        cmd.arg(a);
    }
}

pub fn check_password(password: Option<&str>) -> Result<(), String> {
    if cfg!(windows) && password.is_some_and(|p| p.contains('"')) {
        return Err("Passwords containing a double quote (\") can't be passed to 7-Zip on Windows. Please use a password without it.".into());
    }
    Ok(())
}

fn pw_arg(password: Option<&str>) -> String {
    match password {
        Some(p) if !p.is_empty() => format!("-p{p}"),
        _ => format!("-p{NO_PASSWORD}"),
    }
}

/// Map 7-Zip error text to our error codes.
fn classify_error(stderr: &str, had_password: bool) -> String {
    let low = stderr.to_lowercase();
    if low.contains("wrong password")
        || low.contains("cannot open encrypted archive")
        || low.contains("can not open encrypted archive")
    {
        return if had_password {
            ERR_WRONG_PASSWORD.into()
        } else {
            ERR_PASSWORD_REQUIRED.into()
        };
    }
    let msg: Vec<&str> = stderr
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with("7-Zip") && !l.contains('%'))
        .collect();
    if msg.is_empty() {
        "7-Zip could not open this archive.".into()
    } else {
        msg.join(" · ")
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Entry {
    pub path: String,
    pub size: u64,
    pub dir: bool,
    pub encrypted: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Listing {
    pub entries: Vec<Entry>,
    pub format: String,
    pub encrypted: bool,
}

/// Normalise an archive entry name for display / comparisons: forward slashes,
/// no empty, "." or ".." segments, no drive prefix.
pub fn clean_entry_path(raw: &str) -> String {
    raw.replace('\\', "/")
        .split('/')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty() && *s != "." && *s != ".." && !s.ends_with(':'))
        .collect::<Vec<_>>()
        .join("/")
}

pub fn list(exe: &Path, archive: &Path, password: Option<&str>) -> Result<Listing, String> {
    let had_pw = password.is_some_and(|p| !p.is_empty());
    check_password(password)?;
    let mut cmd = base_command(exe);
    add_args(
        &mut cmd,
        [
            std::ffi::OsString::from("l"),
            "-slt".into(),
            "-ba".into(),
            "-sccUTF-8".into(),
            pw_arg(password).into(),
            "--".into(),
            archive.as_os_str().into(),
        ],
    );
    let out = cmd
        .output()
        .map_err(|e| format!("Could not start 7-Zip: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() {
        return Err(classify_error(&format!("{stderr}\n{stdout}"), had_pw));
    }
    let mut entries = Vec::new();
    let mut format = String::new();
    let mut cur: HashMap<String, String> = HashMap::new();
    let flush = |cur: &mut HashMap<String, String>, entries: &mut Vec<Entry>| {
        if let Some(p) = cur.get("Path") {
            let path = clean_entry_path(p);
            if !path.is_empty() {
                let attrs = cur.get("Attributes").cloned().unwrap_or_default();
                entries.push(Entry {
                    path,
                    size: cur.get("Size").and_then(|s| s.parse().ok()).unwrap_or(0),
                    dir: cur.get("Folder").is_some_and(|f| f == "+") || attrs.starts_with('D'),
                    encrypted: cur.get("Encrypted").is_some_and(|f| f == "+"),
                });
            }
        }
        cur.clear();
    };
    for line in stdout.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            flush(&mut cur, &mut entries);
            continue;
        }
        if let Some((k, v)) = line.split_once(" = ") {
            cur.insert(k.to_string(), v.to_string());
        } else if let Some(k) = line.strip_suffix(" =") {
            cur.insert(k.to_string(), String::new());
        }
    }
    flush(&mut cur, &mut entries);
    // Format comes from the archive header block (only printed without -ba), so sniff the file.
    if let Ok(mut f) = fs::File::open(archive) {
        let mut head = [0u8; 8];
        let n = f.read(&mut head).unwrap_or(0);
        format = sniff_format(&head[..n], archive);
    }
    let encrypted = entries.iter().any(|e| e.encrypted);
    Ok(Listing {
        entries,
        format,
        encrypted,
    })
}

pub fn sniff_format(head: &[u8], path: &Path) -> String {
    if head.starts_with(b"PK") {
        return "zip".into();
    }
    if head.starts_with(b"Rar!") {
        return "rar".into();
    }
    if head.starts_with(&[0x37, 0x7a, 0xbc, 0xaf]) {
        return "7z".into();
    }
    if head.starts_with(&[0x1f, 0x8b]) {
        return "gz".into();
    }
    path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("archive")
        .to_lowercase()
}

/// Running 7-Zip processes by job id, so they can be cancelled.
#[derive(Default, Clone)]
pub struct Jobs {
    inner: Arc<Mutex<HashMap<String, JobHandle>>>,
}

struct JobHandle {
    child: Option<Arc<Mutex<Child>>>,
    cancelled: Arc<std::sync::atomic::AtomicBool>,
}

impl Jobs {
    pub fn register(&self, id: &str) -> Arc<std::sync::atomic::AtomicBool> {
        let flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.inner.lock().unwrap().insert(
            id.to_string(),
            JobHandle {
                child: None,
                cancelled: flag.clone(),
            },
        );
        flag
    }
    fn attach(&self, id: &str, child: Arc<Mutex<Child>>) {
        if let Some(h) = self.inner.lock().unwrap().get_mut(id) {
            h.child = Some(child);
        }
    }
    pub fn finish(&self, id: &str) {
        self.inner.lock().unwrap().remove(id);
    }
    pub fn cancel(&self, id: &str) -> bool {
        let map = self.inner.lock().unwrap();
        if let Some(h) = map.get(id) {
            h.cancelled.store(true, std::sync::atomic::Ordering::SeqCst);
            if let Some(c) = &h.child {
                let _ = c.lock().unwrap().kill();
            }
            true
        } else {
            false
        }
    }
    pub fn is_cancelled(&self, id: &str) -> bool {
        self.inner
            .lock()
            .unwrap()
            .get(id)
            .is_some_and(|h| h.cancelled.load(std::sync::atomic::Ordering::SeqCst))
    }
}

/// Run 7-Zip with `-bsp1`, calling `progress(percent, current_file)` at most ~8x per second.
pub fn run_with_progress(
    exe: &Path,
    args: &[std::ffi::OsString],
    jobs: &Jobs,
    job_id: &str,
    had_password: bool,
    progress: &mut dyn FnMut(u32, &str),
) -> Result<Vec<String>, String> {
    run_capped(exe, args, jobs, job_id, had_password, progress, None)
}

/// 7-Zip was stopped because the folder it writes grew past its byte cap.
pub const ERR_SIZE_CAP: &str = "SIZE_CAP";

/// `run_with_progress`, optionally with a live byte cap: a watcher measures `cap.0` every 20 ms
/// and kills 7-Zip as soon as it holds more than `cap.1` bytes (R16 MEDIUM-1), so a forged size
/// in a header can't fill the drive. Returns `ERR_SIZE_CAP` then.
fn run_capped(
    exe: &Path,
    args: &[std::ffi::OsString],
    jobs: &Jobs,
    job_id: &str,
    had_password: bool,
    progress: &mut dyn FnMut(u32, &str),
    cap: Option<(PathBuf, u64)>,
) -> Result<Vec<String>, String> {
    if jobs.is_cancelled(job_id) {
        return Err(ERR_CANCELLED.into());
    }
    let mut cmd = base_command(exe);
    add_args(&mut cmd, args);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Could not start 7-Zip: {e}"))?;
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let child = Arc::new(Mutex::new(child));
    jobs.attach(job_id, child.clone());
    // A cancel that arrived between the check above and attach() would have found no child.
    if jobs.is_cancelled(job_id) {
        let _ = child.lock().unwrap().kill();
    }
    let over = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let watcher = cap.map(|(dir, limit)| {
        let (child, over, done) = (child.clone(), over.clone(), done.clone());
        std::thread::spawn(move || {
            use std::sync::atomic::Ordering::SeqCst;
            while !done.load(SeqCst) {
                let (bytes, files) = tree_stats(&dir);
                if bytes > limit || files > NESTED_MAX_FILES {
                    over.store(true, SeqCst);
                    if let Ok(mut c) = child.lock() {
                        let _ = c.kill();
                    }
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        })
    });
    let err_thread = std::thread::spawn(move || {
        let mut s = Vec::new();
        let _ = stderr.read_to_end(&mut s);
        String::from_utf8_lossy(&s).into_owned()
    });
    let mut buf = [0u8; 4096];
    let mut line = Vec::new();
    let mut last_emit = Instant::now() - Duration::from_secs(1);
    let mut last_pct = 0u32;
    loop {
        let n = match stdout.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        for &b in &buf[..n] {
            if b == b'\r' || b == b'\n' || b == 8 {
                if !line.is_empty() {
                    let text = String::from_utf8_lossy(&line).into_owned();
                    if let Some((pct, file)) = parse_progress(&text) {
                        last_pct = pct;
                        if last_emit.elapsed() >= Duration::from_millis(120) {
                            progress(pct, &file);
                            last_emit = Instant::now();
                        }
                    }
                    line.clear();
                }
            } else {
                line.push(b);
            }
        }
    }
    // Poll instead of a blocking wait so the watcher can take the lock and kill 7-Zip.
    let status = loop {
        match child.lock().unwrap().try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) => {}
            Err(e) => return Err(e.to_string()),
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    done.store(true, std::sync::atomic::Ordering::SeqCst);
    if let Some(w) = watcher {
        let _ = w.join();
    }
    let err_text = err_thread.join().unwrap_or_default();
    if jobs.is_cancelled(job_id) {
        return Err(ERR_CANCELLED.into());
    }
    if over.load(std::sync::atomic::Ordering::SeqCst) {
        return Err(ERR_SIZE_CAP.into());
    }
    // 7-Zip exit codes: 0 ok, 1 warning (non-fatal), 2 fatal, 7 bad command line, 8 out of memory, 255 user stop
    match status.code() {
        Some(0) => {
            progress(100.max(last_pct), "");
            Ok(Vec::new())
        }
        Some(1) => {
            progress(100, "");
            Ok(Vec::new())
        }
        Some(2) if tolerable_errors(&err_text) => {
            // e.g. a tar hard link pointing outside the archive: skip that entry, keep the rest.
            progress(100, "");
            Ok(err_text
                .lines()
                .filter(|l| l.contains("Cannot create hard link"))
                .map(|l| {
                    format!(
                        "Skipped a hard link that points outside the archive ({})",
                        l.split(" : ").nth(2).unwrap_or("?").trim()
                    )
                })
                .collect())
        }
        _ => Err(classify_error(&err_text, had_password)),
    }
}

/// True when 7-Zip's only errors were hard links it refused to create.
fn tolerable_errors(err: &str) -> bool {
    let errors: Vec<&str> = err
        .lines()
        .filter(|l| l.trim_start().starts_with("ERROR:"))
        .collect();
    // Older 7-Zip builds (23.x) follow a refused hard link with "Cannot set file attribute" for the same entry.
    errors.iter().any(|l| l.contains("Cannot create hard link"))
        && errors.iter().all(|l| {
            l.contains("Cannot create hard link") || l.contains("Cannot set file attribute")
        })
}

/// Parse a 7-Zip progress line such as " 44% 3 - folder/file.bin".
pub fn parse_progress(text: &str) -> Option<(u32, String)> {
    let t = text.trim();
    let pct_end = t.find('%')?;
    let pct: u32 = t[..pct_end].trim().parse().ok()?;
    let rest = t[pct_end + 1..].trim();
    let file = rest
        .split_once("- ")
        .map(|(_, f)| f.trim().to_string())
        .unwrap_or_default();
    Some((pct.min(100), file))
}

/// What to do when the destination already has something with the same name.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Collision {
    Number,
    Replace,
    Skip,
}

impl Collision {
    pub fn parse(s: &str) -> Collision {
        match s {
            "replace" => Collision::Replace,
            "skip" => Collision::Skip,
            _ => Collision::Number,
        }
    }
}

/// Windows-safe single path segment.
pub fn safe_segment(name: &str) -> String {
    let mut s: String = name
        .chars()
        .map(|c| {
            if "<>:\"/\\|?*".contains(c) || (c as u32) < 32 {
                '_'
            } else {
                c
            }
        })
        .collect();
    while s.ends_with('.') || s.ends_with(' ') {
        s.pop();
    }
    let s2 = s.trim().to_string();
    s = if s2.is_empty() || s2 == "." || s2 == ".." {
        "file".into()
    } else {
        s2
    };
    // Same reserved-name rule as downloads (CONIN$, "nul .txt", COM¹, …).
    if crate::download::is_windows_reserved_stem(s.split('.').next().unwrap_or("")) {
        s = format!("_{s}");
    }
    s
}

pub fn archive_stem(path: &Path) -> String {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("archive");
    let lower = name.to_lowercase();
    for ext in [
        ".tar.gz", ".tar.bz2", ".tar.xz", ".zip", ".rar", ".7z", ".tar", ".tgz", ".gz", ".bz2",
        ".xz", ".iso",
    ] {
        if lower.ends_with(ext) && name.len() > ext.len() {
            return safe_segment(&name[..name.len() - ext.len()]);
        }
    }
    safe_segment(name)
}

/// Pick a free name in `parent` for `name` ("name-2", "name-3.ext", ...).
pub fn free_name(parent: &Path, name: &str) -> PathBuf {
    let first = parent.join(name);
    if fs::symlink_metadata(&first).is_err() {
        return first;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    for n in 2.. {
        let p = parent.join(format!("{stem}-{n}{ext}"));
        if fs::symlink_metadata(&p).is_err() {
            return p;
        }
    }
    unreachable!()
}

/// Walk a freshly extracted tree: delete symlinks/junctions and anything that would
/// resolve outside `root`. Returns warnings. (Defence in depth: 7-Zip already
/// strips "../" and absolute paths.)
pub fn sanitize_tree(root: &Path) -> Vec<String> {
    let mut warnings = Vec::new();
    let canon_root = match fs::canonicalize(root) {
        Ok(p) => p,
        Err(_) => return warnings,
    };
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let rd = match fs::read_dir(&dir) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for ent in rd.flatten() {
            let p = ent.path();
            let meta = match fs::symlink_metadata(&p) {
                Ok(m) => m,
                Err(_) => continue,
            };
            let rel = p.strip_prefix(root).unwrap_or(&p).display().to_string();
            if meta.file_type().is_symlink() || is_reparse_point(&meta) {
                let _ = fs::remove_file(&p).or_else(|_| fs::remove_dir(&p));
                warnings.push(format!("{rel}: link removed for safety"));
                continue;
            }
            match fs::canonicalize(&p) {
                Ok(c) if c.starts_with(&canon_root) => {}
                _ => {
                    let _ = fs::remove_file(&p).or_else(|_| fs::remove_dir_all(&p));
                    warnings.push(format!("{rel}: outside the destination, removed"));
                    continue;
                }
            }
            if meta.is_dir() {
                stack.push(p);
            }
        }
    }
    warnings
}

#[cfg(windows)]
fn is_reparse_point(meta: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    meta.file_attributes() & 0x400 != 0
}
#[cfg(not(windows))]
fn is_reparse_point(_meta: &fs::Metadata) -> bool {
    false
}

/// Serialises "pick a name + move into place" across parallel jobs in this process.
static PLACE_LOCK: Mutex<()> = Mutex::new(());

/// Atomically reserve a new, empty folder `name`, `name-2`, ... in `parent`.
/// `create_dir` fails if anything already exists, so two jobs (or another program)
/// can never end up sharing one folder.
pub fn claim_dir(parent: &Path, name: &str) -> std::io::Result<PathBuf> {
    let mut n = 1u32;
    loop {
        let candidate = if n == 1 {
            parent.join(name)
        } else {
            parent.join(format!("{name}-{n}"))
        };
        match fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => n += 1,
            Err(e) => return Err(e),
        }
        if n > 100_000 {
            return Err(std::io::Error::other("no free folder name"));
        }
    }
}

/// Move the *contents* of `src_dir` into the freshly claimed, empty `dst_dir`.
fn move_children(src_dir: &Path, dst_dir: &Path) -> std::io::Result<()> {
    for e in fs::read_dir(src_dir)? {
        let e = e?;
        move_path(&e.path(), &dst_dir.join(e.file_name()))?;
    }
    Ok(())
}

/// Move a file or folder to a free name next to `name` without ever overwriting:
/// folders are claimed with `create_dir`, files with `create_new`.
fn place_numbered(src: &Path, parent: &Path, name: &str) -> std::io::Result<PathBuf> {
    if src.is_dir() {
        let d = claim_dir(parent, name)?;
        move_children(src, &d)?;
        let _ = fs::remove_dir(src);
        Ok(d)
    } else {
        let (stem, ext) = match name.rfind('.') {
            Some(i) if i > 0 => (&name[..i], &name[i..]),
            _ => (name, ""),
        };
        let mut n = 1u32;
        loop {
            let c = if n == 1 {
                parent.join(name)
            } else {
                parent.join(format!("{stem}-{n}{ext}"))
            };
            match fs::OpenOptions::new().write(true).create_new(true).open(&c) {
                Ok(f) => {
                    drop(f); // close it first (Windows can't replace an open file)
                             // we own the empty placeholder; replace it with the real file
                    move_path(src, &c)?;
                    return Ok(c);
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => n += 1,
                Err(e) => return Err(e),
            }
        }
    }
}

/// Move `src` to `dst`. Falls back to copy+delete across volumes.
fn move_path(src: &Path, dst: &Path) -> std::io::Result<()> {
    // Only ever called with a dst that doesn't exist or is our own empty placeholder file.
    if src.is_file()
        && fs::symlink_metadata(dst)
            .map(|m| m.is_file() && m.len() == 0)
            .unwrap_or(false)
    {
        let _ = fs::remove_file(dst);
    }
    match fs::rename(src, dst) {
        Ok(()) => Ok(()),
        Err(_) => {
            copy_recursive(src, dst)?;
            if src.is_dir() {
                fs::remove_dir_all(src)
            } else {
                fs::remove_file(src)
            }
        }
    }
}

fn copy_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    if src.is_dir() {
        fs::create_dir_all(dst)?;
        for e in fs::read_dir(src)? {
            let e = e?;
            copy_recursive(&e.path(), &dst.join(e.file_name()))?;
        }
        Ok(())
    } else {
        fs::copy(src, dst).map(|_| ())
    }
}

/// Is `p` a symlink, or (on Windows) a junction/reparse point? Never follows it.
pub(crate) fn is_link(p: &Path) -> bool {
    fs::symlink_metadata(p)
        .map(|m| m.file_type().is_symlink() || is_reparse_point(&m))
        .unwrap_or(false)
}

/// Remove a link itself (never its target).
fn remove_link(p: &Path) -> std::io::Result<()> {
    fs::remove_file(p).or_else(|_| fs::remove_dir(p))
}

/// Merge `src` into existing `dst`, replacing files with the same name.
/// Links already in the destination are replaced by real files/folders, never followed,
/// so "Replace" can't write outside the chosen folder.
fn merge_replace(src: &Path, dst: &Path, warnings: &mut Vec<String>) -> std::io::Result<()> {
    if is_link(dst) {
        remove_link(dst)?;
        warnings.push(format!(
            "{} was a link (shortcut to another folder or file); the link was replaced with the extracted item. Nothing was written through it.",
            dst.display()
        ));
    }
    if src.is_dir() {
        let dst_meta = fs::symlink_metadata(dst).ok();
        if dst_meta.as_ref().is_some_and(|m| !m.is_dir()) {
            fs::remove_file(dst)?;
        }
        if fs::symlink_metadata(dst).is_err() {
            fs::create_dir(dst)?;
        }
        for e in fs::read_dir(src)? {
            let e = e?;
            merge_replace(&e.path(), &dst.join(e.file_name()), warnings)?;
        }
        fs::remove_dir(src)
    } else {
        match fs::symlink_metadata(dst) {
            Ok(m) if m.is_dir() => fs::remove_dir_all(dst)?,
            Ok(_) => fs::remove_file(dst)?,
            Err(_) => {}
        }
        move_path(src, dst)
    }
}

/// How the extracted files are laid out in the destination.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Layout {
    /// Into a folder named after the archive (or its single top-level folder).
    Folder,
    /// Straight into the destination ("Extract here"): each top-level item placed individually.
    Here,
}

#[derive(Debug, Serialize)]
pub struct ExtractResult {
    /// Where the files ended up (folder, or destination dir for Layout::Here).
    pub output: String,
    pub skipped: bool,
    pub warnings: Vec<String>,
    pub files: usize,
    /// Exact final paths of the .iso files this extraction placed (Folder layout only).
    /// Auto ISO -> GOD only ever touches these paths.
    pub isos: Vec<String>,
    /// True when the files were merged into a folder that already existed (Replace):
    /// the delete-ISO option is then disabled.
    pub merged: bool,
    /// One opaque token per entry of `isos`, issued by the Rust-side registry
    /// (`god::register_extracted`). The page converts by token only; Rust decides whether
    /// deleting is allowed and re-checks the file's identity.
    pub iso_tokens: Vec<String>,
    /// Exact final paths of files this extraction placed that look like Xbox 360 packages
    /// (CON/LIVE/PIRS magic) — DLC candidates (Folder layout only).
    pub dlcs: Vec<String>,
    /// True only when this run created a brand-new folder via claim_dir (Folder layout).
    /// Session grants use this; Skip / Replace-merge / Extract-here never set it.
    pub created: bool,
    /// Archives found inside the archive that were opened too ("Open zips inside zips").
    pub nested: u32,
    /// Archives found inside that were left closed: the option is off, a limit was hit, or the inner
    /// archive needs a password / is damaged.
    pub inner_left: u32,
}

/// .iso files under `root`, as paths relative to it (links never followed).
fn iso_rel_paths(root: &Path) -> Vec<PathBuf> {
    let mut out = vec![];
    let mut stack = vec![PathBuf::new()];
    while let Some(rel) = stack.pop() {
        let Ok(rd) = fs::read_dir(root.join(&rel)) else {
            continue;
        };
        for e in rd.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            let r = rel.join(e.file_name());
            if ft.is_dir() && !is_link(&e.path()) {
                stack.push(r);
            } else if ft.is_file() && r.extension().is_some_and(|x| x.eq_ignore_ascii_case("iso")) {
                out.push(r);
            }
        }
    }
    out.sort();
    out
}

pub struct ExtractOptions<'a> {
    pub archive: &'a Path,
    pub dest_parent: &'a Path,
    pub password: Option<&'a str>,
    pub collision: Collision,
    pub layout: Layout,
    pub skip_junk: bool,
    /// Open archives found inside the extracted files (same folder, same safety rules).
    pub nested: bool,
}

fn is_junk(name: &str) -> bool {
    let n = name.to_lowercase();
    n == "__macosx" || n == ".ds_store" || n == "thumbs.db" || n == "desktop.ini"
}

fn remove_junk(root: &Path) -> usize {
    let mut removed = 0;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if let Ok(rd) = fs::read_dir(&dir) {
            for e in rd.flatten() {
                let p = e.path();
                if is_junk(&e.file_name().to_string_lossy()) {
                    let ok = if p.is_dir() {
                        fs::remove_dir_all(&p)
                    } else {
                        fs::remove_file(&p)
                    };
                    if ok.is_ok() {
                        removed += 1;
                    }
                } else if p.is_dir() {
                    stack.push(p);
                }
            }
        }
    }
    removed
}

/// Bytes of regular files under `root` (links are not followed).
fn tree_bytes(root: &Path) -> u64 {
    let mut n = 0u64;
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        if let Ok(rd) = fs::read_dir(&d) {
            for e in rd.flatten() {
                let Ok(m) = fs::symlink_metadata(e.path()) else {
                    continue;
                };
                if m.is_dir() {
                    stack.push(e.path())
                } else if m.is_file() {
                    n = n.saturating_add(m.len())
                }
            }
        }
    }
    n
}

/// An inner archive may not create more files than this (R17 optional: inode/file-count bomb).
pub const NESTED_MAX_FILES: usize = 100_000;

/// (bytes, files) under `root`, links not followed.
fn tree_stats(root: &Path) -> (u64, usize) {
    let (mut n, mut c) = (0u64, 0usize);
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        if let Ok(rd) = fs::read_dir(&d) {
            for e in rd.flatten() {
                let Ok(m) = fs::symlink_metadata(e.path()) else {
                    continue;
                };
                if m.is_dir() {
                    stack.push(e.path())
                } else {
                    c += 1;
                    if m.is_file() {
                        n = n.saturating_add(m.len())
                    }
                }
            }
        }
    }
    (n, c)
}

fn count_files(root: &Path) -> usize {
    let mut n = 0;
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        if let Ok(rd) = fs::read_dir(&d) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p)
                } else {
                    n += 1
                }
            }
        }
    }
    n
}

/// Removes the staging folder however we leave the function. R26/R27: next to it sits
/// `<staging>.lock`, created and locked *before* the folder (so a staging folder never exists
/// without its held lock), holding a signature line. The OS drops the lock when the process
/// dies, which is how a later run tells an abandoned staging folder from one in use.
struct StagingGuard(PathBuf, Option<fs::File>);
impl Drop for StagingGuard {
    fn drop(&mut self) {
        if self.0.exists() {
            let _ = fs::remove_dir_all(&self.0);
        }
        if let Some(f) = self.1.take() {
            drop(f);
            let _ = fs::remove_file(lock_path(&self.0));
        }
    }
}

const STAGING_PREFIX: &str = ".unpack-tmp-";
const STAGING_SIG: &str = "unpack-staging v1";

fn lock_path(staging: &Path) -> PathBuf {
    let mut n = staging.file_name().unwrap_or_default().to_os_string();
    n.push(".lock");
    staging.with_file_name(n)
}

/// Create `<staging>.lock` (never an existing one), hold an exclusive lock on it and write the
/// signature. Call this before creating the staging folder itself.
fn lock_staging(staging: &Path) -> Option<fs::File> {
    use std::io::Write;
    let mut f = fs::OpenOptions::new().write(true).create_new(true).open(lock_path(staging)).ok()?;
    if f.try_lock().is_err() || writeln!(f, "{STAGING_SIG} pid={}", std::process::id()).is_err() {
        drop(f);
        let _ = fs::remove_file(lock_path(staging));
        return None;
    }
    Some(f)
}

/// R26/R27 L: staging folders of a killed run. A `.unpack-tmp-*` folder is removed only when it
/// is a real folder and its `.lock` is a real file that opens, isn't held by anyone and carries
/// Unpack's signature. A held lock means a job is using it: left alone silently. Anything else
/// (no lock, a symlinked or unreadable lock, a foreign signature, a failed removal) is reported.
fn sweep_stale_staging(parent: &Path, own: &Path, warnings: &mut Vec<String>) {
    use std::io::Read;
    let Ok(rd) = fs::read_dir(parent) else { return };
    let own_lock = lock_path(own);
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let p = e.path();
        if !name.starts_with(STAGING_PREFIX) || p == own || p == own_lock {
            continue;
        }
        let left = |why: &str| {
            format!("{} looks like it was left over from an interrupted extraction, but {why}; it was not touched, delete it if you don't need it.", p.display())
        };
        if let Some(dir) = name.strip_suffix(".lock") {
            // A lock whose folder is gone: remove it if it is ours and free.
            if fs::symlink_metadata(parent.join(dir)).is_err() && !is_link(&p) {
                if let Ok(mut f) = fs::OpenOptions::new().read(true).write(true).open(&p) {
                    let mut sig = String::new();
                    if f.try_lock().is_ok() && f.read_to_string(&mut sig).is_ok() && sig.starts_with(STAGING_SIG) {
                        drop(f);
                        let _ = fs::remove_file(&p);
                    }
                }
            }
            continue;
        }
        if !fs::symlink_metadata(&p).is_ok_and(|m| m.is_dir()) {
            warnings.push(left("it isn't a plain folder"));
            continue;
        }
        let lock = lock_path(&p);
        if fs::symlink_metadata(&lock).is_err() {
            warnings.push(left("it has no lock file"));
            continue;
        }
        if is_link(&lock) {
            warnings.push(left("its lock file is a link"));
            continue;
        }
        let Ok(mut f) = fs::OpenOptions::new().read(true).write(true).open(&lock) else {
            warnings.push(left("its lock file couldn't be opened"));
            continue;
        };
        if f.try_lock().is_err() {
            continue; // in use by a running job
        }
        let mut sig = String::new();
        if f.read_to_string(&mut sig).is_err() || !sig.starts_with(STAGING_SIG) {
            warnings.push(left("its lock file isn't one of mine"));
            continue;
        }
        if let Err(x) = fs::remove_dir_all(&p) {
            warnings.push(format!("{} is left over from an interrupted extraction and couldn't be removed ({x}); you can delete it.", p.display()));
            continue;
        }
        drop(f);
        let _ = fs::remove_file(&lock);
    }
}

/// Extract an archive: 7-Zip writes into a hidden staging folder next to the
/// destination, which is then checked and moved into place (rename = instant on
/// the same drive, so multi-GB archives aren't copied twice).
pub fn extract(
    exe: &Path,
    opts: &ExtractOptions,
    jobs: &Jobs,
    job_id: &str,
    progress: &mut dyn FnMut(u32, &str),
) -> Result<ExtractResult, String> {
    let parent = opts.dest_parent;
    fs::create_dir_all(parent).map_err(|e| format!("Cannot create {}: {e}", parent.display()))?;
    let stem = archive_stem(opts.archive);

    // Decide the final folder name up front so "Skip" can bail before doing any work.
    let listing = list(exe, opts.archive, opts.password)?;
    let tops: std::collections::BTreeSet<String> = listing
        .entries
        .iter()
        .filter(|e| !(opts.skip_junk && e.path.split('/').any(is_junk)))
        .filter_map(|e| e.path.split('/').next().map(|s| s.to_string()))
        .collect();
    let single_root_dir = if tops.len() == 1 {
        let top = tops.iter().next().unwrap().clone();
        let is_dir = listing
            .entries
            .iter()
            .any(|e| e.path.starts_with(&format!("{top}/")) || (e.path == top && e.dir));
        if is_dir {
            Some(top)
        } else {
            None
        }
    } else {
        None
    };

    // Console Content profile folder (Content\<16 hex>, the Content folder, or a drive root
    // holding one): GOD games go in only as their <TitleID> tree, never in a wrapper folder.
    let profile = crate::contentdir::resolve(parent);
    let in_profile = crate::contentdir::is_profile_dir(parent)
        || parent
            .file_name()
            .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case("content"));

    if profile.is_none() && opts.layout == Layout::Folder && opts.collision == Collision::Skip {
        let want = safe_segment(single_root_dir.as_deref().unwrap_or(&stem));
        if fs::symlink_metadata(parent.join(&want)).is_ok() {
            return Ok(ExtractResult {
                output: parent.join(want).display().to_string(),
                skipped: true,
                warnings: vec![],
                files: 0,
                isos: vec![],
                merged: false,
                iso_tokens: vec![],
                dlcs: vec![],
                created: false,
                nested: 0,
                inner_left: 0,
            });
        }
    }

    // R27 L: the lock comes first (create_new, so never someone else's), then the folder.
    fs::create_dir_all(parent).map_err(|e| format!("Cannot write to {}: {e}", parent.display()))?;
    let base = format!("{STAGING_PREFIX}{}", safe_segment(job_id));
    let (staging, lock) = (0..100)
        .find_map(|i| {
            let st = free_name(parent, &if i == 0 { base.clone() } else { format!("{base}-{i}") });
            if fs::symlink_metadata(lock_path(&st)).is_ok() {
                return None;
            }
            lock_staging(&st).map(|l| (st, Some(l)))
        })
        .unwrap_or_else(|| (free_name(parent, &base), None));
    if let Err(e) = fs::create_dir(&staging) {
        drop(lock);
        let _ = fs::remove_file(lock_path(&staging));
        return Err(format!("Cannot write to {}: {e}", parent.display()));
    }
    let _guard = StagingGuard(staging.clone(), lock);
    let mut swept = vec![];
    sweep_stale_staging(parent, &staging, &mut swept);

    check_password(opts.password)?;
    let had_pw = opts.password.is_some_and(|p| !p.is_empty());
    let mut out_arg = std::ffi::OsString::from("-o");
    out_arg.push(staging.as_os_str());
    let args: Vec<std::ffi::OsString> = vec![
        "x".into(),
        "-y".into(),
        "-bso0".into(),
        "-bse2".into(),
        "-bsp1".into(),
        "-snl-".into(), // never create symbolic links
        "-sccUTF-8".into(),
        pw_arg(opts.password).into(),
        out_arg,
        "--".into(),
        opts.archive.as_os_str().to_owned(),
    ];
    let tolerated = run_with_progress(exe, &args, jobs, job_id, had_pw, progress)?;

    let mut warnings = swept;
    warnings.extend(sanitize_tree(&staging));
    warnings.extend(tolerated);
    if opts.skip_junk {
        let n = remove_junk(&staging);
        if n > 0 {
            warnings.push(format!("{n} junk item(s) skipped"));
        }
    }
    let files = count_files(&staging);
    if files == 0
        && fs::read_dir(&staging)
            .map(|mut r| r.next().is_none())
            .unwrap_or(true)
    {
        return Err("The archive opened, but no files were found.".into());
    }
    let inner = open_inner(
        exe,
        &staging,
        opts.nested,
        jobs,
        job_id,
        progress,
        &mut warnings,
    )?;
    let files = if inner.opened > 0 {
        count_files(&staging)
    } else {
        files
    };
    let (nested, inner_left) = (inner.opened, inner.left);

    let place_err = |e: std::io::Error| format!("Could not move files into place: {e}");
    if let Some(profile) = &profile {
        let trees = crate::contentdir::find_title_trees(&staging);
        if !trees.is_empty() {
            return place_title_trees(
                &staging,
                &trees,
                profile,
                opts.collision,
                files,
                nested,
                inner_left,
                warnings,
                jobs,
                job_id,
                progress,
            );
        }
        if in_profile && iso_rel_paths(&staging).is_empty() {
            warnings.push(crate::contentdir::NOT_GOD_HINT.to_string());
        }
    }
    // ISOs aimed at Content\<profile> (or Content): the extraction folder holding them must
    // not sit inside Content, where the console would see a stray non-GOD folder. It goes
    // next to the Content folder instead; the GOD output still lands in the profile folder.
    let outside_parent = profile
        .as_ref()
        .filter(|_| in_profile && !iso_rel_paths(&staging).is_empty())
        .and_then(|pf| crate::contentdir::outside_content(pf));
    // R23 L6: the note says what actually happened (new folder, merged, skipped).
    let outside = outside_parent.is_some();
    let outside_note = |what: &str, at: &Path| -> Option<String> {
        outside.then(|| match what {
            "skip" => format!(
                "{} already exists next to the Content folder, so this archive was skipped; nothing was extracted or converted.",
                at.display()
            ),
            "merge" => format!(
                "The ISO was merged into your existing folder {} (next to the Content folder, not inside it); the GOD game goes into the Content folder.",
                at.display()
            ),
            _ => format!(
                "The extracted ISO folder is {} (next to the Content folder, not inside it); the GOD game goes into the Content folder.",
                at.display()
            ),
        })
    };
    let parent: &Path = outside_parent.as_deref().unwrap_or(parent);
    // Writing into a folder that already exists (Extract here, Replace) must not overlap a convert or
    // another extraction writing the same folder. Taken before PLACE_LOCK, so a long convert never
    // stalls unrelated extractions.
    let wait = || progress(100, crate::destlock::WAIT_MESSAGE);
    let _dest_lock = match (opts.layout, opts.collision) {
        (Layout::Here, _) => Some(crate::destlock::acquire(parent, wait, &|| {
            jobs.is_cancelled(job_id)
        })?),
        (Layout::Folder, Collision::Replace) => {
            let name = match &single_root_dir {
                Some(top) if staging.join(top).is_dir() => safe_segment(top),
                _ => stem.clone(),
            };
            Some(crate::destlock::acquire(&parent.join(name), wait, &|| {
                jobs.is_cancelled(job_id)
            })?)
        }
        _ => None,
    };
    // One job at a time decides names and moves files in (moves are renames, so this is quick).
    let _place = PLACE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let output = match opts.layout {
        Layout::Folder => {
            // If the archive already wraps everything in one folder, use that folder
            // instead of nesting it inside another one with the same name.
            let (src, name) = match &single_root_dir {
                Some(top) if staging.join(top).is_dir() => (staging.join(top), safe_segment(top)),
                _ => (staging.clone(), stem.clone()),
            };
            let iso_rel = iso_rel_paths(&src);
            let dlc_rel = crate::dlc::stfs_rel_paths(&src);
            let target = parent.join(&name);
            let exists = fs::symlink_metadata(&target).is_ok();
            match (exists, opts.collision) {
                (true, Collision::Replace) => {
                    merge_replace(&src, &target, &mut warnings).map_err(place_err)?;
                    warnings.extend(outside_note("merge", &target));
                    let isos: Vec<String> = iso_rel
                        .iter()
                        .map(|r| target.join(r).display().to_string())
                        .collect();
                    let iso_tokens = crate::god::register_extracted(&isos, &target, false);
                    return Ok(ExtractResult {
                        output: target.display().to_string(),
                        skipped: false,
                        warnings,
                        files,
                        isos,
                        merged: true,
                        iso_tokens,
                        dlcs: dlc_rel
                            .iter()
                            .map(|r| target.join(r).display().to_string())
                            .collect(),
                        created: false,
                        nested,
                        inner_left,
                    });
                }
                (true, Collision::Skip) => {
                    warnings.extend(outside_note("skip", &target));
                    return Ok(ExtractResult {
                        output: target.display().to_string(),
                        skipped: true,
                        warnings,
                        files: 0,
                        isos: vec![],
                        merged: false,
                        iso_tokens: vec![],
                        dlcs: vec![],
                        created: false,
                        nested: 0,
                        inner_left: 0,
                    });
                }
                _ => {}
            }
            // Claim a brand-new empty folder (atomic), then move the files into it.
            // Never copies into a folder someone else owns.
            let final_target = claim_dir(parent, &name).map_err(place_err)?;
            move_children(&src, &final_target).map_err(place_err)?;
            warnings.extend(outside_note("new", &final_target));
            let isos: Vec<String> = iso_rel
                .iter()
                .map(|r| final_target.join(r).display().to_string())
                .collect();
            // Delete is never allowed in Replace mode: another archive in the batch may merge
            // into this same folder later and swap the file (Overseer round-3 H1).
            let iso_tokens = crate::god::register_extracted(
                &isos,
                &final_target,
                opts.collision != Collision::Replace,
            );
            return Ok(ExtractResult {
                output: final_target.display().to_string(),
                skipped: false,
                warnings,
                files,
                isos,
                merged: false,
                iso_tokens,
                dlcs: dlc_rel
                    .iter()
                    .map(|r| final_target.join(r).display().to_string())
                    .collect(),
                created: true,
                nested,
                inner_left,
            });
        }
        Layout::Here => {
            let items: Vec<_> = fs::read_dir(&staging)
                .map_err(place_err)?
                .flatten()
                .collect();
            for item in items {
                let name = item.file_name().to_string_lossy().into_owned();
                let target = parent.join(&name);
                if fs::symlink_metadata(&target).is_ok() {
                    match opts.collision {
                        Collision::Replace => merge_replace(&item.path(), &target, &mut warnings)
                            .map_err(place_err)?,
                        Collision::Skip => warnings.push(format!("{name} already exists, skipped")),
                        Collision::Number => {
                            let t =
                                place_numbered(&item.path(), parent, &name).map_err(place_err)?;
                            warnings.push(format!(
                                "{name} saved as {}",
                                t.file_name().unwrap().to_string_lossy()
                            ));
                        }
                    }
                } else {
                    let t = place_numbered(&item.path(), parent, &name).map_err(place_err)?;
                    if t != target {
                        warnings.push(format!(
                            "{name} saved as {}",
                            t.file_name().unwrap().to_string_lossy()
                        ));
                    }
                }
            }
            if outside {
                warnings.push(format!(
                    "The extracted files were put in {} (next to the Content folder, not inside it); the GOD game goes into the Content folder.",
                    parent.display()
                ));
            }
            parent.to_path_buf()
        }
    };
    Ok(ExtractResult {
        output: output.display().to_string(),
        skipped: false,
        warnings,
        files,
        isos: vec![],
        merged: false,
        iso_tokens: vec![],
        dlcs: vec![],
        created: false,
        nested,
        inner_left,
    })
}

fn remove_any(p: &Path) -> std::io::Result<()> {
    if is_link(p) {
        remove_link(p)
    } else if p.is_dir() {
        fs::remove_dir_all(p)
    } else {
        fs::remove_file(p)
    }
}

const ASIDE: &str = ".unpack-old.";
const ASIDE_NEW: &str = ".unpack-new.";
const MANIFEST: &str = ".unpack-replace.";

#[cfg(test)]
thread_local! {
    /// Test-only fault injection: "kill" right before step N, leaving the disk exactly as a
    /// SIGKILL there would. replace_unit counts: 1 manifest, 2-3 set old halves aside, 4-5 move
    /// new halves in, 6 mark finishing, 7.. delete asides, then the manifest. recover_asides
    /// counts every write it does (manifest update, rename, delete) the same way.
    pub(crate) static REPLACE_KILL_AT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(crate) static RECOVER_KILL_AT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}
/// Test-only pause hook shared across threads: called with each replace_unit step number.
#[cfg(test)]
#[allow(clippy::type_complexity)]
pub(crate) static REPLACE_PAUSE: std::sync::Mutex<Option<Box<dyn Fn(usize) + Send>>> = std::sync::Mutex::new(None);

struct Steps {
    n: usize,
    #[cfg_attr(not(test), allow(dead_code))]
    recover: bool,
}
impl Steps {
    fn next(&mut self) -> std::io::Result<()> {
        self.n += 1;
        #[cfg(test)]
        {
            if !self.recover {
                if let Some(f) = REPLACE_PAUSE.lock().unwrap().as_ref() {
                    f(self.n);
                }
            }
            let k = if self.recover { RECOVER_KILL_AT.with(|k| k.get()) } else { REPLACE_KILL_AT.with(|k| k.get()) };
            if k == self.n {
                return Err(std::io::Error::other("killed (test)"));
            }
        }
        Ok(())
    }
}

const FOLDER_LOCK: &str = ".unpack-replace.lock";
/// R29: a note larger than this isn't one of mine and is never read.
const MAX_NOTE: u64 = 64 * 1024;
#[cfg_attr(not(windows), allow(dead_code))]
const FOLDER_SIG: &str = "unpack-folder-lock v1";

/// R27-R29: one lock per Content type folder (`<TitleID>\<type>`), held for the whole of
/// recovery and placement there (archive Replace and GOD conversion alike). It is an OS lock,
/// so it holds across processes, Windows sessions and PCs sharing the drive, and the OS drops
/// it if the holder dies.
/// - Windows: a lock on `<type folder>\.unpack-replace.lock`, opened without delete sharing so
///   nobody can delete or rename over it while any job has it open; signed, never deleted.
/// - Unix: a lock on the type folder itself (a folder can't be swapped for a file with the same
///   name while it holds packages, and there's no lock file anyone could unlink).
///
/// R29 M: after the lock is taken, the locked object must still be the one at the path (same
/// volume + file index / dev + inode); otherwise it is reopened and locked again.
pub(crate) fn lock_type_folder(ct_dst: &Path, cancelled: &dyn Fn() -> bool, waiting: &mut dyn FnMut()) -> Result<fs::File, String> {
    if is_link(ct_dst) {
        return Err(format!("{} is a link; nothing was written through it.", ct_dst.display()));
    }
    fs::create_dir_all(ct_dst).map_err(|e| format!("Could not create {} ({e})", ct_dst.display()))?;
    let target = lock_target(ct_dst);
    let mut told = false;
    for _ in 0..1000 {
        if fs::symlink_metadata(&target).is_ok_and(|m| m.file_type().is_symlink() || (m.is_dir() != cfg!(unix))) {
            return Err(format!("{} isn't what I expect (a link?); I didn't touch that folder. Check it.", target.display()));
        }
        let f = open_lock(&target).map_err(|e| format!("Could not open the lock {} ({e})", target.display()))?;
        #[cfg(test)]
        LOCK_RACE.with(|h| {
            if let Some(h) = h.borrow_mut().take() {
                h(ct_dst)
            }
        });
        loop {
            match f.try_lock() {
                Ok(()) => break,
                Err(fs::TryLockError::WouldBlock) => {
                    if !told {
                        waiting();
                        told = true;
                    }
                    if cancelled() {
                        return Err("Cancelled".into());
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(fs::TryLockError::Error(e)) => return Err(format!("Could not lock {} ({e})", target.display())),
            }
        }
        if handle_id(&f).is_some() && handle_id(&f) == path_id(&target) {
            #[cfg(windows)]
            {
                use std::io::{Seek, Write};
                let mut f = &f;
                let _ = f.rewind();
                let _ = writeln!(f, "{FOLDER_SIG} pid={}", std::process::id());
                let _ = f.set_len(f.stream_position().unwrap_or(0));
            }
            return Ok(f);
        }
        // What we locked is no longer what the path names: drop it and lock the current one.
    }
    Err(format!("Could not lock {}: it keeps being replaced.", target.display()))
}

#[cfg(test)]
thread_local! {
    /// Test-only: runs once between opening the lock and locking it (to swap the path).
    #[allow(clippy::type_complexity)]
    pub(crate) static LOCK_RACE: std::cell::RefCell<Option<Box<dyn FnOnce(&Path)>>> = const { std::cell::RefCell::new(None) };
}

fn lock_target(ct_dst: &Path) -> PathBuf {
    if cfg!(windows) { ct_dst.join(FOLDER_LOCK) } else { ct_dst.to_path_buf() }
}

#[cfg(unix)]
fn open_lock(p: &Path) -> std::io::Result<fs::File> {
    fs::File::open(p)
}
#[cfg(unix)]
fn handle_id(f: &fs::File) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    f.metadata().ok().map(|m| (m.dev(), m.ino()))
}
#[cfg(unix)]
fn path_id(p: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    fs::symlink_metadata(p).ok().map(|m| (m.dev(), m.ino()))
}

#[cfg(windows)]
fn open_lock(p: &Path) -> std::io::Result<fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, FILE_SHARE_WRITE};
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(p)
}
#[cfg(windows)]
fn handle_id(f: &fs::File) -> Option<(u64, u64)> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION};
    let mut i: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: a valid open handle and a properly sized out struct.
    if unsafe { GetFileInformationByHandle(f.as_raw_handle() as _, &mut i) } == 0 {
        return None;
    }
    Some((i.dwVolumeSerialNumber as u64, ((i.nFileIndexHigh as u64) << 32) | i.nFileIndexLow as u64))
}
#[cfg(windows)]
fn path_id(p: &Path) -> Option<(u64, u64)> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE};
    let f = fs::OpenOptions::new()
        .access_mode(0x80) // FILE_READ_ATTRIBUTES
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(p)
        .ok()?;
    handle_id(&f)
}

/// R29 M: rename that never replaces what is at `dst` (atomic where the OS can do it):
/// renameat2(RENAME_NOREPLACE) on Linux, MoveFileExW without MOVEFILE_REPLACE_EXISTING on
/// Windows; elsewhere, or where the drive doesn't support it, link + unlink for files and a
/// checked rename for folders (a folder rename never lands on a non-empty folder).
pub(crate) fn rename_noreplace(src: &Path, dst: &Path) -> std::io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::ffi::OsStrExt;
        let (s, d) = (std::ffi::CString::new(src.as_os_str().as_bytes())?, std::ffi::CString::new(dst.as_os_str().as_bytes())?);
        // SAFETY: two valid NUL-terminated paths.
        let r = unsafe { libc::renameat2(libc::AT_FDCWD, s.as_ptr(), libc::AT_FDCWD, d.as_ptr(), libc::RENAME_NOREPLACE) };
        if r == 0 {
            return Ok(());
        }
        let e = std::io::Error::last_os_error();
        if !matches!(e.raw_os_error(), Some(libc::EINVAL) | Some(libc::ENOSYS) | Some(libc::ENOTSUP)) {
            return Err(e);
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let w = |p: &Path| p.as_os_str().encode_wide().chain(Some(0)).collect::<Vec<u16>>();
        let (s, d) = (w(src), w(dst));
        // SAFETY: two valid NUL-terminated wide paths; flags 0 = never replace, same volume only.
        if unsafe { windows_sys::Win32::Storage::FileSystem::MoveFileExW(s.as_ptr(), d.as_ptr(), 0) } != 0 {
            return Ok(());
        }
        return Err(std::io::Error::last_os_error());
    }
    #[allow(unreachable_code)]
    {
        if fs::symlink_metadata(dst).is_ok() {
            return Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, format!("{} already exists", dst.display())));
        }
        if fs::symlink_metadata(src).is_ok_and(|m| m.is_file()) && fs::hard_link(src, dst).is_ok() {
            return fs::remove_file(src);
        }
        if fs::symlink_metadata(dst).is_ok() {
            return Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, format!("{} already exists", dst.display())));
        }
        fs::rename(src, dst)
    }
}

/// Move `src` to a destination that must not exist: never replaces, never merges into an
/// existing folder. Across volumes it copies, and removes a half-made copy on failure.
fn move_new(src: &Path, dst: &Path) -> std::io::Result<()> {
    if fs::symlink_metadata(dst).is_ok() {
        return Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, format!("{} already exists", dst.display())));
    }
    match rename_noreplace(src, dst) {
        Ok(()) => return Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Err(e),
        Err(_) => {}
    }
    if fs::symlink_metadata(dst).is_ok() {
        return Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, format!("{} already exists", dst.display())));
    }
    if let Err(e) = copy_recursive(src, dst) {
        let _ = remove_any(dst);
        return Err(e);
    }
    if src.is_dir() {
        fs::remove_dir_all(src)
    } else {
        fs::remove_file(src)
    }
}

/// R28/R29: what a package half looks like, so undo and recovery only ever remove or move what
/// provably came from this Replace. A header: its size and full SHA-256. A `.data` folder:
/// every entry's relative path and size, and per file the SHA-256 of its first and last MiB
/// (the whole file when it is 2 MiB or less). A full hash of multi-GB parts would add minutes
/// to every Replace; the header (fully hashed) already identifies the package, and a GOD part
/// starts with its hash table, so head + tail + size tells copies apart in practice. Links
/// never match.
fn fingerprint(p: &Path) -> Option<String> {
    use sha2::{Digest, Sha256};
    fn head(p: &Path, all: bool) -> Option<String> {
        use std::io::{Seek, SeekFrom};
        const PART: u64 = 1 << 20;
        let mut f = fs::File::open(p).ok()?;
        let len = f.metadata().ok()?.len();
        let mut h = Sha256::new();
        let mut buf = vec![];
        if all || len <= 2 * PART {
            f.read_to_end(&mut buf).ok()?;
        } else {
            (&mut f).take(PART).read_to_end(&mut buf).ok()?;
            f.seek(SeekFrom::Start(len - PART)).ok()?;
            f.take(PART).read_to_end(&mut buf).ok()?;
        }
        h.update(&buf);
        Some(format!("{:x}", h.finalize()))
    }
    fn walk(base: &Path, d: &Path, out: &mut Vec<String>) -> Option<()> {
        let mut es: Vec<_> = fs::read_dir(d).ok()?.flatten().collect();
        es.sort_by_key(|e| e.file_name());
        for e in es {
            let md = fs::symlink_metadata(e.path()).ok()?;
            let rel = e.path().strip_prefix(base).ok()?.to_string_lossy().replace('\\', "/");
            if md.file_type().is_symlink() {
                return None;
            } else if md.is_dir() {
                out.push(format!("d {rel}"));
                walk(base, &e.path(), out)?;
            } else {
                out.push(format!("f {rel} {} {}", md.len(), head(&e.path(), false)?));
            }
        }
        Some(())
    }
    let md = fs::symlink_metadata(p).ok()?;
    if md.file_type().is_symlink() || is_link(p) {
        return None;
    }
    if md.is_file() {
        return Some(format!("file {} {}", md.len(), head(p, true)?));
    }
    let mut out = vec![];
    walk(p, p, &mut out)?;
    let mut h = Sha256::new();
    h.update(out.join("\n").as_bytes());
    Some(format!("dir {} {:x}", out.len(), h.finalize()))
}

/// R27/R28: the intent record of one Replace, written (atomically) before anything is renamed.
/// It names every old half with its aside name and every new half with its live and
/// set-aside names and its fingerprint, so recovery knows what is old and what is new.
#[derive(Serialize, serde::Deserialize, Clone)]
struct ReplaceManifest {
    /// "replacing" -> (recovery) "undoing", or (all new halves in) "finishing".
    phase: String,
    /// (live name, aside name, is .data)
    old: Vec<(String, String, bool)>,
    /// (live name, set-aside name used if this has to be undone, is .data, fingerprint)
    new: Vec<(String, String, bool, String)>,
}

fn write_manifest(path: &Path, m: &ReplaceManifest) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, serde_json::to_vec(m).map_err(std::io::Error::other)?)?;
    fs::rename(&tmp, path)
}

fn is_data_name(n: &str) -> bool {
    n.len() == 13 && n.is_char_boundary(8) && n[8..].eq_ignore_ascii_case(".data") && crate::contentdir::is_id8(&n[..8])
}

/// R28 M: a note is acted on only when it is exactly what replace_unit writes: its own tag in
/// every aside name, live names of one MediaID (`<id8>` / `<id8>.data`, flagged correctly), at
/// most one old half of each kind, both new halves, and a known phase. Returns the MediaID.
fn validate_manifest(m: &ReplaceManifest, tag: &str) -> Result<String, String> {
    if !["replacing", "undoing", "finishing"].contains(&m.phase.as_str()) {
        return Err(format!("its phase \"{}\" is unknown", m.phase));
    }
    let ok_live = |n: &str, data: bool| if data { is_data_name(n) } else { n.len() == 8 && crate::contentdir::is_id8(n) };
    let mut ids = std::collections::BTreeSet::new();
    for (live, aside, data) in &m.old {
        if !ok_live(live, *data) || *aside != format!("{ASIDE}{tag}.{live}") {
            return Err(format!("its entry for {live} doesn't match what I write"));
        }
        ids.insert(live[..8].to_ascii_lowercase());
    }
    for (live, aside, data, _) in &m.new {
        if !ok_live(live, *data) || *aside != format!("{ASIDE_NEW}{tag}.{live}") {
            return Err(format!("its entry for {live} doesn't match what I write"));
        }
        ids.insert(live[..8].to_ascii_lowercase());
    }
    let kinds = |v: Vec<bool>| v.iter().filter(|d| **d).count() <= 1 && v.iter().filter(|d| !**d).count() <= 1;
    if ids.len() != 1
        || m.new.len() != 2
        || !kinds(m.old.iter().map(|o| o.2).collect())
        || !kinds(m.new.iter().map(|o| o.2).collect())
    {
        return Err("it doesn't describe exactly one package".into());
    }
    Ok(ids.into_iter().next().unwrap())
}

/// A fresh tag n free for the manifest and every aside name of this Replace.
fn fresh_tag(dir: &Path, names: &[String]) -> Option<String> {
    (0..1000).map(|n| n.to_string()).find(|t| {
        fs::symlink_metadata(dir.join(format!("{MANIFEST}{t}.json"))).is_err()
            && fs::symlink_metadata(dir.join(format!("{MANIFEST}{t}.tmp"))).is_err()
            && names.iter().all(|nm| {
                fs::symlink_metadata(dir.join(format!("{ASIDE}{t}.{nm}"))).is_err()
                    && fs::symlink_metadata(dir.join(format!("{ASIDE_NEW}{t}.{nm}"))).is_err()
            })
    })
}

fn name_of(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// R24-R28: Replace one package, all or nothing, under the type folder's lock. The manifest is
/// written first; then the old halves are renamed aside, the new pair moved in (never onto
/// anything that exists), the manifest marked "finishing" and the asides deleted. Any error
/// undoes what was done, removing only what still matches what it moved in, and never putting
/// an old half back over something; a kill in between is repaired by `recover_asides`.
fn replace_unit(ct_dst: &Path, old: [Option<PathBuf>; 2], new: [(&PathBuf, &PathBuf); 2]) -> Result<Vec<String>, String> {
    let old: Vec<PathBuf> = old.into_iter().flatten().collect();
    let mut all_names: Vec<String> = old.iter().map(|p| name_of(p)).collect();
    all_names.extend(new.iter().map(|(_, d)| name_of(d)));
    let tag = fresh_tag(ct_dst, &all_names).ok_or_else(|| format!("no free name to set the old copy aside in {}", ct_dst.display()))?;
    let mpath = ct_dst.join(format!("{MANIFEST}{tag}.json"));
    let mut fps = vec![];
    for (s, _) in new {
        fps.push(fingerprint(s).ok_or_else(|| format!("couldn't read {}", s.display()))?);
    }
    let mut m = ReplaceManifest {
        phase: "replacing".into(),
        old: old.iter().map(|p| (name_of(p), format!("{ASIDE}{tag}.{}", name_of(p)), is_data_name(&name_of(p)))).collect(),
        new: new
            .iter()
            .zip(&fps)
            .map(|((_, d), fp)| (name_of(d), format!("{ASIDE_NEW}{tag}.{}", name_of(d)), is_data_name(&name_of(d)), fp.clone()))
            .collect(),
    };
    let mut st = Steps { n: 0, recover: false };
    let ioe = |e: std::io::Error| e.to_string();
    st.next().map_err(ioe)?;
    write_manifest(&mpath, &m).map_err(|e| format!("couldn't write {} ({e})", mpath.display()))?;
    let mut aside: Vec<(PathBuf, PathBuf)> = vec![];
    let undo = |aside: &[(PathBuf, PathBuf)], moved: &[(&PathBuf, &String)], e: String| -> String {
        let mut bad = vec![];
        for (d, fp) in moved {
            if fingerprint(d).as_ref() != Some(*fp) {
                bad.push(format!("{} is no longer what I moved there, so I left it", d.display()));
            } else if let Err(x) = remove_any(d) {
                bad.push(format!("couldn't remove {} ({x})", d.display()));
            }
        }
        for (o, a) in aside.iter().rev() {
            if fs::symlink_metadata(o).is_ok() {
                bad.push(format!("the old {} is still at {} because something else is in its place", o.display(), a.display()));
            } else if let Err(x) = fs::rename(a, o) {
                bad.push(format!("the old {} is still at {} ({x}); rename it back", o.display(), a.display()));
            }
        }
        if bad.is_empty() {
            let _ = fs::remove_file(&mpath);
            format!("{e}; the existing package was left as it was")
        } else {
            format!("{e}; putting the existing package back also failed: {}", bad.join("; "))
        }
    };
    for (o, (_, an, _)) in old.iter().zip(&m.old) {
        if let Err(e) = st.next() {
            return Err(e.to_string());
        }
        let a = ct_dst.join(an);
        if let Err(e) = fs::rename(o, &a) {
            return Err(undo(&aside, &[], format!("couldn't set {} aside ({e})", o.display())));
        }
        aside.push((o.clone(), a));
    }
    st.n = 3;
    let mut moved: Vec<(&PathBuf, &String)> = vec![];
    for ((src, dst), fp) in new.into_iter().zip(&fps) {
        if let Err(e) = st.next() {
            return Err(e.to_string());
        }
        if let Err(e) = move_new(src, dst) {
            return Err(undo(&aside, &moved, format!("couldn't move {} in ({e})", dst.display())));
        }
        moved.push((dst, fp));
    }
    st.next().map_err(ioe)?;
    m.phase = "finishing".into();
    write_manifest(&mpath, &m).map_err(|e| format!("couldn't update {} ({e})", mpath.display()))?;
    let mut warns = vec![];
    for (_, a) in aside {
        st.next().map_err(ioe)?;
        if let Err(e) = remove_any(&a) {
            warns.push(format!("The replaced copy {} couldn't be deleted ({e}); you can delete it.", a.display()));
        }
    }
    st.next().map_err(ioe)?;
    let _ = fs::remove_file(&mpath);
    Ok(warns)
}

fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 { one.into() } else { many.into() }
}

/// R25-R28: repair Replaces that were interrupted, from their manifests. Caller holds the type
/// folder's lock (across processes), so no live Replace is ever touched. Every step is safe to
/// crash and re-run:
/// - "finishing": the new pair is in; delete the old copies that are left.
/// - "replacing" with every new half in place (and matching its fingerprint): same as finishing.
/// - otherwise ("undoing"): set every new half that arrived (matching its fingerprint) aside,
///   then put the old halves back. If an old half's place is taken, stop and report.
///
/// Notes that don't validate, asides without a note, and stray note temp files that aren't
/// provably ours are only reported.
fn recover_asides(ct_dst: &Path, warnings: &mut Vec<String>) {
    let Ok(rd) = fs::read_dir(ct_dst) else { return };
    let names: Vec<String> = rd.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    let mut known: std::collections::HashSet<String> = Default::default();
    let mut st = Steps { n: 0, recover: true };
    for tname in names.iter().filter(|n| n.starts_with(MANIFEST) && n.ends_with(".tmp")) {
        // Under the lock nobody is writing a note, so a temp note is left from a kill.
        let p = ct_dst.join(tname);
        let tag = &tname[MANIFEST.len()..tname.len() - 4];
        let ours = fs::symlink_metadata(&p).is_ok_and(|m| m.is_file() && m.len() <= MAX_NOTE)
            && fs::read(&p).ok().and_then(|b| serde_json::from_slice::<ReplaceManifest>(&b).ok()).is_some_and(|m| validate_manifest(&m, tag).is_ok());
        if !(ours && fs::remove_file(&p).is_ok()) {
            warnings.push(format!("{} looks left over from an interrupted Replace; it was not touched, delete it if you don't need it.", p.display()));
        }
    }
    for mname in names.iter().filter(|n| n.starts_with(MANIFEST) && n.ends_with(".json")) {
        let mpath = ct_dst.join(mname);
        let tag = &mname[MANIFEST.len()..mname.len() - 5];
        let m: Option<ReplaceManifest> =
            if fs::symlink_metadata(&mpath).is_ok_and(|m| m.is_file() && m.len() <= MAX_NOTE) { fs::read(&mpath).ok().and_then(|b| serde_json::from_slice(&b).ok()) } else { None };
        let Some(mut m) = m else {
            warnings.push(format!("{} (a note from an interrupted Replace) couldn't be read; nothing was changed, check that folder.", mpath.display()));
            continue;
        };
        let id = match validate_manifest(&m, tag) {
            Ok(id) => id,
            Err(why) => {
                warnings.push(format!("{} (a note from an interrupted Replace) isn't one I can trust ({why}); nothing was changed, check that folder.", mpath.display()));
                continue;
            }
        };
        known.extend(m.old.iter().map(|(_, a, _)| a.clone()));
        if let Err(e) = recover_one(ct_dst, &mpath, &mut m, &id, &mut st, warnings) {
            warnings.push(format!(
                "Repairing an interrupted Replace in {} stopped ({e}); I'll try again next time, nothing was lost.",
                ct_dst.display()
            ));
            return;
        }
    }
    let stray: Vec<String> = names.iter().filter(|n| n.starts_with(ASIDE) && !known.contains(*n)).cloned().collect();
    if !stray.is_empty() {
        let paths = stray.iter().map(|n| ct_dst.join(n).display().to_string()).collect::<Vec<_>>().join(", ");
        warnings.push(format!(
            "{paths} {} left over from an earlier interrupted Replace; {} not touched, check {} and delete {} if you don't need {}.",
            plural(stray.len(), "is", "are"),
            plural(stray.len(), "it was", "they were"),
            plural(stray.len(), "it", "them"),
            plural(stray.len(), "it", "them"),
            plural(stray.len(), "it", "them"),
        ));
    }
}

fn recover_one(
    ct_dst: &Path,
    mpath: &Path,
    m: &mut ReplaceManifest,
    id: &str,
    st: &mut Steps,
    warnings: &mut Vec<String>,
) -> std::io::Result<()> {
    let there = |n: &str| fs::symlink_metadata(ct_dst.join(n)).is_ok();
    let live = |data: bool| -> Option<PathBuf> {
        fs::read_dir(ct_dst).ok()?.flatten().map(|e| e.path()).find(|p| {
            let n = name_of(p);
            !n.starts_with('.') && n.get(..8).is_some_and(|x| x.eq_ignore_ascii_case(id)) && if data { is_data_name(&n) } else { n.len() == 8 }
        })
    };
    let old_aside = |data: bool| m.old.iter().find(|o| o.2 == data).map(|o| there(&o.1));
    // A live half of kind `data` is the NEW one when it matches the fingerprint in the note,
    // its old counterpart is still aside (or there was none), and it isn't set aside already.
    let is_new_live = |data: bool, new_aside: &str, fp: &str| {
        live(data).is_some_and(|p| fingerprint(&p).as_deref() == Some(fp)) && !there(new_aside) && old_aside(data).is_none_or(|aside_there| aside_there)
    };
    let unit = ct_dst.join(id).display().to_string();
    let finish = |m: &ReplaceManifest, st: &mut Steps, warnings: &mut Vec<String>| -> std::io::Result<()> {
        let mut removed = 0;
        for (_, a, _) in &m.old {
            if there(a) {
                st.next()?;
                remove_any(&ct_dst.join(a))?;
                removed += 1;
            }
        }
        st.next()?;
        fs::remove_file(mpath)?;
        if removed > 0 {
            warnings.push(format!("An earlier Replace of {unit} had finished moving the new copy in; I removed the old copy it had set aside."));
        }
        Ok(())
    };
    if m.phase == "finishing" {
        return finish(m, st, warnings);
    }
    if m.phase == "replacing" {
        if m.new.iter().all(|(_, na, d, fp)| is_new_live(*d, na, fp)) {
            st.next()?;
            m.phase = "finishing".into();
            write_manifest(mpath, m)?;
            return finish(m, st, warnings);
        }
        st.next()?;
        m.phase = "undoing".into();
        write_manifest(mpath, m)?;
    }
    // undoing
    for (_, na, d, fp) in m.new.clone() {
        if is_new_live(d, &na, &fp) {
            let p = live(d).unwrap();
            st.next()?;
            fs::rename(&p, ct_dst.join(&na))?;
        }
    }
    let mut back = vec![];
    for (orig, a, d) in m.old.clone() {
        if !there(&a) {
            continue;
        }
        if let Some(p) = live(d) {
            warnings.push(format!(
                "An earlier Replace of {unit} was interrupted, but {} is taken by something that isn't from that Replace. I left the old copy at {} untouched; check them yourself.",
                p.display(),
                ct_dst.join(&a).display()
            ));
            return Ok(());
        }
        st.next()?;
        fs::rename(ct_dst.join(&a), ct_dst.join(&orig))?;
        back.push(orig);
    }
    st.next()?;
    fs::remove_file(mpath)?;
    if back.is_empty() && !m.new.iter().any(|(_, na, _, _)| there(na)) {
        // Killed before anything was renamed: the folder is as it was.
        return Ok(());
    }
    let arrived: Vec<String> = m.new.iter().filter(|(_, na, _, _)| there(na)).map(|(_, na, _, _)| ct_dst.join(na).display().to_string()).collect();
    let mut msg = format!(
        "An earlier Replace of {unit} was interrupted before the new copy was fully in place, so I put the old package back{}.",
        if back.is_empty() { String::new() } else { format!(" ({})", back.join(" and ")) }
    );
    if !arrived.is_empty() {
        msg.push_str(&format!(
            " The part of the new copy that had arrived {} set aside as {}; delete {} if you don't need {}.",
            plural(arrived.len(), "was", "were"),
            arrived.join(" and "),
            plural(arrived.len(), "it", "them"),
            plural(arrived.len(), "it", "them"),
        ));
    }
    warnings.push(msg);
    Ok(())
}

/// Move the GOD `<TitleID>` trees found in `staging` into the Content profile folder `profile`,
/// dropping any wrapper folder. An existing `<TitleID>` folder is merged package by package
/// (`<ContentType>\<MediaID>` and its `.data` folder): new packages are added (e.g. Disc 2 next
/// to Disc 1), a package that is already there is skipped, or replaced only with Replace.
#[allow(clippy::too_many_arguments)]
fn place_title_trees(
    staging: &Path,
    trees: &[PathBuf],
    profile: &Path,
    collision: Collision,
    files: usize,
    nested: u32,
    inner_left: u32,
    mut warnings: Vec<String>,
    jobs: &Jobs,
    job_id: &str,
    progress: &mut dyn FnMut(u32, &str),
) -> Result<ExtractResult, String> {
    let place_err = |e: std::io::Error| format!("Could not move files into place: {e}");
    fs::create_dir_all(profile).map_err(place_err)?;
    let _dest_lock = crate::destlock::acquire(
        profile,
        || progress(100, crate::destlock::WAIT_MESSAGE),
        &|| jobs.is_cancelled(job_id),
    )?;
    let _place = PLACE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let in_trees: usize = trees.iter().map(|t| count_files(t)).sum();
    let left = count_files(staging).saturating_sub(in_trees);
    let mut merged = false;
    let mut placed = 0usize;
    let mut stray = 0usize;
    let (mut present, mut incomplete, mut half) = (0usize, 0usize, 0usize);
    let mut outputs = vec![];
    let is_hex8 = |n: &str| crate::contentdir::is_id8(n);
    let there = |p: &Path| fs::symlink_metadata(p).is_ok();
    for tree in trees {
        let tid = tree.file_name().unwrap().to_string_lossy().to_uppercase();
        let target = profile.join(&tid);
        if is_link(&target) {
            return Err(format!(
                "{} is a link (shortcut to another folder); nothing was written through it.",
                target.display()
            ));
        }
        outputs.push(target.clone());
        if there(&target) {
            merged = true;
        }
        for ct in fs::read_dir(tree).map_err(place_err)?.flatten() {
            let ct_name = ct.file_name().to_string_lossy().into_owned();
            let ct_real_dir = fs::symlink_metadata(ct.path()).is_ok_and(|m| m.is_dir());
            // R23 L3: only <8 hex content type> folders belong in a TitleID folder.
            if !ct_real_dir || !is_hex8(&ct_name) {
                stray += count_files(&ct.path()).max(1);
                warnings.push(format!(
                    "{tid}\\{ct_name} is not a GOD content-type folder; it was left out of the Content folder."
                ));
                continue;
            }
            let ct_dst = target.join(&ct_name);
            if is_link(&ct_dst) {
                warnings.push(format!("{} is a link; skipped", ct_dst.display()));
                continue;
            }
            // R23 M2: a package is the <MediaID> header plus its <MediaID>.data folder, as a unit.
            // R24 L2: keep the real names (".DATA" etc.) and match by MediaID case-insensitively.
            let mut ids: std::collections::BTreeMap<String, (Option<PathBuf>, Option<PathBuf>)> = Default::default();
            for pkg in fs::read_dir(ct.path()).map_err(place_err)?.flatten() {
                let name = pkg.file_name().to_string_lossy().into_owned();
                let md = fs::symlink_metadata(pkg.path()).ok();
                let hdr = md.as_ref().is_some_and(|m| m.is_file()) && is_hex8(&name);
                let data = md.as_ref().is_some_and(|m| m.is_dir())
                    && name.len() == 13
                    && name.is_char_boundary(8)
                    && name[8..].eq_ignore_ascii_case(".data")
                    && is_hex8(&name[..8]);
                if hdr {
                    ids.entry(name.to_ascii_lowercase()).or_default().0 = Some(pkg.path());
                } else if data {
                    ids.entry(name[..8].to_ascii_lowercase()).or_default().1 = Some(pkg.path());
                } else {
                    stray += count_files(&pkg.path()).max(1);
                    warnings.push(format!(
                        "{tid}\\{ct_name}\\{name} is not part of a GOD package; it was left out of the Content folder."
                    ));
                }
            }
            // R27: recovery and placement in this type folder run under its lock.
            let _held = lock_type_folder(&ct_dst, &|| jobs.is_cancelled(job_id), &mut || progress(100, crate::destlock::WAIT_MESSAGE))?;
            recover_asides(&ct_dst, &mut warnings);
            // What is already on the drive, by lowercase MediaID, with its real paths.
            let mut on_drive: std::collections::HashMap<String, (Option<PathBuf>, Option<PathBuf>)> = Default::default();
            if let Ok(rd) = fs::read_dir(&ct_dst) {
                for e in rd.flatten() {
                    let n = e.file_name().to_string_lossy().to_ascii_lowercase();
                    if n.starts_with('.') {
                        continue;
                    }
                    if n.len() == 8 {
                        on_drive.entry(n).or_default().0 = Some(e.path());
                    } else if n.len() == 13 && n.ends_with(".data") && n.is_char_boundary(8) {
                        on_drive.entry(n[..8].to_string()).or_default().1 = Some(e.path());
                    }
                }
            }
            for (key, (sh, sd)) in ids {
                let id = sh
                    .as_ref()
                    .or(sd.as_ref())
                    .and_then(|p| p.file_name())
                    .map(|n| n.to_string_lossy()[..8].to_string())
                    .unwrap_or(key.clone());
                let unit = format!("{tid}\\{ct_name}\\{id}");
                let (Some(sh), Some(sd)) = (sh, sd) else {
                    half += 1;
                    warnings.push(format!(
                        "{unit}: the archive holds only half of this package (header and .data folder belong together); it was not copied."
                    ));
                    continue;
                };
                let data_name = sd.file_name().map(|n| n.to_os_string()).unwrap_or_default();
                let (old_h, old_d) = on_drive.remove(&key).unwrap_or_default();
                let (has_h, has_d) = (old_h.is_some(), old_d.is_some());
                let (dh, dd) = (ct_dst.join(&id), ct_dst.join(&data_name));
                if !has_h && !has_d {
                    fs::create_dir_all(&ct_dst).map_err(place_err)?;
                    move_new(&sh, &dh).map_err(place_err)?;
                    if let Err(e) = move_new(&sd, &dd) {
                        let _ = move_new(&dh, &sh);
                        return Err(place_err(e));
                    }
                    placed += 1;
                } else if collision == Collision::Replace {
                    let w = replace_unit(&ct_dst, [old_h, old_d], [(&sh, &dh), (&sd, &dd)])
                        .map_err(|e| format!("Could not replace {unit}: {e}"))?;
                    warnings.extend(w);
                    placed += 1;
                    warnings.push(format!("{unit} replaced (header and .data)"));
                } else if has_h && has_d {
                    present += 1;
                    warnings.push(format!(
                        "Skipped: {unit} (same MediaID) is already in {}. Nothing was changed.",
                        profile.display()
                    ));
                } else {
                    incomplete += 1;
                    warnings.push(format!(
                        "Skipped: {unit} in {} is incomplete (only its {}); it was left as it is. Use \"Replace files\" to put the whole package from this archive in its place.",
                        profile.display(),
                        if has_h { "header, no .data folder" } else { ".data folder, no header" }
                    ));
                }
            }
        }
    }
    let left = left + stray;
    if left > 0 {
        warnings.push(format!(
            "{left} other file(s) that aren't part of a GOD package were not copied into the Content folder."
        ));
    }
    // R24 L3: only claim "already there" when that is the whole story.
    if placed == 0 {
        warnings.push(if present > 0 && incomplete == 0 && half == 0 {
            "Nothing new was added: every package was already there.".into()
        } else {
            "Nothing was added to the Content folder.".into()
        });
    }
    let output = match outputs.as_slice() {
        [one] => one.clone(),
        _ => profile.to_path_buf(),
    };
    Ok(ExtractResult {
        output: output.display().to_string(),
        skipped: placed == 0,
        warnings,
        files,
        isos: vec![],
        merged,
        iso_tokens: vec![],
        dlcs: vec![],
        created: false,
        nested,
        inner_left,
    })
}

/// Archives opened inside an extracted archive, and archives found but left closed.
struct Inner {
    opened: u32,
    left: u32,
}

const MAX_NESTED_DEPTH: u32 = 3;
/// Across the whole extraction, so a pack of packs (or a bomb) cannot make Unpack open thousands.
const MAX_NESTED_ARCHIVES: u32 = 40;
/// Everything opened from inner archives may add up to this many times what the outer archive
/// unpacked to (but at least NESTED_TOTAL_FLOOR), so a tiny bomb cannot fill the drive (R15 M2).
const NESTED_TOTAL_FACTOR: u64 = 4;
const NESTED_TOTAL_FLOOR: u64 = 512 << 20;
/// An inner archive whose unpacked size is more than this many times its own size (and over
/// NESTED_RATIO_MIN_BYTES) is treated as a bomb and left closed.
const NESTED_MAX_RATIO: u64 = 100;
const NESTED_RATIO_MIN_BYTES: u64 = 16 << 20;
/// Space left free on the drive after opening an inner archive.
const NESTED_FREE_MARGIN: u64 = 256 << 20;

/// Why an inner archive must stay closed, judged from its listing before anything is written.
fn nested_refusal(
    packed: u64,
    listing: &Listing,
    used: u64,
    budget: u64,
    free: Option<u64>,
) -> Result<u64, String> {
    let files: Vec<&Entry> = listing.entries.iter().filter(|e| !e.dir).collect();
    if files.len() > NESTED_MAX_FILES {
        return Err(format!(
            "it holds {} files (more than {NESTED_MAX_FILES})",
            files.len()
        ));
    }
    let unpacked = files.iter().fold(0u64, |a, e| a.saturating_add(e.size));
    if unpacked == 0 && !files.is_empty() && packed > 4096 {
        return Err("its unpacked size can't be read".into());
    }
    if unpacked > NESTED_RATIO_MIN_BYTES && unpacked / packed.max(1) > NESTED_MAX_RATIO {
        return Err(format!(
            "it would unpack to {} from {} (looks like a zip bomb)",
            human(unpacked),
            human(packed)
        ));
    }
    if used.saturating_add(unpacked) > budget {
        return Err(format!(
            "inner archives would unpack to more than {} in total",
            human(budget)
        ));
    }
    if let Some(f) = free {
        if f < unpacked.saturating_add(NESTED_FREE_MARGIN) {
            return Err(format!(
                "there isn't enough free space ({} needed, {} free)",
                human(unpacked),
                human(f)
            ));
        }
    }
    Ok(unpacked)
}

fn human(b: u64) -> String {
    if b >= 1 << 30 {
        format!("{:.1} GB", b as f64 / (1u64 << 30) as f64)
    } else if b >= 1 << 20 {
        format!("{:.1} MB", b as f64 / (1u64 << 20) as f64)
    } else {
        format!("{} KB", b.div_ceil(1024))
    }
}

/// Archive files Unpack opens on its own inside an extracted tree (a narrower list than
/// `is_archive_name`: no `.cab`/`.wim`/... that games ship as data).
pub fn is_nested_archive(name: &str) -> bool {
    let l = name.to_lowercase();
    [".zip", ".rar", ".7z", ".tar", ".tgz", ".gz", ".xz", ".bz2"]
        .iter()
        .any(|e| l.ends_with(e))
}

fn find_nested(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() && !is_link(&p) {
                stack.push(p);
            } else if ft.is_file() && is_nested_archive(&e.file_name().to_string_lossy()) {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// Open the archives found in `root` (the staging folder) in place: each `x/inner.zip` becomes a
/// sibling folder `x/inner`, then that folder is searched in turn (depth <= 3, <= 40 archives in
/// all). Each one is listed first and left closed if it would unpack to more than 100x its size,
/// push the inner total past 4x the outer archive's unpacked size (512 MB minimum), or not fit in
/// the free space; what was really written is checked again afterwards. The inner archive stays where it is. Inner extraction uses the same rules as the outer
/// one (no links, sanitised, junk skipped) and runs inside the staging folder, so a cancel or a
/// failure leaves nothing behind. An inner archive that needs a password or is damaged is left closed.
fn open_inner(
    exe: &Path,
    root: &Path,
    open: bool,
    jobs: &Jobs,
    job_id: &str,
    progress: &mut dyn FnMut(u32, &str),
    warnings: &mut Vec<String>,
) -> Result<Inner, String> {
    let mut inner = Inner { opened: 0, left: 0 };
    let budget = tree_bytes(root)
        .saturating_mul(NESTED_TOTAL_FACTOR)
        .max(NESTED_TOTAL_FLOOR);
    let mut used = 0u64;
    let mut level = vec![root.to_path_buf()];
    for depth in 1..=MAX_NESTED_DEPTH + 1 {
        let mut next = Vec::new();
        for dir in &level {
            for archive in find_nested(dir) {
                if !open || depth > MAX_NESTED_DEPTH || inner.opened >= MAX_NESTED_ARCHIVES {
                    inner.left += 1;
                    continue;
                }
                let rel = archive
                    .strip_prefix(root)
                    .unwrap_or(&archive)
                    .display()
                    .to_string();
                if jobs.is_cancelled(job_id) {
                    return Err(ERR_CANCELLED.into());
                }
                let parent = archive.parent().unwrap_or(root);
                let packed = fs::metadata(&archive).map(|m| m.len()).unwrap_or(0);
                let listed = match list(exe, &archive, None) {
                    Ok(l) => {
                        nested_refusal(packed, &l, used, budget, crate::god::free_space(parent))
                    }
                    Err(e) if e == ERR_PASSWORD_REQUIRED || e == ERR_WRONG_PASSWORD => {
                        Err("it needs a password".into())
                    }
                    Err(e) => Err(format!("it could not be read ({e})")),
                };
                let expected = match listed {
                    Ok(n) => n,
                    Err(why) => {
                        inner.left += 1;
                        warnings.push(format!("{rel} was left closed: {why}."));
                        continue;
                    }
                };
                let out = claim_dir(parent, &archive_stem(&archive))
                    .map_err(|e| format!("Could not open {rel}: {e}"))?;
                let mut out_arg = std::ffi::OsString::from("-o");
                out_arg.push(out.as_os_str());
                let args: Vec<std::ffi::OsString> = vec![
                    "x".into(),
                    "-y".into(),
                    "-bso0".into(),
                    "-bse2".into(),
                    "-bsp1".into(),
                    "-snl-".into(),
                    "-sccUTF-8".into(),
                    pw_arg(None).into(),
                    out_arg,
                    "--".into(),
                    archive.as_os_str().to_owned(),
                ];
                progress(99, &format!("Opening inner archive {rel}"));
                // Live cap: never trust the listed size (a .gz footer is forgeable). Stop 7-Zip as soon
                // as the output passes twice the listed size, the remaining budget, or free space.
                let free_cap = crate::god::free_space(parent)
                    .map(|f| f.saturating_sub(NESTED_FREE_MARGIN))
                    .unwrap_or(u64::MAX);
                let limit = expected
                    .saturating_mul(2)
                    .saturating_add(1 << 20)
                    .min(budget.saturating_sub(used))
                    .min(free_cap);
                match run_capped(
                    exe,
                    &args,
                    jobs,
                    job_id,
                    false,
                    &mut |_, f| progress(99, &format!("{rel}: {f}")),
                    Some((out.clone(), limit)),
                ) {
                    Ok(tolerated) => {
                        // The listing can understate (e.g. a forged header): check what was really written.
                        let (actual, nfiles) = tree_stats(&out);
                        if nfiles > NESTED_MAX_FILES {
                            let _ = fs::remove_dir_all(&out);
                            inner.left += 1;
                            warnings.push(format!("{rel} was left closed: it unpacked to {nfiles} files, more than {NESTED_MAX_FILES}."));
                            continue;
                        }
                        if actual > expected.saturating_mul(2).saturating_add(1 << 20)
                            || used.saturating_add(actual) > budget
                        {
                            let _ = fs::remove_dir_all(&out);
                            inner.left += 1;
                            warnings.push(format!("{rel} was left closed: it unpacked to {}, more than its listing or the size limit allows.", human(actual)));
                            continue;
                        }
                        used = used.saturating_add(actual);
                        warnings.extend(tolerated);
                        warnings.extend(sanitize_tree(&out));
                        remove_junk(&out);
                        inner.opened += 1;
                        next.push(out);
                    }
                    Err(e) if e == ERR_CANCELLED => {
                        let _ = fs::remove_dir_all(&out);
                        return Err(e);
                    }
                    Err(e) if e == ERR_SIZE_CAP => {
                        let _ = fs::remove_dir_all(&out);
                        inner.left += 1;
                        warnings.push(format!("{rel} was left closed: it unpacked past {} or {NESTED_MAX_FILES} files (more than its listing or the limits allow), so Unpack stopped it.", human(limit)));
                    }
                    Err(e) => {
                        let _ = fs::remove_dir_all(&out);
                        inner.left += 1;
                        warnings.push(if e == ERR_PASSWORD_REQUIRED || e == ERR_WRONG_PASSWORD {
                            format!("{rel} needs a password, so it was left closed.")
                        } else {
                            format!("{rel} could not be opened ({e}); it was left closed.")
                        });
                    }
                }
            }
        }
        if next.is_empty() {
            break;
        }
        level = next;
    }
    Ok(inner)
}

/// Temp list file naming exactly one entry. 7-Zip reads names from it literally
/// (`-i@file -scsUTF-8 -spd`), so names with `"`, spaces, a leading `-` or `*` are
/// requested exactly — no command-line quoting involved on any OS.
struct ListFile(PathBuf);
impl ListFile {
    fn new(entry: &str) -> Result<ListFile, String> {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!(
            "unpack-entry-{}-{}.txt",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        fs::write(&p, format!("{entry}\n")).map_err(|e| format!("Cannot write temp file: {e}"))?;
        Ok(ListFile(p))
    }
    fn arg(&self) -> std::ffi::OsString {
        let mut a = std::ffi::OsString::from("-i@");
        a.push(self.0.as_os_str());
        a
    }
}
impl Drop for ListFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// 7-Zip exits 0 with no output when nothing matches; make that an error instead of an empty file.
fn ensure_entry(
    exe: &Path,
    archive: &Path,
    entry: &str,
    password: Option<&str>,
) -> Result<(), String> {
    let listing = list(exe, archive, password)?;
    let want = clean_entry_path(entry);
    match listing.entries.iter().find(|e| e.path == want) {
        Some(e) if e.dir => Err(format!("{entry} is a folder, not a file.")),
        Some(_) => Ok(()),
        None => Err(format!("{entry} was not found in the archive.")),
    }
}

fn entry_command(exe: &Path, archive: &Path, list: &ListFile, password: Option<&str>) -> Command {
    let mut cmd = base_command(exe);
    add_args(
        &mut cmd,
        [
            std::ffi::OsString::from("e"),
            "-so".into(),
            "-spd".into(), // no wildcards
            "-scsUTF-8".into(),
            "-sccUTF-8".into(),
            pw_arg(password).into(),
            list.arg(),
            "--".into(),
            archive.as_os_str().into(),
        ],
    );
    cmd
}

/// Read one entry's bytes (for previews), up to `max` bytes.
pub fn read_entry(
    exe: &Path,
    archive: &Path,
    entry: &str,
    password: Option<&str>,
    max: usize,
) -> Result<Vec<u8>, String> {
    let had_pw = password.is_some_and(|p| !p.is_empty());
    check_password(password)?;
    ensure_entry(exe, archive, entry, password)?;
    let lf = ListFile::new(entry)?;
    let mut child = entry_command(exe, archive, &lf, password)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Could not start 7-Zip: {e}"))?;
    // drain stderr concurrently so a chatty 7-Zip can't block on a full pipe
    let mut stderr = child.stderr.take().unwrap();
    let err_thread = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });
    let mut out = Vec::new();
    let mut stdout = child.stdout.take().unwrap();
    let mut buf = [0u8; 65536];
    let mut truncated = false;
    loop {
        let n = stdout.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        if out.len() + n > max {
            truncated = true;
            break;
        }
        out.extend_from_slice(&buf[..n]);
    }
    if truncated {
        let _ = child.kill();
        let _ = child.wait();
        let _ = err_thread.join();
        return Err("TOO_LARGE".into());
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    let err = err_thread.join().unwrap_or_default();
    if !status.success() {
        return Err(classify_error(&err, had_pw));
    }
    Ok(out)
}

/// Create a ZIP or 7Z from files/folders, with optional password (AES-256).
#[allow(clippy::too_many_arguments)]
pub fn create(
    exe: &Path,
    sources: &[PathBuf],
    out_file: &Path,
    format: &str,
    password: Option<&str>,
    level: u32,
    jobs: &Jobs,
    job_id: &str,
    progress: &mut dyn FnMut(u32, &str),
) -> Result<String, String> {
    if sources.is_empty() {
        return Err("Nothing to add.".into());
    }
    check_password(password)?;
    let fmt = match format {
        "7z" => "7z",
        "zip" => "zip",
        _ => return Err("Only ZIP and 7Z can be created.".into()),
    };
    for s in sources {
        if !s.exists() {
            return Err(format!("{} no longer exists.", s.display()));
        }
        if out_file.starts_with(s) {
            return Err(
                "The new archive can't be saved inside a folder that is being archived.".into(),
            );
        }
    }
    let parent = out_file.parent().ok_or("Bad output path")?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    // Build into a temp file, then rename, so a cancelled/failed run never leaves a half archive
    // and an existing archive with the same name is never "updated" by accident.
    let tmp = free_name(
        parent,
        &format!(".unpack-new-{}.{fmt}", safe_segment(job_id)),
    );
    let _ = fs::remove_file(&tmp);
    let mut args: Vec<std::ffi::OsString> = vec![
        "a".into(),
        format!("-t{fmt}").into(),
        format!("-mx={}", level.min(9)).into(),
        "-y".into(),
        "-bso0".into(),
        "-bse2".into(),
        "-bsp1".into(),
        "-sccUTF-8".into(),
    ];
    let had_pw = password.is_some_and(|p| !p.is_empty());
    if had_pw {
        args.push(format!("-p{}", password.unwrap()).into());
        if fmt == "7z" {
            args.push("-mhe=on".into()); // hide file names too
        } else {
            args.push("-mem=AES256".into());
        }
    }
    args.push("--".into());
    args.push(tmp.as_os_str().to_owned());
    for s in sources {
        args.push(s.as_os_str().to_owned());
    }
    let res = run_with_progress(exe, &args, jobs, job_id, had_pw, progress);
    if let Err(e) = res.map(|_| ()) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    replace_file_safely(&tmp, out_file)?;
    Ok(out_file.display().to_string())
}

#[cfg(test)]
mod tests;

/// Extract a single entry straight to `out_file` (streams; works for huge files).
pub fn extract_entry_to(
    exe: &Path,
    archive: &Path,
    entry: &str,
    password: Option<&str>,
    out_file: &Path,
    jobs: &Jobs,
    job_id: &str,
) -> Result<(), String> {
    let had_pw = password.is_some_and(|p| !p.is_empty());
    check_password(password)?;
    ensure_entry(exe, archive, entry, password)?;
    let parent = out_file.parent().ok_or("Bad output path")?;
    let tmp = free_name(parent, &format!(".unpack-part-{}", safe_segment(job_id)));
    let mut file =
        fs::File::create(&tmp).map_err(|e| format!("Cannot write {}: {e}", out_file.display()))?;
    let lf = match ListFile::new(entry) {
        Ok(l) => l,
        Err(e) => {
            drop(file);
            let _ = fs::remove_file(&tmp);
            return Err(e);
        }
    };
    let mut child = entry_command(exe, archive, &lf, password)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Could not start 7-Zip: {e}"))?;
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let err_thread = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });
    let child = Arc::new(Mutex::new(child));
    jobs.attach(job_id, child.clone());
    if jobs.is_cancelled(job_id) {
        let _ = child.lock().unwrap().kill(); // cancel that arrived before attach()
    }
    let copied = std::io::copy(&mut stdout, &mut file);
    let status = child.lock().unwrap().wait().map_err(|e| e.to_string())?;
    let err = err_thread.join().unwrap_or_default();
    drop(file);
    let fail = |msg: String| {
        let _ = fs::remove_file(&tmp);
        Err(msg)
    };
    if jobs.is_cancelled(job_id) {
        return fail(ERR_CANCELLED.into());
    }
    if let Err(e) = copied {
        return fail(e.to_string());
    }
    if !status.success() {
        return fail(classify_error(&err, had_pw));
    }
    replace_file_safely(&tmp, out_file)
}

/// Put the finished temp file at `dst`. An existing file is renamed aside first and deleted only once
/// the new one is in place; if the move fails it is put back, so a failed save never loses the old
/// file (the old code deleted it before renaming). A link at `dst` is moved aside, never written through.
/// The temp file is removed on failure.
pub fn replace_file_safely(tmp: &Path, dst: &Path) -> Result<(), String> {
    let fail = |msg: String| {
        let _ = fs::remove_file(tmp);
        Err(msg)
    };
    let existing = fs::symlink_metadata(dst).ok();
    if existing.as_ref().is_some_and(|m| m.is_dir()) {
        return fail(format!(
            "{} is a folder, so it can't be replaced by a file.",
            dst.display()
        ));
    }
    let parent = dst.parent().unwrap_or(Path::new("."));
    let aside = match existing {
        Some(_) => {
            let a = free_name(parent, &format!(".unpack-old-{}", std::process::id()));
            if let Err(e) = fs::rename(dst, &a) {
                return fail(format!("Could not replace {}: {e}", dst.display()));
            }
            Some(a)
        }
        None => None,
    };
    match fs::rename(tmp, dst) {
        Ok(()) => {
            if let Some(a) = aside {
                let _ = fs::remove_file(a);
            }
            Ok(())
        }
        Err(e) => {
            if let Some(a) = aside {
                let _ = fs::rename(a, dst);
            }
            fail(format!("Could not save {}: {e}", dst.display()))
        }
    }
}
