//! Folders the user chose in a native file dialog. Persisted separately from settings and used
//! to restore path grants on startup (the webview cannot add to this list).

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::safecopy::{plain, simplify, Grants};

const FILE: &str = "picked-folders.json";
const MIGRATION_MARKER: &str = ".picked-folders-library-migration-v1";

/// Roots from the most recent `dlc_scan_drives` call; cleared/replaced on each scan.
#[derive(Default)]
pub struct LastScannedRoots {
    roots: Mutex<Vec<PathBuf>>,
}

impl LastScannedRoots {
    pub fn set_from_scan(&self, paths: &[String]) {
        let mut roots = Vec::new();
        for p in paths {
            if p.trim().is_empty() {
                continue;
            }
            if let Ok(c) = fs::canonicalize(p) {
                roots.push(simplify(c));
            }
        }
        *self.roots.lock().unwrap() = roots;
    }

    /// `record_user_folder` may only persist a path that equals one of these roots.
    pub fn require_scanned_root(&self, path: &Path) -> Result<(), String> {
        let Ok(c) = fs::canonicalize(path) else {
            return Err("That folder doesn't exist or can't be accessed.".into());
        };
        let c = simplify(c);
        if self.roots.lock().unwrap().iter().any(|r| r == &c) {
            Ok(())
        } else {
            Err(
                "That folder wasn't in the last Scan drives result. Scan again, then click Add."
                    .into(),
            )
        }
    }
}

#[derive(Default)]
pub struct PickedFolders {
    paths: Mutex<Vec<PathBuf>>,
    file: Mutex<Option<PathBuf>>,
    app_data: Mutex<Option<PathBuf>>,
}

impl PickedFolders {
    pub fn load(app_data: PathBuf) -> Self {
        let file = app_data.join(FILE);
        let mut paths = Vec::new();
        if let Ok(s) = fs::read_to_string(&file) {
            match serde_json::from_str::<Vec<PathBuf>>(&s) {
                Ok(v) => paths = v.into_iter().take(5000).collect(),
                Err(e) => {
                    backup_corrupt(&file);
                    eprintln!(
                        "unpack: {} is corrupt ({e}); kept previous list empty and backed up the file",
                        file.display()
                    );
                }
            }
        }
        PickedFolders {
            paths: Mutex::new(paths),
            file: Mutex::new(Some(file)),
            app_data: Mutex::new(Some(app_data)),
        }
    }

