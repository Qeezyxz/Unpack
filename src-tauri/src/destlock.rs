use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};
use std::thread::ThreadId;
use std::time::Duration;

pub const WAIT_MESSAGE: &str = "Waiting for another job that is using this folder…";

struct Held {
    id: u64,
    key: PathBuf,
    thread: ThreadId,
}

struct Busy {
    folders: Mutex<Vec<Held>>,
    freed: Condvar,
}

fn busy() -> &'static Busy {
    static BUSY: OnceLock<Busy> = OnceLock::new();
    BUSY.get_or_init(|| Busy {
        folders: Mutex::new(Vec::new()),
        freed: Condvar::new(),
    })
}

/// Canonical form of `dest`. A folder that doesn't exist yet is resolved through its nearest
/// existing ancestor (canonicalized, so a symlinked parent gives the same key) plus the rest of
/// the path (R15 L3).
fn key(dest: &Path) -> PathBuf {
    let mut rest: Vec<std::ffi::OsString> = Vec::new();
    let mut cur = dest.to_path_buf();
    let base = loop {
        if let Ok(c) = std::fs::canonicalize(&cur) {
            break c;
        }
        match (
            cur.file_name().map(|n| n.to_os_string()),
            cur.parent().map(Path::to_path_buf),
        ) {
            (Some(n), Some(p)) if !p.as_os_str().is_empty() => {
                rest.push(n);
                cur = p;
            }
            _ => {
                rest.clear();
                break dest.to_path_buf();
            }
        }
    };
    let mut full = base;
    for n in rest.iter().rev() {
        full.push(n);
    }
    let text = full
        .to_string_lossy()
        .trim_end_matches(['/', '\\'])
        .to_string();
    #[cfg(windows)]
    let text = text.to_lowercase();
    PathBuf::from(text)
}

