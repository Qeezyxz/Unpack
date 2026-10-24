//! Pre-flight checks before a batch extract or FTP send: free space and FAT32 >4 GB files.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::god;
use crate::safecopy;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PreflightIssue {
    pub level: String, // "block" | "warn"
    pub text: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PreflightReport {
    pub ok: bool,
    pub need_bytes: u64,
    pub free_bytes: Option<u64>,
    pub issues: Vec<PreflightIssue>,
}

/// Estimate bytes needed for local paths (files + shallow folder walk, no links).
pub fn estimate_local_need(paths: &[PathBuf]) -> u64 {
    let mut total = 0u64;
    for p in paths {
        let Ok(m) = std::fs::symlink_metadata(p) else {
            continue;
        };
        if m.is_file() {
            total += m.len();
            continue;
        }
        if !m.is_dir() {
            continue;
        }
        let mut stack = vec![p.clone()];
        while let Some(d) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&d) else {
                continue;
            };
            for e in rd.flatten() {
                let Ok(ft) = e.file_type() else { continue };
                if ft.is_symlink() {
                    continue;
                }
                if ft.is_dir() {
                    stack.push(e.path());
                } else if ft.is_file() {
                    total += e.metadata().map(|m| m.len()).unwrap_or(0);
                }
            }
        }
    }
    total
}

/// Check whether writing `need` bytes to `dest` is plausible and whether any listed file exceeds FAT32.
pub fn check_local_dest(dest: &Path, need: u64, large_files: &[PathBuf]) -> PreflightReport {
    let mut issues = vec![];
    let free = god::free_space(dest);
    if let Some(f) = free {
        if need > f.saturating_sub(64 << 20) {
            issues.push(PreflightIssue {
                level: "block".into(),
                text: format!(
                    "Not enough free space on {}: about {} needed, {} free.",
                    safecopy::plain(&dest.display().to_string()),
                    crate::download::human_size(need),
                    crate::download::human_size(f)
                ),
            });
        }
    } else {
        issues.push(PreflightIssue {
            level: "warn".into(),
            text: "Couldn't read free space for that folder — make sure there's room before you start.".into(),
        });
    }
    if safecopy::fs_type(dest)
        .map(|t| safecopy::is_fat(&t))
        .unwrap_or(false)
    {
        for p in large_files {
            if let Ok(m) = std::fs::symlink_metadata(p) {
                if m.is_file() && m.len() > safecopy::FAT_MAX {
                    issues.push(PreflightIssue {
                        level: "block".into(),
                        text: format!(
                            "{} is {} — FAT32 can't hold a single file over 4 GB.",
                            safecopy::plain(
                                &p.file_name()
                                    .map(|n| n.to_string_lossy().into_owned())
                                    .unwrap_or_default()
                            ),
                            crate::download::human_size(m.len())
                        ),
                    });
                }
            }
        }
    }
    let ok = !issues.iter().any(|i| i.level == "block");
    PreflightReport {
        ok,
        need_bytes: need,
        free_bytes: free,
        issues,
    }
}

/// Cap listing cost: only inspect explicit paths, not whole drives.
pub fn large_files_in(paths: &[PathBuf]) -> Vec<PathBuf> {
    paths
        .iter()
        .filter(|p| {
            std::fs::symlink_metadata(p)
                .map(|m| m.is_file() && m.len() > safecopy::FAT_MAX)
                .unwrap_or(false)
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TmpDir;

    #[test]
    fn blocks_when_need_exceeds_free() {
        let d = TmpDir::new("pf-space");
        let p = d.path().join("big.bin");
        std::fs::write(&p, [0u8; 1024]).unwrap();
        let r = check_local_dest(d.path(), 1 << 40, &[]);
        assert!(!r.ok);
        assert!(r.issues.iter().any(|i| i.text.contains("free space")));
    }
}
