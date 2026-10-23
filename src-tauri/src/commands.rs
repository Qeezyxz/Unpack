use crate::{archive, convert, discord, download, fileops, hashutil, library, progress, stfs, usb, xbox};
use serde::Serialize;
use std::path::PathBuf;
use tauri::{AppHandle, Manager};
use tauri_plugin_dialog::{DialogExt, FilePath};
use tauri_plugin_notification::NotificationExt;
use tauri_plugin_opener::OpenerExt;

fn dialog_path(p: FilePath) -> Option<String> {
    p.into_path()
        .ok()
        .map(|pb| pb.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
}

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
    let body = serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?;
    fileops::write_atomic(&path, body.as_bytes())?;
    if let Some(enabled) = settings.get("discordPresence").and_then(|v| v.as_bool()) {
        if let Some(presence) = app.try_state::<discord::PresenceHandle>() {
            presence.set_enabled(enabled);
        }
    }
    Ok(())
}

#[tauri::command]
pub fn discord_set_activity(app: AppHandle, view: String, job: Option<String>) -> Result<(), String> {
    let text = discord::activity_for(&view, job.as_deref());
    if let Some(presence) = app.try_state::<discord::PresenceHandle>() {
        presence.update(&text.details, &text.state);
    }
    Ok(())
}

#[tauri::command]
pub fn discord_set_enabled(app: AppHandle, enabled: bool) -> Result<(), String> {
    if let Some(presence) = app.try_state::<discord::PresenceHandle>() {
        presence.set_enabled(enabled);
    }
    Ok(())
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
        .filter_map(dialog_path)
        .collect())
}

#[tauri::command]
pub fn pick_folder(app: AppHandle, title: Option<String>) -> Result<Option<String>, String> {
    let mut dialog = app.dialog().file();
    if let Some(title) = title {
        dialog = dialog.set_title(&title);
    }
    Ok(dialog.blocking_pick_folder().and_then(dialog_path))
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
    Ok(dialog.blocking_save_file().and_then(dialog_path))
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
    if path.starts_with("http://") || path.starts_with("https://") {
        app.opener()
            .open_url(path, None::<&str>)
            .map_err(|e| e.to_string())
    } else {
        app.opener()
            .open_path(path, None::<&str>)
            .map_err(|e| e.to_string())
    }
}

#[tauri::command]
pub fn xbox_test(host: String, port: u16, user: String, pass: String) -> Result<String, String> {
    xbox::test_login(&host, port, &user, &pass)
}

#[tauri::command]
pub async fn extract_archives(
    app: AppHandle,
    archives: Vec<String>,
    dest: String,
    skip_junk: Option<bool>,
    existing: Option<String>,
    nested: Option<bool>,
) -> Result<Vec<archive::ArchiveResult>, String> {
    if dest.trim().is_empty() {
        return Err("choose a destination folder".into());
    }
    progress::emit(&app, "extract", "Extracting archives", 0, archives.len() as u64);
    let app_job = app.clone();
    let opts = archive::ExtractOptions {
        skip_junk: skip_junk.unwrap_or(true),
        existing: existing.unwrap_or_else(|| "number".into()),
        nested: nested.unwrap_or(true),
    };
    let result = tauri::async_runtime::spawn_blocking(move || {
        let paths: Vec<PathBuf> = archives.into_iter().map(PathBuf::from).collect();
        archive::extract_archives_with(
            &paths,
            &PathBuf::from(dest),
            &opts,
            Some(&mut |message, current, total| {
                progress::emit(&app_job, "extract", message, current, total);
            }),
        )
    })
    .await
    .map_err(|e| e.to_string())??;
    progress::emit(&app, "extract", "Done", 1, 1);
    Ok(result)
}

