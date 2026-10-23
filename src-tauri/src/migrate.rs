//! First-run migration from the old app identifier (R15 L5).
//!
//! 1.6.0 changed the identifier from `com.justin.unpack` to `com.qeezyxz.unpack`, which moves the
//! app data folders (catalog journal, seen.json, launch.ini backups, FTP resume records, grants,
//! downloads) and the WebView storage (saved consoles, drives and settings in localStorage). On
//! the first start under the new identifier the old folders are copied over, before Tauri or the
//! WebView open anything. Nothing is deleted: the old folders stay as they are, and files that
//! already exist in the new folder are never overwritten. Links are not followed.

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

/// Copy `old` into `new` when `old` exists and `new` has not been migrated yet and holds no data
/// of its own (missing, empty, or only the marker). Returns true when it copied.
pub fn migrate_dir(old: &Path, new: &Path) -> std::io::Result<bool> {
    let Ok(m) = fs::symlink_metadata(old) else { return Ok(false) };
    if !m.is_dir() { return Ok(false); }
    if new.join(MARKER).exists() { return Ok(false); }
    let new_has_data = fs::read_dir(new).map(|mut r| r.next().is_some()).unwrap_or(false);
    if new_has_data {
        // The new identifier already has its own data: never mix the two. Mark it so we don't re-check.
        let _ = fs::write(new.join(MARKER), b"skipped: new folder already had data\n");
        return Ok(false);
    }
    fs::create_dir_all(new)?;
    copy_tree(old, new)?;
    fs::write(new.join(MARKER), format!("copied from {}\n", old.display()))?;
    Ok(true)
}

fn copy_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for e in fs::read_dir(src)? {
        let e = e?;
        let from = e.path();
        let to = dst.join(e.file_name());
        let meta = fs::symlink_metadata(&from)?;
        if crate::safecopy::is_link(&meta) { continue; }
        if meta.is_dir() {
            copy_tree(&from, &to)?;
        } else if meta.is_file() && fs::symlink_metadata(&to).is_err() {
            // A locked file (e.g. a WebView LOCK file) is skipped rather than failing the whole copy.
            if let Err(err) = fs::copy(&from, &to) { eprintln!("migration: could not copy {}: {err}", from.display()); }
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
}
