//! Where a catalog install goes: a local drive/folder the user picked (path grants), or the
//! console over the existing FTP client. Paths are FATX segments relative to the drive root.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::ftp::{self, Console, Ftp};

pub enum Target {
    /// Canonical, grant-checked root (e.g. the USB drive or a folder standing in for one).
    Local { root: PathBuf },
    Console { con: Console, drive: String },
}

#[cfg(test)]
pub(crate) mod fail {
    thread_local! { pub static LOCAL_SWAP: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
}

#[derive(Clone, Copy, Debug)]
enum SwapStage { Aside, NewRefused, NewUnverified }

/// One launch.ini operation at a time in this process (repair vs. replace; R14).
static INI_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
pub fn ini_lock() -> std::sync::MutexGuard<'static, ()> { INI_LOCK.lock().unwrap_or_else(|e| e.into_inner()) }

#[cfg(not(test))]
const RECOVER_BACKOFF_MS: [u64; 3] = [0, 1000, 3000];
#[cfg(test)]
const RECOVER_BACKOFF_MS: [u64; 3] = [0, 30, 60];

/// First line of an error, without a trailing period (for composing messages).
/// Drops the generic "The upload is kept as …" tail of `Ftp::rename` errors, which doesn't apply here.
#[cfg(test)]
pub fn short_err_for_test(e: &str) -> String { short_err(e) }
fn short_err(e: &str) -> String { e.lines().next().unwrap_or("").split(". The upload is kept").next().unwrap_or("").trim_end_matches('.').to_string() }

/// `<name>.old`, `.old-2` … `.old-99`, then a time-based name: never refuses (R13 LOW-2).
pub fn free_old_name(parent: &[String], name: &str, mut exists: impl FnMut(&[String]) -> Result<Option<u64>, String>) -> Result<Vec<String>, String> {
    let mut old = parent.to_vec();
    old.push(format!("{name}.old"));
    for n in 2..100 {
        if exists(&old)?.is_none() { return Ok(old); }
        *old.last_mut().unwrap() = format!("{name}.old-{n}");
    }
    // Unix seconds (10 digits) sort after .old-99 and by recency; bump on collision.
    let mut t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs().max(100);
    loop {
        *old.last_mut().unwrap() = format!("{name}.old-{t}");
        if exists(&old)?.is_none() { return Ok(old); }
        t += 1;
    }
}

/// Is this file name one of Unpack's launch.ini leftovers? (".old", ".old-N", ".new.part", ".unpack-new")
pub fn is_old_name(n: &str, name: &str) -> bool {
    // Case-insensitive: the Xbox file system and FAT ignore case (R14 MEDIUM-1).
    if n.len() < name.len() || !n.is_char_boundary(name.len()) || !n[..name.len()].eq_ignore_ascii_case(name) { return false; }
    let rest = n[name.len()..].to_ascii_lowercase();
    let rest = rest.as_str();
    rest == ".old" || rest.strip_prefix(".old-").map_or(false, |d| !d.is_empty() && d.len() <= 10 && d.bytes().all(|b| b.is_ascii_digit()))
}

pub fn sha256_bytes(b: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(b).iter().map(|x| format!("{x:02x}")).collect()
}

pub fn sha256_file(p: &Path) -> Result<(String, u64), String> {
    use sha2::{Digest, Sha256};
    let mut f = ftp::open_no_follow(p)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut n = 0u64;
    loop {
        let k = f.read(&mut buf).map_err(|e| format!("Cannot read {}: {e}", p.display()))?;
        if k == 0 { break; }
        h.update(&buf[..k]);
        n += k as u64;
    }
    Ok((h.finalize().iter().map(|b| format!("{b:02x}")).collect(), n))
}

/// Every regular file under `dir` (relative segments), refusing links and non-FATX names.
pub fn walk_stage(dir: &Path) -> Result<Vec<(PathBuf, Vec<String>)>, String> {
    let croot = fs::canonicalize(dir).map_err(|e| e.to_string())?;
    let mut out = vec![];
    let mut stack = vec![(dir.to_path_buf(), Vec::<String>::new())];
    while let Some((d, rel)) = stack.pop() {
        let mut items: Vec<_> = fs::read_dir(&d).map_err(|e| e.to_string())?.flatten().collect();
        items.sort_by_key(|e| e.file_name());
        for e in items {
            let name = e.file_name().to_string_lossy().into_owned();
            let p = e.path();
            let m = fs::symlink_metadata(&p).map_err(|e| e.to_string())?;
            if crate::safecopy::is_link(&m) {
                return Err(format!("The download contains a link ({name}); not installing it."));
            }
            ftp::check_segment(&name).map_err(|m| format!("The download contains a name the Xbox can't store: {m}"))?;
            let mut r = rel.clone();
            r.push(name);
            if r.len() > 16 { return Err("The download's folders are nested too deeply.".into()); }
            let c = fs::canonicalize(&p).map_err(|e| e.to_string())?;
            if !c.starts_with(&croot) { return Err("The download tried to leave its folder; not installing it.".into()); }
            if m.is_dir() { stack.push((p, r)); } else if m.is_file() { out.push((p, r)); } else { return Err(format!("The download contains a special file ({}).", r.join("/"))); }
        }
    }
    out.sort_by(|a, b| a.1.cmp(&b.1));
    Ok(out)
}

fn scratch_dir() -> Result<PathBuf, String> {
    let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let d = std::env::temp_dir().join(format!("unpack-catalog-{}-{n}", std::process::id()));
    fs::create_dir_all(&d).map_err(|e| e.to_string())?;
    Ok(d)
}

impl Target {
    pub fn key(&self) -> String {
        match self {
            Target::Local { root } => format!("local:{}", root.display()),
            Target::Console { con, drive } => format!("console:{}/{}", con.addr.ip(), drive.to_ascii_lowercase()),
        }
    }
    pub fn label(&self) -> String {
        match self {
            Target::Local { root } => crate::safecopy::plain(&root.display().to_string()),
            Target::Console { con, drive } => format!("{} /{drive}", con.addr.ip()),
        }
    }
    /// DashLaunch device prefix for paths written into launch.ini.
    pub fn device(&self) -> String {
        match self {
            Target::Local { .. } => "Usb:".into(),
            Target::Console { drive, .. } => {
                let d = drive.to_ascii_lowercase();
                if d.starts_with("hdd") { "Hdd:".into() } else if d.starts_with("usb") { "Usb:".into() } else { format!("{drive}:") }
            }
        }
    }
    pub fn is_console(&self) -> bool { matches!(self, Target::Console { .. }) }
    /// FAT32 / FATX: 4 GB per file.
    pub fn is_fat(&self) -> bool {
        match self {
            Target::Local { root } => crate::safecopy::fs_type(root).map_or(false, |t| crate::safecopy::is_fat(&t)),
            Target::Console { .. } => true,
        }
    }
    pub fn free(&self) -> Option<u64> {
        match self {
            Target::Local { root } => crate::god::free_space(root),
            Target::Console { .. } => None,
        }
    }
    fn full(&self, rel: &[String]) -> Vec<String> {
        match self {
            Target::Console { drive, .. } => std::iter::once(drive.clone()).chain(rel.iter().cloned()).collect(),
            Target::Local { .. } => rel.to_vec(),
        }
    }
    fn ftp(&self) -> Result<Ftp, String> {
        match self {
            Target::Console { con, .. } => Ftp::connect(con.addr, &con.user, &con.pass),
            _ => Err("not a console".into()),
        }
    }
    /// Local path under root; every existing component must be a real folder/file, no links.
    fn local(&self, rel: &[String]) -> Result<PathBuf, String> {
        let Target::Local { root } = self else { return Err("not local".into()) };
        let mut cur = root.clone();
        for s in rel {
            ftp::check_segment(s)?;
            cur.push(s);
            if let Ok(m) = fs::symlink_metadata(&cur) {
                if crate::safecopy::is_link(&m) {
                    return Err(format!("{} is a link or junction; not touching it.", cur.display()));
                }
            }
        }
        Ok(cur)
    }

