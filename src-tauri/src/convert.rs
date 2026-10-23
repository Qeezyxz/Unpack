use crate::paths;
use iso2god::executable::{TitleExecutionInfo, TitleInfo};
use iso2god::god::{self, ContentType};
use iso2god::iso::{self, DirectoryTable, VolumeDescriptor};
use iso2god::{game_list, god::HashList};
use serde::Serialize;
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

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
    pub discs: Vec<IsoInfo>,
    pub merged: bool,
}

pub fn inspect_iso(path: &Path) -> Result<IsoInfo, String> {
    let file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let meta = file.metadata().map_err(|e| e.to_string())?;
    let mut iso = iso::IsoReader::read(file).map_err(|e| format!("not a Xbox ISO: {e}"))?;
    let title = TitleInfo::from_image(&mut iso).map_err(|e| format!("no default.xex/xbe: {e}"))?;
    Ok(iso_info(
        path,
        &title.execution_info,
        title.content_type,
        iso.get_max_used_prefix_size().min(meta.len()),
    ))
}

fn display_name_for_title(title_id: u32) -> String {
    // Never collapse missing catalog entries to one "(unknown)" folder — that
    // overwrites the previous ISO. TitleID hex keeps each conversion distinct.
    game_list::find_title_by_id(title_id)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("{title_id:08X}"))
}

fn iso_info(path: &Path, exe: &TitleExecutionInfo, content_type: ContentType, used_bytes: u64) -> IsoInfo {
    let mut info = IsoInfo {
        path: path.display().to_string(),
        title_id: format!("{:08X}", exe.title_id),
        title_id_num: exe.title_id,
        name: display_name_for_title(exe.title_id),
        content_kind: match content_type {
            ContentType::GamesOnDemand => "Games on Demand".into(),
            ContentType::XboxOriginal => "Xbox Original".into(),
        },
        disc_number: exe.disc_number,
        disc_count: exe.disc_count,
        media_id: format!("{:08X}", exe.media_id),
        used_bytes,
    };
    apply_disc_hint(&mut info, path);
    info
}

fn apply_disc_hint(info: &mut IsoInfo, path: &Path) {
    if let Some(n) = disc_hint_from_name(path) {
        if info.disc_count <= 1 && info.disc_number <= 1 {
            info.disc_number = n;
            info.disc_count = info.disc_count.max(n).max(2);
        } else if info.disc_number == 0 {
            info.disc_number = n;
        }
    }
    if info.disc_number == 0 {
        info.disc_number = 1;
    }
    if info.disc_count == 0 {
        info.disc_count = 1;
    }
}

/// Redump / dump names: `Game (Disc 2).iso`, `Game-disk1.iso`.
pub fn disc_hint_from_name(path: &Path) -> Option<u8> {
    let stem = path
        .file_stem()
        .or_else(|| path.file_name())
        .map(|n| n.to_string_lossy().to_ascii_lowercase())?;
    for label in ["disc", "disk", "dvd"] {
        if let Some(n) = digit_after_label(&stem, label) {
            return Some(n);
        }
    }
    for label in ["(cd", " cd", "-cd", "_cd"] {
        if let Some(n) = digit_after_label(&stem, label) {
            return Some(n);
        }
    }
    None
}

fn digit_after_label(haystack: &str, label: &str) -> Option<u8> {
    let mut start = 0;
    while let Some(pos) = haystack[start..].find(label) {
        let abs = start + pos + label.len();
        let rest = haystack[abs..].trim_start_matches(|c: char| {
            c == ' ' || c == '-' || c == '_' || c == '.' || c == '#'
        });
        if let Some(ch) = rest.chars().next() {
            if ch.is_ascii_digit() {
                let n = ch.to_digit(10).unwrap() as u8;
                if (1..=9).contains(&n) {
                    return Some(n);
                }
            }
        }
        start = abs;
    }
    None
}

