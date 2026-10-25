//! FAT32 destinations: the 4 GiB limit is per file. Only a single output file of 4 GiB or more
//! is refused; archive size and total size never are. An oversized ISO with Auto ISO → GOD / XEX
//! on is extracted to a staging folder on a non-FAT local drive, converted onto the FAT drive,
//! and the staged copy is deleted afterwards (see `god::StageGuard`).
use crate::download::human_size;
use crate::safecopy::{self, FAT_MAX};
use std::path::{Path, PathBuf};

#[derive(Debug, PartialEq, Eq)]
pub enum Plan {
    /// Write straight to the destination.
    Direct,
    /// Extract into a non-FAT staging folder first; these ISOs (name, size) are too big.
    Stage(Vec<(String, u64)>),
}

pub fn is_iso(name: &str) -> bool {
    name.to_lowercase().ends_with(".iso")
}

fn base(n: &str) -> String {
    safecopy::plain(n.rsplit(['/', '\\']).next().unwrap_or(n))
}

/// `dest_fs`: file system of the destination; `files`: each output file (name, uncompressed size);
/// `auto_iso`: "off" | "god" | "xex".
pub fn plan(
    dest_fs: Option<&str>,
    files: &[(String, u64)],
    auto_iso: &str,
) -> Result<Plan, String> {
    let Some(fs) = dest_fs.filter(|t| safecopy::is_fat(t)) else {
        return Ok(Plan::Direct);
    };
    let auto = auto_iso == "god" || auto_iso == "xex";
    let mut staged = vec![];
    for (n, s) in files {
        if *s <= FAT_MAX {
            continue;
        }
        if is_iso(n) && auto {
            staged.push((n.clone(), *s));
            continue;
        }
        let hint = if is_iso(n) {
            "Turn on Auto ISO → GOD (Settings or the bar above the list): Unpack then stages the ISO on your PC's drive and writes the GOD game, which is split into parts under 4 GB, to this drive."
        } else {
            "Extract to an NTFS or exFAT folder instead."
        };
        return Err(format!(
            "{} inside the archive is {}, and the destination is {fs}, which can't hold a single file of 4 GB or more. {hint}",
            base(n),
            human_size(*s)
        ));
    }
    Ok(if staged.is_empty() {
        Plan::Direct
    } else {
        Plan::Stage(staged)
    })
}

pub const STAGE_DIR: &str = "Unpack-stage";
const MARGIN: u64 = 256 << 20;

/// Places to stage, in order: UNPACK_STAGE_DIR (tests / power users), then the system temp folder.
pub fn stage_candidates() -> Vec<PathBuf> {
    let mut v = vec![];
    if let Ok(p) = std::env::var("UNPACK_STAGE_DIR") {
        if !p.is_empty() {
            v.push(PathBuf::from(p));
        }
    }
    v.push(std::env::temp_dir());
    v
}

/// Marker written into every staging job folder at creation; only folders carrying it are swept.
pub const MARKER: &str = ".unpack-stage";
const MARKER_TEXT: &[u8] = b"unpack-stage v1\n";
const STALE_SECS: u64 = 12 * 3600;

/// Staging job folders this process created and hasn't finished with (extracting, or waiting
/// for its ISOs to be registered). Never swept.
fn active() -> &'static std::sync::Mutex<std::collections::HashSet<PathBuf>> {
    static A: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<PathBuf>>> =
        std::sync::OnceLock::new();
    A.get_or_init(Default::default)
}
pub fn release_job(dir: &Path) {
    active()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .remove(dir);
}
/// In use: still being written by this process, or holds an ISO waiting to be converted.
pub fn in_use(dir: &Path) -> bool {
    active()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .contains(dir)
        || crate::god::stage_in_use(dir)
}

#[cfg(windows)]
fn is_reparse(m: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    m.file_attributes() & 0x400 != 0
}
#[cfg(not(windows))]
fn is_reparse(_m: &std::fs::Metadata) -> bool {
    false
}
/// A real directory: not a symlink, junction or other reparse point.
fn real_dir(p: &Path) -> bool {
    std::fs::symlink_metadata(p)
        .map(|m| m.file_type().is_dir() && !m.file_type().is_symlink() && !is_reparse(&m))
        .unwrap_or(false)
}

/// The `Unpack-stage` folder under `c`, created if missing. Refused if it is a symlink,
/// junction or not a directory, so staging (and the sweep) never follows a link elsewhere.
fn stage_root_in(c: &Path) -> Option<PathBuf> {
    let root = c.join(STAGE_DIR);
    if std::fs::symlink_metadata(&root).is_err() {
        std::fs::create_dir_all(c).ok()?;
        let _ = std::fs::create_dir(&root);
    }
    real_dir(&root).then_some(root)
}

