//! Shared safe-write helpers for copies onto USB drives / user folders (Overseer R5):
//! - every folder we create or walk through under the target root must be a real directory
//!   (no symlinks, no junctions/reparse points), and the final path must still be under the root;
//! - files are written as `<name>.partial`, hash-verified, then renamed (never over an existing file);
//! - FAT32/exFAT-less targets: files of 4 GiB or more are refused up front;
//! - a path registry ("grants") so commands only act on paths the user picked in a dialog or
//!   that came from a scan/extraction, not on arbitrary strings from the page.

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;

use sha1::{Digest, Sha1};

pub const PARTIAL: &str = ".partial";
pub const FAT_MAX: u64 = 0xFFFF_FFFF; // FAT32 max file size is 4 GiB - 1

/// Windows `canonicalize` returns `\\?\C:\...`; show and use the plain `C:\...` form.
pub fn simplify(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        if rest.as_bytes().get(1) == Some(&b':') {
            return PathBuf::from(rest.to_string());
        }
    }
    p
}

/// True for symlinks and, on Windows, any reparse point (junctions, mount points).
pub fn is_link(m: &fs::Metadata) -> bool {
    if m.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if m.file_attributes() & 0x400 != 0 {
            return true;
        }
    }
    false
}

/// Create (or walk) `root/rel` one component at a time. Refuses links/junctions and anything
/// that isn't a directory, and checks the canonical result stays under the canonical root.
pub fn ensure_dir_under(root: &Path, rel: &Path) -> Result<PathBuf, String> {
    let rm =
        fs::symlink_metadata(root).map_err(|e| format!("Cannot open {}: {e}", root.display()))?;
    if !rm.is_dir() && !rm.file_type().is_symlink() {
        return Err(format!("{} is not a folder.", root.display()));
    }
    let croot =
        fs::canonicalize(root).map_err(|e| format!("Cannot open {}: {e}", root.display()))?;
    let mut cur = croot.clone();
    for c in rel.components() {
        let Component::Normal(name) = c else {
            return Err(format!("Unsafe path component in {}", rel.display()));
        };
        cur.push(name);
        match fs::symlink_metadata(&cur) {
            Ok(m) if is_link(&m) => {
                return Err(format!(
                    "{} is a link or junction; not writing through it.",
                    cur.display()
                ))
            }
            Ok(m) if m.is_dir() => {}
            Ok(_) => return Err(format!("{} exists and is not a folder.", cur.display())),
            Err(_) => match fs::create_dir(&cur) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let m = fs::symlink_metadata(&cur).map_err(|e| e.to_string())?;
                    if is_link(&m) || !m.is_dir() {
                        return Err(format!("{} changed while copying; stopped.", cur.display()));
                    }
                }
                Err(e) => return Err(format!("Cannot create {}: {e}", cur.display())),
            },
        }
    }
    let canon = fs::canonicalize(&cur).map_err(|e| e.to_string())?;
    if !canon.starts_with(&croot) {
        return Err(format!(
            "{} resolves outside {}; stopped.",
            cur.display(),
            croot.display()
        ));
    }
    Ok(simplify(canon))
}

pub fn sha1_reader(mut r: impl Read) -> std::io::Result<([u8; 20], u64)> {
    let mut h = Sha1::new();
    let mut buf = vec![0u8; 1 << 16];
    let mut n = 0u64;
    loop {
        let k = r.read(&mut buf)?;
        if k == 0 {
            break;
        }
        h.update(&buf[..k]);
        n += k as u64;
    }
    Ok((h.finalize().into(), n))
}