pub fn stem_without_disc(path: &Path) -> String {
    let mut s = path
        .file_stem()
        .map(|n| n.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    for n in 1..=9u8 {
        for pat in [
            format!("(disc {n})"),
            format!("(disk {n})"),
            format!("(dvd {n})"),
            format!("(cd {n})"),
            format!("(disc{n})"),
            format!("(disk{n})"),
            format!("-disc{n}"),
            format!("_disc{n}"),
            format!("-disk{n}"),
            format!("_disk{n}"),
            format!(" disc {n}"),
            format!(" disk {n}"),
            format!("-d{n}"),
            format!("_d{n}"),
        ] {
            s = s.replace(&pat, "");
        }
    }
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn is_iso_path(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str(),
        "iso" | "000" | "bin"
    )
}

pub fn convert_isos(
    sources: &[PathBuf],
    dest_dir: &Path,
    mode: &str,
    trim: bool,
    verify: bool,
    auto_companions: bool,
    mut progress: impl FnMut(String, u64, u64),
) -> Result<Vec<ConvertReport>, String> {
    if sources.is_empty() {
        return Err("choose an ISO".into());
    }
    if dest_dir.as_os_str().is_empty() {
        return Err("choose an output folder".into());
    }
    let paths = expand_iso_set(sources, auto_companions, &mut progress)?;
    let groups = group_discs(&paths)?;
    let mut reports = Vec::new();
    let game_total = groups.len() as u64;
    for (gi, discs) in groups.into_iter().enumerate() {
        let name = discs
            .first()
            .and_then(|(_, info)| info.as_ref().map(|i| i.name.clone()))
            .unwrap_or_else(|| "game".into());
        progress(
            format!("Game {}/{}: {name} ({} disc{})", gi + 1, game_total, discs.len(), if discs.len() == 1 { "" } else { "s" }),
            gi as u64,
            game_total.max(1),
        );
        let report = match mode {
            "xex" | "extracted" | "folder" => extract_xex_set(&discs, dest_dir, verify, &mut progress)?,
            _ => convert_god_set(&discs, dest_dir, trim, verify, &mut progress)?,
        };
        reports.push(report);
    }
    Ok(reports)
}

fn expand_iso_set(
    sources: &[PathBuf],
    auto_companions: bool,
    progress: &mut dyn FnMut(String, u64, u64),
) -> Result<Vec<PathBuf>, String> {
    let mut out: Vec<PathBuf> = Vec::new();
    for src in sources {
        if src.as_os_str().is_empty() {
            continue;
        }
        if !out.iter().any(|p| p == src) {
            out.push(src.clone());
        }
    }
    if !auto_companions {
        return Ok(out);
    }
    let selected = out.clone();
    for path in selected {
        let hinted = disc_hint_from_name(&path).is_some()
            || inspect_iso(&path)
                .map(|i| i.disc_count > 1 || i.disc_number > 1)
                .unwrap_or(false);
        if !hinted {
            continue;
        }
        progress(format!("Looking for other discs next to {}", file_name(&path)), 0, 1);
        let Some(parent) = path.parent() else { continue };
        let stem = stem_without_disc(&path);
        let title = inspect_iso(&path).ok().map(|i| i.title_id);
        let siblings: Vec<PathBuf> = fs::read_dir(parent)
            .map_err(|e| e.to_string())?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_file() && is_iso_path(p))
            .collect();
        let inspect_all = siblings.len() <= 32;
        for sib in siblings {
            if out.iter().any(|p| p == &sib) {
                continue;
            }
            let same_stem = stem_without_disc(&sib) == stem && !stem.is_empty();
            let same_title = inspect_all
                && title
                    .as_ref()
                    .and_then(|tid| inspect_iso(&sib).ok().map(|i| i.title_id == *tid))
                    .unwrap_or(false);
            if same_stem || same_title {
                out.push(sib);
            }
        }
    }
    Ok(out)
}

