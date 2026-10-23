use crate::{archive, convert, download, hashutil, library, progress, stfs, usb, xbox};
use serde::Serialize;
use std::path::PathBuf;
use tauri::{AppHandle, Manager};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_notification::NotificationExt;
use tauri_plugin_opener::OpenerExt;

#[derive(Debug, Serialize)]
pub struct AppMeta {
    pub name: String,
    pub version: String,
    pub title_count: usize,
    pub seven_zip: Option<String>,
    pub config_dir: String,
}

fn settings_path(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_config_dir()
        .map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir.join("settings.json"))
}

#[tauri::command]
pub fn app_meta(app: AppHandle) -> Result<AppMeta, String> {
    let config_dir = app
        .path()
        .app_config_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    Ok(AppMeta {
        name: "Unpack".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        title_count: library::title_count(),
        seven_zip: archive::find_seven_zip().map(|p| p.display().to_string()),
        config_dir,
    })
}

#[tauri::command]
pub fn load_settings(app: AppHandle) -> Result<serde_json::Value, String> {
    let path = settings_path(&app)?;
    if !path.exists() {
        return Ok(serde_json::json!({}));
    }
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    serde_json::from_str(&text).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn save_settings(app: AppHandle, settings: serde_json::Value) -> Result<(), String> {
    let path = settings_path(&app)?;
    std::fs::write(path, serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn pick_files(app: AppHandle, title: Option<String>, extensions: Option<Vec<String>>) -> Result<Vec<String>, String> {
    let mut dialog = app.dialog().file();
    if let Some(title) = title {
        dialog = dialog.set_title(&title);
    }
    if let Some(exts) = extensions {
        let refs: Vec<&str> = exts.iter().map(|s| s.as_str()).collect();
        dialog = dialog.add_filter("Files", &refs);
    }
    Ok(dialog
        .blocking_pick_files()
        .unwrap_or_default()
        .into_iter()
        .map(|p| p.to_string())
        .collect())
}

#[tauri::command]
pub fn pick_folder(app: AppHandle, title: Option<String>) -> Result<Option<String>, String> {
    let mut dialog = app.dialog().file();
    if let Some(title) = title {
        dialog = dialog.set_title(&title);
    }
    Ok(dialog.blocking_pick_folder().map(|p| p.to_string()))
}

#[tauri::command]
pub fn pick_save(app: AppHandle, title: Option<String>, default_name: Option<String>) -> Result<Option<String>, String> {
    let mut dialog = app.dialog().file();
    if let Some(title) = title {
        dialog = dialog.set_title(&title);
    }
    if let Some(name) = default_name {
        dialog = dialog.set_file_name(&name);
    }
    Ok(dialog.blocking_save_file().map(|p| p.to_string()))
}

#[tauri::command]
pub fn notify(app: AppHandle, title: String, body: String) -> Result<(), String> {
    app.notification()
        .builder()
        .title(title)
        .body(body)
        .show()
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn open_path(app: AppHandle, path: String) -> Result<(), String> {
    app.opener()
        .open_path(path, None::<&str>)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn extract_archives(app: AppHandle, archives: Vec<String>, dest: String) -> Result<Vec<archive::ArchiveResult>, String> {
    progress::emit(&app, "extract", "Extracting archives", 0, archives.len() as u64);
    let paths: Vec<PathBuf> = archives.into_iter().map(PathBuf::from).collect();
    let result = archive::extract_archives(&paths, &PathBuf::from(dest))?;
    progress::emit(&app, "extract", "Done", 1, 1);
    Ok(result)
}

#[tauri::command]
pub fn create_archive(inputs: Vec<String>, dest: String) -> Result<archive::ArchiveResult, String> {
    let paths: Vec<PathBuf> = inputs.into_iter().map(PathBuf::from).collect();
    archive::create_archive(&paths, &PathBuf::from(dest))
}

#[tauri::command]
pub fn inspect_iso(path: String) -> Result<convert::IsoInfo, String> {
    convert::inspect_iso(&PathBuf::from(path))
}

#[tauri::command]
pub fn convert_iso(
    app: AppHandle,
    path: String,
    dest: String,
    mode: String,
    trim: bool,
    verify: bool,
) -> Result<convert::ConvertReport, String> {
    let app2 = app.clone();
    convert::convert_iso(
        &PathBuf::from(path),
        &PathBuf::from(dest),
        &mode,
        trim,
        verify,
        move |message, current, total| progress::emit(&app2, "convert", message, current, total),
    )
}

#[tauri::command]
pub fn search_titles(query: String, limit: Option<usize>) -> Vec<library::TitleRecord> {
    library::search_titles(&query, limit.unwrap_or(80))
}

#[tauri::command]
pub fn lookup_title(title_id: String) -> Option<library::TitleRecord> {
    library::lookup_title(&title_id)
}

#[tauri::command]
pub fn scan_library(root: String) -> Result<Vec<library::LibraryItem>, String> {
    library::scan_library(&PathBuf::from(root), 4)
}

#[tauri::command]
pub fn parse_package(path: String) -> Result<stfs::StfsInfo, String> {
    stfs::parse_stfs_file(&PathBuf::from(path))
}

#[tauri::command]
pub fn place_content(path: String, library_root: String) -> Result<library::LibraryItem, String> {
    library::place_content(&PathBuf::from(path), &PathBuf::from(library_root))
}

#[tauri::command]
pub fn volume_info(path: String) -> Result<usb::VolumeInfo, String> {
    usb::filesystem_for(&PathBuf::from(path))
}

#[tauri::command]
pub fn copy_to_usb(
    sources: Vec<String>,
    dest: String,
    verify: bool,
    overwrite_protected: bool,
) -> Result<usb::CopyReport, String> {
    let dest = PathBuf::from(dest);
    let mut last = None;
    for src in sources {
        last = Some(usb::copy_tree(
            &PathBuf::from(src),
            &dest,
            verify,
            overwrite_protected,
        )?);
    }
    last.ok_or_else(|| "no files selected".to_string())
}

#[tauri::command]
pub fn hash_file(path: String) -> Result<serde_json::Value, String> {
    let p = PathBuf::from(path);
    Ok(serde_json::json!({
        "sha1": hashutil::sha1_file(&p)?,
        "sha256": hashutil::sha256_file(&p)?,
    }))
}

#[tauri::command]
pub fn download_url(
    app: AppHandle,
    url: String,
    dest_dir: String,
    expected_hash: Option<String>,
) -> Result<download::DownloadReport, String> {
    let dest = download::dest_for_url(&PathBuf::from(dest_dir), &url);
    let app2 = app.clone();
    download::download_url(
        &url,
        &dest,
        expected_hash.as_deref(),
        move |message, current, total| progress::emit(&app2, "download", message, current, total),
    )
}

#[tauri::command]
pub fn discover_xbox(port: Option<u16>) -> Vec<xbox::XboxHost> {
    xbox::discover(port.unwrap_or(21), 180)
}

#[tauri::command]
pub fn send_to_xbox(
    app: AppHandle,
    host: String,
    port: Option<u16>,
    user: String,
    pass: String,
    dest: String,
    paths: Vec<String>,
    include_launch_ini: bool,
) -> Result<xbox::SendReport, String> {
    let app2 = app.clone();
    let files: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();
    xbox::send_paths(
        &host,
        port.unwrap_or(21),
        &user,
        &pass,
        &dest,
        &files,
        include_launch_ini,
        move |message, current, total| progress::emit(&app2, "xbox", message, current, total),
    )
}

#[tauri::command]
pub fn list_dir(path: String) -> Result<Vec<String>, String> {
    library::list_dir(&PathBuf::from(path))
}
