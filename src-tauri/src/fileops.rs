use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Sibling temp/backup path: `.name.unpack-tmp` next to `path`.
/// Hidden on Unix; valid on FAT32. Never named `launch.ini`.
pub fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "file".into());
    path.with_file_name(format!(".{name}.{suffix}"))
}

/// Move `tmp` onto `dest` without deleting `dest` first.
/// On Windows, rename-over-file fails, so the original is moved aside,
/// `tmp` is renamed in, then the backup is removed. A crash between
/// those steps leaves either the original or a `.unpack-old` sibling,
/// never a missing `launch.ini`.
pub fn replace_file(tmp: &Path, dest: &Path) -> Result<(), String> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    if dest.exists() {
        let bak = sibling(dest, "unpack-old");
        clear_path(&bak)?;
        fs::rename(dest, &bak).map_err(|e| format!("set aside {}: {e}", dest.display()))?;
        if let Err(e) = rename_or_copy(tmp, dest) {
            let _ = fs::rename(&bak, dest);
            let _ = fs::remove_file(tmp);
            return Err(format!("replace {}: {e}", dest.display()));
        }
        let _ = fs::remove_file(&bak);
        Ok(())
    } else {
        rename_or_copy(tmp, dest).map_err(|e| format!("install {}: {e}", dest.display()))
    }
}

/// Same as [`replace_file`] for directories (GOD `.data` folders).
pub fn replace_dir(tmp: &Path, dest: &Path) -> Result<(), String> {
    if !tmp.is_dir() {
        return Err(format!("{} is not a folder", tmp.display()));
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    if dest.exists() {
        let bak = sibling(dest, "unpack-old");
        clear_path(&bak)?;
        fs::rename(dest, &bak).map_err(|e| format!("set aside {}: {e}", dest.display()))?;
        if let Err(e) = fs::rename(tmp, dest) {
            let _ = fs::rename(&bak, dest);
            return Err(format!("replace {}: {e}", dest.display()));
        }
        let _ = fs::remove_dir_all(&bak);
        Ok(())
    } else {
        fs::rename(tmp, dest).map_err(|e| format!("install {}: {e}", dest.display()))
    }
}

/// Write `data` to `path` via a temp file, then [`replace_file`].
pub fn write_atomic(path: &Path, data: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let tmp = sibling(path, "unpack-tmp");
    clear_path(&tmp)?;
    {
        let mut f = fs::File::create(&tmp).map_err(|e| format!("write {}: {e}", tmp.display()))?;
        f.write_all(data).map_err(|e| e.to_string())?;
        f.flush().map_err(|e| e.to_string())?;
    }
    replace_file(&tmp, path)
}

/// Copy `src` to a sibling temp, then [`replace_file`] onto `dest`.
/// `dest` is not truncated until the copy has finished.
pub fn copy_replace(src: &Path, dest: &Path) -> Result<(), String> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let tmp = sibling(dest, "unpack-tmp");
    clear_path(&tmp)?;
    if let Err(e) = fs::copy(src, &tmp) {
        let _ = fs::remove_file(&tmp);
        return Err(format!("copy {}: {e}", src.display()));
    }
    let src_len = fs::metadata(src).map_err(|e| e.to_string())?.len();
    let tmp_len = fs::metadata(&tmp).map_err(|e| e.to_string())?.len();
    if src_len != tmp_len {
        let _ = fs::remove_file(&tmp);
        return Err(format!(
            "copy size mismatch for {}: {src_len} vs {tmp_len}",
            src.display()
        ));
    }
    replace_file(&tmp, dest)
}

/// A real folder, not a symlink or junction that could point back up the tree.
pub fn is_plain_dir(path: &Path) -> bool {
    fs::symlink_metadata(path).map(|m| m.is_dir()).unwrap_or(false)
}

/// Folders nested deeper than this are almost certainly a link loop.
pub const MAX_FOLDER_DEPTH: u32 = 48;

pub fn too_deep_message(path: &Path) -> String {
    format!(
        "{} is nested too deeply (is there a shortcut or link that points back up the folder tree?)",
        path.display()
    )
}

