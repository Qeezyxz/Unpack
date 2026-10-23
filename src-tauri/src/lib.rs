pub mod archive;
pub mod god;
pub mod dlc;
pub mod library;
pub mod mods;
pub mod gamedb;
pub mod safecopy;
pub mod download;
pub mod iso_extract;
pub mod modnames;
pub mod ftp;
pub mod catalog;
pub mod discord;
pub mod destlock;
pub mod discs;
pub mod migrate;
#[cfg(test)]
pub mod testutil;

use std::path::{Path, PathBuf};

use archive::{Collision, ExtractOptions, Jobs, Layout};
use serde::Serialize;
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager, State};

/// What the app was asked to do from the command line / Explorer menu.
#[derive(Clone, Serialize, Debug, PartialEq)]
pub struct LaunchRequest {
    /// "open" | "extract-here" | "extract-folder"
    pub action: String,
    pub paths: Vec<String>,
}

/// Parse argv (without the exe name). Unknown switches are ignored; relative paths
/// are resolved against `cwd` (matters when a second instance forwards its args).
pub fn parse_args(args: &[String], cwd: &std::path::Path) -> Option<LaunchRequest> {
    let mut action = "open".to_string();
    let mut paths = Vec::new();
    let mut only_paths = false;
    for a in args {
        match a.as_str() {
            "--" if !only_paths => only_paths = true,
            "--extract-here" if !only_paths => action = "extract-here".into(),
            "--extract-folder" if !only_paths => action = "extract-folder".into(),
            s if s.starts_with('-') && !only_paths => {}
            s => {
                let p = PathBuf::from(s);
                let p = if p.is_absolute() { p } else { cwd.join(p) };
                if p.exists() {
                    paths.push(p.display().to_string());
                }
            }
        }
    }
    if paths.is_empty() { None } else { Some(LaunchRequest { action, paths }) }
}

#[derive(Default)]
struct PendingLaunch(Mutex<Vec<LaunchRequest>>);

/// The page calls this once it's ready; returns requests that arrived before that.
#[tauri::command]
fn take_launch_requests(pending: State<'_, PendingLaunch>) -> Vec<LaunchRequest> {
    std::mem::take(&mut *pending.0.lock().unwrap())
}

#[derive(Clone, Serialize)]
struct Progress {
    id: String,
    percent: u32,
    file: String,
}

fn seven_zip() -> Result<PathBuf, String> {
    archive::find_7za().ok_or_else(|| "The bundled 7-Zip (7za) is missing. Reinstall Unpack.".to_string())
}

fn opt_pw(p: &Option<String>) -> Option<&str> {
    p.as_deref().filter(|s| !s.is_empty())
}

#[derive(Serialize)]
struct NativeInfo {
    available: bool,
    path: String,
}

#[tauri::command]
fn native_info() -> NativeInfo {
    match archive::find_7za() {
        Some(p) => NativeInfo { available: true, path: p.display().to_string() },
        None => NativeInfo { available: false, path: String::new() },
    }
}

#[derive(Serialize)]
struct FileInfo {
    path: String,
    name: String,
    size: u64,
    is_dir: bool,
}

#[tauri::command]
fn file_info(path: String) -> Result<FileInfo, String> {
    let p = PathBuf::from(&path);
    let m = std::fs::metadata(&p).map_err(|e| format!("{path}: {e}"))?;
    Ok(FileInfo {
        name: p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        path,
        size: m.len(),
        is_dir: m.is_dir(),
    })
}