/// Same folder, or one inside the other.
fn overlaps(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

/// Held while a job writes into a destination folder. Another job aimed at the same folder, a
/// folder inside it or a folder containing it waits for it, so an extract and a convert never
/// write the same tree at once. Jobs on unrelated folders still run side by side. The same thread
/// may nest locks (a job never waits for itself).
pub struct DestGuard {
    id: u64,
}

impl Drop for DestGuard {
    fn drop(&mut self) {
        let b = busy();
        let mut folders = b.folders.lock().unwrap_or_else(|e| e.into_inner());
        folders.retain(|h| h.id != self.id);
        drop(folders);
        b.freed.notify_all();
    }
}

/// Block until `dest` is free, checking `cancelled` while queued (returns `ERR_CANCELLED`).
/// `on_wait` runs once if this job has to queue.
pub fn acquire(
    dest: &Path,
    on_wait: impl FnOnce(),
    cancelled: &dyn Fn() -> bool,
) -> Result<DestGuard, String> {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let key = key(dest);
    let me = std::thread::current().id();
    let b = busy();
    let mut folders = b.folders.lock().unwrap_or_else(|e| e.into_inner());
    let mut on_wait = Some(on_wait);
    while folders
        .iter()
        .any(|h| h.thread != me && overlaps(&h.key, &key))
    {
        if cancelled() {
            return Err(crate::archive::ERR_CANCELLED.into());
        }
        if let Some(notify) = on_wait.take() {
            notify();
        }
        folders = b
            .freed
            .wait_timeout(folders, Duration::from_millis(200))
            .unwrap_or_else(|e| e.into_inner())
            .0;
    }
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    folders.push(Held {
        id,
        key,
        thread: me,
    });
    Ok(DestGuard { id })
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
        let first = acquire(&dest, || {}, &|| false).unwrap();
        let waited = Arc::new(AtomicBool::new(false));
        let got_it = Arc::new(AtomicBool::new(false));
        let (w, g, d) = (waited.clone(), got_it.clone(), dest.clone());
        let t = std::thread::spawn(move || {
            let _second = acquire(&d, || w.store(true, Ordering::SeqCst), &|| false).unwrap();
            g.store(true, Ordering::SeqCst);
        });
        std::thread::sleep(Duration::from_millis(150));
        assert!(
            waited.load(Ordering::SeqCst),
            "second job should report it is queued"
        );
        assert!(
            !got_it.load(Ordering::SeqCst),
            "second job must not start yet"
        );
        drop(first);
        t.join().unwrap();
        assert!(got_it.load(Ordering::SeqCst));
        let _ = std::fs::remove_dir_all(&dest);
    }

    #[test]
    fn different_folders_do_not_block_each_other() {
        let a = fresh("a");
        let b = fresh("b");
        let _ga = acquire(&a, || panic!("a should be free"), &|| false).unwrap();
        let _gb = acquire(&b, || panic!("b should be free"), &|| false).unwrap();
        let _ = std::fs::remove_dir_all(&a);
        let _ = std::fs::remove_dir_all(&b);
    }

    #[test]
    fn trailing_slash_is_the_same_folder() {
        let dest = fresh("slash");
        let with_slash = PathBuf::from(format!("{}/", dest.display()));
        assert_eq!(key(&dest), key(&with_slash));
        let _ = std::fs::remove_dir_all(&dest);
    }

    #[test]
    fn r15_l3_waiting_job_can_be_cancelled() {
        let dest = fresh("cancel");
        let _first = acquire(&dest, || {}, &|| false).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let (s, d) = (stop.clone(), dest.clone());
        let t = std::thread::spawn(move || {
            acquire(&d, || {}, &|| s.load(Ordering::SeqCst)).map(|_| ())
        });
        std::thread::sleep(Duration::from_millis(100));
        stop.store(true, Ordering::SeqCst);
        assert_eq!(
            t.join().unwrap().unwrap_err(),
            crate::archive::ERR_CANCELLED
        );
        let _ = std::fs::remove_dir_all(&dest);
    }

    #[test]
    fn r15_l3_parent_and_child_block_each_other() {
        let dest = fresh("tree");
        let _parent = acquire(&dest, || {}, &|| false).unwrap();
        let waited = Arc::new(AtomicBool::new(false));
        let (w, d) = (waited.clone(), dest.join("Game"));
        let t = std::thread::spawn(move || {
            let stop = std::time::Instant::now();
            acquire(&d, || w.store(true, Ordering::SeqCst), &move || {
                stop.elapsed() > Duration::from_millis(300)
            })
            .is_err()
        });
        assert!(t.join().unwrap(), "child folder must wait for the parent");
        assert!(waited.load(Ordering::SeqCst));
        // and the other way round
        let other = fresh("tree2");
        let _child = acquire(&other.join("x/y"), || {}, &|| false).unwrap();
        let o = other.clone();
        let t = std::thread::spawn(move || {
            let stop = std::time::Instant::now();
            acquire(&o, || {}, &move || {
                stop.elapsed() > Duration::from_millis(300)
            })
            .is_err()
        });
        assert!(t.join().unwrap(), "parent must wait for a child");
        // the same thread may nest
        let _nested = acquire(
            &dest.join("Game"),
            || panic!("own thread must not wait"),
            &|| false,
        )
        .unwrap();
        let _ = std::fs::remove_dir_all(&dest);
        let _ = std::fs::remove_dir_all(&other);
    }

    #[cfg(unix)]
    #[test]
    fn r15_l3_missing_folder_resolves_through_a_symlinked_parent() {
        let real = fresh("real");
        let link = real.with_file_name(format!(
            "{}-link",
            real.file_name().unwrap().to_string_lossy()
        ));
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert_eq!(key(&link.join("new/deeper")), key(&real.join("new/deeper")));
        assert_eq!(
            key(&link.join("new/deeper")),
            std::fs::canonicalize(&real).unwrap().join("new/deeper")
        );
        let _ = std::fs::remove_file(&link);
        let _ = std::fs::remove_dir_all(&real);
    }
}
