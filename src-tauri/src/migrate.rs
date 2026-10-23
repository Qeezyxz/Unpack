//! First-run migration from the old app identifier (R15 L5).
//!
//! 1.6.0 changed the identifier from `com.justin.unpack` to `com.qeezyxz.unpack`, which moves the
//! app data folders (catalog journal, seen.json, launch.ini backups, FTP resume records, grants,
//! downloads) and the WebView storage (saved consoles, drives and settings in localStorage). On
//! the first start under the new identifier the old folders are copied over, before Tauri or the
//! WebView open anything. The old folders stay as they are (except Downloads, which is moved),
//! a new folder that already has data is never touched, and links are not followed. See
//! `migrate_dir` for the crash-safe staging.

use std::fs;
use std::path::{Path, PathBuf};

pub const OLD_ID: &str = "com.justin.unpack";
pub const NEW_ID: &str = "com.qeezyxz.unpack";
pub const MARKER: &str = ".migrated-from-com.justin.unpack";

/// Base folders that hold `<identifier>` folders on this OS (Roaming + Local on Windows, where
/// WebView2 keeps `EBWebView` under Local; XDG data/config on Linux; Application Support on macOS).
fn bases() -> Vec<PathBuf> {
    let mut v = Vec::new();
    let env = |k: &str| std::env::var_os(k).filter(|x| !x.is_empty()).map(PathBuf::from);
    #[cfg(windows)]
    {
        v.extend(env("APPDATA"));
        v.extend(env("LOCALAPPDATA"));
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(h) = env("HOME") { v.push(h.join("Library/Application Support")); v.push(h.join("Library/WebKit")); }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let home = env("HOME");
        v.extend(env("XDG_DATA_HOME").or_else(|| home.as_ref().map(|h| h.join(".local/share"))));
        v.extend(env("XDG_CONFIG_HOME").or_else(|| home.as_ref().map(|h| h.join(".config"))));
    }
    v.dedup();
    v
}

/// Run once at startup. Returns the folders that were migrated (for logging).
pub fn run_startup() -> Vec<PathBuf> {
    let mut done = Vec::new();
    for b in bases() {
        let (old, new) = (b.join(OLD_ID), b.join(NEW_ID));
        match migrate_dir(&old, &new) {
            Ok(true) => done.push(new),
            Ok(false) => {}
            Err(e) => eprintln!("migration from {} skipped: {e}", old.display()),
        }
    }
    done
}

/// Folder that is moved (same drive, instant) rather than copied, so it isn't doubled on disk.
pub const MOVED: &str = "Downloads";
/// Written into the staged copy; while present, the migration still has steps left (R16 L1).
const PENDING: &str = ".migration-pending";
/// Free space kept spare after the copy.
const FREE_MARGIN: u64 = 64 << 20;