    fn save(&self) {
        let paths = self.paths.lock().unwrap();
        if let Some(f) = self.file.lock().unwrap().as_ref() {
            if let Some(d) = f.parent() {
                let _ = fs::create_dir_all(d);
            }
            let body = serde_json::to_string(&*paths).unwrap_or_default();
            static N: AtomicU64 = AtomicU64::new(0);
            let tmp = f.with_file_name(format!(
                ".picked-folders.{}.{:?}.{}.tmp",
                std::process::id(),
                std::thread::current().id(),
                N.fetch_add(1, Ordering::SeqCst)
            ));
            if let Ok(mut out) = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&tmp)
            {
                if out.write_all(body.as_bytes()).is_ok()
                    && out.sync_all().is_ok()
                    && fs::rename(&tmp, f).is_ok()
                {
                    return;
                }
            }
            let _ = fs::remove_file(&tmp);
        }
    }

    /// A path the user chose in a native dialog: grant now; remember for auto-restore when safe.
    pub fn record_dialog_pick(&self, grants: &Grants, p: &Path) -> Result<(), String> {
        let Ok(c) = fs::canonicalize(p) else {
            return Err("That folder doesn't exist or can't be accessed.".into());
        };
        let c = simplify(c);
        if is_system_drive_root(&c) {
            return Err(
                "Unpack can't use the whole system drive as a folder. Pick a folder on your PC or a USB drive root like E:\\.".into(),
            );
        }
        let disp = c.display().to_string();
        if is_system_path(&disp) {
            return Err(
                "That folder is a Windows or Unix system location. Pick a normal folder instead.".into(),
            );
        }
        if !allow_auto_restore(&c) {
            return Err("That folder can't be remembered for automatic access.".into());
        }
        grants.grant(&c);
        {
            let mut paths = self.paths.lock().unwrap();
            if !paths.iter().any(|x| x == &c) {
                paths.push(c.clone());
            }
        }
        self.save();
        Ok(())
    }

    /// Approved paths (migration confirm) that pass bulk safety rules.
    pub fn remember_approved(&self, grants: &Grants, paths: &[String]) {
        for p in paths {
            if p.trim().is_empty() || reject_for_bulk_add(p) {
                continue;
            }
            let pb = PathBuf::from(p);
            if !pb.exists() {
                continue;
            }
            let Ok(c) = fs::canonicalize(&pb) else {
                continue;
            };
            let c = simplify(c);
            if !allow_auto_restore(&c) {
                continue;
            }
            {
                let mut list = self.paths.lock().unwrap();
                if !list.iter().any(|x| x == &c) {
                    list.push(c.clone());
                }
            }
            grants.grant(&c);
        }
        self.save();
    }

    /// Rebuild persisted grants from this list (ignores old grants.json entries).
    pub fn restore_grants(&self, grants: &Grants) {
        grants.clear_persist();
        let paths = self.paths.lock().unwrap().clone();
        let mut kept = Vec::new();
        for p in paths {
            if !p.exists() {
                continue;
            }
            let Ok(c) = fs::canonicalize(&p) else {
                continue;
            };
            let c = simplify(c);
            if !allow_auto_restore(&c) {
                continue;
            }
            grants.grant(&c);
            if !kept.iter().any(|x| x == &c) {
                kept.push(c);
            }
        }
        *self.paths.lock().unwrap() = kept;
        self.save();
    }

    pub fn run_library_migration_if_needed(
        &self,
        app: &tauri::AppHandle,
        grants: &Grants,
    ) {
        let app_data = self.app_data.lock().unwrap().clone();
        let Some(app_data) = app_data else {
            return;
        };
        let marker = app_data.join(MIGRATION_MARKER);
        if marker.is_file() {
            return;
        }
        let libraries = read_libraries_from_web_storage(&app_data);
        let candidates: Vec<String> = libraries
            .into_iter()
            .filter(|p| !p.trim().is_empty() && !reject_for_bulk_add(p))
            .filter(|p| Path::new(p).exists())
            .collect();
        if !candidates.is_empty() {
            let body = candidates
                .iter()
                .map(|p| format!("• {}", plain(p)))
                .collect::<Vec<_>>()
                .join("\n");
            let msg = format!(
                "Unpack 1.7.4 needs your OK to access these game library folders again (saved in your settings):\n\n{body}\n\nAllow access? You can change library folders anytime in Settings."
            );
            use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
            let yes = app
                .dialog()
                .message(msg)
                .title("Game library folder access")
                .kind(MessageDialogKind::Info)
                .buttons(MessageDialogButtons::YesNo)
                .blocking_show();
            if yes {
                self.remember_approved(grants, &candidates);
            }
        }
        let _ = fs::write(marker, b"1");
    }
}

fn backup_corrupt(file: &Path) {
    static N: AtomicU64 = AtomicU64::new(0);
    let first = file.with_extension("json.bak");
    let bak = if first.exists() {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let n = N.fetch_add(1, Ordering::SeqCst);
        file.with_file_name(format!("picked-folders.corrupt-{ts}-{n}.json.bak"))
    } else {
        first
    };
    let _ = fs::copy(file, &bak);
}

/// True for `/` on Unix and the Windows system volume root (e.g. `C:\`), not removable USB roots like `E:\`.
pub fn is_system_drive_root(p: &Path) -> bool {
    if p == Path::new("/") {
        return true;
    }
    if !is_bare_drive_root(p) {
        return false;
    }
    #[cfg(windows)]
    {
        if let Some(sys) = windows_system_volume_root() {
            return same_bare_root(p, &sys);
        }
    }
    is_hostile_root(p)
}

