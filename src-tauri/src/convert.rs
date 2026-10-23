use crate::paths;
use iso2god::executable::{TitleExecutionInfo, TitleInfo};
use iso2god::god::{self, ContentType};
use iso2god::iso::{self, DirectoryTable, VolumeDescriptor};
use iso2god::{game_list, god::HashList};
use serde::Serialize;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

#[derive(Debug, Clone, Serialize)]
pub struct IsoInfo {
    pub path: String,
    pub title_id: String,
    pub title_id_num: u32,
    pub name: String,
    pub content_kind: String,
    pub disc_number: u8,
    pub disc_count: u8,
    pub media_id: String,
    pub used_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConvertReport {
    pub info: IsoInfo,
    pub output: String,
    pub mode: String,
    pub verified: bool,
    pub files: Vec<String>,
}

pub fn inspect_iso(path: &Path) -> Result<IsoInfo, String> {
    let file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let meta = file.metadata().map_err(|e| e.to_string())?;
    let mut iso = iso::IsoReader::read(file).map_err(|e| format!("not a Xbox ISO: {e}"))?;
    let title = TitleInfo::from_image(&mut iso).map_err(|e| format!("no default.xex/xbe: {e}"))?;
    Ok(iso_info(path, &title.execution_info, title.content_type, iso.get_max_used_prefix_size().min(meta.len())))
}

fn iso_info(path: &Path, exe: &TitleExecutionInfo, content_type: ContentType, used_bytes: u64) -> IsoInfo {
    let name = game_list::find_title_by_id(exe.title_id).unwrap_or_else(|| "(unknown)".into());
    IsoInfo {
        path: path.display().to_string(),
        title_id: format!("{:08X}", exe.title_id),
        title_id_num: exe.title_id,
        name,
        content_kind: match content_type {
            ContentType::GamesOnDemand => "Games on Demand".into(),
            ContentType::XboxOriginal => "Xbox Original".into(),
        },
        disc_number: exe.disc_number,
        disc_count: exe.disc_count,
        media_id: format!("{:08X}", exe.media_id),
        used_bytes,
    }
}

pub fn convert_iso(
    source: &Path,
    dest_dir: &Path,
    mode: &str,
    trim: bool,
    verify: bool,
    mut progress: impl FnMut(String, u64, u64),
) -> Result<ConvertReport, String> {
    match mode {
        "xex" | "extracted" | "folder" => extract_xex(source, dest_dir, verify, &mut progress),
        _ => convert_god(source, dest_dir, trim, verify, &mut progress),
    }
}

fn convert_god(
    source: &Path,
    dest_dir: &Path,
    trim: bool,
    verify: bool,
    progress: &mut dyn FnMut(String, u64, u64),
) -> Result<ConvertReport, String> {
    progress("Reading ISO metadata".into(), 0, 1);
    let source_iso_file = File::open(source).map_err(|e| e.to_string())?;
    let source_iso_file_meta = fs::metadata(source).map_err(|e| e.to_string())?;
    let mut source_iso = iso::IsoReader::read(source_iso_file).map_err(|e| e.to_string())?;
    let title_info = TitleInfo::from_image(&mut source_iso).map_err(|e| e.to_string())?;
    let exe_info = title_info.execution_info.clone();
    let content_type = title_info.content_type;
    let info = iso_info(source, &exe_info, content_type, source_iso.get_max_used_prefix_size());

    let data_size = if trim {
        source_iso.get_max_used_prefix_size()
    } else {
        source_iso_file_meta.len() - source_iso.volume_descriptor.root_offset
    };
    let block_count = data_size.div_ceil(god::BLOCK_SIZE as u64);
    let part_count = block_count.div_ceil(god::BLOCKS_PER_PART).max(1);
    let root_offset = source_iso.volume_descriptor.root_offset;
    drop(source_iso);

    fs::create_dir_all(dest_dir).map_err(|e| e.to_string())?;
    let dest_dir = dest_dir.to_path_buf();
    let file_layout = god::FileLayout::new(&dest_dir, &exe_info, content_type);
    ensure_empty_dir(&file_layout.data_dir_path()).map_err(|e| e.to_string())?;

    for part_index in 0..part_count {
        progress(
            format!("Writing GOD part {}/{part_count}", part_index + 1),
            part_index,
            part_count,
        );
        let mut iso_data_volume = File::open(source).map_err(|e| e.to_string())?;
        iso_data_volume
            .seek(SeekFrom::Start(root_offset))
            .map_err(|e| e.to_string())?;
        let part_path = file_layout.part_file_path(part_index);
        let part_file = File::options()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&part_path)
            .map_err(|e| e.to_string())?;
        god::write_part(iso_data_volume, part_index, part_file).map_err(|e| e.to_string())?;
    }

    progress("Hashing MHT chain".into(), part_count, part_count);
    let mut mht = read_part_mht(&file_layout, part_count - 1)?;
    for prev_part_index in (0..part_count - 1).rev() {
        let mut prev_mht = read_part_mht(&file_layout, prev_part_index)?;
        prev_mht.add_hash(&mht.digest());
        write_part_mht(&file_layout, prev_part_index, &prev_mht)?;
        mht = prev_mht;
    }

