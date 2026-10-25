//! Trusted native drag-and-drop: grants happen only in the webview handler. The page may call
//! `session_drop_prepare` once to learn which dropped paths are usable — not to widen grants.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::picked_folders::validate_session_grant;
use crate::safecopy::{simplify, Grants};

pub const PENDING_DROP_TTL: Duration = Duration::from_secs(8);

#[derive(Default)]
pub struct PendingNativeDrop {
    batch: Mutex<Option<DropBatch>>,
}

struct DropBatch {
    at: Instant,
    dropped: HashSet<String>,
    granted: HashSet<String>,
}

fn drop_path_key(p: &Path) -> String {
    if let Ok(c) = fs::canonicalize(p) {
        return simplify(c).to_string_lossy().into_owned();
    }
    p.to_string_lossy().into_owned()
}

/// Called from the trusted native drag-drop handler only.
pub fn register_native_drop(
    pending: &PendingNativeDrop,
    paths: &[&Path],
    grants: &Grants,
) -> Option<String> {
    let mut dropped = HashSet::new();
    let mut granted = HashSet::new();
    let mut refused = None;
    for p in paths {
        let p = *p;
        let key = drop_path_key(p);
        dropped.insert(key.clone());
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
                        granted.insert(key);
                    }
                    Err(e) if refused.is_none() => refused = Some(e),
                    Err(_) => {}
                }
            }
        }
    }
    *pending.batch.lock().unwrap() = Some(DropBatch {
        at: Instant::now(),
        dropped,
        granted,
    });
    refused
}

/// Page may call once per native drop; never grants — only returns paths already granted above.
pub fn consume_session_drop_prepare(
    pending: &PendingNativeDrop,
    requested: &[PathBuf],
) -> (Vec<PathBuf>, Option<String>) {
    let batch = pending.batch.lock().unwrap().take();
    let Some(batch) = batch else {
        return (
            Vec::new(),
            Some("No file or folder was dropped.".into()),
        );
    };
    if batch.at.elapsed() > PENDING_DROP_TTL {
        return (
            Vec::new(),
            Some("The drop session expired. Drop the files again.".into()),
        );
    }

    let mut allowed = Vec::new();
    let mut foreign = false;
    for p in requested {
        let key = drop_path_key(p);
        if !batch.dropped.contains(&key) {
            foreign = true;
            continue;
        }
        if batch.granted.contains(&key) {
            allowed.push(p.clone());
        }
    }
    let notify = if foreign {
        Some(
            "That path wasn't part of the drop. Drop it again if you meant to use it.".into(),
        )
    } else {
        None
    };
    (allowed, notify)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TmpDir;

    #[test]
    fn session_drop_prepare_without_native_drop_grants_nothing() {
        let tmp = TmpDir::new("no-native-drop");
        let secret = tmp.path().join("secret");
        fs::create_dir_all(&secret).unwrap();
        let secret_file = secret.join("x.txt");
        fs::write(&secret_file, b"x").unwrap();

        let grants = Grants::default();
        let pending = PendingNativeDrop::default();
        let (allowed, refused) = consume_session_drop_prepare(&pending, &[secret]);
        assert!(allowed.is_empty());
        assert!(refused.is_some());
        assert!(grants.check(secret_file.to_str().unwrap()).is_err());
    }

    #[test]
    fn native_drop_then_prepare_allows_only_dropped_paths() {
        let tmp = TmpDir::new("native-drop-ok");
        let good = tmp.path().join("mods");
        fs::create_dir_all(&good).unwrap();
        let other = tmp.path().join("other");
        fs::create_dir_all(&other).unwrap();

        let grants = Grants::default();
        let pending = PendingNativeDrop::default();
        let dropped = [good.as_path()];
        register_native_drop(&pending, &dropped, &grants);
        let (allowed, _) = consume_session_drop_prepare(&pending, &[good.clone(), other.clone()]);
        assert_eq!(allowed.len(), 1);
        assert_eq!(allowed[0], good);
        assert!(grants.check(good.to_str().unwrap()).is_ok());
        assert!(grants.check(other.to_str().unwrap()).is_err());
    }
}