/// New marked job folder `job-<tag>` (numbered if taken) in `root`, registered as active.
pub fn new_job_dir(root: &Path, tag: &str) -> std::io::Result<PathBuf> {
    let dir = crate::archive::claim_dir(root, &format!("job-{tag}"))?;
    active()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(dir.clone());
    let mark = || -> std::io::Result<()> {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(dir.join(MARKER))?;
        f.write_all(MARKER_TEXT)
    };
    if let Err(e) = mark() {
        release_job(&dir);
        let _ = std::fs::remove_dir_all(&dir);
        return Err(e);
    }
    Ok(dir)
}

/// First candidate on a non-FAT drive with room for `need` bytes. Returns `<candidate>/Unpack-stage`.
pub fn pick_stage_root(need: u64, candidates: &[PathBuf], what: &str) -> Result<PathBuf, String> {
    let mut tried = vec![];
    for c in candidates {
        let Some(root) = stage_root_in(c) else {
            tried.push(format!(
                "{} can't be used (it is a link or not a folder)",
                c.join(STAGE_DIR).display()
            ));
            continue;
        };
        let fs = safecopy::fs_type(&root);
        if fs.as_deref().map(safecopy::is_fat).unwrap_or(false) {
            tried.push(format!("{} is {}", c.display(), fs.unwrap_or_default()));
            continue;
        }
        sweep(&root);
        match crate::god::free_space(&root) {
            Some(f) if f >= need.saturating_add(MARGIN) => return Ok(root),
            Some(f) => tried.push(format!("{} has {} free", c.display(), human_size(f))),
            None => tried.push(format!("couldn't read free space on {}", c.display())),
        }
    }
    Err(format!(
        "{what} has to be staged on a non-FAT32 drive before it's converted, and that needs about {} free. No drive qualified ({}). Free up space on your system drive and try again.",
        human_size(need.saturating_add(MARGIN)),
        tried.join("; ")
    ))
}