#[cfg(test)]
thread_local! {
    /// Test hook: copying a file with this name fails (stands in for a locked/unreadable file).
    pub static FAIL_NAME: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
    /// Test hook: stop (as if killed) after this many files were copied.
    pub static KILL_AFTER: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

fn staging_of(new: &Path) -> PathBuf {
    let mut n = new.as_os_str().to_owned();
    n.push(".migrating");
    PathBuf::from(n)
}

/// Sibling of the new folder, present while a migration has been started but not finished
/// (R17 M1). While it exists, whatever the WebView wrote into the new folder after a failed run is
/// not treated as the user's data: it is moved aside to `<new>.pre-migration` and the migration
/// is retried. Removed only after a fully successful run.
fn retry_marker(new: &Path) -> PathBuf { sibling(new, ".migration-incomplete") }
fn backup_of(new: &Path) -> PathBuf { sibling(new, ".pre-migration") }
fn sibling(new: &Path, suffix: &str) -> PathBuf {
    let mut n = new.as_os_str().to_owned();
    n.push(suffix);
    PathBuf::from(n)
}

#[cfg(test)]
thread_local! {
    /// Test hook: pretend Downloads is on another drive (rename fails), forcing the copy path.
    pub static FORCE_COPY_DOWNLOADS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Unpack's own files (written by Unpack itself, not the WebView's caches).
const OWN: [&str; 4] = ["catalog", "grants.json", "ftp-resume", MOVED];

/// True when `new` holds data the user made under the new identifier: Unpack's own files, or a
/// WebView localStorage that contains Unpack keys. WebView caches alone don't count (R17 M1).
fn has_own_data(new: &Path) -> bool {
    OWN.iter().any(|n| fs::symlink_metadata(new.join(n)).is_ok()) || has_own_data_webview(&new.join("EBWebView"))
}

/// Copy `old` into `new` when `old` exists and `new` has not been migrated yet and holds no data
/// of its own. Returns true when it migrated.
///
/// Crash-safe (R16 L1): everything is copied into `<new>.migrating`, every file is checked by size,
/// and only then is the folder renamed to `<new>` in one step. A run that is killed leaves only the
/// `.migrating` folder, which the next start throws away and redoes. `Downloads` is moved, not
/// copied. Any file that can't be copied fails the run; free space is checked before copying.
/// A failed or refused run leaves `<new>.migration-incomplete`, so the next start retries even
/// though the WebView has written into the new folder meanwhile (R17 M1).
pub fn migrate_dir(old: &Path, new: &Path) -> std::io::Result<bool> {
    let Ok(m) = fs::symlink_metadata(old) else { return Ok(false) };
    if !m.is_dir() { return Ok(false); }
    if new.join(MARKER).exists() { return Ok(false); }
    if new.join(PENDING).exists() {
        // Killed after the rename: only the Downloads move, the merge-back and the marker are left.
        return finish(old, new).map(|_| true);
    }
    let retry = retry_marker(new).exists();
    if !retry && has_own_data(new) {
        // The new identifier already has its own data: never mix the two. Mark it so we don't re-check.
        let _ = fs::write(new.join(MARKER), b"skipped: new folder already had data\n");
        return Ok(false);
    }
    if let Some(p) = new.parent() { fs::create_dir_all(p)?; }
    fs::write(retry_marker(new), format!("migration from {} not finished yet\n", old.display()))?;
    let stage = staging_of(new);
    if stage.exists() { fs::remove_dir_all(&stage)?; } // an interrupted earlier run
    let (files, bytes) = measure(old, true)?;
    if let Some(free) = old.parent().and_then(crate::god::free_space) {
        if bytes.saturating_add(FREE_MARGIN) > free {
            return Err(std::io::Error::other(format!("needs {bytes} bytes, only {free} free; will retry next start")));
        }
    }
    eprintln!("migration: copying {files} files ({} MB) from {}", bytes >> 20, old.display());
    fs::create_dir_all(&stage)?;
    let mut count = 0usize;
    if let Err(e) = copy_tree(old, &stage, true, &mut count) {
        let _ = fs::remove_dir_all(&stage);
        return Err(std::io::Error::other(format!("{e}; will retry next start")));
    }
    // Verify: same files, same sizes.
    if measure(&stage, true)? != (files, bytes) {
        let _ = fs::remove_dir_all(&stage);
        return Err(std::io::Error::other("copy did not match the old folder; will retry next start"));
    }
    fs::write(stage.join(PENDING), b"")?;
    if new.exists() {
        // Whatever is there was written after an earlier failed run (or is empty): keep it aside.
        let bak = backup_of(new);
        if bak.exists() {
            // A backup from an earlier attempt: merge the (newer) current folder into it.
            merge_newer(new, &bak)?;
            fs::remove_dir_all(new)?;
        } else {
            fs::rename(new, &bak)?;
        }
    }
    fs::rename(&stage, new)?;
    finish(old, new)?;
    Ok(true)
}

fn finish(old: &Path, new: &Path) -> std::io::Result<()> {
    move_downloads(&old.join(MOVED), &new.join(MOVED))?;
    // Data written under the new identifier after a failed run is newer than the old folder's:
    // merge it in (R18 M1), then drop the backup.
    let bak = backup_of(new);
    if fs::symlink_metadata(&bak).is_ok_and(|m| m.is_dir()) {
        merge_newer(&bak, new)?;
        fs::remove_dir_all(&bak)?;
    }
    fs::write(new.join(MARKER), format!("copied from {}\n", old.display()))?;
    let _ = fs::remove_file(new.join(PENDING));
    let _ = fs::remove_file(retry_marker(new));
    Ok(())
}

fn log(msg: String) { eprintln!("migration: {msg}"); }

/// Merge the folder `src` (written later) into `dst` (R18 M1/L1). Idempotent, so a kill part-way
/// is safe (finish() reruns it):
/// - catalog/journal.json: union of installs by (target, entry_id); on a clash the later
///   `installed_at` wins.
/// - catalog/seen.json: highest version per catalog (rollback floor never lowered).
/// - other JSON: arrays are unioned; objects take `src`'s value per key (logged when it differs).
/// - other files present on both sides with different content: `src` wins, the `dst` one is kept
///   as `<name>.old-version`.
/// - Downloads: files are moved in, renamed `name (2).ext` on a clash.
/// - EBWebView: localStorage can't be merged per key, so whichever profile has Unpack settings
///   made later wins: `src`'s if it has any (the other is kept as `EBWebView.old-version`).
/// - WebView caches and other non-Unpack files: copied only when missing.
pub(crate) fn merge_newer(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for e in fs::read_dir(src)?.flatten() {
        let name = e.file_name();
        let (from, to) = (e.path(), dst.join(&name));
        let m = fs::symlink_metadata(&from)?;
        if crate::safecopy::is_link(&m) || name == PENDING || name == MARKER { continue; }
        let n = name.to_string_lossy();
        if n == MOVED {
            if m.is_dir() { merge_downloads(&from, &to)?; }
        } else if n == "EBWebView" {
            if m.is_dir() && has_own_data_webview(&from) {
                if to.exists() {
                    let keep = sibling(&to, ".old-version");
                    if keep.exists() { fs::remove_dir_all(&keep)?; }
                    fs::rename(&to, &keep)?;
                }
                rename_or_copy(&from, &to)?;
                log(format!("kept the settings made after the failed start; the 1.5 profile is saved as {}", sibling(&to, ".old-version").display()));
            } else if !to.exists() && m.is_dir() {
                rename_or_copy(&from, &to)?;
            }
        } else if OWN.contains(&n.as_ref()) {
            merge_tree(&from, &to)?;
        } else if fs::symlink_metadata(&to).is_err() {
            rename_or_copy(&from, &to)?;
        }
    }
    Ok(())
}

fn rename_or_copy(from: &Path, to: &Path) -> std::io::Result<()> {
    if fs::rename(from, to).is_ok() { return Ok(()); }
    let mut n = 0;
    if fs::symlink_metadata(from)?.is_dir() { copy_tree(from, to, false, &mut n) } else { fs::copy(from, to).map(|_| ()) }
}

fn merge_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
    let m = fs::symlink_metadata(src)?;
    if crate::safecopy::is_link(&m) { return Ok(()); }
    if m.is_dir() {
        if fs::symlink_metadata(dst).is_ok_and(|d| !d.is_dir()) { return Ok(()); }
        fs::create_dir_all(dst)?;
        for e in fs::read_dir(src)?.flatten() { merge_tree(&e.path(), &dst.join(e.file_name()))?; }
        return Ok(());
    }
    if !m.is_file() { return Ok(()); }
    let new_b = fs::read(src)?;
    let Ok(old_b) = fs::read(dst) else { return write_atomic(dst, &new_b) };
    if old_b == new_b { return Ok(()); }
    let name = dst.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let parse = |b: &[u8]| serde_json::from_slice::<serde_json::Value>(b).ok();
    if let (Some(o), Some(n)) = (parse(&old_b), parse(&new_b)) {
        if let Some(v) = merge_json(name, o, n, dst) {
            return write_atomic(dst, &serde_json::to_vec_pretty(&v).unwrap());
        }
    }
    let keep = sibling(dst, ".old-version");
    write_atomic(&keep, &old_b)?;
    log(format!("{} differed: kept the newer one, the older is {}", dst.display(), keep.display()));
    write_atomic(dst, &new_b)
}