/// Copy `src` to `root/rel` safely. Returns Ok(false) if an identical file is already there.
/// Errors (without touching it) if a different file is there.
pub fn copy_file_safe(
    src: &Path,
    root: &Path,
    rel: &Path,
    progress: &mut dyn FnMut(u64),
) -> Result<bool, String> {
    let name = rel.file_name().ok_or("bad file name")?.to_owned();
    let dir = ensure_dir_under(root, rel.parent().unwrap_or(Path::new("")))?;
    let dest = dir.join(&name);
    let (src_hash, src_len) = File::open(src)
        .and_then(sha1_reader)
        .map_err(|e| format!("Cannot read {}: {e}", src.display()))?;
    match fs::symlink_metadata(&dest) {
        Ok(m) if is_link(&m) => {
            return Err(format!("{} is a link; not touching it.", dest.display()))
        }
        Ok(m)
            if m.is_file()
                && m.len() == src_len
                && File::open(&dest)
                    .and_then(sha1_reader)
                    .map(|h| h.0 == src_hash)
                    .unwrap_or(false) =>
        {
            return Ok(false)
        }
        Ok(_) => {
            return Err(format!(
                "{} already exists with different content. Nothing was overwritten.",
                dest.display()
            ))
        }
        Err(_) => {}
    }
    let mut pname = name.clone();
    pname.push(PARTIAL);
    let partial = dir.join(pname);
    // A stale .partial (earlier unplug/crash) is ours to replace, but only if it's a plain file.
    if let Ok(m) = fs::symlink_metadata(&partial) {
        if is_link(&m) || !m.is_file() {
            return Err(format!(
                "{} is not a regular file; not touching it.",
                partial.display()
            ));
        }
        fs::remove_file(&partial)
            .map_err(|e| format!("Cannot clear stale {}: {e}", partial.display()))?;
    }
    let res = (|| -> Result<(), String> {
        let mut out = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&partial)
            .map_err(|e| format!("Cannot create {}: {e}", partial.display()))?;
        let mut f = File::open(src).map_err(|e| e.to_string())?;
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let k = f.read(&mut buf).map_err(|e| format!("Read failed: {e}"))?;
            if k == 0 {
                break;
            }
            out.write_all(&buf[..k])
                .map_err(|e| format!("Write failed: {e}"))?;
            progress(k as u64);
        }
        out.sync_all().map_err(|e| format!("Write failed: {e}"))?;
        drop(out);
        let (h, n) = File::open(&partial)
            .and_then(sha1_reader)
            .map_err(|e| e.to_string())?;
        if h != src_hash || n != src_len {
            return Err(format!(
                "Copy check failed for {} (the copy doesn't match the original).",
                dest.display()
            ));
        }
        // never rename over something that appeared meanwhile
        if fs::symlink_metadata(&dest).is_ok() {
            return Err(format!(
                "{} appeared while copying; nothing was overwritten.",
                dest.display()
            ));
        }
        fs::rename(&partial, &dest).map_err(|e| format!("Cannot finish {}: {e}", dest.display()))
    })();
    if res.is_err() {
        let _ = fs::remove_file(&partial);
    }
    res.map(|_| true)
}

/// Filesystem type name of the volume holding `p` ("FAT32", "exFAT", "NTFS", "vfat", "msdos"...).
#[cfg(windows)]
pub fn fs_type(p: &Path) -> Option<String> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    extern "system" {
        fn GetVolumePathNameW(file: *const u16, out: *mut u16, len: u32) -> i32;
        fn GetVolumeInformationW(
            root: *const u16,
            name: *mut u16,
            nlen: u32,
            serial: *mut u32,
            maxc: *mut u32,
            flags: *mut u32,
            fs: *mut u16,
            fslen: u32,
        ) -> i32;
    }
    let w: Vec<u16> = p.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut root = [0u16; 512];
    let mut fsn = [0u16; 64];
    // SAFETY: NUL-terminated input, sized output buffers.
    unsafe {
        if GetVolumePathNameW(w.as_ptr(), root.as_mut_ptr(), 512) == 0 {
            return None;
        }
        if GetVolumeInformationW(
            root.as_ptr(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            fsn.as_mut_ptr(),
            64,
        ) == 0
        {
            return None;
        }
    }
    let n = fsn.iter().position(|&c| c == 0).unwrap_or(0);
    Some(String::from_utf16_lossy(&fsn[..n]))
}
#[cfg(not(windows))]
pub fn fs_type(p: &Path) -> Option<String> {
    let o = std::process::Command::new("stat")
        .args(["-f", "-c", "%T"])
        .arg(p)
        .output()
        .ok()?;
    Some(String::from_utf8_lossy(&o.stdout).trim().to_string()).filter(|s| !s.is_empty())
}

pub fn is_fat(t: &str) -> bool {
    let t = t.to_ascii_lowercase();
    t == "fat32" || t == "fat" || t == "fat16" || t == "vfat" || t == "msdos"
}

const FAT32_HINT: &str = "The destination is a FAT32 drive, which cannot hold a file over 4 GB. Pick an NTFS or exFAT folder, or convert to Games on Demand (GOD) instead.";

