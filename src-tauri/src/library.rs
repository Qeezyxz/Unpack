use crate::fileops;
use crate::paths;
use crate::stfs;
use iso2god::game_list;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use std::sync::OnceLock;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TitleRecord {
    pub title_id: String,
    pub name: String,
    pub title_type: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct LibraryItem {
    pub path: String,
    pub kind: String,
    pub title_id: Option<String>,
    pub name: String,
    pub detail: String,
}

struct TitleDb {
    records: Vec<TitleRecord>,
}

fn db() -> &'static TitleDb {
    static DB: OnceLock<TitleDb> = OnceLock::new();
    DB.get_or_init(|| {
        let raw = include_str!("../vendor/iso2god/src/game_list/titles.jsonl");
        let mut records = Vec::new();
        for line in raw.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let v: serde_json::Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            records.push(TitleRecord {
                title_id: v
                    .get("TitleID")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_uppercase(),
                name: v
                    .get("Name")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_string(),
                title_type: v
                    .get("TitleType")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
            });
        }
        TitleDb { records }
    })
}

pub fn title_count() -> usize {
    db().records.len()
}

pub fn lookup_title(title_id: &str) -> Option<TitleRecord> {
    let needle = title_id.trim().trim_start_matches("0x").to_uppercase();
    db().records.iter().find(|r| r.title_id == needle).cloned()
}

#[allow(dead_code)]
pub fn lookup_title_u32(title_id: u32) -> Option<String> {
    game_list::find_title_by_id(title_id)
}

pub fn search_titles(query: &str, limit: usize) -> Vec<TitleRecord> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return db().records.iter().take(limit).cloned().collect();
    }
    db().records
        .iter()
        .filter(|r| r.name.to_lowercase().contains(&q) || r.title_id.to_lowercase().contains(&q))
        .take(limit)
        .cloned()
        .collect()
}

pub fn scan_library(root: &Path, max_depth: u32) -> Result<Vec<LibraryItem>, String> {
    let mut items = Vec::new();
    walk(root, 0, max_depth, &mut items)?;
    Ok(items)
}

fn walk(dir: &Path, depth: u32, max_depth: u32, items: &mut Vec<LibraryItem>) -> Result<(), String> {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => return Err(format!("{}: {e}", dir.display())),
    };
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            if looks_like_title_id(&name) {
                if let Some(item) = scan_title_folder(&path, &name) {
                    items.push(item);
                    continue;
                }
            }
            if path.join("default.xex").is_file() || path.join("default.xbe").is_file() {
                items.push(extracted_item(&path));
                continue;
            }
            if depth < max_depth {
                walk(&path, depth + 1, max_depth, items)?;
            }
        } else {
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if matches!(ext.as_str(), "iso" | "000" | "bin") {
                items.push(LibraryItem {
                    path: path.display().to_string(),
                    kind: "iso".into(),
                    title_id: None,
                    name,
                    detail: "Disc image".into(),
                });
            } else if let Ok(info) = stfs::parse_stfs_file(&path) {
                items.push(LibraryItem {
                    path: path.display().to_string(),
                    kind: info.content_label.to_lowercase().replace(' ', "_"),
                    title_id: Some(info.title_id.clone()),
                    name: if info.display_name.is_empty() {
                        lookup_title(&info.title_id)
                            .map(|t| t.name)
                            .unwrap_or(name)
                    } else {
                        info.display_name
                    },
                    detail: format!("{} · {}", info.content_label, info.title_id),
                });
            }
        }
    }
    Ok(())
}

fn looks_like_title_id(name: &str) -> bool {
    name.len() == 8 && name.chars().all(|c| c.is_ascii_hexdigit())
}

fn scan_title_folder(path: &Path, title_id: &str) -> Option<LibraryItem> {
    let god = path.join(format!("{:08X}", paths::god_content_type()));
    let name = lookup_title(title_id)
        .map(|t| t.name)
        .unwrap_or_else(|| title_id.to_string());
    if god.is_dir() {
        return Some(LibraryItem {
            path: path.display().to_string(),
            kind: "god".into(),
            title_id: Some(title_id.to_uppercase()),
            name,
            detail: "Games on Demand package".into(),
        });
    }
    None
}

