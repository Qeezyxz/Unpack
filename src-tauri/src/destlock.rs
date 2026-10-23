use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex, OnceLock};

pub const WAIT_MESSAGE: &str = "Waiting for another job that is using this folder…";

struct Busy {
    folders: Mutex<HashSet<PathBuf>>,
    freed: Condvar,
}

fn busy() -> &'static Busy {
    static BUSY: OnceLock<Busy> = OnceLock::new();
    BUSY.get_or_init(|| Busy {
        folders: Mutex::new(HashSet::new()),
        freed: Condvar::new(),
    })
}

/// Canonical form of `path` even when the folder does not exist yet: the
/// nearest existing parent is resolved and the missing names are put back on.
fn resolve(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().map(|d| d.join(path)).unwrap_or_else(|_| path.to_path_buf())
    };
    let mut missing: Vec<std::ffi::OsString> = Vec::new();
    let mut probe = absolute.as_path();
    loop {
        if let Ok(real) = std::fs::canonicalize(probe) {
            let mut out = real;
            for name in missing.iter().rev() {
                out.push(name);
            }
            return out;
        }
        match (probe.file_name(), probe.parent()) {
            (Some(name), Some(parent)) => {
                missing.push(name.to_os_string());
                probe = parent;
            }
            _ => return absolute,
        }
    }
}

fn key(dest: &Path) -> PathBuf {
    let base = resolve(dest);
    let text = base.to_string_lossy().into_owned();
    let text = text.strip_prefix(r"\\?\").unwrap_or(&text);
    let text = text.trim_end_matches(['/', '\\']).to_string();
    #[cfg(windows)]
    let text = text.to_lowercase();
    PathBuf::from(text)
}

/// Held while a job writes into a destination folder. Another job aimed at
/// the same folder waits for it, so an extract and a convert never write the
/// same tree at once. Jobs on different folders still run side by side.
pub struct DestGuard {
    key: PathBuf,
}

impl Drop for DestGuard {
    fn drop(&mut self) {
        let b = busy();
        if let Ok(mut folders) = b.folders.lock() {
            folders.remove(&self.key);
        }
        b.freed.notify_all();
    }
}

/// Block until `dest` is free. `on_wait` runs once if this job has to queue.
pub fn acquire(dest: &Path, on_wait: impl FnOnce()) -> DestGuard {
    let key = key(dest);
    let b = busy();
    let mut folders = b.folders.lock().unwrap_or_else(|e| e.into_inner());
    let mut on_wait = Some(on_wait);
    while folders.contains(&key) {
        if let Some(notify) = on_wait.take() {
            notify();
        }
        folders = b.freed.wait(folders).unwrap_or_else(|e| e.into_inner());
    }
    folders.insert(key.clone());
    DestGuard { key }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    fn fresh(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "unpack-destlock-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn same_folder_waits_for_the_first_job() {
        let dest = fresh("same");
        let first = acquire(&dest, || {});
        let waited = Arc::new(AtomicBool::new(false));
        let got_it = Arc::new(AtomicBool::new(false));
        let (w, g, d) = (waited.clone(), got_it.clone(), dest.clone());
        let t = std::thread::spawn(move || {
            let _second = acquire(&d, || w.store(true, Ordering::SeqCst));
            g.store(true, Ordering::SeqCst);
        });
        std::thread::sleep(Duration::from_millis(150));
        assert!(waited.load(Ordering::SeqCst), "second job should report it is queued");
        assert!(!got_it.load(Ordering::SeqCst), "second job must not start yet");
        drop(first);
        t.join().unwrap();
        assert!(got_it.load(Ordering::SeqCst));
        let _ = std::fs::remove_dir_all(&dest);
    }

    #[test]
    fn different_folders_do_not_block_each_other() {
        let a = fresh("a");
        let b = fresh("b");
        let _ga = acquire(&a, || panic!("a should be free"));
        let _gb = acquire(&b, || panic!("b should be free"));
        let _ = std::fs::remove_dir_all(&a);
        let _ = std::fs::remove_dir_all(&b);
    }

    #[test]
    fn a_folder_has_one_key_before_and_after_it_exists() {
        let parent = fresh("later");
        let child = parent.join("Out").join("Games");
        let before = key(&child);
        std::fs::create_dir_all(&child).unwrap();
        assert_eq!(before, key(&child));
        let relative = std::env::current_dir().unwrap();
        assert_eq!(key(Path::new(".")), key(&relative));
        let _ = std::fs::remove_dir_all(&parent);
    }

    #[test]
    fn trailing_slash_is_the_same_folder() {
        let dest = fresh("slash");
        let with_slash = PathBuf::from(format!("{}/", dest.display()));
        assert_eq!(key(&dest), key(&with_slash));
        let _ = std::fs::remove_dir_all(&dest);
    }
}
