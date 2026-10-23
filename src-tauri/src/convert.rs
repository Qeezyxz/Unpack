use crate::fileops;
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

fn catalog_name(title_id: u32) -> Option<String> {
    game_list::find_title_by_id(title_id)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn display_name_for_title(title_id: u32) -> String {
    catalog_name(title_id).unwrap_or_else(|| format!("{title_id:08X}"))
}

/// Inspect may still show the hex ID. Convert must not write a folder for an
/// ID that is not in the TitleID list.
fn require_known_title(title_id: u32) -> Result<String, String> {
    catalog_name(title_id).ok_or_else(|| {
        format!("Title ID {title_id:08X} not recognized. Conversion stopped.")
    })
}

fn missing_executable_message(source: &Path) -> String {
    let name = file_name(source);
    match disc_hint_from_name(source) {
        Some(n) if n > 1 => format!(
            "{name} is Disc {n} and has no default.xex. Convert Disc 1 first, or pick both discs together."
        ),
        _ => format!("{name} has no default.xex or default.xbe, so it does not look like a game disc."),
    }
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
        if let Some(tid) = discs
            .iter()
            .find_map(|(_, info)| info.as_ref().map(|i| i.title_id_num))
        {
            if let Err(err) = require_known_title(tid) {
                progress(err.clone(), gi as u64, game_total.max(1));
                return Err(err);
            }
        }
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

fn disc_number_of(path: &Path, info: Option<&IsoInfo>) -> u8 {
    info.map(|i| i.disc_number)
        .or_else(|| disc_hint_from_name(path))
        .filter(|n| *n > 0)
        .unwrap_or(1)
}

fn group_discs(paths: &[PathBuf]) -> Result<Vec<Vec<(PathBuf, Option<IsoInfo>)>>, String> {
    let items: Vec<(PathBuf, Option<IsoInfo>)> = paths
        .iter()
        .map(|path| (path.clone(), inspect_iso(path).ok()))
        .collect();
    let groups = group_disc_items(items);
    if groups.is_empty() {
        return Err("no ISOs to convert".into());
    }
    Ok(groups)
}

/// Disc 2 dumps without default.xex still join disc 1 when the filename stem matches.
fn group_disc_items(
    items: Vec<(PathBuf, Option<IsoInfo>)>,
) -> Vec<Vec<(PathBuf, Option<IsoInfo>)>> {
    let mut keyed: BTreeMap<String, Vec<(PathBuf, Option<IsoInfo>)>> = BTreeMap::new();
    let mut ungrouped = Vec::new();
    for (path, info) in items {
        if let Some(ref iso) = info {
            keyed
                .entry(format!("id:{}", iso.title_id))
                .or_default()
                .push((path, info));
        } else {
            ungrouped.push((path, info));
        }
    }
    for (path, info) in ungrouped {
        let stem = stem_without_disc(&path);
        let mut placed = false;
        if !stem.is_empty() {
            for group in keyed.values_mut() {
                if group
                    .iter()
                    .any(|(p, _)| stem_without_disc(p) == stem)
                {
                    group.push((path.clone(), info.clone()));
                    placed = true;
                    break;
                }
            }
        }
        if !placed {
            keyed
                .entry(format!("stem:{}", stem_without_disc(&path)))
                .or_default()
                .push((path, info));
        }
    }
    let mut groups: Vec<_> = keyed.into_values().collect();
    for group in &mut groups {
        group.sort_by_key(|(path, info)| {
            (disc_number_of(path, info.as_ref()), path.display().to_string())
        });
    }
    groups.sort_by_key(|g| g.first().map(|(p, _)| p.display().to_string()).unwrap_or_default());
    groups
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
    let out = dest_dir.join(&folder_name);
    fs::create_dir_all(dest_dir).map_err(|e| e.to_string())?;
    // New games extract into a staging folder, then rename over `out`.
    // An existing folder is merged in place so disc 2 keeps disc 1's default.xex.
    let staging = if out.exists() {
        None
    } else {
        Some(fileops::Staging::new(
            dest_dir.join(format!(".{folder_name}.unpack-tmp")),
        )?)
    };
    let target = staging
        .as_ref()
        .map(|s| s.path().to_path_buf())
        .unwrap_or_else(|| out.clone());
    let mut playable_exists =
        target.join("default.xex").is_file() || target.join("default.xbe").is_file();
    let mut files = Vec::new();
    let mut infos = Vec::new();
    let disc_total = discs.len() as u64;
    for (i, (path, preview)) in discs.iter().enumerate() {
        let n = disc_number_of(path, preview.as_ref());
        let merge = i > 0 || (n > 1 && playable_exists);
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
        let (info, part_files) = extract_iso_into(path, &target, merge, preview.as_ref(), progress)?;
        files.extend(part_files);
        if let Some(info) = info {
            infos.push(info);
        }
        playable_exists =
            target.join("default.xex").is_file() || target.join("default.xbe").is_file();
    }
    if let Some(staging) = staging {
        let stage_path = staging.path().to_path_buf();
        fileops::replace_dir(&stage_path, &out)?;
        for f in &mut files {
            let p = PathBuf::from(&*f);
            if let Ok(rel) = p.strip_prefix(&stage_path) {
                *f = out.join(rel).display().to_string();
            }
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
    require_known_title(exe_info.title_id)?;
    let content_type = title_info.content_type;
    let info = iso_info(source, &exe_info, content_type, source_iso.get_max_used_prefix_size());

    let data_size = if trim {
        source_iso.get_max_used_prefix_size()
    } else {
        source_iso_file_meta
            .len()
            .saturating_sub(source_iso.volume_descriptor.root_offset)
    };
    let block_count = data_size.div_ceil(god::BLOCK_SIZE as u64);
    let part_count = block_count.div_ceil(god::BLOCKS_PER_PART).max(1);
    let root_offset = source_iso.volume_descriptor.root_offset;
    drop(source_iso);

    fs::create_dir_all(dest_dir).map_err(|e| e.to_string())?;
    let dest_dir = dest_dir.to_path_buf();
    // Write into a staging tree, then swap over the live TitleID/00007000/MediaID
    // layout so an interrupted convert cannot wipe a previous good GOD package.
    let staging = fileops::Staging::new(dest_dir.join(format!(
        ".{:08X}-{:08X}.unpack-tmp",
        exe_info.title_id, exe_info.media_id
    )))?;
    let file_layout = god::FileLayout::new(staging.path(), &exe_info, content_type);
    fs::create_dir_all(file_layout.data_dir_path()).map_err(|e| e.to_string())?;

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
    let staging_header = file_layout.con_header_file_path();
    if let Some(parent) = staging_header.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    File::options()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&staging_header)
        .map_err(|e| e.to_string())?
        .write_all(&con_header)
        .map_err(|e| e.to_string())?;

    let final_layout = god::FileLayout::new(&dest_dir, &exe_info, content_type);
    install_god_package(
        &file_layout.data_dir_path(),
        &staging_header,
        &final_layout.data_dir_path(),
        &final_layout.con_header_file_path(),
    )?;
    let header_path = final_layout.con_header_file_path();

    let mut files = vec![header_path.display().to_string()];
    for i in 0..part_count {
        files.push(final_layout.part_file_path(i).display().to_string());
    }
    if verify {
        if !header_path.is_file() || fs::metadata(&header_path).map(|m| m.len()).unwrap_or(0) == 0 {
            return Err("verification failed: missing CON header".into());
        }
        for i in 0..part_count {
            let p = final_layout.part_file_path(i);
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
        return Err(missing_executable_message(source));
    }
    if let Some(t) = &title_info {
        require_known_title(t.execution_info.title_id)?;
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

/// Characters that would escape the folder or that Windows cannot store.
fn is_unsafe_name_char(c: char) -> bool {
    matches!(c, '/' | '\\' | '\0' | ':' | '<' | '>' | '"' | '|' | '?' | '*')
}

fn truncated_message(name: &str, got: u64, expected: u64) -> String {
    format!(
        "The ISO ends early: {name} should be {expected} bytes but only {got} were there. The file is incomplete or damaged, so nothing was kept for it."
    )
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
            .map(|c| if is_unsafe_name_char(c) { '_' } else { c })
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
            let tmp = fileops::sibling(&dest_path, "unpack-tmp");
            fileops::clear_path(&tmp)?;
            {
                let mut out = File::create(&tmp).map_err(|e| e.to_string())?;
                let mut limited = (&mut reader).take(entry.size as u64);
                let copied_bytes =
                    std::io::copy(&mut limited, &mut out).map_err(|e| e.to_string())?;
                out.flush().map_err(|e| e.to_string())?;
                if copied_bytes != entry.size as u64 {
                    drop(out);
                    let _ = fs::remove_file(&tmp);
                    return Err(truncated_message(&safe, copied_bytes, entry.size as u64));
                }
            }
            fileops::replace_file(&tmp, &dest_path)?;
            files.push(dest_path.display().to_string());
            *copied += 1;
            progress(format!("Extracted {safe}"), *copied, total);
        }
    }
    Ok(())
}

/// Move a finished GOD package from staging onto `{TitleID}/00007000/{MediaID}`.
/// The live header is not touched until the `.data` folder is in place; if the
/// header rename fails, both the new data and the previous package are restored.
fn install_god_package(
    new_data: &Path,
    new_hdr: &Path,
    dest_data: &Path,
    dest_hdr: &Path,
) -> Result<(), String> {
    if let Some(parent) = dest_hdr.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let data_bak = take_aside(dest_data)?;
    if let Err(e) = fs::rename(new_data, dest_data) {
        put_back(data_bak, dest_data);
        return Err(format!("install GOD data {}: {e}", dest_data.display()));
    }
    let hdr_bak = take_aside(dest_hdr)?;
    if let Err(e) = fs::rename(new_hdr, dest_hdr).or_else(|_| {
        fs::copy(new_hdr, dest_hdr).map(|_| ())?;
        fs::remove_file(new_hdr)
    }) {
        put_back(hdr_bak, dest_hdr);
        let _ = fs::rename(dest_data, new_data);
        put_back(data_bak, dest_data);
        return Err(format!("install GOD header {}: {e}", dest_hdr.display()));
    }
    if let Some(p) = data_bak {
        let _ = fileops::clear_path(&p);
    }
    if let Some(p) = hdr_bak {
        let _ = fileops::clear_path(&p);
    }
    Ok(())
}

fn take_aside(path: &Path) -> Result<Option<PathBuf>, String> {
    if !path.exists() {
        return Ok(None);
    }
    let bak = fileops::sibling(path, "unpack-old");
    fileops::clear_path(&bak)?;
    fs::rename(path, &bak).map_err(|e| format!("set aside {}: {e}", path.display()))?;
    Ok(Some(bak))
}

fn put_back(bak: Option<PathBuf>, dest: &Path) {
    if dest.exists() {
        let _ = fileops::clear_path(dest);
    }
    if let Some(bak) = bak {
        let _ = fs::rename(&bak, dest);
    }
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
    fn unknown_title_logs_hex_and_stops() {
        let err = require_known_title(0xDEADBEEF).unwrap_err();
        assert!(err.contains("DEADBEEF"), "{err}");
        assert!(err.to_lowercase().contains("not recognized"), "{err}");
        assert!(err.to_lowercase().contains("stopped"), "{err}");
        let known = display_name_for_title(0x4D5307E6);
        assert!(known.to_lowercase().contains("halo"));
        assert_eq!(display_name_for_title(0xDEADBEEF), "DEADBEEF");
    }

    #[test]
    fn unknown_title_convert_writes_no_folder() {
        let root = std::env::temp_dir().join(format!(
            "unpack-unknown-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let iso = root.join("Mystery.iso");
        let dest = root.join("out");
        fs::create_dir_all(&dest).unwrap();
        write_xsf_iso(
            &iso,
            &[(
                "default.xex",
                minimal_xex(0xDEADBEEF, 1, 1, 0x11111111, b"UNKNXEX"),
            )],
        );

        let inspected = inspect_iso(&iso).expect("inspect should still return the hex Title ID");
        assert_eq!(inspected.title_id, "DEADBEEF");
        assert_eq!(inspected.name, "DEADBEEF");

        let mut logged = Vec::new();
        let err = convert_isos(
            &[iso.clone()],
            &dest,
            "xex",
            false,
            true,
            false,
            |message, _, _| logged.push(message),
        )
        .unwrap_err();
        assert!(err.contains("DEADBEEF"), "{err}");
        assert!(err.to_lowercase().contains("not recognized"), "{err}");
        assert!(err.to_lowercase().contains("stopped"), "{err}");
        assert!(
            logged.iter().any(|m| m.contains("DEADBEEF")),
            "progress log should still include the hex Title ID, got {logged:?}"
        );

        let names: Vec<String> = fs::read_dir(&dest)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            names.is_empty(),
            "convert must not write a folder for an unknown Title ID, got {names:?}"
        );
        assert!(!dest.join("DEADBEEF").exists());

        let god_err = convert_isos(&[iso], &dest, "god", true, true, false, |_, _, _| {})
            .unwrap_err();
        assert!(god_err.contains("DEADBEEF"), "{god_err}");
        assert!(
            fs::read_dir(&dest).unwrap().next().is_none(),
            "GOD convert must not write a TitleID folder"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn unsafe_names_are_flattened() {
        for c in ['/', '\\', ':', '<', '>', '"', '|', '?', '*', '\0'] {
            assert!(is_unsafe_name_char(c), "{c:?}");
        }
        assert!(!is_unsafe_name_char('.'));
        assert!(!is_unsafe_name_char('a'));
    }

    #[test]
    fn truncated_iso_stops_instead_of_saving_a_short_file() {
        let root = std::env::temp_dir().join(format!(
            "unpack-truncated-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let iso = root.join("Sample.iso");
        let dest = root.join("out");
        fs::create_dir_all(&dest).unwrap();
        let title = 0x4D5307E6;
        write_xsf_iso(
            &iso,
            &[
                ("default.xex", minimal_xex(title, 1, 1, 0x11111111, b"XEX")),
                ("big.bin", vec![7u8; 10_000]),
            ],
        );
        let full = fs::metadata(&iso).unwrap().len();
        let file = fs::OpenOptions::new().write(true).open(&iso).unwrap();
        file.set_len(full - 6_000).unwrap();
        drop(file);

        let err = convert_isos(&[iso], &dest, "xex", false, true, false, |_, _, _| {})
            .unwrap_err();
        assert!(err.contains("ends early") && err.contains("big.bin"), "{err}");
        assert!(
            fs::read_dir(&dest).unwrap().next().is_none(),
            "a damaged ISO must not leave a half-extracted folder"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn lone_disc_two_gets_a_plain_message() {
        let msg = missing_executable_message(Path::new("Halo 3 (Disc 2).iso"));
        assert!(msg.contains("Disc 2"), "{msg}");
        assert!(msg.contains("Convert Disc 1 first"), "{msg}");
        let other = missing_executable_message(Path::new("Photos.iso"));
        assert!(other.contains("does not look like a game disc"), "{other}");
        assert!(!other.contains("Disc"), "{other}");
    }

    #[test]
    fn lone_disc_two_without_xex_stops_and_writes_nothing() {
        let root = std::env::temp_dir().join(format!(
            "unpack-lone-disc2-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let disc2 = root.join("Sample (Disc 2).iso");
        let dest = root.join("out");
        fs::create_dir_all(&dest).unwrap();
        write_xsf_iso(&disc2, &[("extra.bin", b"only-on-disc-2".to_vec())]);

        let err = convert_isos(&[disc2], &dest, "xex", false, true, false, |_, _, _| {})
            .unwrap_err();
        assert!(err.contains("Disc 2"), "{err}");
        assert!(err.contains("Convert Disc 1 first"), "{err}");
        assert!(
            fs::read_dir(&dest).unwrap().next().is_none(),
            "a lone disc 2 must not leave a folder behind"
        );
        let _ = fs::remove_dir_all(&root);
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

    #[test]
    fn groups_unreadable_disc2_with_matching_stem() {
        let disc1 = IsoInfo {
            path: "Game (Disc 1).iso".into(),
            title_id: "4D5307E6".into(),
            title_id_num: 0x4D5307E6,
            name: "Halo 3".into(),
            content_kind: "Games on Demand".into(),
            disc_number: 1,
            disc_count: 2,
            media_id: "11111111".into(),
            used_bytes: 1,
        };
        let groups = group_disc_items(vec![
            (PathBuf::from("Game (Disc 1).iso"), Some(disc1)),
            (PathBuf::from("Game (Disc 2).iso"), None),
        ]);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].len(), 2);
        assert!(groups[0][0].0.to_string_lossy().contains("Disc 1"));
        assert!(groups[0][1].0.to_string_lossy().contains("Disc 2"));
    }

    fn minimal_xex(title_id: u32, disc_number: u8, disc_count: u8, media_id: u32, tag: &[u8]) -> Vec<u8> {
        let mut xex = Vec::new();
        xex.extend(b"XEX2");
        xex.extend(0u32.to_be_bytes());
        xex.extend(0u32.to_be_bytes());
        xex.extend(0u32.to_be_bytes());
        xex.extend(0u32.to_be_bytes());
        xex.extend(1u32.to_be_bytes());
        xex.extend(0x0004_0006u32.to_be_bytes());
        xex.extend(32u32.to_be_bytes());
        xex.extend(media_id.to_be_bytes());
        xex.extend(0u32.to_be_bytes());
        xex.extend(0u32.to_be_bytes());
        xex.extend(title_id.to_be_bytes());
        xex.push(0);
        xex.push(0);
        xex.push(disc_number);
        xex.push(disc_count);
        xex.extend(tag);
        xex
    }

    fn dir_entry(name: &str, sector: u32, size: u32) -> Vec<u8> {
        let mut entry = Vec::new();
        entry.extend(0u16.to_le_bytes());
        entry.extend(0u16.to_le_bytes());
        entry.extend(sector.to_le_bytes());
        entry.extend(size.to_le_bytes());
        entry.push(0x80);
        entry.push(name.len() as u8);
        entry.extend(name.as_bytes());
        while entry.len() % 4 != 0 {
            entry.push(0);
        }
        entry
    }

    fn write_xsf_iso(path: &Path, files: &[(&str, Vec<u8>)]) {
        const SECTOR: usize = 0x800;
        let mut next_sector = 0x22u32;
        let mut layout = Vec::new();
        for (name, data) in files {
            layout.push((*name, next_sector, data.clone()));
            next_sector += (data.len() as u32).div_ceil(SECTOR as u32).max(1);
        }
        let mut dir = Vec::new();
        for (name, sector, data) in &layout {
            dir.extend(dir_entry(name, *sector, data.len() as u32));
        }
        dir.extend(0xffffu16.to_le_bytes());
        dir.extend(0xffffu16.to_le_bytes());
        let mut iso = vec![0u8; (next_sector as usize) * SECTOR];
        let vol = 0x20 * SECTOR;
        iso[vol..vol + 20].copy_from_slice(b"MICROSOFT*XBOX*MEDIA");
        iso[vol + 20..vol + 24].copy_from_slice(&0x21u32.to_le_bytes());
        iso[vol + 24..vol + 28].copy_from_slice(&(SECTOR as u32).to_le_bytes());
        let dir_off = 0x21 * SECTOR;
        iso[dir_off..dir_off + dir.len()].copy_from_slice(&dir);
        for (_name, sector, data) in &layout {
            let off = (*sector as usize) * SECTOR;
            iso[off..off + data.len()].copy_from_slice(data);
        }
        fs::write(path, iso).unwrap();
    }

    #[test]
    fn convert_merges_disc1_and_disc2_into_one_playable_folder() {
        let root = std::env::temp_dir().join(format!("unpack-multidisc-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let disc1 = root.join("Sample (Disc 1).iso");
        let disc2 = root.join("Sample (Disc 2).iso");
        let dest = root.join("out");
        let title = 0x4D5307E6;
        let xex1 = minimal_xex(title, 1, 2, 0x11111111, b"DISC1XEX");
        let xex2 = minimal_xex(title, 2, 2, 0x22222222, b"DISC2XEX");
        write_xsf_iso(
            &disc1,
            &[
                ("default.xex", xex1.clone()),
                ("shared.dat", b"from-disc-1".to_vec()),
            ],
        );
        write_xsf_iso(
            &disc2,
            &[
                ("default.xex", xex2),
                ("shared.dat", b"from-disc-2".to_vec()),
                ("extra.bin", b"only-on-disc-2".to_vec()),
            ],
        );

        let reports = convert_isos(
            &[disc1.clone(), disc2.clone()],
            &dest,
            "xex",
            false,
            true,
            false,
            |_, _, _| {},
        )
        .unwrap();
        assert_eq!(reports.len(), 1);
        assert!(reports[0].merged);
        assert_eq!(reports[0].discs.len(), 2);
        let folder = PathBuf::from(&reports[0].output);
        let xex = fs::read(folder.join("default.xex")).unwrap();
        assert!(xex.windows(b"DISC1XEX".len()).any(|w| w == b"DISC1XEX"));
        assert!(!xex.windows(b"DISC2XEX".len()).any(|w| w == b"DISC2XEX"));
        assert_eq!(fs::read(folder.join("extra.bin")).unwrap(), b"only-on-disc-2");
        assert_eq!(fs::read(folder.join("shared.dat")).unwrap(), b"from-disc-2");
        assert!(reports[0].info.name.to_lowercase().contains("halo"));

        let found = convert_isos(&[disc1.clone()], &dest, "xex", false, true, true, |_, _, _| {}).unwrap();
        assert_eq!(found.len(), 1);
        assert!(found[0].merged, "auto companion scan should pick up Disc 2");

        let god_dest = root.join("god");
        let god = convert_isos(
            &[disc1, disc2],
            &god_dest,
            "god",
            true,
            true,
            false,
            |_, _, _| {},
        )
        .unwrap();
        assert_eq!(god.len(), 1);
        assert!(god[0].merged);
        let title_dir = god_dest.join("4D5307E6").join("00007000");
        assert!(title_dir.is_dir(), "GOD discs should share one TitleID folder");
        let media: Vec<String> = fs::read_dir(&title_dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            media.iter().any(|n| n == "11111111") && media.iter().any(|n| n == "22222222"),
            "expected disc 1 and disc 2 GOD packages, got {media:?}"
        );

        let _ = fs::remove_dir_all(&root);
    }
}