/// Does this error say a file was too large for the file system (EFBIG / ERROR_FILE_TOO_LARGE)?
pub fn looks_file_too_large(err: &str) -> bool {
    let l = err.to_ascii_lowercase();
    l.contains("file too large")
        || l.contains("efbig")
        || l.contains("os error 27)")
        || l.contains("os error 223)")
        || l.contains("file size exceeds")
        || l.contains("too large for the destination")
}

/// Add the FAT32 explanation to a failed write, but only when the FAT32 4 GB limit is the likely
/// cause (R15 L4): the error says "file too large", or the job had a file of 4 GiB or more.
/// Error codes the page matches on (`CANCELLED`, `PASSWORD_REQUIRED`, ...) are left exactly as they are.
pub fn fat32_hint_text(err: String, dest_is_fat: bool, has_big_file: bool) -> String {
    let is_code = !err.is_empty()
        && err
            .chars()
            .all(|c| c.is_ascii_uppercase() || c == '_' || c == ':' || c.is_ascii_digit());
    let cause = looks_file_too_large(&err) || has_big_file;
    if dest_is_fat
        && cause
        && !is_code
        && !err.starts_with("SLOT_OCCUPIED")
        && !err.contains("FAT32")
    {
        format!("{err} {FAT32_HINT}")
    } else {
        err
    }
}

/// `fat32_hint_text` for a destination folder on disk. `big_file` is only asked on a FAT drive
/// when the error itself doesn't say "too large" (e.g. it lists the archive).
pub fn with_fat32_hint(err: String, dest: &Path, big_file: impl FnOnce() -> bool) -> String {
    let fat = fs_type(dest).map(|t| is_fat(&t)).unwrap_or(false);
    let big = fat && !looks_file_too_large(&err) && big_file();
    fat32_hint_text(err, fat, big)
}

/// Refuse up front if the target is FAT and any file is 4 GiB or larger.
pub fn fat_check(target: &Path, sizes: impl Iterator<Item = (PathBuf, u64)>) -> Result<(), String> {
    fat_check_type(fs_type(target).as_deref(), sizes)
}
pub fn fat_check_type(
    t: Option<&str>,
    sizes: impl Iterator<Item = (PathBuf, u64)>,
) -> Result<(), String> {
    let Some(t) = t else { return Ok(()) };
    if !is_fat(t) {
        return Ok(());
    }
    for (p, n) in sizes {
        if n > FAT_MAX {
            return Err(format!("{} is {:.1} GB, but the target drive is {t}, which can't hold files of 4 GB or more. Nothing was copied. Use an NTFS/exFAT drive (GOD games are already split into small parts).", p.display(), n as f64 / 1e9));
        }
    }
    Ok(())
}

// ---- Grants: paths the user picked or that came from a scan/extraction ----

#[derive(Default)]
pub struct Grants {
    /// Paths the user picked (or approved from a drive scan). Persisted.
    persist: Mutex<HashSet<PathBuf>>,
    /// Folders Unpack created this session (extraction claim_dir). Not persisted.
    session: Mutex<HashSet<PathBuf>>,
    file: Mutex<Option<PathBuf>>,
}

