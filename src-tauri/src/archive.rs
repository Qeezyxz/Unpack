use serde::Serialize;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Debug, Clone, Serialize)]
pub struct ArchiveResult {
    pub archive: String,
    pub destination: String,
    pub status: String,
    #[serde(default)]
    pub nested: u32,
    /// Zip/rar/7z files inside the result that were left closed because
    /// "open zips inside zips" is off.
    #[serde(default)]
    pub inner_left: u32,
    #[serde(default)]
    pub isos: Vec<String>,
}

pub fn find_seven_zip() -> Option<PathBuf> {
    if let Ok(custom) = std::env::var("UNPACK_7ZA") {
        let p = PathBuf::from(custom);
        if p.exists() {
            return Some(p);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for name in ["7za.exe", "7z.exe", "7za", "7z", "7zr"] {
                let candidate = dir.join(name);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    for name in ["7za", "7z", "7zr"] {
        if let Some(p) = on_path(name) {
            return Some(p);
        }
    }
    None
}

fn on_path(name: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&paths) {
        let p = dir.join(name);
        if p.is_file() {
            return Some(p);
        }
        #[cfg(windows)]
        {
            let p = dir.join(format!("{name}.exe"));
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

#[derive(Debug, Clone)]
pub struct ExtractOptions {
    pub skip_junk: bool,
    pub existing: String,
    /// Open zip/rar/7z files found inside an extracted folder (Xbox dumps
    /// are often a zip that contains another rar that contains the ISO).
    pub nested: bool,
}

impl Default for ExtractOptions {
    fn default() -> Self {
        Self {
            skip_junk: true,
            existing: "number".into(),
            nested: true,
        }
    }
}

const MAX_NESTED_DEPTH: u32 = 3;
const MAX_NESTED_ARCHIVES: usize = 40;

#[cfg(test)]
pub fn extract_archives(archives: &[PathBuf], dest: &Path) -> Result<Vec<ArchiveResult>, String> {
    extract_archives_with(archives, dest, &ExtractOptions::default(), None)
}

pub fn extract_archives_with(
    archives: &[PathBuf],
    dest: &Path,
    opts: &ExtractOptions,
    mut progress: Option<&mut dyn FnMut(String, u64, u64)>,
) -> Result<Vec<ArchiveResult>, String> {
    let seven = find_seven_zip().ok_or_else(|| {
        "7-Zip was not found. Put 7za.exe next to Unpack.exe, or install 7-Zip / p7zip and set UNPACK_7ZA.".to_string()
    })?;
    fs_create(dest)?;
    let mut results = Vec::new();
    let total = archives.len() as u64;
    for (i, archive) in archives.iter().enumerate() {
        emit_progress(
            &mut progress,
            format!("Extracting {}", archive_stem(archive)),
            i as u64,
            total.max(1),
        );
        let stem = archive_stem(archive);
        let Some(out) = unique_dir(dest, &stem, &opts.existing)? else {
            results.push(ArchiveResult {
                archive: archive.display().to_string(),
                destination: dest.join(&stem).display().to_string(),
                status: "skipped (folder already exists)".into(),
                nested: 0,
                inner_left: 0,
                isos: Vec::new(),
            });
            continue;
        };
        match extract_one(&seven, archive, &out, opts, true) {
            Ok(status) => {
                let (nested, inner_left) = if opts.nested {
                    extract_nested(&seven, &out, opts, 1, &mut progress)?
                } else {
                    (0, find_archives(&out).len() as u32)
                };
                results.push(ArchiveResult {
                    archive: archive.display().to_string(),
                    destination: out.display().to_string(),
                    status: if nested > 0 {
                        format!("{status} (opened {nested} inner archive{})", if nested == 1 { "" } else { "s" })
                    } else {
                        status
                    },
                    nested,
                    inner_left,
                    isos: find_isos(&out),
                });
            }
            Err(e) => return Err(e),
        }
    }
    Ok(results)
}

fn extract_one(
    seven: &Path,
    archive: &Path,
    out: &Path,
    opts: &ExtractOptions,
    created_fresh: bool,
) -> Result<String, String> {
    let fresh = created_fresh && !out.exists();
    fs_create(out)?;
    let mut args: Vec<OsString> = vec!["x".into(), "-y".into()];
    if opts.existing == "overwrite" {
        args.push("-aoa".into());
    }
    // 7-Zip wants `-oDIR` as one argv token so paths with spaces stay intact.
    let mut out_arg = OsString::from("-o");
    out_arg.push(out.as_os_str());
    args.push(out_arg);
    if opts.skip_junk {
        args.extend(junk_excludes().into_iter().map(OsString::from));
    }
    args.push("--".into());
    args.push(archive.as_os_str().to_os_string());
    match run_7z(seven, &args) {
        Ok(status) => Ok(status),
        Err(e) => {
            if fresh {
                let _ = std::fs::remove_dir_all(out);
            }
            Err(e)
        }
    }
}

fn emit_progress(
    progress: &mut Option<&mut dyn FnMut(String, u64, u64)>,
    message: String,
    current: u64,
    total: u64,
) {
    if let Some(cb) = progress {
        cb(message, current, total);
    }
}

fn extract_nested(
    seven: &Path,
    root: &Path,
    opts: &ExtractOptions,
    depth: u32,
    progress: &mut Option<&mut dyn FnMut(String, u64, u64)>,
) -> Result<(u32, u32), String> {
    if depth > MAX_NESTED_DEPTH {
        return Ok((0, 0));
    }
    let nested = find_archives(root);
    if nested.is_empty() {
        return Ok((0, 0));
    }
    if nested.len() > MAX_NESTED_ARCHIVES {
        return Err(format!(
            "stopped: found {} archives inside {} (limit is {MAX_NESTED_ARCHIVES})",
            nested.len(),
            root.display()
        ));
    }
    let mut opened = 0u32;
    let mut left = 0u32;
    for (i, archive) in nested.iter().enumerate() {
        emit_progress(
            progress,
            format!("Opening inner {}", archive_stem(archive)),
            i as u64,
            nested.len() as u64,
        );
        let parent = archive.parent().unwrap_or(root);
        let stem = archive_stem(archive);
        let Some(out) = unique_dir(parent, &stem, "number")? else {
            continue;
        };
        // A broken or locked inner archive must not undo the outer one that
        // already came out fine; it just stays where it is.
        if extract_one(seven, archive, &out, opts, true).is_err() {
            left += 1;
            continue;
        }
        opened += 1;
        let (deeper, deeper_left) = extract_nested(seven, &out, opts, depth + 1, progress)?;
        opened += deeper;
        left += deeper_left;
    }
    Ok((opened, left))
}

/// `game.part2.rar` and later volumes open together with `game.part1.rar`.
fn is_later_rar_volume(lower_name: &str) -> bool {
    let Some(base) = lower_name.strip_suffix(".rar") else {
        return false;
    };
    let Some((_, number)) = base.rsplit_once(".part") else {
        return false;
    };
    !number.is_empty()
        && number.chars().all(|c| c.is_ascii_digit())
        && number.trim_start_matches('0') != "1"
}

fn is_archive_file(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if is_later_rar_volume(&name) {
        return false;
    }
    if name.ends_with(".tar.gz") || name.ends_with(".tgz") || name.ends_with(".tar.xz") {
        return true;
    }
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str(),
        "zip" | "rar" | "7z" | "tar" | "gz" | "xz" | "bz2"
    )
}

fn is_iso_file(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str(),
        "iso" | "000"
    )
}

fn find_archives(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk_files(root, 0, 6, &mut |path| {
        if is_archive_file(path) {
            out.push(path.to_path_buf());
        }
    });
    out
}

fn find_isos(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    walk_files(root, 0, 8, &mut |path| {
        if is_iso_file(path) {
            out.push(path.display().to_string());
        }
    });
    out
}

fn walk_files(dir: &Path, depth: u32, max_depth: u32, visit: &mut dyn FnMut(&Path)) {
    if depth > max_depth {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if crate::fileops::is_plain_dir(&path) {
            walk_files(&path, depth + 1, max_depth, visit);
        } else {
            visit(&path);
        }
    }
}

fn archive_stem(path: &Path) -> String {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    // Windows drops a trailing space or dot from a folder name, so the folder
    // we report would not be the folder that exists.
    let stem = stem.trim().trim_end_matches('.').trim_end();
    if stem.is_empty() {
        "archive".into()
    } else if crate::paths::is_windows_device_name(stem) {
        format!("_{stem}")
    } else {
        stem.to_string()
    }
}

fn unique_dir(parent: &Path, stem: &str, existing: &str) -> Result<Option<PathBuf>, String> {
    let first = parent.join(stem);
    if !first.exists() {
        return Ok(Some(first));
    }
    match existing {
        "skip" => Ok(None),
        "overwrite" => Ok(Some(first)),
        _ => {
            let mut n = 2u32;
            loop {
                let candidate = parent.join(format!("{stem} ({n})"));
                if !candidate.exists() {
                    return Ok(Some(candidate));
                }
                n += 1;
                if n > 9_999 {
                    return Err(format!("too many existing folders named {stem}"));
                }
            }
        }
    }
}

fn junk_excludes() -> Vec<String> {
    [
        "-xr!__MACOSX",
        "-x!Thumbs.db",
        "-x!desktop.ini",
        "-x!*.DS_Store",
        "-x!*.nfo",
        "-x!*.url",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

pub fn create_archive(inputs: &[PathBuf], dest: &Path) -> Result<ArchiveResult, String> {
    let seven = find_seven_zip().ok_or_else(|| "7-Zip was not found".to_string())?;
    if let Some(parent) = dest.parent() {
        fs_create(parent)?;
    }
    let mut args: Vec<OsString> = vec!["a".into(), "-y".into(), "--".into(), dest.as_os_str().to_os_string()];
    for input in inputs {
        args.push(input.as_os_str().to_os_string());
    }
    let status = run_7z(&seven, &args)?;
    Ok(ArchiveResult {
        archive: dest.display().to_string(),
        destination: dest.display().to_string(),
        status,
        nested: 0,
        inner_left: 0,
        isos: Vec::new(),
    })
}

fn run_7z(bin: &Path, args: &[OsString]) -> Result<String, String> {
    let mut command = Command::new(bin);
    command
        .args(args.iter().map(|a| a.as_os_str()))
        .stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // No flashing console window, and no password prompt waiting on a keyboard.
        command.creation_flags(0x0800_0000);
    }
    let output = command
        .output()
        .map_err(|e| format!("failed to run {}: {e}", bin.display()))?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let combined = format!("{stderr}{stdout}");
    if !output.status.success() {
        let lower = combined.to_ascii_lowercase();
        if lower.contains("wrong password")
            || lower.contains("encrypted")
            || lower.contains("enter password")
        {
            return Err("this archive is password-protected; Unpack does not prompt for passwords".into());
        }
        if combined.to_ascii_lowercase().contains("cannot open")
            || combined.to_ascii_lowercase().contains("is not supported")
            || combined.to_ascii_lowercase().contains("crc failed")
            || combined.to_ascii_lowercase().contains("data error")
        {
            return Err(format!(
                "archive is unreadable or corrupted: {}",
                combined.trim()
            ));
        }
        return Err(format!(
            "7-Zip failed ({:?}): {stderr}{stdout}",
            output.status.code()
        ));
    }
    Ok(if stdout.trim().is_empty() { stderr } else { stdout })
}

fn fs_create(path: &Path) -> Result<(), String> {
    std::fs::create_dir_all(path).map_err(|e| format!("create {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    #[test]
    fn folder_names_from_archives_are_safe_on_windows() {
        assert_eq!(archive_stem(Path::new("/x/Halo 3.zip")), "Halo 3");
        assert_eq!(archive_stem(Path::new("/x/Halo 3 .zip")), "Halo 3");
        assert_eq!(archive_stem(Path::new("/x/CON.zip")), "_CON");
        assert_eq!(archive_stem(Path::new("/x/aux.7z")), "_aux");
        assert_eq!(archive_stem(Path::new("/x/game.part1.rar")), "game.part1");
        assert_eq!(archive_stem(Path::new("/x/. .zip")), "archive");
    }

    use super::*;
    use std::io::Write;

    #[test]
    fn extract_roundtrip_when_7zip_present() {
        let Some(bin) = find_seven_zip() else {
            eprintln!("skipping archive test: 7-Zip not installed");
            return;
        };
        let root = std::env::temp_dir().join(format!("unpack-arc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let src = root.join("src");
        let dest = root.join("out with spaces");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::File::create(src.join("note.txt"))
            .unwrap()
            .write_all(b"unpack-archive")
            .unwrap();
        let archive = root.join("pack.zip");
        run_7z(
            &bin,
            &[
                OsString::from("a"),
                OsString::from("-y"),
                archive.as_os_str().to_os_string(),
                src.join("note.txt").as_os_str().to_os_string(),
            ],
        )
        .unwrap();
        extract_archives(&[archive.clone()], &dest).unwrap();
        let folder = dest.join("pack");
        let extracted = std::fs::read_to_string(folder.join("note.txt"))
            .or_else(|_| std::fs::read_to_string(folder.join("src").join("note.txt")))
            .unwrap();
        assert_eq!(extracted, "unpack-archive");
        extract_archives(&[archive], &dest).unwrap();
        assert!(dest.join("pack (2)").is_dir());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn opens_zip_inside_zip_when_nested() {
        let Some(bin) = find_seven_zip() else {
            eprintln!("skipping nested archive test: 7-Zip not installed");
            return;
        };
        let root = std::env::temp_dir().join(format!("unpack-nested-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let note = root.join("note.txt");
        std::fs::write(&note, b"inner-secret").unwrap();
        let inner = root.join("inner.zip");
        run_7z(
            &bin,
            &[
                OsString::from("a"),
                OsString::from("-y"),
                inner.as_os_str().to_os_string(),
                note.as_os_str().to_os_string(),
            ],
        )
        .unwrap();
        let outer = root.join("outer.zip");
        run_7z(
            &bin,
            &[
                OsString::from("a"),
                OsString::from("-y"),
                outer.as_os_str().to_os_string(),
                inner.as_os_str().to_os_string(),
            ],
        )
        .unwrap();
        let dest = root.join("out");
        let mut opts = ExtractOptions::default();
        opts.nested = true;
        extract_archives_with(&[outer.clone()], &dest, &opts, None).unwrap();
        let text = std::fs::read_to_string(dest.join("outer").join("inner").join("note.txt"))
            .or_else(|_| {
                std::fs::read_to_string(dest.join("outer").join("inner").join("src").join("note.txt"))
            })
            .or_else(|_| std::fs::read_to_string(dest.join("outer").join("note.txt")))
            .unwrap();
        assert_eq!(text, "inner-secret");

        let dest2 = root.join("out2");
        opts.nested = false;
        let closed = extract_archives_with(&[outer], &dest2, &opts, None).unwrap();
        assert_eq!(closed[0].nested, 0);
        assert_eq!(
            closed[0].inner_left, 1,
            "turning nested off must still report the inner zip it left closed"
        );
        assert!(!dest2.join("outer").join("inner").is_dir(), "inner zip must stay closed");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_broken_inner_archive_does_not_undo_the_outer_one() {
        let Some(bin) = find_seven_zip() else {
            return;
        };
        let root = std::env::temp_dir().join(format!("unpack-badinner-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let stage = root.join("stage");
        std::fs::create_dir_all(&stage).unwrap();
        std::fs::write(stage.join("keep.txt"), b"keep").unwrap();
        std::fs::write(stage.join("broken.zip"), b"this is not a zip at all").unwrap();
        let outer = root.join("outer.zip");
        run_7z(
            &bin,
            &[
                OsString::from("a"),
                OsString::from("-y"),
                outer.as_os_str().to_os_string(),
                stage.join("keep.txt").as_os_str().to_os_string(),
                stage.join("broken.zip").as_os_str().to_os_string(),
            ],
        )
        .unwrap();
        let dest = root.join("out");
        let done = extract_archives_with(&[outer], &dest, &ExtractOptions::default(), None)
            .expect("the outer archive came out fine, so this is not an error");
        assert_eq!(std::fs::read_to_string(dest.join("outer").join("keep.txt")).unwrap(), "keep");
        assert!(dest.join("outer").join("broken.zip").is_file(), "the bad inner file stays");
        assert_eq!(done[0].nested, 0);
        assert_eq!(done[0].inner_left, 1);
        let litter: Vec<_> = std::fs::read_dir(dest.join("outer")).unwrap().flatten().collect();
        assert_eq!(litter.len(), 2, "no half-made folder for the bad inner archive");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn unique_dir_adds_a_number() {
        let root = std::env::temp_dir().join(format!("unpack-unique-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("Game")).unwrap();
        let next = unique_dir(&root, "Game", "number").unwrap().unwrap();
        assert_eq!(next.file_name().unwrap(), "Game (2)");
        assert!(unique_dir(&root, "Game", "skip").unwrap().is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn later_rar_volumes_are_not_opened_on_their_own() {
        assert!(is_later_rar_volume("game.part2.rar"));
        assert!(is_later_rar_volume("game.part03.rar"));
        assert!(is_later_rar_volume("game.part10.rar"));
        assert!(!is_later_rar_volume("game.part1.rar"));
        assert!(!is_later_rar_volume("game.part001.rar"));
        assert!(!is_later_rar_volume("game.rar"));
        assert!(!is_later_rar_volume("party.rar"));
        assert!(is_archive_file(Path::new("x/game.part1.rar")));
        assert!(!is_archive_file(Path::new("x/game.part2.rar")));
    }

    #[test]
    fn password_protected_and_broken_archives_give_plain_errors() {
        let Some(bin) = find_seven_zip() else {
            eprintln!("skipping: 7-Zip not installed");
            return;
        };
        let root = std::env::temp_dir().join(format!("unpack-badarc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let note = root.join("note.txt");
        std::fs::write(&note, b"x").unwrap();
        let locked = root.join("locked.7z");
        run_7z(
            &bin,
            &[
                OsString::from("a"),
                OsString::from("-y"),
                OsString::from("-psecret"),
                OsString::from("-mhe=on"),
                locked.as_os_str().to_os_string(),
                note.as_os_str().to_os_string(),
            ],
        )
        .unwrap();
        let dest = root.join("out");
        let err = extract_archives(&[locked], &dest).unwrap_err();
        assert!(err.contains("password-protected"), "{err}");
        assert!(!dest.join("locked").exists(), "a failed extract must not leave a folder");

        let broken = root.join("broken.zip");
        std::fs::write(&broken, b"PK\x03\x04 this is not really a zip").unwrap();
        let err = extract_archives(&[broken], &dest).unwrap_err();
        assert!(!err.is_empty());
        assert!(!dest.join("broken").exists(), "{err}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn create_archive_handles_odd_names() {
        if find_seven_zip().is_none() {
            return;
        }
        let root = std::env::temp_dir().join(format!("unpack-mkarc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let odd = root.join("-dash name.txt");
        std::fs::write(&odd, b"odd").unwrap();
        let out = root.join("-made.zip");
        create_archive(&[odd], &out).unwrap();
        let dest = root.join("out");
        extract_archives(&[out], &dest).unwrap();
        let found: Vec<_> = std::fs::read_dir(dest.join("-made")).unwrap().flatten().collect();
        assert_eq!(found.len(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }
}