/// `o` = migrated (older), `n` = written later. None: not mergeable, treat as an unknown file.
fn merge_json(name: &str, o: serde_json::Value, n: serde_json::Value, at: &Path) -> Option<serde_json::Value> {
    use serde_json::Value;
    match (name, o, n) {
        ("journal.json", Value::Object(mut om), Value::Object(nm)) if om.get("installs").is_some_and(|v| v.is_array()) && nm.get("installs").is_some_and(|v| v.is_array()) => {
            let key = |r: &Value| (r["target"].as_str().unwrap_or("").to_string(), r["entry_id"].as_str().unwrap_or("").to_string());
            let at_of = |r: &Value| r["installed_at"].as_u64().unwrap_or(0);
            let mut out: Vec<Value> = om["installs"].as_array().cloned().unwrap_or_default();
            for r in nm["installs"].as_array().unwrap() {
                match out.iter().position(|x| key(x) == key(r)) {
                    Some(i) => { if at_of(r) >= at_of(&out[i]) { out[i] = r.clone(); } }
                    None => out.push(r.clone()),
                }
            }
            log(format!("merged install journals ({} records)", out.len()));
            om.insert("installs".into(), Value::Array(out));
            Some(Value::Object(om))
        }
        ("seen.json", Value::Object(mut om), Value::Object(nm)) => {
            for (k, v) in nm {
                let hi = v.as_u64().unwrap_or(0).max(om.get(&k).and_then(|x| x.as_u64()).unwrap_or(0));
                om.insert(k, Value::from(hi));
            }
            Some(Value::Object(om))
        }
        (_, Value::Array(mut oa), Value::Array(na)) => {
            for v in na { if !oa.contains(&v) { oa.push(v); } }
            Some(Value::Array(oa))
        }
        (_, Value::Object(mut om), Value::Object(nm)) => {
            for (k, v) in nm {
                if om.get(&k).is_some_and(|x| *x != v) { log(format!("{}: \"{k}\" set after the failed start kept (was {})", at.display(), om[&k])); }
                om.insert(k, v);
            }
            Some(Value::Object(om))
        }
        _ => None,
    }
}