impl Grants {
    /// Persist grants in `file` (the app's own data folder), loading what's there.
    pub fn with_file(file: PathBuf) -> Self {
        let g = Grants::default();
        if let Ok(s) = fs::read_to_string(&file) {
            if let Ok(v) = serde_json::from_str::<Vec<PathBuf>>(&s) {
                g.persist.lock().unwrap().extend(v.into_iter().take(5000));
            }
        }
        *g.file.lock().unwrap() = Some(file);
        g
    }
    fn save(&self) {
        let s = self.persist.lock().unwrap();
        if let Some(f) = self.file.lock().unwrap().as_ref() {
            let v: Vec<&PathBuf> = s.iter().collect();
            if let Some(d) = f.parent() {
                let _ = fs::create_dir_all(d);
            }
            let _ = fs::write(f, serde_json::to_string(&v).unwrap_or_default());
        }
    }
    /// User-picked / scan-approved path — persisted across restarts.
    pub fn grant(&self, p: &Path) {
        let Ok(c) = fs::canonicalize(p) else { return };
        if self.persist.lock().unwrap().insert(simplify(c)) {
            self.save();
        }
    }
    /// Folder Unpack created this run (claim_dir). Session-only; never written to disk.
    pub fn grant_session(&self, p: &Path) {
        let Ok(c) = fs::canonicalize(p) else { return };
        self.session.lock().unwrap().insert(simplify(c));
    }
    fn allowed(&self, c: &Path) -> bool {
        self.persist
            .lock()
            .unwrap()
            .iter()
            .any(|g| c.starts_with(g))
            || self
                .session
                .lock()
                .unwrap()
                .iter()
                .any(|g| c.starts_with(g))
    }
    /// The canonical path if `p` is a granted path or inside one.
    pub fn check(&self, p: &str) -> Result<PathBuf, String> {
        let c =
            simplify(fs::canonicalize(p).map_err(|e| format!("Cannot open {}: {e}", plain(p)))?);
        if self.allowed(&c) {
            Ok(c)
        } else {
            Err(format!(
                "{} wasn't picked in Unpack. Pick it again with the button.",
                plain(p)
            ))
        }
    }
    /// Like check, but also accepts a path that does not exist yet whose parent *is* granted
    /// (e.g. create_archive outFile, download destination file).
    pub fn check_or_parent(&self, p: &str) -> Result<PathBuf, String> {
        match self.check(p) {
            Ok(c) => Ok(c),
            Err(_) => {
                let pb = PathBuf::from(p);
                let parent = pb
                    .parent()
                    .ok_or_else(|| format!("{} wasn't picked in Unpack.", plain(p)))?;
                let _ = self.check(&parent.display().to_string())?;
                Ok(simplify(pb))
            }
        }
    }
}