pub fn clear_path(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Ok(());
    }
    if path.is_dir() {
        fs::remove_dir_all(path).map_err(|e| format!("remove {}: {e}", path.display()))
    } else {
        fs::remove_file(path).map_err(|e| format!("remove {}: {e}", path.display()))
    }
}

fn rename_or_copy(from: &Path, dest: &Path) -> io::Result<()> {
    match fs::rename(from, dest) {
        Ok(()) => Ok(()),
        Err(_) => {
            fs::copy(from, dest)?;
            fs::remove_file(from)?;
            Ok(())
        }
    }
}

/// Staging directory that is deleted on drop unless [`Staging::keep`] was called.
pub struct Staging {
    path: PathBuf,
    keep: bool,
}

impl Staging {
    pub fn new(path: PathBuf) -> Result<Self, String> {
        if path.exists() {
            clear_path(&path)?;
        }
        fs::create_dir_all(&path).map_err(|e| format!("create {}: {e}", path.display()))?;
        Ok(Self { path, keep: false })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    #[cfg(test)]
    pub fn keep(mut self) {
        self.keep = true;
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        if !self.keep && self.path.exists() {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

/// Say plainly when a chosen output folder cannot be used, before any work
/// starts. Nothing is created: only the nearest folder that already exists is
/// looked at.
pub fn check_output_folder(path: &Path) -> Result<(), String> {
    let mut probe = Some(path);
    while let Some(p) = probe {
        match fs::metadata(p) {
            Ok(meta) if meta.is_dir() => return Ok(()),
            Ok(_) => {
                return Err(format!(
                    "The output folder \"{}\" can't be used because \"{}\" is a file, not a folder.",
                    path.display(),
                    p.display()
                ))
            }
            Err(_) => probe = p.parent().filter(|q| !q.as_os_str().is_empty()),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("unpack-fileops-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn replace_file_keeps_original_until_new_is_in_place() {
        let dir = root("file");
        let dest = dir.join("launch.ini");
        fs::write(&dest, b"keep").unwrap();
        let tmp = sibling(&dest, "unpack-tmp");
        fs::write(&tmp, b"new").unwrap();
        replace_file(&tmp, &dest).unwrap();
        assert_eq!(fs::read_to_string(&dest).unwrap(), "new");
        assert!(!tmp.exists());
        assert!(!sibling(&dest, "unpack-old").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_atomic_creates_parent_and_replaces() {
        let dir = root("atomic");
        let dest = dir.join("nested").join("settings.json");
        write_atomic(&dest, b"{\"a\":1}").unwrap();
        assert_eq!(fs::read_to_string(&dest).unwrap(), "{\"a\":1}");
        write_atomic(&dest, b"{\"a\":2}").unwrap();
        assert_eq!(fs::read_to_string(&dest).unwrap(), "{\"a\":2}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn replace_dir_swaps_folders() {
        let dir = root("dir");
        let dest = dir.join("MEDIA.data");
        fs::create_dir_all(&dest).unwrap();
        fs::write(dest.join("Data0000"), b"old").unwrap();
        let tmp = dir.join("staging.data");
        fs::create_dir_all(&tmp).unwrap();
        fs::write(tmp.join("Data0000"), b"new").unwrap();
        replace_dir(&tmp, &dest).unwrap();
        assert_eq!(fs::read_to_string(dest.join("Data0000")).unwrap(), "new");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn staging_cleans_up_unless_kept() {
        let dir = root("stage");
        let stage_path = dir.join("tmp");
        {
            let staging = Staging::new(stage_path.clone()).unwrap();
            fs::write(staging.path().join("x"), b"1").unwrap();
            assert!(stage_path.is_dir());
        }
        assert!(!stage_path.exists());
        let staging = Staging::new(stage_path.clone()).unwrap();
        fs::write(staging.path().join("x"), b"1").unwrap();
        staging.keep();
        assert!(stage_path.is_dir());
        let _ = fs::remove_dir_all(&dir);
    }
}
