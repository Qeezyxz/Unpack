//! Trusted native drag-and-drop: validate, grant, and tell the page which paths to open.

use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::sync::Mutex;

use tauri::{AppHandle, Emitter};

use crate::picked_folders::validate_session_grant;
use crate::safecopy::{simplify, Grants};

pub struct NativeDropOutcome {
    pub accepted: Vec<String>,
    pub refused: Option<String>,
}

/// Dedupe key for one physical drop path (trailing separators; case on Windows).
pub fn drop_dedupe_key(p: &Path) -> String {
    let c = fs::canonicalize(p)
        .map(simplify)
        .unwrap_or_else(|_| simplify(p.to_path_buf()));
    let s = c.display().to_string();
    #[cfg(windows)]
    {
        s.to_lowercase()
    }
    #[cfg(not(windows))]
    {
        s
    }
}

/// Holds accepted drops until the page calls `drop_listener_ready`.
#[derive(Default)]
pub struct DropDelivery {
    listener_ready: Mutex<bool>,
    queued: Mutex<Vec<Vec<String>>>,
}

impl DropDelivery {
    pub fn set_listener_ready(&self, app: &AppHandle) {
        *self.listener_ready.lock().unwrap() = true;
        let batches: Vec<Vec<String>> = self.queued.lock().unwrap().drain(..).collect();
        for batch in batches {
            if !batch.is_empty() {
                let _ = app.emit("native-drop-accepted", batch);
            }
        }
    }

    pub fn emit_accepted(&self, app: &AppHandle, accepted: Vec<String>) {
        if accepted.is_empty() {
            return;
        }
        if *self.listener_ready.lock().unwrap() {
            let _ = app.emit("native-drop-accepted", accepted);
        } else {
            self.queued.lock().unwrap().push(accepted);
        }
    }
}

/// Called only from the native drag-drop handler (or the test bridge simulator).
pub fn process_native_drop(paths: &[&Path], grants: &Grants) -> NativeDropOutcome {
    let mut accepted = Vec::new();
    let mut refused = None;
    let mut seen = HashSet::new();
    for p in paths {
        let p = *p;
        if !seen.insert(drop_dedupe_key(p)) {
            continue;
        }
        let meta = fs::symlink_metadata(p);
        if let Ok(m) = meta {
            if crate::safecopy::is_link(&m) {
                continue;
            }
            let grant_target = if m.is_dir() {
                Some(p)
            } else if m.is_file() {
                p.parent()
            } else {
                None
            };
            if let Some(target) = grant_target {
                match validate_session_grant(target) {
                    Ok(c) => {
                        grants.grant_session(&c);
                        accepted.push(p.to_string_lossy().into_owned());
                    }
                    Err(e) if refused.is_none() => refused = Some(e),
                    Err(_) => {}
                }
            }
        }
    }
    NativeDropOutcome { accepted, refused }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TmpDir;

    #[test]
    fn without_native_drop_handler_nothing_is_granted() {
        let tmp = TmpDir::new("no-native-drop");
        let secret = tmp.path().join("secret");
        fs::create_dir_all(&secret).unwrap();
        let secret_file = secret.join("x.txt");
        fs::write(&secret_file, b"x").unwrap();

        let grants = Grants::default();
        assert!(grants.check(secret_file.to_str().unwrap()).is_err());
    }

    #[test]
    fn native_drop_accepts_only_granted_paths() {
        let tmp = TmpDir::new("native-drop-ok");
        let good = tmp.path().join("mods");
        fs::create_dir_all(&good).unwrap();
        let other = tmp.path().join("other");
        fs::create_dir_all(&other).unwrap();

        let grants = Grants::default();
        let dropped = [good.as_path()];
        let outcome = process_native_drop(&dropped, &grants);
        assert_eq!(outcome.accepted.len(), 1);
        assert_eq!(outcome.accepted[0], good.to_string_lossy());
        assert!(grants.check(good.to_str().unwrap()).is_ok());
        assert!(grants.check(other.to_str().unwrap()).is_err());
    }

    #[test]
    fn duplicate_paths_in_one_drop_accepted_once() {
        let tmp = TmpDir::new("native-drop-dedupe");
        let good = tmp.path().join("mods");
        fs::create_dir_all(&good).unwrap();
        let grants = Grants::default();
        let dropped = [good.as_path(), good.as_path()];
        let outcome = process_native_drop(&dropped, &grants);
        assert_eq!(outcome.accepted.len(), 1);
    }

    #[test]
    fn refused_paths_omitted_from_accepted() {
        if !cfg!(unix) {
            return;
        }
        let grants = Grants::default();
        let outcome = process_native_drop(&[Path::new("/etc")], &grants);
        assert!(outcome.accepted.is_empty());
        assert!(outcome.refused.is_some());
        assert!(grants.check("/etc").is_err());
    }
}
