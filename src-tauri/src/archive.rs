use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Serialize)]
pub struct ArchiveResult {
    pub archive: String,
    pub destination: String,
    pub status: String,
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

pub fn extract_archives(archives: &[PathBuf], dest: &Path) -> Result<Vec<ArchiveResult>, String> {
    let seven = find_seven_zip().ok_or_else(|| {
        "7-Zip was not found. Put 7za.exe next to Unpack.exe, or install 7-Zip / p7zip and set UNPACK_7ZA.".to_string()
    })?;
    fs_create(dest)?;
    let mut results = Vec::new();
    for archive in archives {
        let status = run_7z(
            &seven,
            &["x", "-y", &format!("-o{}", dest.display()), "--", &archive.display().to_string()],
        )?;
        results.push(ArchiveResult {
            archive: archive.display().to_string(),
            destination: dest.display().to_string(),
            status,
        });
    }
    Ok(results)
}

pub fn create_archive(inputs: &[PathBuf], dest: &Path) -> Result<ArchiveResult, String> {
    let seven = find_seven_zip().ok_or_else(|| "7-Zip was not found".to_string())?;
    if let Some(parent) = dest.parent() {
        fs_create(parent)?;
    }
    let mut args = vec!["a".to_string(), "-y".to_string(), dest.display().to_string()];
    for input in inputs {
        args.push(input.display().to_string());
    }
    let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let status = run_7z(&seven, &arg_refs)?;
    Ok(ArchiveResult {
        archive: dest.display().to_string(),
        destination: dest.display().to_string(),
        status,
    })
}

fn run_7z(bin: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new(bin)
        .args(args)
        .output()
        .map_err(|e| format!("failed to run {}: {e}", bin.display()))?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if !output.status.success() {
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
        let dest = root.join("out");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::File::create(src.join("note.txt"))
            .unwrap()
            .write_all(b"unpack-archive")
            .unwrap();
        let archive = root.join("pack.zip");
        run_7z(
            &bin,
            &[
                "a",
                "-y",
                &archive.display().to_string(),
                &src.join("note.txt").display().to_string(),
            ],
        )
        .unwrap();
        extract_archives(&[archive], &dest).unwrap();
        let extracted = std::fs::read_to_string(dest.join("note.txt"))
            .or_else(|_| std::fs::read_to_string(dest.join("src").join("note.txt")))
            .unwrap();
        assert_eq!(extracted, "unpack-archive");
        let _ = std::fs::remove_dir_all(&root);
    }
}