/// Remove leftover staging job folders (crash, or ISOs never converted): only real `job-*`
/// folders holding Unpack's marker file, untouched for 12 hours and not in use. Links are never
/// followed (`remove_dir_all` unlinks links inside without following them).
pub fn sweep(root: &Path) {
    if !real_dir(root) {
        return;
    }
    let Ok(rd) = std::fs::read_dir(root) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if !e.file_name().to_string_lossy().starts_with("job-") || !real_dir(&p) {
            continue;
        }
        let marker = std::fs::symlink_metadata(p.join(MARKER))
            .ok()
            .filter(|m| m.file_type().is_file());
        let Some(m) = marker else { continue };
        let ok_text = std::fs::read(p.join(MARKER)).is_ok_and(|t| t == MARKER_TEXT);
        let stale = m
            .modified()
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|d| d.as_secs() > STALE_SECS);
        if ok_text && stale && !in_use(&p) {
            let _ = std::fs::remove_dir_all(&p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const GB: u64 = 1_000_000_000;

    #[test]
    fn many_files_under_4gb_pass_even_when_total_is_6gb() {
        let files = vec![("a.bin".to_string(), 3 * GB), ("b.bin".to_string(), 3 * GB)];
        assert_eq!(plan(Some("FAT32"), &files, "off"), Ok(Plan::Direct));
    }

    #[test]
    fn single_big_file_is_refused_by_name() {
        let files = vec![
            ("dir/movie.mkv".to_string(), 4_500_000_000),
            ("x.txt".into(), 10),
        ];
        let e = plan(Some("FAT32"), &files, "god").unwrap_err();
        assert!(e.starts_with("movie.mkv inside the archive is"), "{e}");
        assert!(e.contains("4 GB") && e.contains("FAT32"), "{e}");
    }

    #[test]
    fn exactly_fat_max_fits_one_more_does_not() {
        assert_eq!(
            plan(Some("FAT32"), &[("a".into(), FAT_MAX)], "off"),
            Ok(Plan::Direct)
        );
        assert!(plan(Some("FAT32"), &[("a".into(), FAT_MAX + 1)], "off").is_err());
    }

    #[test]
    fn big_iso_with_auto_god_or_xex_is_staged_not_blocked() {
        let files = vec![
            ("Bully (USA).iso".to_string(), 7_000_000_000),
            ("readme.txt".into(), 100),
        ];
        for m in ["god", "xex"] {
            assert_eq!(
                plan(Some("FAT32"), &files, m),
                Ok(Plan::Stage(vec![("Bully (USA).iso".into(), 7_000_000_000)]))
            );
        }
    }

    #[test]
    fn big_iso_with_auto_off_names_the_iso_and_suggests_god() {
        let e = plan(
            Some("FAT32"),
            &[("Bully (USA).iso".into(), 7_000_000_000)],
            "off",
        )
        .unwrap_err();
        assert!(
            e.starts_with("Bully (USA).iso inside the archive is"),
            "{e}"
        );
        assert!(e.contains("Auto ISO → GOD"), "{e}");
        assert!(!e.contains(".7z"), "{e}");
    }

    #[test]
    fn non_fat_never_blocks() {
        for fs in [Some("NTFS"), Some("exFAT"), None] {
            assert_eq!(
                plan(fs, &[("a.iso".into(), 9 * GB)], "off"),
                Ok(Plan::Direct)
            );
        }
    }

    fn age(p: &Path, secs: u64) {
        let t = std::time::SystemTime::now() - std::time::Duration::from_secs(secs);
        std::fs::File::options()
            .write(true)
            .open(p)
            .unwrap()
            .set_modified(t)
            .unwrap();
    }

    #[test]
    fn sweep_removes_only_stale_marked_unused_job_dirs() {
        let d = crate::testutil::TmpDir::new("cs31-sweep");
        let root = stage_root_in(d.path()).unwrap();
        // stale marked job, not in use -> removed
        let old = new_job_dir(&root, "old").unwrap();
        release_job(&old);
        age(&old.join(MARKER), 2 * 86400);
        // stale but still active in this process (L2) -> kept
        let live = new_job_dir(&root, "live").unwrap();
        age(&live.join(MARKER), 2 * 86400);
        // job-* without marker, and a non-job folder -> kept
        std::fs::create_dir_all(root.join("job-nomark")).unwrap();
        std::fs::create_dir_all(root.join("not-unpack-dir")).unwrap();
        std::fs::write(root.join("not-unpack-dir/user.txt"), b"x").unwrap();
        // forged marker with other text -> kept
        std::fs::create_dir_all(root.join("job-forged")).unwrap();
        std::fs::write(root.join("job-forged").join(MARKER), b"nope").unwrap();
        age(&root.join("job-forged").join(MARKER), 2 * 86400);
        sweep(&root);
        assert!(!old.exists());
        assert!(live.exists() && root.join("job-nomark").exists());
        assert!(root.join("not-unpack-dir/user.txt").exists() && root.join("job-forged").exists());
        release_job(&live);
        sweep(&root);
        assert!(!live.exists(), "released + stale is swept");
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_stage_root_is_refused_and_never_swept() {
        let d = crate::testutil::TmpDir::new("cs31-link");
        let victim = d.path().join("victim");
        let job = victim.join("job-x");
        std::fs::create_dir_all(&job).unwrap();
        std::fs::write(job.join(MARKER), MARKER_TEXT).unwrap();
        age(&job.join(MARKER), 2 * 86400);
        let cand = d.path().join("cand");
        std::fs::create_dir_all(&cand).unwrap();
        std::os::unix::fs::symlink(&victim, cand.join(STAGE_DIR)).unwrap();
        let e = pick_stage_root(1, &[cand], "x").unwrap_err();
        assert!(e.contains("link"), "{e}");
        sweep(&d.path().join("cand").join(STAGE_DIR));
        assert!(job.exists());
    }

    #[cfg(unix)]
    #[test]
    fn sweep_unlinks_job_links_without_following() {
        let d = crate::testutil::TmpDir::new("cs31-joblink");
        let root = stage_root_in(d.path()).unwrap();
        let victim = d.path().join("victim");
        std::fs::create_dir_all(&victim).unwrap();
        std::fs::write(victim.join(MARKER), MARKER_TEXT).unwrap();
        std::os::unix::fs::symlink(&victim, root.join("job-link")).unwrap();
        sweep(&root);
        assert!(victim.join(MARKER).exists());
    }

    #[test]
    fn stage_root_skips_dirs_without_room() {
        let d = crate::testutil::TmpDir::new("cs31-stage-pick");
        let e = pick_stage_root(u64::MAX / 2, &[d.path().to_path_buf()], "Bully.iso").unwrap_err();
        assert!(e.contains("Bully.iso") && e.contains("non-FAT32"), "{e}");
        let ok = pick_stage_root(1024, &[d.path().to_path_buf()], "x").unwrap();
        assert_eq!(ok, d.path().join(STAGE_DIR));
    }
}