fn group_discs(paths: &[PathBuf]) -> Result<Vec<Vec<(PathBuf, Option<IsoInfo>)>>, String> {
    let mut keyed: BTreeMap<String, Vec<(PathBuf, Option<IsoInfo>)>> = BTreeMap::new();
    for path in paths {
        let info = inspect_iso(path).ok();
        let key = info
            .as_ref()
            .map(|i| format!("id:{}", i.title_id))
            .unwrap_or_else(|| format!("stem:{}", stem_without_disc(path)));
        keyed.entry(key).or_default().push((path.clone(), info));
    }
    let mut groups: Vec<_> = keyed.into_values().collect();
    for group in &mut groups {
        group.sort_by_key(|(path, info)| {
            let n = info
                .as_ref()
                .map(|i| i.disc_number)
                .or_else(|| disc_hint_from_name(path))
                .unwrap_or(1);
            (n, path.display().to_string())
        });
    }
    groups.sort_by_key(|g| g.first().map(|(p, _)| p.display().to_string()).unwrap_or_default());
    if groups.is_empty() {
        return Err("no ISOs to convert".into());
    }
    Ok(groups)
}

fn convert_god_set(
    discs: &[(PathBuf, Option<IsoInfo>)],
    dest_dir: &Path,
    trim: bool,
    verify: bool,
    progress: &mut dyn FnMut(String, u64, u64),
) -> Result<ConvertReport, String> {
    let mut files = Vec::new();
    let mut infos = Vec::new();
    let mut last_header = PathBuf::new();
    let disc_total = discs.len() as u64;
    for (i, (path, preview)) in discs.iter().enumerate() {
        progress(
            format!(
                "GOD disc {}/{} ({})",
                i + 1,
                disc_total,
                preview.as_ref().map(|p| p.name.as_str()).unwrap_or("ISO")
            ),
            i as u64,
            disc_total.max(1),
        );
        let one = convert_god(path, dest_dir, trim, verify, progress)?;
        last_header = PathBuf::from(&one.output);
        files.extend(one.files);
        infos.push(one.info);
    }
    let title_folder = infos
        .first()
        .map(|i| dest_dir.join(&i.title_id))
        .unwrap_or_else(|| last_header.clone());
    Ok(ConvertReport {
        info: infos.first().cloned().ok_or_else(|| "GOD convert produced no disc".to_string())?,
        output: if infos.len() > 1 {
            title_folder.display().to_string()
        } else {
            last_header.display().to_string()
        },
        mode: "god".into(),
        verified: verify,
        files,
        merged: infos.len() > 1,
        discs: infos,
    })
}

fn extract_xex_set(
    discs: &[(PathBuf, Option<IsoInfo>)],
    dest_dir: &Path,
    verify: bool,
    progress: &mut dyn FnMut(String, u64, u64),
) -> Result<ConvertReport, String> {
    let name = discs
        .iter()
        .find_map(|(_, info)| info.as_ref().map(|i| i.name.clone()))
        .unwrap_or_else(|| {
            discs
                .first()
                .map(|(p, _)| stem_without_disc(p))
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "Game".into())
        });
    let folder_name = paths::sanitize_xbox_folder_name(&name);
    let out = dest_dir.join(folder_name);
    ensure_empty_dir(&out)?;
    let mut files = Vec::new();
    let mut infos = Vec::new();
    let disc_total = discs.len() as u64;
    for (i, (path, preview)) in discs.iter().enumerate() {
        let merge = i > 0;
        progress(
            format!(
                "{} disc {}/{} into {}",
                if merge { "Merging" } else { "Extracting" },
                i + 1,
                disc_total,
                out.display()
            ),
            i as u64,
            disc_total.max(1),
        );
        let (info, part_files) = extract_iso_into(path, &out, merge, preview.as_ref(), progress)?;
        files.extend(part_files);
        if let Some(info) = info {
            infos.push(info);
        }
    }
    if verify {
        let has_exe = out.join("default.xex").is_file() || out.join("default.xbe").is_file();
        if !has_exe {
            return Err("verification failed: merged folder has no default.xex/xbe (convert disc 1 first)".into());
        }
    }
    let info = infos.first().cloned().ok_or_else(|| {
        format!("could not read a default.xex/xbe from {}", discs[0].0.display())
    })?;
    progress("Done".into(), 1, 1);
    Ok(ConvertReport {
        info,
        output: out.display().to_string(),
        mode: "xex".into(),
        verified: verify,
        files,
        merged: discs.len() > 1,
        discs: infos,
    })
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
        discs: Vec::new(),
        merged: false,
    })
}