#[cfg(windows)]
fn windows_system_volume_root() -> Option<PathBuf> {
    let sys = std::env::var("SystemRoot")
        .or_else(|_| std::env::var("WINDIR"))
        .ok()?;
    let p = PathBuf::from(sys);
    let s = p.to_string_lossy();
    if s.len() >= 2 && s.as_bytes()[1] == b':' && s.as_bytes()[0].is_ascii_alphabetic() {
        let ch = char::from(s.as_bytes()[0]);
        return Some(PathBuf::from(format!("{}:\\", ch)));
    }
    None
}

#[cfg(windows)]
fn same_bare_root(a: &Path, b: &Path) -> bool {
    normalize_bare_root(a) == normalize_bare_root(b)
}

#[cfg(windows)]
fn normalize_bare_root(p: &Path) -> String {
    let s = p.to_string_lossy().trim_end_matches(['\\', '/']).to_string();
    if s == "/" {
        return "/".into();
    }
    #[cfg(windows)]
    if s.len() == 2 && s.as_bytes()[1] == b':' {
        return format!("{}:", char::from(s.as_bytes()[0]).to_ascii_uppercase());
    }
    #[cfg(windows)]
    if s.len() == 3 && s.as_bytes()[1] == b':' && s.as_bytes()[2] == b'\\' {
        return format!("{}:", char::from(s.as_bytes()[0]).to_ascii_uppercase());
    }
    s.to_ascii_lowercase()
}

/// Hostile roots we never auto-restore (legacy check; prefer `is_system_drive_root`).
pub fn is_hostile_root(p: &Path) -> bool {
    let s = p.to_string_lossy();
    if s == "/" {
        return true;
    }
    let lower = s.trim_end_matches(['\\', '/']).to_ascii_lowercase();
    lower == "c:" || lower == "c:\\"
}

/// Bare drive / filesystem root (`D:\`, `E:/`, `/mnt/usb`).
pub fn is_bare_drive_root(p: &Path) -> bool {
    let s = p.to_string_lossy().trim_end_matches(['\\', '/']).to_string();
    if s == "/" {
        return true;
    }
    if s.len() == 2 && s.as_bytes()[1] == b':' {
        return s.as_bytes()[0].is_ascii_alphabetic();
    }
    #[cfg(windows)]
    if s.len() == 3 && s.as_bytes()[1] == b':' && s.as_bytes()[2] == b'\\' {
        return s.as_bytes()[0].is_ascii_alphabetic();
    }
    false
}

/// Normalize a path string for system-folder checks (string only; use `fs::canonicalize` at pick/restore time).
pub fn normalize_path_for_check(p: &str) -> String {
    let mut s = p.trim().to_string();
    while let Some(rest) = s.strip_prefix(r"\\?\") {
        s = rest.to_string();
    }
    while let Some(rest) = s.strip_prefix(r"\\.\\") {
        s = rest.to_string();
    }
    if s.starts_with("//") {
        return s.replace('/', "\\").to_ascii_lowercase();
    }
    if s.starts_with('/') {
        return s.to_ascii_lowercase();
    }
    s.replace('/', "\\").to_ascii_lowercase()
}

