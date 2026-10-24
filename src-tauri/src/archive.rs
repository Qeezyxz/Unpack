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
        self.inner.lock().unwrap().get(id).is_some_and(|h| {
            h.cancelled.load(std::sync::atomic::Ordering::SeqCst)
        })
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
fn is_link(p: &Path) -> bool {
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

/// Removes the staging folder however we leave the function.
struct StagingGuard(PathBuf);
impl Drop for StagingGuard {
    fn drop(&mut self) {
        if self.0.exists() {
            let _ = fs::remove_dir_all(&self.0);
        }
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

    if opts.layout == Layout::Folder && opts.collision == Collision::Skip {
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

    let staging = free_name(parent, &format!(".unpack-tmp-{}", safe_segment(job_id)));
    fs::create_dir_all(&staging)
        .map_err(|e| format!("Cannot write to {}: {e}", parent.display()))?;
    let _guard = StagingGuard(staging.clone());

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

    let mut warnings = sanitize_tree(&staging);
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
