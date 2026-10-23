mod archive;
mod commands;
mod convert;
mod discord;
mod download;
mod ftp;
mod hashutil;
mod library;
mod net;
mod paths;
mod progress;
mod stfs;
mod usb;
mod xbox;

use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .setup(|app| {
            app.manage(discord::PresenceHandle::start());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::app_meta,
            commands::load_settings,
            commands::save_settings,
            commands::pick_files,
            commands::pick_folder,
            commands::pick_save,
            commands::notify,
            commands::open_path,
            commands::extract_archives,
            commands::create_archive,
            commands::inspect_iso,
            commands::convert_iso,
            commands::search_titles,
            commands::lookup_title,
            commands::scan_library,
            commands::parse_package,
            commands::place_content,
            commands::volume_info,
            commands::copy_to_usb,
            commands::hash_file,
            commands::download_url,
            commands::discover_xbox,
            commands::send_to_xbox,
            commands::xbox_test,
            commands::list_dir,
            commands::discord_set_activity,
            commands::discord_set_enabled,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Unpack");
}