fn raw_path_is_device_or_unc(p: &str) -> bool {
    let t = p.trim();
    t.starts_with(r"\\.\") || t.starts_with(r"\\?\") || t.starts_with("//") || (t.starts_with("\\\\") && !t.starts_with(r"\\?\"))
}

/// Windows / Unix system folders (mirrors importcheck.js `isSystemPath`).
pub fn is_system_path(p: &str) -> bool {
    if raw_path_is_device_or_unc(p) {
        return true;
    }
    regex_hostile(normalize_path_for_check(p))
}

fn windows_system_folder_on_drive(s: &str) -> bool {
    s.len() >= 3
        && s.as_bytes()[1] == b':'
        && s.as_bytes()[2] == b'\\'
        && (s[3..].starts_with("windows")
            || s[3..].starts_with("program files")
            || s[3..].starts_with("program files (x86)")
            || s[3..].starts_with("programdata")
            || s[3..].starts_with("$recycle.bin")
            || s[3..].starts_with("system volume information"))
}

fn windows_users_blocked(s: &str) -> bool {
    if s.len() < 10 || s.as_bytes().get(1) != Some(&b':') || s.as_bytes().get(2) != Some(&b'\\') {
        return false;
    }
    let after_drive = &s[3..];
    if !after_drive.starts_with("users\\") {
        return false;
    }
    let rest = &after_drive[6..];
    let first = rest.split('\\').next().unwrap_or("");
    if first.eq_ignore_ascii_case("default") || first.eq_ignore_ascii_case("all users") {
        return true;
    }
    rest.contains("\\appdata\\") || rest.ends_with("\\appdata")
}

fn regex_hostile(s: String) -> bool {
    if windows_system_folder_on_drive(&s) {
        return true;
    }
    if windows_users_blocked(&s) {
        return true;
    }
    if s.contains("\\system32\\") || s.contains("\\syswow64\\") {
        return true;
    }
    if s.starts_with("\\\\") && s.len() > 2 {
        return true;
    }
    s.starts_with("/etc/")
        || s == "/etc"
        || s.starts_with("/usr/")
        || s.starts_with("/bin/")
        || s.starts_with("/sbin/")
        || s.starts_with("/boot/")
        || s.starts_with("/dev/")
        || s.starts_with("/proc/")
        || s.starts_with("/sys/")
        || s.starts_with("/system/")
        || s.starts_with("/library/")
        || s.starts_with("/windows/")
        || s.starts_with("\\etc\\")
        || s.starts_with("\\usr\\")
        || s.starts_with("\\bin\\")
        || s.starts_with("\\sbin\\")
        || s.starts_with("\\boot\\")
        || s.starts_with("\\dev\\")
        || s.starts_with("\\proc\\")
        || s.starts_with("\\sys\\")
        || s.starts_with("\\system\\")
        || s.starts_with("\\library\\")
        || s.starts_with("\\windows\\")
}

/// Paths we refuse when seeding from settings / import (not dialog picks).
pub fn reject_for_bulk_add(p: &str) -> bool {
    let pb = Path::new(p);
    is_system_path(p) || is_system_drive_root(pb)
}

/// Whether a remembered path may be re-granted on startup (USB volume roots like `E:\` are OK).
pub fn allow_auto_restore(p: &Path) -> bool {
    if !p.exists() {
        return false;
    }
    let disp = p.display().to_string();
    if is_system_path(&disp) || is_system_drive_root(p) {
        return false;
    }
    true
}

/// Read `unpack.libraries` from WebView leveldb under app data (best-effort).
pub fn read_libraries_from_web_storage(app_data: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let ebw = app_data.join("EBWebView");
    if !ebw.is_dir() {
        return out;
    }
    let mut scan = |bytes: &[u8]| {
        if let Some(v) = extract_local_storage_string(bytes, "unpack.libraries") {
            if let Ok(arr) = serde_json::from_str::<Vec<String>>(&v) {
                for s in arr {
                    if !out.contains(&s) {
                        out.push(s);
                    }
                }
            }
        }
    };
    walk_leveldb(&ebw, &mut scan);
    out
}

fn walk_leveldb(dir: &Path, f: &mut dyn FnMut(&[u8])) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk_leveldb(&p, f);
            continue;
        }
        if let Ok(b) = fs::read(&p) {
            f(&b);
        }
    }
}

fn extract_local_storage_string(blob: &[u8], key: &str) -> Option<String> {
    let needle = key.as_bytes();
    let mut i = 0;
    while i + needle.len() < blob.len() {
        if blob[i..].starts_with(needle) {
            let rest = &blob[i + needle.len()..];
            if let Some(s) = parse_json_string_value(rest) {
                return Some(s);
            }
        }
        i += 1;
    }
    None
}