    /// File size, u64::MAX for a folder, None when missing.
    pub fn stat(&self, rel: &[String]) -> Result<Option<u64>, String> {
        match self {
            Target::Local { .. } => Ok(fs::symlink_metadata(self.local(rel)?).ok().map(|m| if m.is_dir() { u64::MAX } else { m.len() })),
            Target::Console { .. } => {
                let mut f = self.ftp()?;
                let r = f.size_of(&self.full(rel));
                f.quit();
                r
            }
        }
    }

    /// (name, size, is_dir) of a folder's entries; None when the folder is missing.
    pub fn list(&self, rel: &[String]) -> Result<Option<Vec<(String, u64, bool)>>, String> {
        match self {
            Target::Local { .. } => {
                let p = self.local(rel)?;
                let Ok(rd) = fs::read_dir(&p) else { return Ok(None) };
                Ok(Some(rd.flatten().filter_map(|e| { let m = fs::symlink_metadata(e.path()).ok()?; if crate::safecopy::is_link(&m) { return None; } Some((e.file_name().to_string_lossy().into_owned(), m.len(), m.is_dir())) }).collect()))
            }
            Target::Console { .. } => {
                let mut f = self.ftp()?;
                let full = self.full(rel);
                let r = if f.cwd(&full)? { f.list(&full).map(|v| Some(v.into_iter().map(|e| (e.name, e.size, e.is_dir)).collect())) } else { Ok(None) };
                f.quit();
                r
            }
        }
    }