fn extracted_item(path: &Path) -> LibraryItem {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    LibraryItem {
        path: path.display().to_string(),
        kind: "extracted".into(),
        title_id: None,
        name,
        detail: "Extracted XEX/XBE folder".into(),
    }
}

pub fn place_content(file: &Path, library_root: &Path) -> Result<LibraryItem, String> {
    let info = stfs::parse_stfs_file(file)?;
    let dest_dir = paths::content_dest(library_root, info.title_id_num, info.content_type);
    fs::create_dir_all(&dest_dir).map_err(|e| e.to_string())?;
    let dest = dest_dir.join(
        file.file_name()
            .ok_or_else(|| "file has no name".to_string())?,
    );
    fileops::copy_replace(file, &dest)?;
    Ok(LibraryItem {
        path: dest.display().to_string(),
        kind: info.content_label.to_lowercase(),
        title_id: Some(info.title_id.clone()),
        name: info.display_name.clone(),
        detail: format!("Placed {} for {}", info.content_label, info.title_id),
    })
}

pub fn list_dir(path: &Path) -> Result<Vec<String>, String> {
    let mut names: Vec<String> = fs::read_dir(path)
        .map_err(|e| e.to_string())?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_database_has_about_4900_entries() {
        assert!(title_count() > 5500, "got {}", title_count());
        let aurora = lookup_title("00000166").expect("Aurora");
        assert!(aurora.name.to_lowercase().contains("aurora"));
        assert_eq!(lookup_title_u32(0x00000166).unwrap().to_lowercase(), aurora.name.to_lowercase());
        let halo = search_titles("halo 3", 10);
        assert!(halo.iter().any(|t| t.name.to_lowercase().contains("halo")));
        let hawk = lookup_title("41560810").expect("Tony Hawk Project 8 from 360 list");
        assert!(hawk.name.to_lowercase().contains("project 8"));
        let afl = lookup_title("4143000D").expect("AFL Live 2003 from original Xbox list");
        assert!(afl.name.to_lowercase().contains("afl live"));
        assert_eq!(
            lookup_title_u32(0x41560810).unwrap().to_lowercase(),
            hawk.name.to_lowercase()
        );
    }

    fn csv_title_ids(raw: &str) -> Vec<String> {
        let mut ids = Vec::new();
        for (i, line) in raw.lines().enumerate() {
            if i == 0 || line.trim().is_empty() {
                continue;
            }
            let tid = line.split('\t').next().unwrap_or("").trim().to_uppercase();
            if tid.len() == 8 {
                ids.push(tid);
            }
        }
        ids.sort();
        ids.dedup();
        ids
    }

    #[test]
    fn catalog_contains_every_uploaded_title_id() {
        let xbox360 = csv_title_ids(include_str!(
            "../vendor/iso2god/src/game_list/gamelist_xbox360.csv"
        ));
        let xbox = csv_title_ids(include_str!(
            "../vendor/iso2god/src/game_list/gamelist_xbox.csv"
        ));
        assert!(xbox360.len() > 3000, "360 list too small: {}", xbox360.len());
        assert!(xbox.len() > 1000, "Xbox list too small: {}", xbox.len());
        for tid in xbox360.iter().chain(xbox.iter()) {
            assert!(
                lookup_title(tid).is_some(),
                "TitleID {tid} from the uploaded game list is missing from the conversion catalog"
            );
            let num = u32::from_str_radix(tid, 16).unwrap();
            assert!(
                lookup_title_u32(num).is_some(),
                "find_title_by_id missed {tid}"
            );
        }
    }

    #[test]
    fn places_dlc_under_content_titleid() {
        let root = std::env::temp_dir().join(format!("unpack-lib-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let mut buf = vec![0u8; 0x500];
        buf[0..4].copy_from_slice(b"LIVE");
        buf[0x344..0x348].copy_from_slice(&0x00000002u32.to_be_bytes());
        buf[0x360..0x364].copy_from_slice(&0x41560817u32.to_be_bytes());
        let src = root.join("dlc.bin");
        fs::write(&src, &buf).unwrap();
        let item = place_content(&src, &root).unwrap();
        assert!(item.path.contains("41560817"));
        assert!(item.path.contains("00000002"));
        let _ = fs::remove_dir_all(&root);
    }
}
