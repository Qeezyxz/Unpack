use crate::fileops;
use crate::hashutil;
use crate::paths;
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};

const FAT32_MAX_FILE: u64 = 0xFFFF_FFFF;

#[derive(Debug, Clone, Serialize)]
pub struct VolumeInfo {
    pub path: String,
    pub filesystem: String,
    pub is_fat32: bool,
    pub fat32_limit: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct CopyReport {
    pub copied: u32,
    pub skipped_protected: u32,
    pub bytes: u64,
    pub verified: bool,
    pub warnings: Vec<String>,
}

pub fn filesystem_for(path: &Path) -> Result<VolumeInfo, String> {
    let canon = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let filesystem = detect_filesystem(&canon)?;
    let is_fat32 = filesystem.eq_ignore_ascii_case("fat32")
        || filesystem.eq_ignore_ascii_case("vfat")
        || filesystem.eq_ignore_ascii_case("msdos");
    Ok(VolumeInfo {
        path: canon.display().to_string(),
        filesystem,
        is_fat32,
        fat32_limit: FAT32_MAX_FILE,
    })
}

pub fn exceeds_fat32(size: u64) -> bool {
    size > FAT32_MAX_FILE
}

const FAT32_HINT: &str =
    "The destination is a FAT32 drive, which cannot hold a file over 4 GB. Pick an NTFS or exFAT folder, or convert to Games on Demand (GOD) instead.";

pub fn fat32_hint_text(err: String, dest_is_fat32: bool) -> String {
    if dest_is_fat32 {
        format!("{err} {FAT32_HINT}")
    } else {
        err
    }
}

/// Add the FAT32 explanation to a failed write when the folder is on FAT32.
pub fn with_fat32_hint(err: String, dest: &Path) -> String {
    let is_fat32 = filesystem_for(dest).map(|v| v.is_fat32).unwrap_or(false);
    fat32_hint_text(err, is_fat32)
}

pub fn copy_tree(
    src: &Path,
    dest_dir: &Path,
    verify: bool,
    overwrite_protected: bool,
) -> Result<CopyReport, String> {
    let mut report = CopyReport {
        copied: 0,
        skipped_protected: 0,
        bytes: 0,
        verified: verify,
        warnings: Vec::new(),
    };
    fs::create_dir_all(dest_dir).map_err(|e| e.to_string())?;
    // Probe the destination volume after the folder exists so canonicalize works.
    let vol = filesystem_for(dest_dir).ok();
    if src.is_file() {
        copy_one(
            src,
            &dest_dir.join(file_name(src)),
            verify,
            overwrite_protected,
            vol.as_ref(),
            &mut report,
        )?;
    } else {
        if let (Ok(from), Ok(into)) = (fs::canonicalize(src), fs::canonicalize(dest_dir)) {
            if into.starts_with(&from) {
                return Err(format!(
                    "Cannot copy {} into a folder inside itself ({}). Pick a destination outside it.",
                    src.display(),
                    dest_dir.display()
                ));
            }
        }
        // Keep the folder name so USB:\Halo\default.xex, not USB:\default.xex.
        let nested = dest_dir.join(file_name(src));
        copy_dir(src, &nested, verify, overwrite_protected, vol.as_ref(), &mut report, 0)?;
    }
    Ok(report)
}

pub fn merge_reports(into: &mut CopyReport, other: CopyReport) {
    into.copied += other.copied;
    into.skipped_protected += other.skipped_protected;
    into.bytes += other.bytes;
    into.verified = into.verified && other.verified;
    into.warnings.extend(other.warnings);
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".into())
}

fn copy_dir(
    src: &Path,
    dest: &Path,
    verify: bool,
    overwrite_protected: bool,
    vol: Option<&VolumeInfo>,
    report: &mut CopyReport,
    depth: u32,
) -> Result<(), String> {
    if depth > fileops::MAX_FOLDER_DEPTH {
        return Err(fileops::too_deep_message(src));
    }
    fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    for entry in fs::read_dir(src).map_err(|e| format!("read {}: {e}", src.display()))? {
        let entry = entry.map_err(|e| e.to_string())?;
        let from = entry.path();
        let to = dest.join(entry.file_name());
        if from.is_dir() {
            copy_dir(&from, &to, verify, overwrite_protected, vol, report, depth + 1)?;
        } else {
            copy_one(&from, &to, verify, overwrite_protected, vol, report)?;
        }
    }
    Ok(())
}

fn copy_one(
    src: &Path,
    dest: &Path,
    verify: bool,
    overwrite_protected: bool,
    vol: Option<&VolumeInfo>,
    report: &mut CopyReport,
) -> Result<(), String> {
    let name = file_name(src);
    if paths::is_protected_filename(&name) && dest.exists() && !overwrite_protected {
        report.skipped_protected += 1;
        report.warnings.push(format!(
            "left existing {} in place (interrupted copies must not delete DashLaunch config)",
            dest.display()
        ));
        return Ok(());
    }
    let meta = fs::metadata(src).map_err(|e| e.to_string())?;
    if exceeds_fat32(meta.len()) {
        let fat32 = vol
            .map(|v| v.is_fat32)
            .or_else(|| filesystem_for(dest.parent().unwrap_or(dest)).ok().map(|v| v.is_fat32))
            .unwrap_or(false);
        if fat32 {
            return Err(format!(
                "{} is {} bytes, over the FAT32 4 GiB file limit",
                src.display(),
                meta.len()
            ));
        }
        report.warnings.push(format!(
            "{} is larger than 4 GiB; copy will fail on FAT32 USB drives",
            src.display()
        ));
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    // Copy to a sibling temp, hash, then swap. Never delete dest first.
    let tmp = fileops::sibling(dest, "unpack-tmp");
    fileops::clear_path(&tmp)?;
    fs::copy(src, &tmp).map_err(|e| format!("copy {}: {e}", src.display()))?;
    if verify {
        let a = hashutil::sha256_file(src)?;
        let b = hashutil::sha256_file(&tmp)?;
        if a != b {
            let _ = fs::remove_file(&tmp);
            return Err(format!("verify failed for {}", src.display()));
        }
    }
    fileops::replace_file(&tmp, dest)?;
    report.copied += 1;
    report.bytes += meta.len();
    Ok(())
}

#[cfg(windows)]
fn detect_filesystem(path: &Path) -> Result<String, String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetVolumeInformationW;
    let root = volume_root(path)?;
    let mut root_w: Vec<u16> = root.as_os_str().encode_wide().collect();
    if !root_w.ends_with(&[0]) {
        root_w.push(0);
    }
    let mut fs_name = [0u16; 32];
    let ok = unsafe {
        GetVolumeInformationW(
            root_w.as_ptr(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            fs_name.as_mut_ptr(),
            fs_name.len() as u32,
        )
    };
    if ok == 0 {
        return Err("GetVolumeInformationW failed".into());
    }
    let len = fs_name.iter().position(|c| *c == 0).unwrap_or(fs_name.len());
    Ok(String::from_utf16_lossy(&fs_name[..len]))
}

#[cfg(windows)]
fn volume_root(path: &Path) -> Result<PathBuf, String> {
    let s = path.to_string_lossy();
    if s.len() >= 2 && s.as_bytes()[1] == b':' {
        Ok(PathBuf::from(format!("{}:\\", s.chars().next().unwrap())))
    } else {
        Ok(path.to_path_buf())
    }
}

#[cfg(not(windows))]
fn detect_filesystem(path: &Path) -> Result<String, String> {
    let mounts = fs::read_to_string("/proc/mounts").unwrap_or_default();
    let mut best: Option<(usize, String)> = None;
    for line in mounts.lines() {
        let mut parts = line.split_whitespace();
        let _dev = parts.next();
        let Some(mp) = parts.next() else { continue };
        let Some(fs) = parts.next() else { continue };
        if path.starts_with(mp) {
            let len = mp.len();
            if best.as_ref().map(|(l, _)| len > *l).unwrap_or(true) {
                best = Some((len, fs.to_string()));
            }
        }
    }
    Ok(best.map(|(_, fs)| fs).unwrap_or_else(|| "unknown".into()))
}

#[allow(dead_code)]
pub fn list_fat32_warnings(paths: &[PathBuf], dest: &Path) -> Result<Vec<String>, String> {
    let vol = filesystem_for(dest)?;
    let mut warnings = Vec::new();
    if vol.is_fat32 {
        warnings.push(format!("{} is {}", dest.display(), vol.filesystem));
    }
    for p in paths {
        if p.is_file() {
            if let Ok(meta) = fs::metadata(p) {
                if vol.is_fat32 && exceeds_fat32(meta.len()) {
                    warnings.push(format!(
                        "{} exceeds the FAT32 4 GiB file size limit",
                        p.display()
                    ));
                }
            }
        }
    }
    Ok(warnings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn fat32_failure_gets_a_plain_hint() {
        let hinted = fat32_hint_text("7-Zip failed on big.iso".into(), true);
        assert!(hinted.starts_with("7-Zip failed on big.iso"), "{hinted}");
        assert!(hinted.contains("FAT32"), "{hinted}");
        assert!(hinted.contains("4 GB"), "{hinted}");
        assert_eq!(fat32_hint_text("7-Zip failed".into(), false), "7-Zip failed");
    }

    #[test]
    fn fat32_limit() {
        assert!(!exceeds_fat32(4 * 1024 * 1024 * 1024 - 1));
        assert!(exceeds_fat32(4 * 1024 * 1024 * 1024));
        let _ = list_fat32_warnings(&[], Path::new("/"));
        let _ = crate::paths::arcade_content_type();
        let _ = crate::paths::theme_content_type();
        let _ = crate::paths::title_update_content_type();
        assert_eq!(crate::paths::dlc_content_type(), 0x00000002);
    }

    #[test]
    fn does_not_clobber_launch_ini() {
        let root = std::env::temp_dir().join(format!("unpack-usb-{}", std::process::id()));
        let src_dir = root.join("src");
        let dest_dir = root.join("dest");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&src_dir).unwrap();
        fs::create_dir_all(&dest_dir).unwrap();
        fs::write(src_dir.join("game.xex"), b"new").unwrap();
        fs::write(src_dir.join("launch.ini"), b"new-ini").unwrap();
        fs::create_dir_all(dest_dir.join("src")).unwrap();
        fs::write(dest_dir.join("src").join("launch.ini"), b"keep-me").unwrap();
        let report = copy_tree(&src_dir, &dest_dir, true, false).unwrap();
        assert_eq!(
            fs::read_to_string(dest_dir.join("src").join("launch.ini")).unwrap(),
            "keep-me"
        );
        assert_eq!(
            fs::read_to_string(dest_dir.join("src").join("game.xex")).unwrap(),
            "new"
        );
        assert_eq!(report.skipped_protected, 1);

        let replaced = copy_tree(&src_dir, &dest_dir, true, true).unwrap();
        assert_eq!(
            fs::read_to_string(dest_dir.join("src").join("launch.ini")).unwrap(),
            "new-ini"
        );
        assert!(replaced.copied >= 1);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn verifies_copied_bytes() {
        let root = std::env::temp_dir().join(format!("unpack-usb-v-{}", std::process::id()));
        let src = root.join("a.bin");
        let dest = root.join("out");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&dest).unwrap();
        let mut f = fs::File::create(&src).unwrap();
        f.write_all(&(0..8000u32).map(|x| (x % 251) as u8).collect::<Vec<_>>())
            .unwrap();
        drop(f);
        let report = copy_tree(&src, &dest, true, false).unwrap();
        assert_eq!(report.copied, 1);
        assert!(report.verified);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn copying_a_folder_into_itself_is_refused() {
        let root = std::env::temp_dir().join(format!("unpack-usb-self-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("games/inner")).unwrap();
        fs::write(root.join("games/a.bin"), b"a").unwrap();
        let err = copy_tree(&root.join("games"), &root.join("games/inner"), false, false).unwrap_err();
        assert!(err.contains("inside itself"), "{err}");
        assert!(!root.join("games/inner/games").exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn a_link_loop_ends_with_an_error_not_a_crash() {
        let root = std::env::temp_dir().join(format!("unpack-usb-loop-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src/sub")).unwrap();
        fs::write(root.join("src/sub/a.bin"), b"a").unwrap();
        std::os::unix::fs::symlink(root.join("src"), root.join("src/sub/back")).unwrap();
        let err = copy_tree(&root.join("src"), &root.join("out"), false, false).unwrap_err();
        assert!(err.contains("nested too deeply"), "{err}");
        let _ = fs::remove_dir_all(&root);
    }
}
