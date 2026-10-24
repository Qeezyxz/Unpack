//! USB setup wizard: verify FAT32, create standard folders — never format.

use std::path::Path;

use serde::Serialize;

use crate::safecopy;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct UsbPrepReport {
    pub fat: bool,
    pub fs_type: String,
    pub created: Vec<String>,
    pub warnings: Vec<String>,
    pub big_files: Vec<String>,
}

/// Create `Content/0000000000000000` and `Apps` if missing. Never formats or deletes.
pub fn prepare_usb(root: &Path) -> Result<UsbPrepReport, String> {
    let meta = std::fs::metadata(root).map_err(|e| format!("{}: {e}", root.display()))?;
    if !meta.is_dir() {
        return Err("Pick the USB drive's top folder, not a file.".into());
    }
    let fs = safecopy::fs_type(root).unwrap_or_else(|| "unknown".into());
    let fat = safecopy::is_fat(&fs);
    let mut warnings = vec![];
    if !fat {
        warnings.push(format!(
            "This drive is {fs}, not FAT32. Many Xbox 360 USB setups need FAT32."
        ));
    }
    let mut created = vec![];
    for sub in ["Content/0000000000000000", "Apps"] {
        let p = root.join(sub);
        if !p.exists() {
            std::fs::create_dir_all(&p).map_err(|e| format!("Couldn't create {sub}: {e}"))?;
            created.push(p.display().to_string());
        }
    }
    let mut big_files = vec![];
    if fat {
        let mut stack = vec![root.to_path_buf()];
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
                    let len = e.metadata().map(|m| m.len()).unwrap_or(0);
                    if len > safecopy::FAT_MAX {
                        big_files.push(format!(
                            "{} ({})",
                            e.path().display(),
                            crate::download::human_size(len)
                        ));
                    }
                }
            }
        }
        if !big_files.is_empty() {
            warnings.push("Some files on this USB are over 4 GB — FAT32 can't hold them.".into());
        }
    }
    Ok(UsbPrepReport {
        fat,
        fs_type: fs,
        created,
        warnings,
        big_files,
    })
}
