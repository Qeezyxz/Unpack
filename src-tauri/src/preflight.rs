//! Pre-flight checks before a batch extract or FTP send: free space, plus per-file FAT32 refusals.

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
/// `fat_blocks`: per-file FAT32 refusals from `fatstage::plan` (already worded for the user).
pub fn check_local_dest(dest: &Path, need: u64, fat_blocks: &[String]) -> PreflightReport {
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
    for t in fat_blocks {
        issues.push(PreflightIssue {
            level: "block".into(),
            text: t.clone(),
        });
    }
    let ok = !issues.iter().any(|i| i.level == "block");
    PreflightReport {
        ok,
        need_bytes: need,
        free_bytes: free,
        issues,
    }
}

/// FAT32 is a per-file limit: look at what each archive will write (via 7-Zip's listing), never
/// at the archive's own size or the total (a 6.5 GB .7z of small files fits fine). Returns the
/// user-facing refusals from `fatstage::plan`. `auto_iso` is "off" | "god" | "xex".
pub fn fat_blocks(
    dest: &Path,
    paths: &[PathBuf],
    exe: Option<&Path>,
    auto_iso: &str,
) -> Vec<String> {
    let fs = safecopy::fs_type(dest);
    if !fs.as_deref().is_some_and(safecopy::is_fat) {
        return vec![];
    }
    let mut out = vec![];
    for p in paths {
        let name = p.display().to_string();
        let files: Vec<(String, u64)> = if crate::archive::is_archive_name(&name) {
            match exe.map(|e| crate::archive::list(e, p, None)) {
                Some(Ok(l)) => l
                    .entries
                    .iter()
                    .filter(|e| !e.dir)
                    .map(|e| (e.path.clone(), e.size))
                    .collect(),
                _ => continue, // encrypted / unreadable: checked again when extracting
            }
        } else {
            match std::fs::symlink_metadata(p) {
                Ok(m) if m.is_file() => vec![(name.clone(), m.len())],
                _ => continue,
            }
        };
        if let Err(e) = crate::fatstage::plan(fs.as_deref(), &files, auto_iso) {
            out.push(e);
        }
    }
    out
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