fn write_atomic(p: &Path, b: &[u8]) -> std::io::Result<()> {
    if let Some(d) = p.parent() { fs::create_dir_all(d)?; }
    let tmp = sibling(p, ".merge-tmp");
    fs::write(&tmp, b)?;
    fs::rename(&tmp, p)
}

/// Move every file of `src` into `dst`, renaming on a clash (identical files are just dropped).
fn merge_downloads(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for e in fs::read_dir(src)?.flatten() {
        let from = e.path();
        let m = fs::symlink_metadata(&from)?;
        if crate::safecopy::is_link(&m) { continue; }
        let mut to = dst.join(e.file_name());
        if m.is_dir() { merge_downloads(&from, &to)?; continue; }
        if !m.is_file() { continue; }
        if let Ok(t) = fs::symlink_metadata(&to) {
            if t.len() == m.len() && fs::read(&to).ok() == fs::read(&from).ok() { continue; }
            let p = Path::new(&e.file_name()).to_path_buf();
            let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let ext = p.extension().map(|x| format!(".{}", x.to_string_lossy())).unwrap_or_default();
            let mut i = 2;
            while fs::symlink_metadata(&to).is_ok() { to = dst.join(format!("{stem} ({i}){ext}")); i += 1; }
            log(format!("{} was renamed to {} (name taken)", from.display(), to.display()));
        }
        // copy then remove (never a cut-off file under the final name)
        if fs::rename(&from, &to).is_err() {
            let part = sibling(&to, ".migrating");
            fs::copy(&from, &part)?;
            fs::rename(&part, &to)?;
            let _ = fs::remove_file(&from);
        }
    }
    Ok(())
}

fn has_own_data_webview(ebw: &Path) -> bool {
    let Ok(profiles) = fs::read_dir(ebw) else { return false };
    for p in profiles.flatten() {
        let Ok(files) = fs::read_dir(p.path().join("Local Storage/leveldb")) else { continue };
        for f in files.flatten() {
            if fs::read(f.path()).is_ok_and(|b| b.windows(7).any(|w| w == b"unpack.")) { return true; }
        }
    }
    false
}

/// Move `from` (a Downloads folder) to `to` when `to` doesn't exist. Across drives: copy into
/// `to.migrating`, verify sizes, rename, then remove the old one (R17 L1); a leftover
/// `.migrating` from a killed run is thrown away and redone.
fn move_downloads(from: &Path, to: &Path) -> std::io::Result<()> {
    let part = sibling(to, ".migrating");
    if part.exists() { fs::remove_dir_all(&part)?; }
    if !fs::symlink_metadata(from).is_ok_and(|m| m.is_dir()) || fs::symlink_metadata(to).is_ok() { return Ok(()); }
    #[cfg(test)]
    let forced = FORCE_COPY_DOWNLOADS.with(|f| f.get());
    #[cfg(not(test))]
    let forced = false;
    if !forced && fs::rename(from, to).is_ok() { return Ok(()); }
    let want = measure(from, false)?;
    let mut n = 0;
    copy_tree(from, &part, false, &mut n)?;
    if measure(&part, false)? != want {
        let _ = fs::remove_dir_all(&part);
        return Err(std::io::Error::other("Downloads copy did not match; will retry next start"));
    }
    fs::rename(&part, to)?;
    let _ = fs::remove_dir_all(from);
    Ok(())
}

/// (files, bytes) under `dir`, links skipped; `skip_moved` leaves out the top-level Downloads.
fn measure(dir: &Path, skip_moved: bool) -> std::io::Result<(usize, u64)> {
    let mut t = (0usize, 0u64);
    for e in fs::read_dir(dir)? {
        let e = e?;
        if skip_moved && e.file_name() == MOVED { continue; }
        if e.file_name() == PENDING { continue; }
        let m = fs::symlink_metadata(e.path())?;
        if crate::safecopy::is_link(&m) { continue; }
        if m.is_dir() {
            let (f, b) = measure(&e.path(), false)?;
            t = (t.0 + f, t.1 + b);
        } else if m.is_file() {
            t = (t.0 + 1, t.1 + m.len());
        }
    }
    Ok(t)
}

