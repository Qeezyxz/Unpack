//! Test-only helpers. `TmpDir` removes its folder when dropped, so `cargo test` leaves nothing
//! behind in the temp folder (Overseer R10 LOW-3).
use std::path::{Path, PathBuf};

pub struct TmpDir(PathBuf);

fn test_temp_base() -> PathBuf {
    #[cfg(windows)]
    {
        let base = std::env::temp_dir();
        let disp = base.to_string_lossy().to_lowercase();
        // Wine on Linux CI: POSIX temp. Real Windows / Wine %TEMP% under Users is treated as a
        // system path by picked_folders and cannot be auto-restored in tests.
        if disp.starts_with('/')
            || disp.starts_with(r"c:\users\")
            || disp.starts_with(r"c:\windows\")
        {
            return PathBuf::from(format!(r"C:\unpack-test-{}", std::process::id()));
        }
        return base;
    }
    std::env::temp_dir()
}

impl TmpDir {
    /// `<temp>/unpack-<name>-<pid>`, emptied first.
    pub fn new(name: &str) -> TmpDir {
        let p = test_temp_base().join(format!("unpack-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        TmpDir(p)
    }
    /// Same folder but canonical (safecopy tests compare canonical paths).
    pub fn canonical(name: &str) -> TmpDir {
        let t = TmpDir::new(name);
        let c = std::fs::canonicalize(&t.0).unwrap();
        let t2 = TmpDir(c);
        std::mem::forget(t);
        t2
    }
    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl std::ops::Deref for TmpDir {
    type Target = PathBuf;
    fn deref(&self) -> &PathBuf {
        &self.0
    }
}

impl AsRef<Path> for TmpDir {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TmpDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Cached test fixtures live under the crate's target folder, not the system temp folder.
pub fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("test-fixtures")
}