    /// Up to `max` bytes of a file (`whole`: error if larger). None when missing.
    pub fn read(&self, rel: &[String], max: usize, whole: bool) -> Result<Option<Vec<u8>>, String> {
        match self {
            Target::Local { .. } => {
                let p = self.local(rel)?;
                let Ok(m) = fs::symlink_metadata(&p) else { return Ok(None) };
                if !m.is_file() { return Err(format!("{} isn't a file.", p.display())); }
                if whole && m.len() > max as u64 { return Err(format!("{} is larger than {max} bytes.", p.display())); }
                let mut b = vec![];
                ftp::open_no_follow(&p)?.take(max as u64).read_to_end(&mut b).map_err(|e| e.to_string())?;
                Ok(Some(b))
            }
            Target::Console { .. } => {
                let mut f = self.ftp()?;
                let full = self.full(rel);
                let r = match f.size_of(&full)? {
                    None => Ok(None),
                    Some(u64::MAX) => Err(format!("{} is a folder.", ftp::join_remote(&full))),
                    Some(_) => f.retr(&full, max, whole).map(Some),
                };
                f.quit();
                r
            }
        }
    }

    /// Copy every staged file into `dest` (folder segments). Checks all destinations first:
    /// an identical file is skipped, a different one stops everything before anything is written.
    pub fn put_dir(&self, stage: &Path, dest: &[String], progress: &mut dyn FnMut(u32, &str), cancelled: &dyn Fn() -> bool) -> Result<(), String> {
        let files = walk_stage(stage)?;
        match self {
            Target::Local { root } => {
                let mut plan = vec![];
                for (p, rel) in &files {
                    let mut r = dest.to_vec();
                    r.extend(rel.iter().cloned());
                    let lp = self.local(&r)?;
                    if let Ok(m) = fs::symlink_metadata(&lp) {
                        let same = m.is_file() && sha256_file(&lp).ok() == sha256_file(p).ok();
                        if !same { return Err(format!("{} already exists with different content. Nothing was installed.", crate::safecopy::plain(&lp.display().to_string()))); }
                        continue;
                    }
                    plan.push((p.clone(), r));
                }
                let total: u64 = plan.iter().map(|(p, _)| fs::metadata(p).map(|m| m.len()).unwrap_or(0)).sum::<u64>().max(1);
                let mut done = 0u64;
                for (p, r) in plan {
                    if cancelled() { return Err(crate::archive::ERR_CANCELLED.into()); }
                    let rel: PathBuf = r.iter().collect();
                    let label = r.join("/");
                    crate::safecopy::copy_file_safe(&p, root, &rel, &mut |k| { done += k; progress((done * 100 / total) as u32, &label); })?;
                }
                Ok(())
            }
            Target::Console { con, .. } => {
                let full = self.full(dest);
                ftp::send(con, stage, &full, true, false, progress, cancelled).map(|_| ())
            }
        }
    }