fn extract_iso_into(
    source: &Path,
    out: &Path,
    merge: bool,
    preview: Option<&IsoInfo>,
    progress: &mut dyn FnMut(String, u64, u64),
) -> Result<(Option<IsoInfo>, Vec<String>), String> {
    progress("Reading ISO filesystem".into(), 0, 1);
    let file = File::open(source).map_err(|e| e.to_string())?;
    let mut iso = iso::IsoReader::read(file).map_err(|e| format!("{}: {e}", source.display()))?;
    let title_info = TitleInfo::from_image(&mut iso).ok();
    if title_info.is_none() && !merge {
        return Err(format!("no default.xex/xbe in {}", source.display()));
    }
    let info = title_info.as_ref().map(|t| {
        iso_info(
            source,
            &t.execution_info,
            t.content_type,
            iso.get_max_used_prefix_size(),
        )
    });
    let _ = preview;
    fs::create_dir_all(out).map_err(|e| e.to_string())?;
    let total = count_files(&iso.directory_table).max(1);
    let mut copied = 0u64;
    let mut files = Vec::new();
    extract_dir(
        source,
        &iso.volume_descriptor,
        &iso.directory_table,
        out,
        merge,
        &mut copied,
        total,
        &mut files,
        progress,
    )?;
    Ok((info, files))
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

/// Disc 1 is the playable executable. Later discs may overlay assets, but must
/// never replace default.xex / default.xbe.
pub fn merge_replace_allowed(dest: &Path) -> bool {
    if !dest.exists() {
        return true;
    }
    let Some(name) = dest.file_name().and_then(|n| n.to_str()) else {
        return true;
    };
    !name.eq_ignore_ascii_case("default.xex") && !name.eq_ignore_ascii_case("default.xbe")
}

fn extract_dir(
    iso_path: &Path,
    volume: &VolumeDescriptor,
    dir: &DirectoryTable,
    dest: &Path,
    merge: bool,
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
            extract_dir(
                iso_path,
                volume,
                sub,
                &dest_path,
                merge,
                copied,
                total,
                files,
                progress,
            )?;
        } else {
            if merge && !merge_replace_allowed(&dest_path) {
                *copied += 1;
                progress(format!("Kept disc 1 {safe}"), *copied, total);
                continue;
            }
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

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
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

    #[test]
    fn unknown_title_uses_title_id_not_unknown() {
        let name = display_name_for_title(0xDEADBEEF);
        assert_eq!(name, "DEADBEEF");
        assert_ne!(name.to_lowercase(), "(unknown)");
        let known = display_name_for_title(0x4D5307E6);
        assert!(known.to_lowercase().contains("halo"));
    }

    #[test]
    fn reads_disc_number_from_filename() {
        assert_eq!(
            disc_hint_from_name(Path::new("Halo 3 (Disc 2).iso")),
            Some(2)
        );
        assert_eq!(
            disc_hint_from_name(Path::new("Game (Disk 1).ISO")),
            Some(1)
        );
        assert_eq!(disc_hint_from_name(Path::new("RPG-disc2.iso")), Some(2));
        assert_eq!(disc_hint_from_name(Path::new("Halo 3.iso")), None);
        assert_eq!(disc_hint_from_name(Path::new("Coded Arms.iso")), None);
        assert_eq!(
            stem_without_disc(Path::new("Halo 3 (USA) (Disc 1).iso")),
            stem_without_disc(Path::new("Halo 3 (USA) (Disc 2).iso"))
        );
    }

    #[test]
    fn merge_keeps_disc1_executable() {
        let root = std::env::temp_dir().join(format!("unpack-merge-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let xex = root.join("default.xex");
        let data = root.join("disc2.dat");
        fs::write(&xex, b"disc1-xex").unwrap();
        assert!(!merge_replace_allowed(&xex));
        assert!(merge_replace_allowed(&data));
        fs::write(&data, b"old").unwrap();
        assert!(merge_replace_allowed(&data));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn groups_same_title_by_disc_order() {
        let a = PathBuf::from("Game (Disc 2).iso");
        let b = PathBuf::from("Game (Disc 1).iso");
        assert!(disc_hint_from_name(&a).unwrap() > disc_hint_from_name(&b).unwrap());
        assert_eq!(stem_without_disc(&a), stem_without_disc(&b));
    }
}