/// Strip the Windows `\\?\` prefix from strings shown to the user.
pub fn plain(s: &str) -> String {
    simplify(PathBuf::from(s)).display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tmp(n: &str) -> crate::testutil::TmpDir {
        crate::testutil::TmpDir::canonical(&format!("safe-{n}"))
    }
    #[cfg(unix)]
    #[test]
    fn r5_refuses_symlinked_components() {
        let d = tmp("link");
        let (usb, else_) = (d.join("usb"), d.join("elsewhere"));
        fs::create_dir_all(&usb).unwrap();
        fs::create_dir_all(&else_).unwrap();
        std::os::unix::fs::symlink(&else_, usb.join("Content")).unwrap();
        fs::write(d.join("f"), b"x").unwrap();
        assert!(copy_file_safe(
            &d.join("f"),
            &usb,
            Path::new("Content/0000000000000000/f"),
            &mut |_| {}
        )
        .unwrap_err()
        .contains("link"));
        assert!(
            fs::read_dir(&else_).unwrap().next().is_none(),
            "nothing written outside the USB"
        );
        assert!(ensure_dir_under(&usb, Path::new("../x")).is_err());
        fs::remove_dir_all(&d).unwrap();
    }
    #[test]
    fn r5_partial_then_rename_and_stale_partial_cleared() {
        let d = tmp("part");
        let usb = d.join("usb");
        fs::create_dir_all(usb.join("Plugins")).unwrap();
        fs::write(d.join("menu.xex"), vec![7u8; 300_000]).unwrap();
        fs::write(usb.join("Plugins/menu.xex.partial"), b"half").unwrap(); // left by an unplug
        assert!(copy_file_safe(
            &d.join("menu.xex"),
            &usb,
            Path::new("Plugins/menu.xex"),
            &mut |_| {}
        )
        .unwrap());
        assert_eq!(
            fs::read(usb.join("Plugins/menu.xex")).unwrap().len(),
            300_000
        );
        assert!(!usb.join("Plugins/menu.xex.partial").exists());
        assert!(
            !copy_file_safe(
                &d.join("menu.xex"),
                &usb,
                Path::new("Plugins/menu.xex"),
                &mut |_| {}
            )
            .unwrap(),
            "identical skipped"
        );
        fs::write(usb.join("Plugins/menu.xex"), b"OTHER").unwrap();
        assert!(copy_file_safe(
            &d.join("menu.xex"),
            &usb,
            Path::new("Plugins/menu.xex"),
            &mut |_| {}
        )
        .is_err());
        assert_eq!(fs::read(usb.join("Plugins/menu.xex")).unwrap(), b"OTHER");
        fs::remove_dir_all(&d).unwrap();
    }
    #[test]
    fn r5_fat_limit() {
        assert!(
            is_fat("FAT32")
                && is_fat("vfat")
                && is_fat("msdos")
                && !is_fat("exFAT")
                && !is_fat("NTFS")
        );
        let hinted = fat32_hint_text("7-Zip failed on big.iso".into(), true, true);
        assert!(
            hinted.starts_with("7-Zip failed on big.iso")
                && hinted.contains("FAT32")
                && hinted.contains("4 GB"),
            "{hinted}"
        );
        assert_eq!(
            fat32_hint_text("7-Zip failed".into(), false, true),
            "7-Zip failed"
        );
        for code in [
            "CANCELLED",
            "PASSWORD_REQUIRED",
            "WRONG_PASSWORD",
            "SLOT_OCCUPIED:2:x",
        ] {
            assert_eq!(
                fat32_hint_text(code.into(), true, true),
                code,
                "codes the page matches on are never decorated"
            );
        }
        assert_eq!(
            fat32_hint_text(hinted.clone(), true, true),
            hinted,
            "no double hint"
        );
        // R15 L4: unrelated failures on a FAT drive get no hint
        for e in [
            "The archive is damaged (CRC failed).",
            "There is not enough space on the disk.",
            "Cannot write to E:\\x: path not found",
        ] {
            assert_eq!(fat32_hint_text(e.into(), true, false), e);
        }
        assert!(fat32_hint_text(
            "Write failed: File too large (os error 27)".into(),
            true,
            false
        )
        .contains("FAT32"));
        assert!(
            fat32_hint_text("Could not write: (os error 223)".into(), true, false)
                .contains("FAT32")
        );
        let big = || {
            vec![
                (PathBuf::from("a"), 10),
                (PathBuf::from("game.iso"), 5_000_000_000u64),
            ]
            .into_iter()
        };
        assert!(fat_check_type(Some("FAT32"), big())
            .unwrap_err()
            .contains("4 GB"));
        assert!(fat_check_type(Some("NTFS"), big()).is_ok());
        assert!(fat_check_type(
            Some("vfat"),
            vec![(PathBuf::from("x"), FAT_MAX)].into_iter()
        )
        .is_ok());
        assert!(fs_type(&std::env::temp_dir()).is_some());
    }
    #[test]
    fn r5_grants() {
        let d = tmp("grant");
        fs::create_dir_all(d.join("lib/game")).unwrap();
        fs::create_dir_all(d.join("other")).unwrap();
        let g = Grants::with_file(d.join("state/grants.json"));
        g.grant(&d.join("lib"));
        assert!(g.check(d.join("lib/game").to_str().unwrap()).is_ok());
        assert!(
            g.check(d.join("lib/../other").to_str().unwrap()).is_err(),
            "../ escape refused"
        );
        assert!(g.check(d.join("other").to_str().unwrap()).is_err());
        let g2 = Grants::with_file(d.join("state/grants.json"));
        assert!(
            g2.check(d.join("lib/game").to_str().unwrap()).is_ok(),
            "persisted"
        );
        // session grants are not persisted
        fs::create_dir_all(d.join("newclaim")).unwrap();
        g.grant_session(&d.join("newclaim"));
        assert!(g.check(d.join("newclaim").to_str().unwrap()).is_ok());
        let g3 = Grants::with_file(d.join("state/grants.json"));
        assert!(
            g3.check(d.join("newclaim").to_str().unwrap()).is_err(),
            "session not on disk"
        );
        assert!(g3.check(d.join("lib/game").to_str().unwrap()).is_ok());
        fs::remove_dir_all(&d).unwrap();
    }
    #[test]
    fn r5_addendum_skip_does_not_grant_existing() {
        // Mirrors Overseer: claiming an existing folder name with Skip must not grant it.
        // (The real extract path is tested via created:false; here we assert grant_session is opt-in.)
        let d = tmp("skipgrant");
        fs::create_dir_all(d.join("secret")).unwrap();
        fs::write(d.join("secret/notes.txt"), b"x").unwrap();
        let g = Grants::default();
        // without grant_session, secret isn't usable
        assert!(g
            .check(d.join("secret/notes.txt").to_str().unwrap())
            .is_err());
        g.grant(&d); // parent picked
        assert!(
            g.check(d.join("secret/notes.txt").to_str().unwrap())
                .is_ok(),
            "under picked parent"
        );
        fs::remove_dir_all(&d).unwrap();
    }
}