    /// Remove a file only if it is still what Unpack put there (local: SHA-256; console: size).
    /// Ok(true) removed, Ok(false) missing or changed (kept).
    pub fn delete_if(&self, rel: &[String], size: u64, sha: &str) -> Result<bool, String> {
        match self {
            Target::Local { .. } => {
                let p = self.local(rel)?;
                let Ok(m) = fs::symlink_metadata(&p) else { return Ok(false) };
                if !m.is_file() || m.len() != size || sha256_file(&p)?.0 != sha { return Ok(false); }
                fs::remove_file(&p).map_err(|e| format!("Cannot remove {}: {e}", p.display()))?;
                // Tidy now-empty folders Unpack may have created (never the root).
                let Target::Local { root } = self else { unreachable!() };
                let mut d = p.parent().map(Path::to_path_buf);
                while let Some(dir) = d {
                    if dir == *root || !dir.starts_with(root) || fs::remove_dir(&dir).is_err() { break; }
                    d = dir.parent().map(Path::to_path_buf);
                }
                Ok(true)
            }
            Target::Console { .. } => {
                let mut f = self.ftp()?;
                let full = self.full(rel);
                let r = match f.size_of(&full)? {
                    Some(n) if n == size => f.dele(&full),
                    _ => Ok(false),
                };
                f.quit();
                r
            }
        }
    }

    /// Rename a file at the drive root area; never over an existing name.
    pub fn rename_new(&self, from: &[String], to: &[String]) -> Result<(), String> {
        match self {
            Target::Local { .. } => {
                let (a, b) = (self.local(from)?, self.local(to)?);
                if fs::symlink_metadata(&b).is_ok() { return Err(format!("{} already exists.", b.display())); }
                let m = fs::symlink_metadata(&a).map_err(|e| format!("Cannot open {}: {e}", a.display()))?;
                if !m.is_file() || crate::safecopy::is_link(&m) { return Err(format!("{} isn't a plain file.", a.display())); }
                fs::rename(&a, &b).map_err(|e| format!("Cannot rename {}: {e}", a.display()))
            }
            Target::Console { .. } => {
                let mut f = self.ftp()?;
                let (a, b) = (self.full(from), self.full(to));
                let r = (|| { if f.size_of(&b)?.is_some() { return Err(format!("{} already exists.", ftp::join_remote(&b))); } f.rename(&a, &b) })();
                f.quit();
                r
            }
        }
    }

    /// Create a new small file; fails if the name exists.
    pub fn write_new(&self, rel: &[String], bytes: &[u8]) -> Result<(), String> {
        match self {
            Target::Local { root } => {
                let (name, parent) = rel.split_last().ok_or("empty path")?;
                let dir = crate::safecopy::ensure_dir_under(root, &parent.iter().collect::<PathBuf>())?;
                let mut f = fs::OpenOptions::new().write(true).create_new(true).open(dir.join(name)).map_err(|e| format!("Cannot create {name}: {e}"))?;
                f.write_all(bytes).and_then(|_| f.sync_all()).map_err(|e| format!("Cannot write {name}: {e}"))
            }
            Target::Console { .. } => {
                let mut f = self.ftp()?;
                let r = self.ftp_put(&mut f, rel, bytes, false);
                f.quit();
                r
            }
        }
    }