#[tauri::command]
async fn list_archive(path: String, password: Option<String>) -> Result<archive::Listing, String> {
    let exe = seven_zip()?;
    tauri::async_runtime::spawn_blocking(move || archive::list(&exe, &PathBuf::from(path), opt_pw(&password)))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn read_entry(path: String, entry: String, password: Option<String>) -> Result<tauri::ipc::Response, String> {
    let exe = seven_zip()?;
    let bytes = tauri::async_runtime::spawn_blocking(move || {
        archive::read_entry(&exe, &PathBuf::from(path), &entry, opt_pw(&password), 4 * 1024 * 1024)
    })
    .await
    .map_err(|e| e.to_string())??;
    Ok(tauri::ipc::Response::new(bytes))
}

#[allow(clippy::too_many_arguments)]
#[tauri::command]
async fn extract_archive(
    app: AppHandle,
    jobs: State<'_, Jobs>,
    grants: State<'_, safecopy::Grants>,
    id: String,
    path: String,
    dest: String,
    password: Option<String>,
    collision: String,
    layout: String,
    skip_junk: bool,
    seal_isos: Option<bool>,
    nested: Option<bool>,
 ) -> Result<archive::ExtractResult, String> {
    // Dest must be a picked folder, or (Extract beside) the archive's own parent.
    let dest_path = match grants.check(&dest) {
        Ok(d) => d,
        Err(e) => {
            let arch = PathBuf::from(&path);
            let parent = arch.parent().ok_or_else(|| e.clone())?;
            let (Ok(d), Ok(p)) = (std::fs::canonicalize(&dest), std::fs::canonicalize(parent)) else { return Err(e) };
            let d = safecopy::simplify(d);
            if d != safecopy::simplify(p) { return Err(e); }
            d
        }
    };
    let exe = seven_zip()?;
    let fat_dest = dest_path.clone();
    let (fat_exe, fat_arch, fat_pw) = (exe.clone(), PathBuf::from(&path), password.clone());
    let registry = jobs.inner().clone();
    registry.register(&id);
    let jobs = registry.clone();
    let jid = id.clone();
    let res = tauri::async_runtime::spawn_blocking(move || {
        let archive_path = PathBuf::from(&path);
        let opts = ExtractOptions {
            archive: &archive_path,
            dest_parent: &dest_path,
            password: opt_pw(&password),
            collision: Collision::parse(&collision),
            layout: if layout == "here" { Layout::Here } else { Layout::Folder },
            skip_junk,
            nested: nested.unwrap_or(false),
        };
        let mut cb = |percent: u32, file: &str| {
            let _ = app.emit("job-progress", Progress { id: jid.clone(), percent, file: file.to_string() });
        };
        let r = archive::extract(&exe, &opts, &jobs, &jid, &mut cb);
        if let (Ok(res), Some(true)) = (&r, seal_isos) {
            cb(100, "Fingerprinting extracted ISOs");
            god::seal_tokens(&res.iso_tokens);
        }
        r
    })
    .await
    .map_err(|e| e.to_string());
    // Unregister on every exit path (success, error, panic in the worker).
    registry.finish(&id);
    let r = res?.map_err(|e| safecopy::with_fat32_hint(e, &fat_dest, || {
        archive::list(&fat_exe, &fat_arch, opt_pw(&fat_pw)).map(|l| l.entries.iter().any(|e| !e.dir && e.size > safecopy::FAT_MAX)).unwrap_or(false)
    }))?;
    // Overseer R5 addendum: only session-grant folders we actually created this run.
    // Skip / Replace-merge / Extract-here never create a new claim_dir folder.
    if r.created && !r.skipped {
        grants.grant_session(Path::new(&r.output));
    }
    Ok(r)
}

#[tauri::command]
async fn extract_entry(jobs: State<'_, Jobs>, id: String, path: String, entry: String, password: Option<String>, out_file: String) -> Result<(), String> {
    let exe = seven_zip()?;
    let registry = jobs.inner().clone();
    registry.register(&id);
    let j = registry.clone();
    let jid = id.clone();
    let res = tauri::async_runtime::spawn_blocking(move || {
        archive::extract_entry_to(&exe, &PathBuf::from(path), &entry, opt_pw(&password), &PathBuf::from(out_file), &j, &jid)
    })
    .await
    .map_err(|e| e.to_string());
    registry.finish(&id);
    res?
}

/// Fallback when 7za is missing: hand the raw archive to the in-page WebAssembly engines.
#[tauri::command]
async fn read_file(path: String) -> Result<tauri::ipc::Response, String> {
    // Only used for the in-page (wasm) fallback; refuse anything that isn't an archive so a
    // compromised page can't use it to read arbitrary files.
    if !archive::is_archive_name(&path) {
        return Err("Only archives can be opened this way.".into());
    }
    let size = std::fs::metadata(&path).map_err(|e| e.to_string())?.len();
    if size > 1500 * 1024 * 1024 {
        return Err("ARCHIVE_TOO_LARGE".into());
    }
    let bytes = tauri::async_runtime::spawn_blocking(move || std::fs::read(path)).await.map_err(|e| e.to_string())?.map_err(|e| e.to_string())?;
    Ok(tauri::ipc::Response::new(bytes))
}

#[allow(clippy::too_many_arguments)]
#[tauri::command]
async fn create_archive(
    app: AppHandle,
    jobs: State<'_, Jobs>,
    grants: State<'_, safecopy::Grants>,
    id: String,
    sources: Vec<String>,
    out_file: String,
    format: String,
    password: Option<String>,
    level: u32,
) -> Result<String, String> {
    let out_file = grants.check_or_parent(&out_file)?.display().to_string();
    let exe = seven_zip()?;
    let registry = jobs.inner().clone();
    registry.register(&id);
    let j = registry.clone();
    let jid = id.clone();
    let res = tauri::async_runtime::spawn_blocking(move || {
        let srcs: Vec<PathBuf> = sources.iter().map(PathBuf::from).collect();
        let mut cb = |percent: u32, file: &str| {
            let _ = app.emit("job-progress", Progress { id: jid.clone(), percent, file: file.to_string() });
        };
        let out_file = PathBuf::from(out_file);
        let _dest_lock = match out_file.parent() { Some(p) => Some(destlock::acquire(p, || cb(0, destlock::WAIT_MESSAGE), &|| j.is_cancelled(&jid))?), None => None };
        archive::create(&exe, &srcs, &out_file, &format, opt_pw(&password), level, &j, &jid, &mut cb)
    })
    .await
    .map_err(|e| e.to_string());
    registry.finish(&id);
    res?
}

#[tauri::command]
async fn god_extracted(app: AppHandle, jobs: State<'_, Jobs>, id: String, token: String, delete_iso: bool) -> Result<god::ExtractedGod, String> {
    let registry = jobs.inner().clone();
    registry.register(&id);
    let j = registry.clone();
    let jid = id.clone();
    let res = tauri::async_runtime::spawn_blocking(move || {
        let mut cb = |percent: u32, file: &str| {
            let _ = app.emit("job-progress", Progress { id: jid.clone(), percent, file: file.to_string() });
        };
        god::convert_registered(&token, delete_iso, &j, &jid, &mut cb)
    })
    .await
    .map_err(|e| e.to_string());
    registry.finish(&id);
    res?
}

#[tauri::command]
async fn dlc_install(grants: State<'_, safecopy::Grants>, path: String, libraries: Vec<String>, move_file: bool) -> Result<dlc::DlcResult, String> {
    let path = grants.check(&path)?;
    // only libraries the user picked/approved (or extraction outputs); others are ignored
    let libs: Vec<PathBuf> = libraries.iter().filter(|s| !s.trim().is_empty()).filter_map(|l| grants.check(l).ok()).collect();
    tauri::async_runtime::spawn_blocking(move || {
        dlc::install_dlc(&path, &libs, move_file)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn library_scan(libraries: Vec<String>) -> Result<Vec<library::Game>, String> {
    tauri::async_runtime::spawn_blocking(move || library::scan(&libraries.iter().filter(|s| !s.trim().is_empty()).map(PathBuf::from).collect::<Vec<_>>()))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn copy_to_usb(app: AppHandle, jobs: State<'_, Jobs>, grants: State<'_, safecopy::Grants>, id: String, game: String, target: String) -> Result<library::CopyResult, String> {
    let (game, target) = (grants.check(&game)?, grants.check(&target)?);
    let registry = jobs.inner().clone();
    registry.register(&id);
    let j = registry.clone();
    let jid = id.clone();
    let res = tauri::async_runtime::spawn_blocking(move || {
        let mut cb = |percent: u32, file: &str| {
            let _ = app.emit("job-progress", Progress { id: jid.clone(), percent, file: file.to_string() });
        };
        let _dest_lock = destlock::acquire(&target, || cb(0, destlock::WAIT_MESSAGE), &|| j.is_cancelled(&jid))?;
        library::copy_to_usb(&game, &target, &mut cb, &|| j.is_cancelled(&jid))
    })
    .await
    .map_err(|e| e.to_string());
    registry.finish(&id);
    res?
}

// ---- Send to Xbox (FTP) ----
/// Xbox FTP servers usually allow a single connection: one FTP session at a time, app-wide.
static FTP_BUSY: Mutex<()> = Mutex::new(());

pub(crate) fn ftp_lock() -> Result<std::sync::MutexGuard<'static, ()>, String> {
    match FTP_BUSY.try_lock() {
        Ok(g) => Ok(g),
        Err(std::sync::TryLockError::Poisoned(p)) => Ok(p.into_inner()),
        Err(_) => Err("Unpack is already talking to the console. Wait for that to finish (Xbox FTP allows one connection).".into()),
    }
}

pub fn ftp_console(host: &str, port: u16, user: &str, pass: &str) -> Result<ftp::Console, String> {
    let user: String = if user.is_empty() { "xboxftp".into() } else { user.chars().take(64).collect() };
    let pass: String = pass.chars().take(128).collect();
    Ok(ftp::Console { addr: ftp::check_target(host, port)?, user, pass, resume_dir: ftp::resume_dir() })
}

/// Connect, log in, list `path` (root lists drives). Used by Test connection and the browser.
pub fn ftp_list_blocking(con: ftp::Console, path: String) -> Result<Vec<ftp::Entry>, String> {
    let _g = ftp_lock()?;
    let segs = ftp::split_remote(&path)?;
    let mut f = ftp::Ftp::connect(con.addr, &con.user, &con.pass)?;
    let r = f.list(&segs);
    f.quit();
    r
}

#[tauri::command]
async fn ftp_list(host: String, port: u16, user: String, pass: String, path: String) -> Result<Vec<ftp::Entry>, String> {
    let con = ftp_console(&host, port, &user, &pass)?;
    tauri::async_runtime::spawn_blocking(move || ftp_list_blocking(con, path)).await.map_err(|e| e.to_string())?
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FtpSendArgs {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub pass: String,
    pub src: String,
    pub kind: String,
    pub drive: String,
    pub title_id: Option<String>,
    pub custom: Option<String>,
    #[serde(default)]
    pub parallel: bool,
    /// User-edited destination folder (replaces the auto-detected one).
    #[serde(default)]
    pub dest: Option<String>,
}

#[derive(Serialize)]
pub struct FtpDest {
    pub path: String,
    pub contents_only: bool,
}

/// Auto-detected destination for a queue item, or (with `dest`) a check of an edited one.
pub fn ftp_dest_for(a: &FtpSendArgs, src: &Path) -> Result<(Vec<String>, bool), String> {
    let (def, contents_only) = ftp::destination(&a.kind, &a.drive, src, a.title_id.as_deref(), a.custom.as_deref())?;
    match a.dest.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(p) => Ok((ftp::custom_dest(p)?, contents_only)),
        None => Ok((def, contents_only)),
    }
}

#[tauri::command]
fn ftp_dest(args: FtpSendArgs) -> Result<FtpDest, String> {
    // Only path text is computed here; no file is read, so no grant is needed.
    ftp_dest_for(&args, Path::new(&args.src)).map(|(d, c)| FtpDest { path: ftp::join_remote(&d), contents_only: c })
}

/// Shared by the app and the test bridge: `src` must already be grant-checked.
pub fn ftp_send_blocking(a: &FtpSendArgs, src: &Path, progress: &mut dyn FnMut(u32, &str), cancelled: &dyn Fn() -> bool) -> Result<ftp::SendResult, String> {
    let con = ftp_console(&a.host, a.port, &a.user, &a.pass)?;
    let (dest, contents_only) = ftp_dest_for(a, src)?;
    let _g = ftp_lock()?;
    ftp::send(&con, src, &dest, contents_only, a.parallel, progress, cancelled)
}

#[tauri::command]
async fn ftp_send(app: AppHandle, jobs: State<'_, Jobs>, grants: State<'_, safecopy::Grants>, id: String, args: FtpSendArgs) -> Result<ftp::SendResult, String> {
    let src = grants.check(&args.src)?;
    let registry = jobs.inner().clone();
    registry.register(&id);
    let j = registry.clone();
    let jid = id.clone();
    let res = tauri::async_runtime::spawn_blocking(move || {
        let mut cb = |percent: u32, file: &str| {
            let _ = app.emit("job-progress", Progress { id: jid.clone(), percent, file: file.to_string() });
        };
        ftp_send_blocking(&args, &src, &mut cb, &|| j.is_cancelled(&jid))
    })
    .await
    .map_err(|e| e.to_string());
    registry.finish(&id);
    res?
}

#[tauri::command]
async fn mod_identify(builtin: State<'_, mods::BuiltinSet>, paths: Vec<String>, hints: Option<Vec<String>>, learned: Option<Vec<modnames::Learned>>) -> Result<Vec<Result<mods::ModInfo, String>>, String> {
    let hints = hints.unwrap_or_default();
    let learned: Vec<modnames::Learned> = mods::merge_learned(mods::merge_learned(learned.unwrap_or_default().into_iter().take(1000).collect(), &builtin.learned), &catalog::learned());
    tauri::async_runtime::spawn_blocking(move || paths.iter().enumerate().map(|(i, p)| mods::identify_learned(&PathBuf::from(p), hints.get(i).map(|s| s.as_str()).filter(|s| !s.is_empty()), &learned)).collect()).await.map_err(|e| e.to_string())
}

#[derive(Serialize)]
struct ModNameKey { key: String, tid: String, game: String }

/// Mod catalog (see catalog.rs). One command; `cmd` picks the action. `id` = progress job id.
#[tauri::command]
async fn catalog(app: AppHandle, jobs: State<'_, Jobs>, cmd: String, args: Option<serde_json::Value>, id: Option<String>) -> Result<serde_json::Value, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?.join("catalog");
    let ctx = catalog::install::Ctx { store: catalog::Store::new(dir), exe: archive::find_7za(), tuning: Default::default() };
    let jid = id.unwrap_or_else(|| format!("catalog-{cmd}"));
    let registry = jobs.inner().clone();
    registry.register(&jid);
    let j = registry.clone();
    let jid2 = jid.clone();
    let app2 = app.clone();
    let res = tauri::async_runtime::spawn_blocking(move || {
        let grants = app2.state::<safecopy::Grants>();
        let mut cb = |percent: u32, file: &str| { let _ = app.emit("job-progress", Progress { id: jid2.clone(), percent, file: file.to_string() }); };
        catalog::call(&ctx, &cmd, args.unwrap_or(serde_json::json!({})), &|p| grants.check(p), &mut cb, &|| j.is_cancelled(&jid2))
    }).await.map_err(|e| e.to_string());
    registry.finish(&jid);
    res?
}

#[tauri::command]
fn builtin_menus(builtin: State<'_, mods::BuiltinSet>) -> mods::BuiltinSet {
    builtin.inner().clone()
}

#[tauri::command]
fn mod_name_key(name: String, tid: String) -> Result<ModNameKey, String> {
    let name: String = name.chars().take(200).collect();
    modnames::learn_key(&name, &tid).map(|(key, tid, game)| ModNameKey { key, tid, game })
}

#[tauri::command]
fn game_search(q: String) -> Vec<gamedb::Entry> {
    gamedb::search(&q, 40)
}

#[tauri::command]
async fn mod_copy(app: AppHandle, jobs: State<'_, Jobs>, grants: State<'_, safecopy::Grants>, id: String, path: String, target: String, mode: String) -> Result<mods::ModCopyResult, String> {
    let (path, target) = (grants.check(&path)?, grants.check(&target)?);
    let registry = jobs.inner().clone();
    registry.register(&id);
    let j = registry.clone();
    let jid = id.clone();
    let res = tauri::async_runtime::spawn_blocking(move || {
        let mut cb = |percent: u32, file: &str| {
            let _ = app.emit("job-progress", Progress { id: jid.clone(), percent, file: file.to_string() });
        };
        let _dest_lock = destlock::acquire(&target, || cb(0, destlock::WAIT_MESSAGE), &|| j.is_cancelled(&jid))?;
        mods::copy_mod(&path, &target, &mode, &mut cb, &|| j.is_cancelled(&jid))
    })
    .await
    .map_err(|e| e.to_string());
    registry.finish(&id);
    res?
}

#[tauri::command]
async fn dlc_scan_drives(grants: State<'_, safecopy::Grants>) -> Result<Vec<String>, String> {
    let roots = tauri::async_runtime::spawn_blocking(dlc::scan_content_roots).await.map_err(|e| e.to_string())?;
    roots.iter().for_each(|r| grants.grant(Path::new(r)));
    Ok(roots)
}

/// Native pickers run from Rust so the picked paths can be granted (Overseer R5 L2).
#[tauri::command]
async fn pick_path(app: AppHandle, grants: State<'_, safecopy::Grants>, kind: String, title: String) -> Result<Vec<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    let a = app.clone();
    let picked: Vec<PathBuf> = tauri::async_runtime::spawn_blocking(move || {
        let d = a.dialog().file().set_title(&title);
        let v = if kind == "folder" { d.blocking_pick_folder().map(|p| vec![p]) } else { d.blocking_pick_files() };
        v.unwrap_or_default().into_iter().filter_map(|p| p.into_path().ok()).collect()
    })
    .await
    .map_err(|e| e.to_string())?;
    picked.iter().for_each(|p| grants.grant(p));
    Ok(picked.iter().map(|p| p.display().to_string()).collect())
}


#[tauri::command]
async fn download_url(
    app: AppHandle,
    jobs: State<'_, Jobs>,
    grants: State<'_, safecopy::Grants>,
    id: String,
    url: String,
    dest_dir: Option<String>,
    expected_sha256: Option<String>,
    max_bytes: Option<u64>,
) -> Result<download::DownloadResult, String> {
    let dir = match dest_dir {
        Some(d) if !d.trim().is_empty() => grants.check(&d)?,
        _ => {
            let base = app.path().app_data_dir().map_err(|e| e.to_string())?.join("Downloads");
            std::fs::create_dir_all(&base).map_err(|e| e.to_string())?;
            grants.grant(&base);
            base
        }
    };
    let expected_sha256 = download::parse_expected_sha256(expected_sha256.as_deref())?;
    let opts = download::DownloadOpts {
        max_bytes: max_bytes.filter(|n| *n > 0).unwrap_or(download::DEFAULT_MAX_BYTES),
        expected_sha256,
    };
    let registry = jobs.inner().clone();
    registry.register(&id);
    let j = registry.clone();
    let jid = id.clone();
    let res = tauri::async_runtime::spawn_blocking(move || {
        let mut cb = |percent: u32, file: &str| {
            let _ = app.emit("job-progress", Progress { id: jid.clone(), percent, file: file.to_string() });
        };
        download::download_with(&url, &dir, &opts, &mut cb, &|| j.is_cancelled(&jid))
    })
    .await
    .map_err(|e| e.to_string());
    registry.finish(&id);
    let r = res??;
    grants.grant(Path::new(&r.path));
    Ok(r)
}

#[tauri::command]
fn download_folder(app: AppHandle, grants: State<'_, safecopy::Grants>) -> Result<String, String> {
    let base = app.path().app_data_dir().map_err(|e| e.to_string())?.join("Downloads");
    std::fs::create_dir_all(&base).map_err(|e| e.to_string())?;
    grants.grant(&base);
    Ok(base.display().to_string())
}

#[tauri::command]
fn list_archives_in_folder(grants: State<'_, safecopy::Grants>, path: String) -> Result<Vec<String>, String> {
    let root = grants.check(&path)?;
    download::list_archives(&root)
}

#[tauri::command]
fn notify_done(app: AppHandle, title: String, body: String) -> Result<(), String> {
    use tauri_plugin_notification::NotificationExt;
    app.notification().builder().title(title).body(body).show().map_err(|e| e.to_string())
}

#[tauri::command]
fn set_taskbar_progress(app: AppHandle, pct: Option<u64>) -> Result<(), String> {
    let Some(w) = app.get_webview_window("main") else { return Ok(()) };
    #[cfg(windows)]
    {
        use tauri::window::{ProgressBarState, ProgressBarStatus};
        if let Some(p) = pct {
            let _ = w.set_progress_bar(ProgressBarState { status: Some(ProgressBarStatus::Normal), progress: Some(p.min(100)) });
        } else {
            let _ = w.set_progress_bar(ProgressBarState { status: Some(ProgressBarStatus::None), progress: None });
        }
    }
    #[cfg(not(windows))]
    { let _ = (w, pct); }
    Ok(())
}


#[tauri::command]
async fn pick_save(app: AppHandle, grants: State<'_, safecopy::Grants>, title: String, default_path: Option<String>) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    let a = app.clone();
    let picked = tauri::async_runtime::spawn_blocking(move || {
        let mut d = a.dialog().file().set_title(&title);
        if let Some(p) = default_path { d = d.set_file_name(p); }
        d.blocking_save_file().and_then(|p| p.into_path().ok())
    })
    .await
    .map_err(|e| e.to_string())?;
    if let Some(ref p) = picked {
        if let Some(parent) = p.parent() { grants.grant(parent); }
    }
    Ok(picked.map(|p| p.display().to_string()))
}

#[tauri::command]
async fn iso_info(path: String) -> Result<god::IsoInfo, String> {
    tauri::async_runtime::spawn_blocking(move || god::iso_info(&PathBuf::from(path))).await.map_err(|e| e.to_string())?
}

/// Other discs of the same game found next to `path` (same folder, Disc/CD/DVD n naming), so the
/// page can offer to convert them together. Read-only; only inside a granted folder.
#[tauri::command]
async fn iso_companions(grants: State<'_, safecopy::Grants>, path: String) -> Result<Vec<String>, String> {
    let p = grants.check(&path)?;
    tauri::async_runtime::spawn_blocking(move || discs::companions(&p).map(|v| v.into_iter().map(|x| x.display().to_string()).collect()))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn iso_to_god(app: AppHandle, jobs: State<'_, Jobs>, grants: State<'_, safecopy::Grants>, id: String, path: String, dest: String, title: Option<String>) -> Result<god::GodResult, String> {
    let dest = grants.check(&dest)?;
    let registry = jobs.inner().clone();
    registry.register(&id);
    let j = registry.clone();
    let jid = id.clone();
    let res = tauri::async_runtime::spawn_blocking(move || {
        let mut cb = |percent: u32, file: &str| {
            let _ = app.emit("job-progress", Progress { id: jid.clone(), percent, file: file.to_string() });
        };
        god::convert(&PathBuf::from(path), &dest, title.as_deref(), &j, &jid, &mut cb)
    })
    .await
    .map_err(|e| e.to_string());
    registry.finish(&id);
    res?
}


#[tauri::command]
async fn iso_to_xex(app: AppHandle, jobs: State<'_, Jobs>, grants: State<'_, safecopy::Grants>, id: String, path: String, dest: String, title: Option<String>) -> Result<iso_extract::ExtractResult, String> {
    let dest = grants.check(&dest)?;
    let registry = jobs.inner().clone();
    registry.register(&id);
    let j = registry.clone();
    let jid = id.clone();
    let res = tauri::async_runtime::spawn_blocking(move || {
        let mut cb = |percent: u32, file: &str| {
            let _ = app.emit("job-progress", Progress { id: jid.clone(), percent, file: file.to_string() });
        };
        // Manual: dest is parent; claim named folder inside.
        iso_extract::extract_named(&PathBuf::from(path), &dest, title.as_deref(), &j, &jid, &mut cb)
    })
    .await
    .map_err(|e| e.to_string());
    registry.finish(&id);
    res?
}

#[tauri::command]
async fn xex_extracted(app: AppHandle, jobs: State<'_, Jobs>, id: String, token: String, delete_iso: bool) -> Result<iso_extract::ExtractedXex, String> {
    let registry = jobs.inner().clone();
    registry.register(&id);
    let j = registry.clone();
    let jid = id.clone();
    let res = tauri::async_runtime::spawn_blocking(move || {
        let mut cb = |percent: u32, file: &str| {
            let _ = app.emit("job-progress", Progress { id: jid.clone(), percent, file: file.to_string() });
        };
        iso_extract::extract_registered(&token, delete_iso, &j, &jid, &mut cb)
    })
    .await
    .map_err(|e| e.to_string());
    registry.finish(&id);
    res?
}

#[tauri::command]
fn cancel_job(jobs: State<'_, Jobs>, id: String) -> bool {
    jobs.cancel(&id)
}

pub fn run() {
    // Before Tauri or the WebView touch their folders: carry data over from the old identifier.
    for d in migrate::run_startup() { eprintln!("migrated app data into {}", d.display()); }
    let cwd = std::env::current_dir().unwrap_or_default();
    let first: Vec<String> = std::env::args().skip(1).collect();
    let pending = PendingLaunch::default();
    if let Some(req) = parse_args(&first, &cwd) {
        pending.0.lock().unwrap().push(req);
    }
    tauri::Builder::default()
        // Must be registered first: a second launch (double-click / right-click while
        // Unpack is open) hands its arguments to this window and exits.
        .plugin(tauri_plugin_single_instance::init(|app, argv, cwd| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.unminimize();
                let _ = w.set_focus();
            }
            if let Some(req) = parse_args(&argv.into_iter().skip(1).collect::<Vec<_>>(), std::path::Path::new(&cwd)) {
                // Queue it too, in case the page hasn't attached its listener yet.
                app.state::<PendingLaunch>().0.lock().unwrap().push(req.clone());
                let _ = app.emit("launch-request", ());
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_opener::init())
        .manage(pending)
        .manage(Jobs::default())
        .setup(|app| {
            let dir = app.path().app_data_dir().unwrap_or_else(|_| std::env::temp_dir().join("Unpack"));
            let grants = safecopy::Grants::with_file(dir.join("grants.json"));
            ftp::set_resume_dir(dir.join("ftp-resume"));
            // Built-in menus (ships empty): validated once; their files are granted for this
            // session so Copy to USB / Send to Xbox work on them. Read-only in the UI.
            let menus = app.path().resource_dir().map(|d| d.join("menus")).map(|d| mods::load_builtin(&d)).unwrap_or_default();
            for p in &menus.problems { eprintln!("built-in menus: skipped {p}"); }
            for m in &menus.menus { grants.grant_session(Path::new(&m.path)); }
            app.manage(grants);
            app.manage(menus);
            app.manage(discord::PresenceHandle::start());
            Ok(())
        })
        // Grant dropped folders in Rust (session-only). The page cannot call grant_dir.
        .on_webview_event(|webview, event| {
            if let tauri::WebviewEvent::DragDrop(tauri::DragDropEvent::Drop { paths, .. }) = event {
                let grants = webview.state::<safecopy::Grants>();
                for p in paths {
                    let meta = std::fs::symlink_metadata(p);
                    if let Ok(m) = meta {
                        if safecopy::is_link(&m) { continue; }
                        if m.is_dir() {
                            grants.grant_session(p);
                        } else if m.is_file() {
                            if let Some(parent) = p.parent() {
                                grants.grant_session(parent);
                            }
                        }
                    }
                }
            }
        })
        .invoke_handler(tauri::generate_handler![native_info, file_info, list_archive, read_entry, extract_archive, extract_entry, read_file, cancel_job, take_launch_requests, create_archive, iso_info, iso_companions, iso_to_god, iso_to_xex, god_extracted, xex_extracted, dlc_install, dlc_scan_drives, library_scan, copy_to_usb, mod_identify, mod_name_key, mod_copy, game_search, pick_path, pick_save, download_url, download_folder, list_archives_in_folder, notify_done, set_taskbar_progress, ftp_list, ftp_send, ftp_dest, builtin_menus, catalog, discord_set_activity, discord_set_enabled])
        .run(tauri::generate_context!())
        .expect("error while running Unpack");
}

/// Discord Rich Presence (local IPC to the Discord app; no network from Unpack). The page sends the
/// current view and an optional short job label; the text shown is built in Rust.
#[tauri::command]
fn discord_set_activity(app: AppHandle, view: String, job: Option<String>) -> Result<(), String> {
    let text = discord::activity_for(&view, job.as_deref());
    if let Some(presence) = app.try_state::<discord::PresenceHandle>() {
        presence.update(&text.details, &text.state);
    }
    Ok(())
}

/// The Settings toggle. Off clears the activity and closes the connection.
#[tauri::command]
fn discord_set_enabled(app: AppHandle, enabled: bool) -> Result<(), String> {
    if let Some(presence) = app.try_state::<discord::PresenceHandle>() {
        presence.set_enabled(enabled);
    }
    Ok(())
}

#[cfg(test)]
mod launch_tests {
    use super::*;
    #[test]
    fn parses_explorer_args() {
        // Own folder per process: parallel test runs shared fixed names in the temp dir (R18 flake).
        let tmpd = crate::testutil::TmpDir::new("launch-args");
        let d = tmpd.path().to_path_buf();
        let f = d.join("unpack-launch-test.zip");
        std::fs::write(&f, b"x").unwrap();
        let r = parse_args(&["--extract-here".into(), f.display().to_string()], &d).unwrap();
        assert_eq!(r.action, "extract-here");
        assert_eq!(r.paths, vec![f.display().to_string()]);
        let r = parse_args(&["unpack-launch-test.zip".into()], &d).unwrap();
        assert_eq!(r.action, "open");
        assert_eq!(r.paths.len(), 1);
        assert!(parse_args(&["--extract-folder".into(), "missing.zip".into()], &d).is_none());
        assert!(parse_args(&["--some-webview-flag".into()], &d).is_none());
        // L6: after "--" a name starting with "-" is a file, not a flag
        let dash = d.join("-dash-launch-test.zip");
        std::fs::write(&dash, b"x").unwrap();
        let r = parse_args(&["--extract-here".into(), "--".into(), "-dash-launch-test.zip".into()], &d).unwrap();
        assert_eq!((r.action.as_str(), r.paths.len()), ("extract-here", 1));
        assert!(parse_args(&["-dash-launch-test.zip".into()], &d).is_none());
        let _ = std::fs::remove_file(dash);
        let _ = std::fs::remove_file(&f);
    }
}