#[tauri::command]
pub async fn create_archive(inputs: Vec<String>, dest: String) -> Result<archive::ArchiveResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let paths: Vec<PathBuf> = inputs.into_iter().map(PathBuf::from).collect();
        archive::create_archive(&paths, &PathBuf::from(dest))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn inspect_iso(path: String) -> Result<convert::IsoInfo, String> {
    tauri::async_runtime::spawn_blocking(move || convert::inspect_iso(&PathBuf::from(path)))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn convert_iso(
    app: AppHandle,
    path: Option<String>,
    paths: Option<Vec<String>>,
    dest: String,
    mode: String,
    trim: bool,
    verify: bool,
    auto_companions: Option<bool>,
) -> Result<Vec<convert::ConvertReport>, String> {
    let mut sources: Vec<String> = paths.unwrap_or_default();
    if let Some(path) = path {
        if !path.trim().is_empty() && !sources.iter().any(|p| p == &path) {
            sources.insert(0, path);
        }
    }
    sources.retain(|p| !p.trim().is_empty());
    if sources.is_empty() || dest.trim().is_empty() {
        return Err("choose an ISO and an output folder".into());
    }
    let app2 = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let bufs: Vec<PathBuf> = sources.into_iter().map(PathBuf::from).collect();
        convert::convert_isos(
            &bufs,
            &PathBuf::from(dest),
            &mode,
            trim,
            verify,
            auto_companions.unwrap_or(true),
            move |message, current, total| progress::emit(&app2, "convert", message, current, total),
        )
    })
    .await
    .map_err(|e| e.to_string())?
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
pub async fn scan_library(root: String) -> Result<Vec<library::LibraryItem>, String> {
    if root.trim().is_empty() {
        return Err("choose a library folder".into());
    }
    tauri::async_runtime::spawn_blocking(move || library::scan_library(&PathBuf::from(root), 4))
        .await
        .map_err(|e| e.to_string())?
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
pub async fn copy_to_usb(
    sources: Vec<String>,
    dest: String,
    verify: bool,
    overwrite_protected: bool,
) -> Result<usb::CopyReport, String> {
    if dest.trim().is_empty() {
        return Err("choose a USB / destination folder".into());
    }
    if sources.is_empty() {
        return Err("no files selected".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        let dest = PathBuf::from(dest);
        let mut report = usb::CopyReport {
            copied: 0,
            skipped_protected: 0,
            bytes: 0,
            verified: verify,
            warnings: Vec::new(),
        };
        for src in sources {
            let one = usb::copy_tree(&PathBuf::from(src), &dest, verify, overwrite_protected)?;
            usb::merge_reports(&mut report, one);
        }
        Ok(report)
    })
    .await
    .map_err(|e| e.to_string())?
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
pub async fn download_url(
    app: AppHandle,
    url: String,
    dest_dir: String,
    expected_hash: Option<String>,
) -> Result<download::DownloadReport, String> {
    if dest_dir.trim().is_empty() {
        return Err("choose a download folder".into());
    }
    let dest = download::dest_for_url(&PathBuf::from(dest_dir), &url);
    let app2 = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        download::download_url(
            &url,
            &dest,
            expected_hash.as_deref(),
            move |message, current, total| progress::emit(&app2, "download", message, current, total),
        )
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn discover_xbox(port: Option<u16>) -> Vec<xbox::XboxHost> {
    tauri::async_runtime::spawn_blocking(move || xbox::discover(port.unwrap_or(21), 180))
        .await
        .unwrap_or_default()
}

#[tauri::command]
pub async fn send_to_xbox(
    app: AppHandle,
    host: String,
    port: Option<u16>,
    user: String,
    pass: String,
    dest: String,
    paths: Vec<String>,
    include_launch_ini: bool,
) -> Result<xbox::SendReport, String> {
    if paths.is_empty() {
        return Err("choose files or folders to send".into());
    }
    let app2 = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
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
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn list_dir(path: String) -> Result<Vec<String>, String> {
    library::list_dir(&PathBuf::from(path))
}