    /// Replace a small file (launch.ini) without ever leaving the target without it (R12 HIGH-1):
    /// write the new copy beside it, move the original aside to `<name>.old` (unique), move the new
    /// one in, read it back, and only then delete the old one. On any error after the original was
    /// moved, the original is put back (rename, or rewritten from its bytes as a last resort).
    pub fn replace(&self, rel: &[String], bytes: &[u8]) -> Result<(), String> {
        let _g = ini_lock();
        match self {
            Target::Local { .. } => {
                let p = self.local(rel)?;
                let side = |suffix: &str| { let mut t = p.clone().into_os_string(); t.push(suffix); PathBuf::from(t) };
                let tmp = side(".unpack-new");
                if let Ok(m) = fs::symlink_metadata(&tmp) {
                    if !m.is_file() || crate::safecopy::is_link(&m) { return Err(format!("{} isn't a plain file; not touching it.", tmp.display())); }
                    fs::remove_file(&tmp).map_err(|e| e.to_string())?;
                }
                let mut f = fs::OpenOptions::new().write(true).create_new(true).open(&tmp).map_err(|e| format!("Cannot write {}: {e}", tmp.display()))?;
                f.write_all(bytes).and_then(|_| f.sync_all()).map_err(|e| format!("Cannot write {}: {e}", tmp.display()))?;
                drop(f);
                let original = match fs::symlink_metadata(&p) {
                    Ok(m) if m.is_file() && !crate::safecopy::is_link(&m) => Some(fs::read(&p).map_err(|e| format!("Cannot read {}: {e}", p.display()))?),
                    Ok(_) => { let _ = fs::remove_file(&tmp); return Err(format!("{} isn't a plain file; not touching it.", p.display())); }
                    Err(_) => None,
                };
                let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                let parent_dir = p.parent().map(Path::to_path_buf).unwrap_or_default();
                let old_segs = free_old_name(&[], &name, |segs| Ok(fs::symlink_metadata(parent_dir.join(&segs[0])).ok().map(|m| m.len())))?;
                let old = parent_dir.join(&old_segs[0]);
                if original.is_some() {
                    if let Err(e) = fs::rename(&p, &old) { let _ = fs::remove_file(&tmp); return Err(format!("Cannot move {name} aside: {e}. Nothing was changed.")); }
                }
                let swap = (|| -> Result<(), String> {
                    #[cfg(test)]
                    if fail::LOCAL_SWAP.with(|c| c.get()) { return Err("injected failure".into()); }
                    fs::rename(&tmp, &p).map_err(|e| format!("Cannot move the new {name} into place: {e}"))?;
                    if fs::read(&p).ok().as_deref() != Some(bytes) { return Err(format!("The new {name} didn't read back as written")); }
                    Ok(())
                })();
                match swap {
                    Ok(()) => { if original.is_some() { let _ = fs::remove_file(&old); } Ok(()) }
                    Err(e) => {
                        let _ = fs::remove_file(&tmp);
                        let Some(orig) = original else { let _ = fs::remove_file(&p); return Err(format!("{e}.")); };
                        let _ = fs::remove_file(&p);
                        let ok = fs::rename(&old, &p).is_ok() || fs::write(&p, &orig).is_ok();
                        let ok = ok && fs::read(&p).ok().as_deref() == Some(&orig[..]);
                        if ok { let _ = fs::remove_file(&old); }
                        Err(if ok { format!("{e}. The original {name} was put back.") } else { format!("{e}. The original {name} could NOT be put back; it is saved as {}.", old.display()) })
                    }
                }
            }
            Target::Console { .. } => {
                let mut f = self.ftp()?;
                let r = self.ftp_put(&mut f, rel, bytes, true);
                f.quit();
                r
            }
        }
    }

    /// After a failed swap: reconnect (up to 3 tries, with backoff) and make sure launch.ini exists.
    /// Returns (true, _) when the new file turned out to be fully in place after all.
    fn ftp_recover(&self, full: &[String], old: &[String], new_segs: &[String], orig: &[u8], new: Option<&[u8]>, tmpdir: &Path, stage: SwapStage) -> (bool, String) {
        let name = full.last().cloned().unwrap_or_default();
        let mut last = String::new();
        for (k, wait) in RECOVER_BACKOFF_MS.iter().enumerate() {
            if k > 0 { std::thread::sleep(std::time::Duration::from_millis(*wait)); }
            let mut g = match self.ftp() { Ok(g) => g, Err(e) => { last = e; continue } };
            let r = (|| -> Result<(bool, String), String> {
                if let Some(n) = g.size_of(full)? {
                    if let Some(nb) = new {
                        if n == nb.len() as u64 && g.retr(full, 1 << 20, true)? == nb {
                            let _ = g.dele(old);
                            let _ = g.dele(new_segs);
                            return Ok((true, String::new()));
                        }
                    }
                    let _ = g.dele(new_segs);
                    if g.size_of(old)?.is_some() && n == orig.len() as u64 && g.retr(full, 1 << 20, true)? == orig { let _ = g.dele(old); }
                    return Ok((false, format!("Your {name} is unchanged.")));
                }
                if g.size_of(old)?.is_some() && g.rename(old, full).is_ok() && g.size_of(full)? == Some(orig.len() as u64) {
                    let _ = g.dele(new_segs);
                    return Ok((false, format!("The original {name} was put back.")));
                }
                let lo = tmpdir.join("original.bin");
                fs::write(&lo, orig).map_err(|e| e.to_string())?;
                g.stor(&lo, 0, full, &mut |_| {}, &|| false)?;
                if g.size_of(full)? != Some(orig.len() as u64) { return Err("re-upload didn't verify".into()); }
                let _ = g.dele(old);
                let _ = g.dele(new_segs);
                Ok((false, format!("The original {name} was put back (uploaded again).")))
            })();
            g.quit();
            match r { Ok(v) => return v, Err(e) => last = e }
        }
        // Say only what is known (R14 LOW-2/3).
        let why = short_err(&last);
        let old_s = ftp::join_remote(old);
        (false, match stage {
            SwapStage::Aside => format!("Unpack couldn't reconnect to check ({why}). Your {name} was most likely not moved; if it was, it is safe as {old_s}. Unpack checks and repairs this automatically the next time it connects to this console."),
            SwapStage::NewRefused => format!("The original {name} could NOT be put back yet ({why}); it is safe on the console as {old_s}. Unpack restores it automatically the next time it connects to this console."),
            SwapStage::NewUnverified => format!("The new {name} was renamed into place but couldn't be verified, and Unpack couldn't reconnect to check ({why}). The previous one is kept as {old_s}; Unpack checks which is in place the next time it connects."),
        })
    }