    let last_part_size = fs::metadata(file_layout.part_file_path(part_count - 1))
        .map(|m| m.len())
        .map_err(|e| e.to_string())?;
    let mut con_header = god::ConHeaderBuilder::new()
        .with_execution_info(&exe_info)
        .with_block_counts(block_count as u32, 0)
        .with_data_parts_info(
            part_count as u32,
            last_part_size + (part_count - 1) * (god::BLOCK_SIZE as u64) * 0xa290,
        )
        .with_content_type(content_type)
        .with_mht_hash(&mht.digest());
    if let Some(game_title) = game_list::find_title_by_id(exe_info.title_id) {
        con_header = con_header.with_game_title(&game_title);
    }
    let con_header = con_header.finalize();
    let header_path = file_layout.con_header_file_path();
    if let Some(parent) = header_path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    File::options()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&header_path)
        .map_err(|e| e.to_string())?
        .write_all(&con_header)
        .map_err(|e| e.to_string())?;

    let mut files = vec![header_path.display().to_string()];
    for i in 0..part_count {
        files.push(file_layout.part_file_path(i).display().to_string());
    }
    if verify {
        if !header_path.is_file() || fs::metadata(&header_path).map(|m| m.len()).unwrap_or(0) == 0 {
            return Err("verification failed: missing CON header".into());
        }
        for i in 0..part_count {
            let p = file_layout.part_file_path(i);
            if !p.is_file() {
                return Err(format!("verification failed: missing {}", p.display()));
            }
        }
    }
    progress("Done".into(), 1, 1);
    Ok(ConvertReport {
        info,
        output: header_path.display().to_string(),
        mode: "god".into(),
        verified: verify,
        files,
    })
}

fn extract_xex(
    source: &Path,
    dest_dir: &Path,
    verify: bool,
    progress: &mut dyn FnMut(String, u64, u64),
) -> Result<ConvertReport, String> {
    progress("Reading ISO filesystem".into(), 0, 1);
    let file = File::open(source).map_err(|e| e.to_string())?;
    let mut iso = iso::IsoReader::read(file).map_err(|e| e.to_string())?;
    let title_info = TitleInfo::from_image(&mut iso).map_err(|e| e.to_string())?;
    let info = iso_info(
        source,
        &title_info.execution_info,
        title_info.content_type,
        iso.get_max_used_prefix_size(),
    );
    let folder_name = paths::sanitize_xbox_folder_name(&info.name);
    let out = dest_dir.join(folder_name);
    fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let total = count_files(&iso.directory_table).max(1);
    let mut copied = 0u64;
    let mut files = Vec::new();
    extract_dir(
        source,
        &iso.volume_descriptor,
        &iso.directory_table,
        &out,
        &mut copied,
        total,
        &mut files,
        progress,
    )?;
    if verify {
        let has_exe = out.join("default.xex").is_file() || out.join("default.xbe").is_file();
        if !has_exe {
            return Err("verification failed: extracted folder has no default.xex/xbe".into());
        }
    }
    progress("Done".into(), 1, 1);
    Ok(ConvertReport {
        info,
        output: out.display().to_string(),
        mode: "xex".into(),
        verified: verify,
        files,
    })
}

fn count_files(dir: &DirectoryTable) -> u64 {
    dir.entries
        .iter()
        .map(|e| {
            if let Some(sub) = &e.subdirectory {
                count_files(sub)
            } else {
                1
            }
        })
        .sum()
}

fn extract_dir(
    iso_path: &Path,
    volume: &VolumeDescriptor,
    dir: &DirectoryTable,
    dest: &Path,
    copied: &mut u64,
    total: u64,
    files: &mut Vec<String>,
    progress: &mut dyn FnMut(String, u64, u64),
) -> Result<(), String> {
    fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    let mut reader = File::open(iso_path).map_err(|e| e.to_string())?;
    for entry in &dir.entries {
        if entry.name.is_empty() || entry.name == "." || entry.name == ".." {
            continue;
        }
        let safe = entry
            .name
            .chars()
            .map(|c| if matches!(c, '/' | '\\' | '\0') { '_' } else { c })
            .collect::<String>();
        let dest_path = dest.join(&safe);
        if let Some(sub) = &entry.subdirectory {
            extract_dir(iso_path, volume, sub, &dest_path, copied, total, files, progress)?;
        } else {
            let position = volume.root_offset + (entry.sector as u64) * volume.sector_size;
            reader
                .seek(SeekFrom::Start(position))
                .map_err(|e| e.to_string())?;
            let mut out = File::create(&dest_path).map_err(|e| e.to_string())?;
            let mut limited = (&mut reader).take(entry.size as u64);
            std::io::copy(&mut limited, &mut out).map_err(|e| e.to_string())?;
            files.push(dest_path.display().to_string());
            *copied += 1;
            progress(format!("Extracted {safe}"), *copied, total);
        }
    }
    Ok(())
}

fn ensure_empty_dir(path: &Path) -> Result<(), String> {
    if path.exists() {
        fs::remove_dir_all(path).map_err(|e| e.to_string())?;
    }
    fs::create_dir_all(path).map_err(|e| e.to_string())
}

fn read_part_mht(file_layout: &god::FileLayout, part_index: u64) -> Result<HashList, String> {
    let part_file = file_layout.part_file_path(part_index);
    let mut part_file = File::options()
        .read(true)
        .open(part_file)
        .map_err(|e| e.to_string())?;
    HashList::read(&mut part_file).map_err(|e| e.to_string())
}

fn write_part_mht(
    file_layout: &god::FileLayout,
    part_index: u64,
    mht: &HashList,
) -> Result<(), String> {
    let part_file = file_layout.part_file_path(part_index);
    let mut part_file = File::options()
        .write(true)
        .open(part_file)
        .map_err(|e| e.to_string())?;
    mht.write(&mut part_file).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inspect_rejects_random_file() {
        let path = std::env::temp_dir().join("unpack-not-an-iso.bin");
        std::fs::write(&path, b"hello").unwrap();
        assert!(inspect_iso(&path).is_err());
    }
}
