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
    if OWN.iter().any(|n| fs::symlink_metadata(new.join(n)).is_ok()) { return true; }
    let Ok(profiles) = fs::read_dir(new.join("EBWebView")) else { return false };
    for p in profiles.flatten() {
        let Ok(files) = fs::read_dir(p.path().join("Local Storage/leveldb")) else { continue };
        for f in files.flatten() {
            if fs::read(f.path()).is_ok_and(|b| b.windows(7).any(|w| w == b"unpack.")) { return true; }
        }
    }
    false
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
        if bak.exists() { fs::remove_dir_all(&bak)?; }
        fs::rename(new, &bak)?;
    }
    fs::rename(&stage, new)?;
    finish(old, new)?;
    Ok(true)
}

fn finish(old: &Path, new: &Path) -> std::io::Result<()> {
    // Unpack's own files written after a failed run (installs journaled, grants) are newer than
    // the old folder's: put them back over the migrated copy. WebView data stays the old profile.
    let bak = backup_of(new);
    for n in OWN.iter().filter(|n| **n != MOVED) {
        let from = bak.join(n);
        match fs::symlink_metadata(&from) {
            Ok(m) if m.is_dir() => overlay(&from, &new.join(n))?,
            Ok(m) if m.is_file() => { fs::copy(&from, new.join(n))?; }
            _ => {}
        }
    }
    move_downloads(&old.join(MOVED), &new.join(MOVED))?;
    move_downloads(&bak.join(MOVED), &new.join(MOVED))?;
    fs::write(new.join(MARKER), format!("copied from {}\n", old.display()))?;
    let _ = fs::remove_file(new.join(PENDING));
    let _ = fs::remove_file(retry_marker(new));
    Ok(())
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

/// Copy every file of `src` over `dst` (overwriting), links skipped.
fn overlay(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for e in fs::read_dir(src)?.flatten() {
        let m = fs::symlink_metadata(e.path())?;
        if crate::safecopy::is_link(&m) { continue; }
        if m.is_dir() { overlay(&e.path(), &dst.join(e.file_name()))?; } else if m.is_file() { fs::copy(e.path(), dst.join(e.file_name()))?; }
    }
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
        assert!(backup_of(&new).join("hsts-storage.sqlite").is_file(), "post-failure data kept aside, not deleted");
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
}