    fn ftp_put(&self, f: &mut Ftp, rel: &[String], bytes: &[u8], replace: bool) -> Result<(), String> {
        let full = self.full(rel);
        let (name, parent) = full.split_last().ok_or("empty path")?;
        if !replace && f.size_of(&full)?.is_some() {
            return Err(format!("{} already exists on the console.", ftp::join_remote(&full)));
        }
        let tmpdir = scratch_dir()?;
        let local = tmpdir.join("upload.bin");
        fs::write(&local, bytes).map_err(|e| e.to_string())?;
        let mut new_segs = parent.to_vec();
        new_segs.push(ftp::part_name(&format!("{name}.new")));
        let res = (|| -> Result<(), String> {
            let shown = ftp::join_remote(&full);
            let existing = f.size_of(&full)?;
            // The original's bytes, for a last-resort restore if renaming it back fails.
            let original = match existing { Some(_) => Some(f.retr(&full, 1 << 20, true)?), None => None };
            let _ = f.dele(&new_segs);
            f.stor(&local, 0, &new_segs, &mut |_| {}, &|| false)?;
            if f.size_of(&new_segs)? != Some(bytes.len() as u64) {
                let _ = f.dele(&new_segs);
                return Err(format!("Upload of {shown} didn't arrive complete. Nothing was changed."));
            }
            let Some(orig) = original else {
                if f.size_of(&full)?.is_some() {
                    let _ = f.dele(&new_segs);
                    return Err(format!("{shown} appeared on the console; not replacing it."));
                }
                f.rename(&new_segs, &full)?;
                return match f.size_of(&full)? {
                    Some(n) if n == bytes.len() as u64 => Ok(()),
                    _ => Err(format!("Size check failed for {shown} after writing it.")),
                };
            };
            // 1. Move the original aside (never delete it before the new one is in place).
            let old = free_old_name(parent, name, |segs| f.size_of(segs))?;
            if let Err(e) = f.rename(&full, &old) {
                // A dropped connection leaves it unknown whether the move happened: check on a fresh one.
                f.close(); // one-connection servers: free the slot for the recovery connection
                let (_, note) = self.ftp_recover(&full, &old, &new_segs, &orig, None, &tmpdir, SwapStage::Aside);
                return Err(format!("Couldn't move {name} aside ({}). {note}", short_err(&e)));
            }
            // 2. Move the new one in and verify it.
            let mut renamed = false;
            let swap = (|| -> Result<(), String> {
                f.rename(&new_segs, &full)?;
                renamed = true;
                match f.size_of(&full)? {
                    Some(n) if n == bytes.len() as u64 => Ok(()),
                    _ => Err(format!("Size check failed for {shown} after writing it")),
                }
            })();
            match swap {
                Ok(()) => {
                    // A refused/failed delete leaves an .old behind; the next ini operation prunes it.
                    let _ = f.dele(&old);
                    Ok(())
                }
                Err(e) => {
                    // 3. Recover on a FRESH connection (the old one may be dead), with retries.
                    f.close();
                    let stage = if renamed { SwapStage::NewUnverified } else { SwapStage::NewRefused };
                    let (new_ok, note) = self.ftp_recover(&full, &old, &new_segs, &orig, Some(bytes), &tmpdir, stage);
                    if new_ok { Ok(()) } else if renamed { Err(format!("Couldn't confirm the {name} change. {note}")) } else { Err(format!("{}. {note}", short_err(&e))) }
                }
            }
        })();
        let _ = fs::remove_dir_all(&tmpdir);
        res
    }
}