fn copy_tree(src: &Path, dst: &Path, skip_moved: bool, count: &mut usize) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for e in fs::read_dir(src)? {
        let e = e?;
        if skip_moved && e.file_name() == MOVED { continue; }
        let from = e.path();
        let to = dst.join(e.file_name());
        let meta = fs::symlink_metadata(&from)?;
        if crate::safecopy::is_link(&meta) { continue; }
        if meta.is_dir() {
            copy_tree(&from, &to, false, count)?;
        } else if meta.is_file() {
            #[cfg(test)]
            {
                if FAIL_NAME.with(|f| f.borrow().as_deref() == e.file_name().to_str()) {
                    return Err(std::io::Error::other(format!("could not copy {}", from.display())));
                }
                if KILL_AFTER.with(|k| k.get()) == Some(*count) {
                    return Err(std::io::Error::other("killed"));
                }
            }
            fs::copy(&from, &to).map_err(|err| std::io::Error::other(format!("could not copy {}: {err}", from.display())))?;
            *count += 1;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r15_l5_old_data_is_copied_once_and_kept() {
        let d = crate::testutil::TmpDir::new("migrate");
        let old = d.join(OLD_ID);
        let new = d.join(NEW_ID);
        for (p, b) in [
            ("catalog/journal.json", "{\"installs\":[1]}"),
            ("catalog/seen.json", "{\"v\":7}"),
            ("catalog/backups/x-launch.ini", "[Plugins]"),
            ("ftp-resume/abc.json", "{}"),
            ("grants.json", "[]"),
            ("EBWebView/Default/Local Storage/leveldb/000003.log", "unpack.consoles"),
        ] {
            let f = old.join(p);
            fs::create_dir_all(f.parent().unwrap()).unwrap();
            fs::write(&f, b).unwrap();
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink("/etc", old.join("evil")).unwrap();
        assert!(migrate_dir(&old, &new).unwrap());
        assert_eq!(fs::read_to_string(new.join("catalog/journal.json")).unwrap(), "{\"installs\":[1]}");
        assert_eq!(fs::read_to_string(new.join("catalog/seen.json")).unwrap(), "{\"v\":7}");
        assert!(new.join("catalog/backups/x-launch.ini").is_file() && new.join("ftp-resume/abc.json").is_file());
        assert!(new.join("EBWebView/Default/Local Storage/leveldb/000003.log").is_file(), "WebView storage (localStorage) copied");
        assert!(fs::symlink_metadata(new.join("evil")).is_err(), "links are not copied");
        assert!(old.join("catalog/journal.json").is_file(), "old folder is kept");
        // second start: no-op, nothing overwritten
        fs::write(new.join("catalog/journal.json"), "{\"installs\":[]}").unwrap();
        assert!(!migrate_dir(&old, &new).unwrap());
        assert_eq!(fs::read_to_string(new.join("catalog/journal.json")).unwrap(), "{\"installs\":[]}");
    }

    #[test]
    fn r15_l5_new_folder_with_its_own_data_is_left_alone() {
        let d = crate::testutil::TmpDir::new("migrate2");
        let (old, new) = (d.join(OLD_ID), d.join(NEW_ID));
        fs::create_dir_all(old.join("catalog")).unwrap();
        fs::write(old.join("catalog/journal.json"), "old").unwrap();
        fs::create_dir_all(new.join("catalog")).unwrap();
        fs::write(new.join("catalog/journal.json"), "new").unwrap();
        assert!(!migrate_dir(&old, &new).unwrap());
        assert_eq!(fs::read_to_string(new.join("catalog/journal.json")).unwrap(), "new");
        // no old folder: nothing happens
        let none = d.join("nothing");
        assert!(!migrate_dir(&none, &d.join("fresh")).unwrap());
        assert!(!d.join("fresh").exists());
    }

    #[test]
    fn r16_l1_kill_unreadable_downloads_and_retry() {
        let d = crate::testutil::TmpDir::new("migrate3");
        let (old, new) = (d.join(OLD_ID), d.join(NEW_ID));
        for (p, b) in [("a.json", "1"), ("catalog/journal.json", "j"), ("catalog/big.bin", "0123456789"), ("Downloads/game.zip", "zip")] {
            let f = old.join(p);
            fs::create_dir_all(f.parent().unwrap()).unwrap();
            fs::write(&f, b).unwrap();
        }
        // 1. killed after one file: new folder untouched, only a stage; next start redoes it fully.
        KILL_AFTER.with(|k| k.set(Some(1)));
        assert!(migrate_dir(&old, &new).is_err());
        KILL_AFTER.with(|k| k.set(None));
        assert!(!new.exists() && !new.join(MARKER).exists());
        // simulate a real kill leaving a truncated staged file behind
        fs::create_dir_all(staging_of(&new).join("catalog")).unwrap();
        fs::write(staging_of(&new).join("catalog/big.bin"), "01").unwrap();
        // 2. an unreadable file fails the run: no marker, nothing in <new>, retried later.
        FAIL_NAME.with(|f| *f.borrow_mut() = Some("journal.json".into()));
        assert!(migrate_dir(&old, &new).is_err());
        FAIL_NAME.with(|f| *f.borrow_mut() = None);
        assert!(!new.exists() && !staging_of(&new).exists());
        // 3. clean run: everything copied whole, Downloads moved (not doubled), marker written.
        assert!(migrate_dir(&old, &new).unwrap());
        assert_eq!(fs::read_to_string(new.join("catalog/big.bin")).unwrap(), "0123456789");
        assert_eq!(fs::read_to_string(new.join("catalog/journal.json")).unwrap(), "j");
        assert_eq!(fs::read_to_string(new.join("Downloads/game.zip")).unwrap(), "zip");
        assert!(!old.join("Downloads").exists(), "Downloads moved, not copied");
        assert!(old.join("catalog/big.bin").is_file(), "old data kept");
        assert!(new.join(MARKER).exists() && !new.join(PENDING).exists() && !staging_of(&new).exists());
        // 4. killed after the rename (pending marker): next start finishes the Downloads move.
        let (old2, new2) = (d.join("o2"), d.join("n2"));
        fs::create_dir_all(old2.join("Downloads")).unwrap();
        fs::write(old2.join("Downloads/x"), "x").unwrap();
        fs::create_dir_all(&new2).unwrap();
        fs::write(new2.join("a.json"), "1").unwrap();
        fs::write(new2.join(PENDING), "").unwrap();
        assert!(migrate_dir(&old2, &new2).unwrap());
        assert!(new2.join("Downloads/x").is_file() && new2.join(MARKER).exists() && !new2.join(PENDING).exists());
    }

    fn put(root: &Path, items: &[(&str, &str)]) {
        for (p, b) in items {
            let f = root.join(p);
            fs::create_dir_all(f.parent().unwrap()).unwrap();
            fs::write(&f, b).unwrap();
        }
    }

    /// R17 M1 repro: an unreadable seen.json fails the first start; the WebView then writes its
    /// caches into the new folder; after the file is readable again the next start still migrates.
    #[cfg(unix)]
    #[test]
    fn r17_m1_failed_migration_is_retried_after_webview_wrote() {
        use std::os::unix::fs::PermissionsExt;
        let d = crate::testutil::TmpDir::new("migrate-r17");
        let (old, new) = (d.join(OLD_ID), d.join(NEW_ID));
        put(&old, &[("catalog/journal.json", "J"), ("catalog/seen.json", "S"),
            ("EBWebView/Default/Local Storage/leveldb/000003.log", "unpack.consoles=[1]")]);
        let seen = old.join("catalog/seen.json");
        fs::set_permissions(&seen, fs::Permissions::from_mode(0o000)).unwrap();
        let readable = fs::read(&seen).is_ok(); // root can read anyway: fall back to the hook
        if readable { FAIL_NAME.with(|f| *f.borrow_mut() = Some("seen.json".into())); }
        assert!(migrate_dir(&old, &new).is_err());
        FAIL_NAME.with(|f| *f.borrow_mut() = None);
        fs::set_permissions(&seen, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(retry_marker(&new).exists() && !new.join(MARKER).exists());
        // the app started anyway: WebView caches, a fresh (empty) localStorage, and a new grant
        put(&new, &[("EBWebView/Default/Cache/data_0", "c"), ("EBWebView/Default/Local Storage/leveldb/000001.log", ""),
            ("hsts-storage.sqlite", "h"), ("grants.json", "[\"newer\"]")]);
        assert!(migrate_dir(&old, &new).unwrap(), "retried");
        assert_eq!(fs::read_to_string(new.join("catalog/journal.json")).unwrap(), "J");
        assert_eq!(fs::read_to_string(new.join("catalog/seen.json")).unwrap(), "S");
        assert!(fs::read_to_string(new.join("EBWebView/Default/Local Storage/leveldb/000003.log")).unwrap().contains("unpack.consoles"));
        assert_eq!(fs::read_to_string(new.join("grants.json")).unwrap(), "[\"newer\"]", "own file written after the failure kept");
        assert!(new.join("hsts-storage.sqlite").is_file() && !backup_of(&new).exists(), "post-failure data merged in, backup removed");
        assert!(new.join(MARKER).exists() && !retry_marker(&new).exists());
        assert!(!migrate_dir(&old, &new).unwrap(), "done: no further runs");
    }

    /// R17 M1: refused for lack of space counts as failed too; and WebView caches alone (no marker)
    /// are not "own data".
    #[test]
    fn r17_m1_webview_caches_alone_are_not_own_data() {
        let d = crate::testutil::TmpDir::new("migrate-r17b");
        let (old, new) = (d.join(OLD_ID), d.join(NEW_ID));
        put(&old, &[("catalog/journal.json", "J")]);
        put(&new, &[("EBWebView/Default/Cache/data_0", "c"), ("EBWebView/Default/Local Storage/leveldb/000001.log", "x")]);
        assert!(!has_own_data(&new));
        assert!(migrate_dir(&old, &new).unwrap());
        assert_eq!(fs::read_to_string(new.join("catalog/journal.json")).unwrap(), "J");
        let n2 = d.join("n2");
        put(&n2, &[("EBWebView/Default/Local Storage/leveldb/000003.log", "unpack.drives")]);
        assert!(has_own_data(&n2), "localStorage with Unpack keys is own data");
    }

    /// R17 L1: cross-drive Downloads copy killed mid-way is redone on the next start, not accepted.
    #[test]
    fn r17_l1_cross_drive_downloads_kill_is_redone() {
        let d = crate::testutil::TmpDir::new("migrate-r17c");
        let (old, new) = (d.join(OLD_ID), d.join(NEW_ID));
        put(&old, &[("a.json", "1"), ("Downloads/a.zip", "AAAA"), ("Downloads/b.zip", "BBBBBBBB")]);
        FORCE_COPY_DOWNLOADS.with(|f| f.set(true));
        // a.json is file 0 of the main copy; Downloads copy starts counting at 0 again: kill after 1
        let r = std::panic::catch_unwind(|| {
            KILL_AFTER.with(|k| k.set(Some(1)));
            migrate_dir(&old, &new)
        }).unwrap();
        KILL_AFTER.with(|k| k.set(None));
        assert!(r.is_err());
        assert!(new.join(PENDING).exists() && !new.join(MARKER).exists());
        // plant a cut-off partial as a real kill would leave it
        put(&new, &[("Downloads.migrating/b.zip", "BB")]);
        assert!(migrate_dir(&old, &new).unwrap());
        FORCE_COPY_DOWNLOADS.with(|f| f.set(false));
        assert_eq!(fs::read_to_string(new.join("Downloads/b.zip")).unwrap(), "BBBBBBBB");
        assert_eq!(fs::read_to_string(new.join("Downloads/a.zip")).unwrap(), "AAAA");
        assert!(!new.join("Downloads.migrating").exists() && !old.join("Downloads").exists());
        assert!(new.join(MARKER).exists());
    }

    fn journal(recs: &[(&str, u64)]) -> String {
        let v: Vec<_> = recs.iter().map(|(id, at)| serde_json::json!({"target": "usb", "entry_id": id, "installed_at": at})).collect();
        serde_json::json!({"installs": v}).to_string()
    }

    /// R18 M1 (reviewer's repro): after a failed start the user installs NEWMOD, a catalog is seen
    /// at v3 and settings change. The retry keeps OLDMOD and NEWMOD, seen stays at v7, and the
    /// newer settings profile wins (old one kept). R18 L1: post-failure Downloads are merged
    /// (renamed on a clash) and the .pre-migration backup is removed.
    #[test]
    fn r18_m1_retry_merges_instead_of_overwriting() {
        let d = crate::testutil::TmpDir::new("migrate-r18");
        let (old, new) = (d.join(OLD_ID), d.join(NEW_ID));
        put(&old, &[("catalog/journal.json", &journal(&[("OLDMOD", 100), ("BOTH", 100)])), ("catalog/seen.json", r#"{"v":7,"w":1}"#),
            ("catalog/config.json", r#"{"source":"url","url":"OLD"}"#), ("catalog/backups/x.ini", "old"),
            ("grants.json", r#"["a"]"#), ("EBWebView/Default/Local Storage/leveldb/000003.log", "unpack.consoles=OLD"),
            ("Downloads/game.zip", "GAME"), ("Downloads/same.zip", "S")]);
        FAIL_NAME.with(|f| *f.borrow_mut() = Some("seen.json".into()));
        assert!(migrate_dir(&old, &new).is_err());
        FAIL_NAME.with(|f| *f.borrow_mut() = None);
        put(&new, &[("catalog/journal.json", &journal(&[("NEWMOD", 200), ("BOTH", 300)])), ("catalog/seen.json", r#"{"v":3,"z":2}"#),
            ("catalog/config.json", r#"{"source":"url","url":"NEW"}"#), ("catalog/backups/x.ini", "new"),
            ("grants.json", r#"["b"]"#), ("EBWebView/Default/Local Storage/leveldb/000005.log", "unpack.consoles=NEWSETTINGS"),
            ("Downloads/new.zip", "NEW"), ("Downloads/game.zip", "OTHER"), ("Downloads/same.zip", "S")]);
        assert!(migrate_dir(&old, &new).unwrap());
        let j: serde_json::Value = serde_json::from_str(&fs::read_to_string(new.join("catalog/journal.json")).unwrap()).unwrap();
        let ids: Vec<(String, u64)> = j["installs"].as_array().unwrap().iter().map(|r| (r["entry_id"].as_str().unwrap().into(), r["installed_at"].as_u64().unwrap())).collect();
        assert_eq!(ids, vec![("OLDMOD".into(), 100), ("BOTH".into(), 300), ("NEWMOD".into(), 200)]);
        let seen: serde_json::Value = serde_json::from_str(&fs::read_to_string(new.join("catalog/seen.json")).unwrap()).unwrap();
        assert_eq!(seen, serde_json::json!({"v":7,"w":1,"z":2}), "rollback floor never lowered");
        let cfg: serde_json::Value = serde_json::from_str(&fs::read_to_string(new.join("catalog/config.json")).unwrap()).unwrap();
        assert_eq!(cfg["url"], "NEW", "newer setting kept");
        assert_eq!(fs::read_to_string(new.join("grants.json")).unwrap().replace([' ', '\n'], ""), r#"["a","b"]"#);
        assert_eq!(fs::read_to_string(new.join("catalog/backups/x.ini")).unwrap(), "new");
        assert_eq!(fs::read_to_string(new.join("catalog/backups/x.ini.old-version")).unwrap(), "old");
        assert!(fs::read_to_string(new.join("EBWebView/Default/Local Storage/leveldb/000005.log")).unwrap().contains("NEWSETTINGS"));
        assert!(fs::read_to_string(new.join("EBWebView.old-version/Default/Local Storage/leveldb/000003.log")).unwrap().contains("OLD"));
        assert_eq!(fs::read_to_string(new.join("Downloads/game.zip")).unwrap(), "GAME");
        assert_eq!(fs::read_to_string(new.join("Downloads/game (2).zip")).unwrap(), "OTHER");
        assert_eq!(fs::read_to_string(new.join("Downloads/new.zip")).unwrap(), "NEW");
        assert!(!new.join("Downloads/same (2).zip").exists(), "identical file not duplicated");
        assert!(!backup_of(&new).exists() && new.join(MARKER).exists() && !retry_marker(&new).exists());
        // merging again (a resume) changes nothing
        let before = fs::read(new.join("catalog/journal.json")).unwrap();
        merge_newer(&new.join("catalog"), &new.join("catalog")).unwrap();
        assert_eq!(fs::read(new.join("catalog/journal.json")).unwrap(), before);
    }

    /// R18 L1: a backup left by an earlier attempt is merged, not deleted.
    #[test]
    fn r18_l1_earlier_backup_is_merged() {
        let d = crate::testutil::TmpDir::new("migrate-r18b");
        let (old, new) = (d.join(OLD_ID), d.join(NEW_ID));
        put(&old, &[("catalog/journal.json", &journal(&[("OLDMOD", 1)]))]);
        put(&backup_of(&new), &[("catalog/journal.json", &journal(&[("EARLY", 2)])), ("Downloads/early.zip", "E")]);
        put(&new, &[("catalog/journal.json", &journal(&[("LATER", 3)])), ("Downloads/later.zip", "L")]);
        fs::write(retry_marker(&new), "").unwrap();
        assert!(migrate_dir(&old, &new).unwrap());
        let j = fs::read_to_string(new.join("catalog/journal.json")).unwrap();
        assert!(j.contains("OLDMOD") && j.contains("EARLY") && j.contains("LATER"), "{j}");
        assert!(new.join("Downloads/early.zip").is_file() && new.join("Downloads/later.zip").is_file());
        assert!(!backup_of(&new).exists());
    }
}
