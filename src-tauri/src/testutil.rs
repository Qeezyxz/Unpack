//! Test-only helpers. `TmpDir` removes its folder when dropped, so `cargo test` leaves nothing
//! behind in the temp folder (Overseer R10 LOW-3).
use std::path::{Path, PathBuf};

pub struct TmpDir(PathBuf);

impl TmpDir {
    /// `<temp>/unpack-<name>-<pid>`, emptied first.
    pub fn new(name: &str) -> TmpDir {
        let p = std::env::temp_dir().join(format!("unpack-{name}-{}", std::process::id()));
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
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target").join("test-fixtures")
}
