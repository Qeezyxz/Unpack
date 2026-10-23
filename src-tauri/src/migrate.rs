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

/// Copy `old` into `new` when `old` exists and `new` has not been migrated yet and holds no data
/// of its own. Returns true when it migrated.
///
/// Crash-safe (R16 L1): everything is copied into `<new>.migrating`, every file is checked by size,
/// and only then is the folder renamed to `<new>` in one step. A run that is killed leaves only the
/// `.migrating` folder, which the next start throws away and redoes. `Downloads` is moved, not
/// copied. Any file that can't be copied fails the whole run (nothing is marked, the next start
/// retries); free space is checked before copying.
pub fn migrate_dir(old: &Path, new: &Path) -> std::io::Result<bool> {
    let Ok(m) = fs::symlink_metadata(old) else { return Ok(false) };
    if !m.is_dir() { return Ok(false); }
    if new.join(MARKER).exists() { return Ok(false); }
    if new.join(PENDING).exists() {
        // Killed after the rename: only the Downloads move and the marker are left.
        return finish(old, new).map(|_| true);
    }
    let new_has_data = fs::read_dir(new).map(|mut r| r.next().is_some()).unwrap_or(false);
    if new_has_data {
        // The new identifier already has its own data: never mix the two. Mark it so we don't re-check.
        let _ = fs::write(new.join(MARKER), b"skipped: new folder already had data\n");
        return Ok(false);
    }
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
        return Err(e);
    }
    // Verify: same files, same sizes.
    if measure(&stage, true)? != (files, bytes) {
        let _ = fs::remove_dir_all(&stage);
        return Err(std::io::Error::other("copy did not match the old folder; will retry next start"));
    }
    fs::write(stage.join(PENDING), b"")?;
    if new.exists() { fs::remove_dir(new)?; } // empty (checked above)
    fs::rename(&stage, new)?;
    finish(old, new)?;
    Ok(true)
}

fn finish(old: &Path, new: &Path) -> std::io::Result<()> {
    let (from, to) = (old.join(MOVED), new.join(MOVED));
    if fs::symlink_metadata(&from).is_ok_and(|m| m.is_dir()) && fs::symlink_metadata(&to).is_err() {
        if fs::rename(&from, &to).is_err() {
            // Different drive (shouldn't happen for siblings): copy, then remove the old one.
            let mut n = 0;
            copy_tree(&from, &to, false, &mut n)?;
            let _ = fs::remove_dir_all(&from);
        }
    }
    fs::write(new.join(MARKER), format!("copied from {}\n", old.display()))?;
    fs::remove_file(new.join(PENDING))
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
}