fn parse_json_string_value(rest: &[u8]) -> Option<String> {
    let start = rest.iter().position(|&b| b == b'[' || b == b'"')?;
    let slice = &rest[start..];
    if slice.first() == Some(&b'[') {
        let end = slice.iter().rposition(|&b| b == b']')?;
        return Some(String::from_utf8_lossy(&slice[..=end]).to_string());
    }
    if slice.first() != Some(&b'"') {
        return None;
    }
    let mut out = String::new();
    let mut esc = false;
    for &b in slice.iter().skip(1) {
        if esc {
            out.push(b as char);
            esc = false;
            continue;
        }
        if b == b'\\' {
            esc = true;
            continue;
        }
        if b == b'"' {
            return Some(out);
        }
        out.push(b as char);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::safecopy::Grants;
    use std::fs;

    #[test]
    fn grant_paths_ipc_command_removed() {
        let lib = include_str!("lib.rs");
        assert!(lib.contains("paths_are_granted,"));
        assert!(
            !lib
                .lines()
                .any(|l| l.trim() == "grant_paths," || l.contains("fn grant_paths(")),
            "grant_paths must not be a Tauri command"
        );
        let bridge = include_str!("../examples/bridge.rs");
        assert!(!bridge.contains("\"grant_paths\""));
    }

    #[test]
    fn bulk_add_refuses_system_paths_and_system_drive() {
        assert!(reject_for_bulk_add("/"));
        assert!(reject_for_bulk_add(r"C:\Windows\System32"));
        assert!(reject_for_bulk_add("/etc/passwd"));
        assert!(reject_for_bulk_add(r"D:\Windows\System32"));
        assert!(!reject_for_bulk_add(r"D:\Games\Content"));
        assert!(!reject_for_bulk_add(r"E:\"));
        assert!(reject_for_bulk_add(r"C:\"));
        assert!(!reject_for_bulk_add(r"C:\Users\Justin\Downloads\x"));
        assert!(reject_for_bulk_add(r"C:\Users\Justin\AppData\Roaming"));
        assert!(reject_for_bulk_add(r"\\server\share\games"));
    }

    #[test]
    fn users_downloads_allowed_appdata_refused() {
        assert!(!is_system_path(r"C:\Users\Justin\Downloads\x"));
        assert!(is_system_path(r"C:\Users\Justin\AppData\Roaming"));
        assert!(!is_system_path(r"D:\Users\me\Downloads\game"));
        assert!(is_system_path(r"D:\Users\me\AppData\Local"));
        assert!(is_system_path(r"D:\Users\Default"));
        assert!(is_system_path("/etc/passwd"));
    }

    #[test]
    fn record_user_folder_requires_scanned_root() {
        let d = crate::testutil::TmpDir::new("scan-roots");
        let allowed = d.join("usb");
        let other = d.join("other");
        fs::create_dir_all(&allowed).unwrap();
        fs::create_dir_all(&other).unwrap();
        let scan = LastScannedRoots::default();
        scan.set_from_scan(&[allowed.display().to_string()]);
        assert!(scan.require_scanned_root(&other).is_err());
        assert!(scan.require_scanned_root(&allowed).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn restore_skips_symlink_to_root() {
        let d = crate::testutil::TmpDir::new("picked-sym");
        let link = d.join("link");
        std::os::unix::fs::symlink("/", &link).unwrap();
        fs::write(
            d.join("picked-folders.json"),
            serde_json::to_string(&vec![link]).unwrap(),
        )
        .unwrap();
        let grants = Grants::with_file(d.join("grants.json"));
        let picked = PickedFolders::load(d.path().to_path_buf());
        picked.restore_grants(&grants);
        assert!(grants.check("/etc/passwd").is_err());
        assert!(picked.paths.lock().unwrap().is_empty());
    }

    #[test]
    fn dialog_pick_refuses_system_drive_root() {
        let d = crate::testutil::TmpDir::new("picked-root");
        let grants = Grants::with_file(d.join("grants.json"));
        let picked = PickedFolders::load(d.path().to_path_buf());
        let root = if cfg!(windows) {
            Path::new(r"C:\")
        } else {
            Path::new("/")
        };
        assert!(picked.record_dialog_pick(&grants, root).is_err());
    }

    #[test]
    fn auto_restore_rules() {
        assert!(!allow_auto_restore(Path::new("/")));
        assert!(!allow_auto_restore(Path::new(r"C:\")));
        let d = crate::testutil::TmpDir::new("picked-restore-rules");
        fs::create_dir_all(d.join("usb")).unwrap();
        assert!(allow_auto_restore(&d.join("usb")));
    }

    #[test]
    fn system_drive_root_detects_slash() {
        assert!(is_system_drive_root(Path::new("/")));
        assert!(!is_system_drive_root(Path::new(r"E:\")));
    }

    #[cfg(windows)]
    #[test]
    fn windows_usb_root_may_restore_when_present() {
        if Path::new(r"E:\").exists() {
            assert!(allow_auto_restore(Path::new(r"E:\")));
            assert!(!allow_auto_restore(Path::new(r"C:\")));
        }
    }

    #[test]
    fn imported_path_not_granted_without_picked_list() {
        let d = crate::testutil::TmpDir::new("picked-import");
        let grants = Grants::with_file(d.join("grants.json"));
        let picked = PickedFolders::load(d.path().to_path_buf());
        fs::create_dir_all(d.join("lib/games")).unwrap();
        let lib = d.join("lib/games").display().to_string();
        picked.restore_grants(&grants);
        assert!(grants.check(&lib).is_err());
    }

    #[test]
    fn dialog_picked_folder_is_restored() {
        let d = crate::testutil::TmpDir::new("picked-restore");
        let grants = Grants::with_file(d.join("grants.json"));
        let picked = PickedFolders::load(d.path().to_path_buf());
        fs::create_dir_all(d.join("lib/game")).unwrap();
        fs::write(d.join("lib/game/x"), b"x").unwrap();
        let lib = d.join("lib");
        picked.record_dialog_pick(&grants, &lib).unwrap();
        grants.clear_persist();
        picked.restore_grants(&grants);
        assert!(grants.check(d.join("lib/game/x").to_str().unwrap()).is_ok());
    }

    #[test]
    fn remember_approved_skips_system_drive_root() {
        let d = crate::testutil::TmpDir::new("picked-mig");
        let grants = Grants::with_file(d.join("grants.json"));
        let picked = PickedFolders::load(d.path().to_path_buf());
        fs::create_dir_all(d.join("good")).unwrap();
        let good = d.join("good").display().to_string();
        picked.remember_approved(&grants, &[r"C:\".into(), good.clone()]);
        assert!(grants.check(&good).is_ok());
        let saved: Vec<PathBuf> =
            serde_json::from_str(&fs::read_to_string(d.join("picked-folders.json")).unwrap())
                .unwrap();
        assert_eq!(saved.len(), 1);
    }

    #[test]
    fn corrupt_picked_folders_backed_up() {
        let d = crate::testutil::TmpDir::new("picked-corrupt");
        let file = d.join("picked-folders.json");
        fs::write(&file, b"{not json").unwrap();
        let picked = PickedFolders::load(d.path().to_path_buf());
        assert!(picked.paths.lock().unwrap().is_empty());
        assert!(d.join("picked-folders.json.bak").exists());
    }

    #[test]
    fn picked_folders_save_is_atomic() {
        let d = crate::testutil::TmpDir::new("picked-atomic");
        let picked = PickedFolders::load(d.path().to_path_buf());
        fs::create_dir_all(d.join("a")).unwrap();
        picked
            .record_dialog_pick(&Grants::with_file(d.join("grants.json")), &d.join("a"))
            .unwrap();
        let raw = fs::read_to_string(d.join("picked-folders.json")).unwrap();
        let v: Vec<PathBuf> = serde_json::from_str(&raw).unwrap();
        assert_eq!(v.len(), 1);
    }
}
