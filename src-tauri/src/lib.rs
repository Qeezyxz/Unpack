pub mod archive;
pub mod catalog;
pub mod contentdir;
pub mod covers;
pub mod destlock;
pub mod discord;
pub mod discs;
pub mod dlc;
pub mod download;
pub mod fatstage;
pub mod ftp;
pub mod gamedb;
pub mod god;
pub mod hints;
pub mod iso_extract;
pub mod lan;
pub mod library;
pub mod migrate;
pub mod modnames;
pub mod mods;
pub mod preflight;
pub mod safecopy;
pub mod saves;
#[cfg(test)]
pub mod testutil;
pub mod usbprep;

use std::net::Ipv4Addr;
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
            "--convert-god" if !only_paths => action = "convert-god".into(),
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
    if paths.is_empty() {
        None
    } else {
        Some(LaunchRequest { action, paths })
    }
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
    archive::find_7za()
        .ok_or_else(|| "The bundled 7-Zip (7za) is missing. Reinstall Unpack.".to_string())
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
        Some(p) => NativeInfo {
            available: true,
            path: p.display().to_string(),
        },
        None => NativeInfo {
            available: false,
            path: String::new(),
        },
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
        name: p
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        path,
        size: m.len(),
        is_dir: m.is_dir(),
    })
}

#[tauri::command]
async fn list_archive(path: String, password: Option<String>) -> Result<archive::Listing, String> {
    let exe = seven_zip()?;
    tauri::async_runtime::spawn_blocking(move || {
        archive::list(&exe, &PathBuf::from(path), opt_pw(&password))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn read_entry(
    path: String,
    entry: String,
    password: Option<String>,
) -> Result<tauri::ipc::Response, String> {
    let exe = seven_zip()?;
    let bytes = tauri::async_runtime::spawn_blocking(move || {
        archive::read_entry(
            &exe,
            &PathBuf::from(path),
            &entry,
            opt_pw(&password),
            4 * 1024 * 1024,
        )
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
    auto_iso: Option<String>,
) -> Result<archive::ExtractResult, String> {
    // Dest must be a picked folder, or (Extract beside) the archive's own parent.
    let dest_path = match grants.check(&dest) {
        Ok(d) => d,
        Err(e) => {
            let arch = PathBuf::from(&path);
            let parent = arch.parent().ok_or_else(|| e.clone())?;
            let (Ok(d), Ok(p)) = (std::fs::canonicalize(&dest), std::fs::canonicalize(parent))
            else {
                return Err(e);
            };
            let d = safecopy::simplify(d);
            if d != safecopy::simplify(p) {
                return Err(e);
            }
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
        let here = layout == "here";
        let stage = fat_stage_for(
            &exe,
            &archive_path,
            opt_pw(&password),
            &dest_path,
            auto_iso.as_deref().unwrap_or("off"),
            here,
            &jid,
        )?;
        let opts = ExtractOptions {
            archive: &archive_path,
            dest_parent: stage
                .as_ref()
                .map(|s| s.dir.as_path())
                .unwrap_or(&dest_path),
            password: opt_pw(&password),
            collision: Collision::parse(&collision),
            layout: if layout == "here" {
                Layout::Here
            } else {
                Layout::Folder
            },
            skip_junk,
            nested: nested.unwrap_or(false),
        };
        let mut cb = |percent: u32, file: &str| {
            let _ = app.emit(
                "job-progress",
                Progress {
                    id: jid.clone(),
                    percent,
                    file: file.to_string(),
                },
            );
        };
        let r = archive::extract(&exe, &opts, &jobs, &jid, &mut cb);
        let r = match stage {
            Some(st) => finish_fat_stage(r, &st, &dest_path),
            None => r,
        };
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
    let r = res?.map_err(|e| {
        safecopy::with_fat32_hint(e, &fat_dest, || {
            archive::list(&fat_exe, &fat_arch, opt_pw(&fat_pw))
                .map(|l| {
                    l.entries
                        .iter()
                        .any(|e| !e.dir && e.size > safecopy::FAT_MAX)
                })
                .unwrap_or(false)
        })
    })?;
    // Overseer R5 addendum: only session-grant folders we actually created this run.
    // Skip / Replace-merge / Extract-here never create a new claim_dir folder.
    if r.created && !r.skipped {
        grants.grant_session(Path::new(&r.output));
    }
    Ok(r)
}

struct FatStage {
    dir: PathBuf,
    root: PathBuf,
    isos: Vec<String>,
}

/// FAT32 destination: refuse only a single output file of 4 GiB or more. An oversized ISO with
/// Auto ISO → GOD/XEX on is extracted into a staging folder on a non-FAT drive instead.
fn fat_stage_for(
    exe: &Path,
    archive_path: &Path,
    pw: Option<&str>,
    dest: &Path,
    auto_iso: &str,
    here: bool,
    job_id: &str,
) -> Result<Option<FatStage>, String> {
    let Some(fs) = safecopy::fs_type(dest).filter(|t| safecopy::is_fat(t)) else {
        return Ok(None);
    };
    // Listing fails for encrypted names etc.; then 7-Zip's own error (with the FAT32 hint) decides.
    let Ok(l) = archive::list(exe, archive_path, pw) else {
        return Ok(None);
    };
    let files: Vec<(String, u64)> = l
        .entries
        .iter()
        .filter(|e| !e.dir)
        .map(|e| (e.path.clone(), e.size))
        .collect();
    let fatstage::Plan::Stage(big) = fatstage::plan(Some(&fs), &files, auto_iso)? else {
        return Ok(None);
    };
    let isos: Vec<String> = big
        .iter()
        .map(|(n, _)| n.rsplit(['/', '\\']).next().unwrap_or(n).to_string())
        .collect();
    if here {
        return Err(format!(
            "{} is over 4 GB and the destination is {fs}. Auto ISO → GOD only runs when the archive gets its own folder, so use Extract (not Extract here).",
            safecopy::plain(&isos.join(", "))
        ));
    }
    let need: u64 = files.iter().map(|f| f.1).sum();
    let root = fatstage::pick_stage_root(
        need,
        &fatstage::stage_candidates(),
        &safecopy::plain(&isos.join(", ")),
    )?;
    let tag: String = job_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(32)
        .collect();
    let dir = fatstage::new_job_dir(&root, &tag).map_err(|e| {
        format!(
            "Couldn't create a staging folder in {}: {e}",
            root.display()
        )
    })?;
    Ok(Some(FatStage { dir, root, isos }))
}

/// Where a staged extraction's other files go: the same rule as an unstaged extract of an
/// archive holding ISOs (archive.rs) — never inside a console `Content` / `Content\<profile>`
/// folder, but next to the `Content` folder; the GOD game itself still goes into the profile.
fn leftover_parent(dest: &Path) -> PathBuf {
    let in_profile = contentdir::is_profile_dir(dest)
        || dest
            .file_name()
            .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case("content"));
    contentdir::resolve(dest)
        .filter(|_| in_profile)
        .and_then(|pf| contentdir::outside_content(&pf))
        .unwrap_or_else(|| dest.to_path_buf())
}

/// Move everything except the oversized-ISO staging copies into a new folder on the FAT drive and
/// point the ISO tokens at the real destination. The staging folder is removed on any failure.
fn finish_fat_stage(
    r: Result<archive::ExtractResult, String>,
    st: &FatStage,
    dest: &Path,
) -> Result<archive::ExtractResult, String> {
    let cleanup = || {
        fatstage::release_job(&st.dir);
        let _ = std::fs::remove_dir_all(&st.dir);
    };
    let mut res = match r {
        Ok(res) => res,
        Err(e) => {
            cleanup();
            return Err(e);
        }
    };
    let staged_out = PathBuf::from(&res.output);
    let name = staged_out
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Extracted".into());
    // Only the ISOs over 4 GB stay in staging; smaller ISOs and other files go to the FAT drive.
    let keep: Vec<PathBuf> = res
        .isos
        .iter()
        .map(PathBuf::from)
        .filter(|p| {
            std::fs::metadata(p)
                .map(|m| m.len() > safecopy::FAT_MAX)
                .unwrap_or(false)
        })
        .collect();
    // 1.7.3: bound for a Content profile, extras (readme, NFO, links, pictures) are skipped
    // rather than left in a folder next to Content. Real game files keep the safe placement.
    if leftover_parent(dest) != dest {
        if let Some(w) = archive::drop_extras(&staged_out) {
            res.warnings.push(w);
        }
    }
    let moved = (|| -> std::io::Result<(PathBuf, u64)> {
        let real = archive::claim_dir(&leftover_parent(dest), &name)?;
        let n = archive::move_tree_except(&staged_out, &real, &keep)?;
        Ok((real, n))
    })();
    let (real, n) = match moved {
        Ok(x) => x,
        Err(e) => {
            cleanup();
            return Err(format!(
                "Couldn't move the extracted files onto the destination: {e}"
            ));
        }
    };
    let remap = |p: &String| {
        let pb = PathBuf::from(p);
        if keep.contains(&pb) {
            return p.clone();
        }
        pb.strip_prefix(&staged_out)
            .map(|rel| real.join(rel).display().to_string())
            .unwrap_or_else(|_| p.clone())
    };
    // ISOs that fit were moved: register them again at their new place (normal, unstaged).
    let mut tokens = vec![];
    let mut isos = vec![];
    for (i, p) in res.isos.iter().enumerate() {
        let pb = PathBuf::from(p);
        if keep.contains(&pb) {
            isos.push(p.clone());
            tokens.push(res.iso_tokens.get(i).cloned().unwrap_or_default());
        } else {
            let np = remap(p);
            let t = god::register_extracted(std::slice::from_ref(&np), &real, false);
            isos.push(np);
            tokens.push(t.into_iter().next().unwrap_or_default());
        }
    }
    god::mark_staged(
        &tokens
            .iter()
            .zip(&isos)
            .filter(|(_, p)| keep.contains(&PathBuf::from(p)))
            .map(|(t, _)| t.clone())
            .collect::<Vec<_>>(),
        &st.dir,
        dest,
    );
    res.dlcs = res.dlcs.iter().map(remap).collect();
    res.isos = isos;
    res.iso_tokens = tokens;
    if n == 0 {
        let _ = std::fs::remove_dir(&real);
        res.output = dest.display().to_string();
        res.created = false;
    } else {
        res.output = real.display().to_string();
        res.created = true;
    }
    res.warnings.push(format!(
        "{} is over 4 GB, so it was extracted to a temporary folder in {} and is converted from there onto this FAT32 drive. The temporary copy is deleted afterwards.",
        safecopy::plain(&st.isos.join(", ")),
        st.root.display()
    ));
    // Registered ISO tokens keep the folder in use from here on.
    fatstage::release_job(&st.dir);
    if !god::stage_in_use(&st.dir) {
        cleanup();
    }
    Ok(res)
}

#[tauri::command]
async fn extract_entry(
    jobs: State<'_, Jobs>,
    id: String,
    path: String,
    entry: String,
    password: Option<String>,
    out_file: String,
) -> Result<(), String> {
    let exe = seven_zip()?;
    let registry = jobs.inner().clone();
    registry.register(&id);
    let j = registry.clone();
    let jid = id.clone();
    let res = tauri::async_runtime::spawn_blocking(move || {
        archive::extract_entry_to(
            &exe,
            &PathBuf::from(path),
            &entry,
            opt_pw(&password),
            &PathBuf::from(out_file),
            &j,
            &jid,
        )
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
    let bytes = tauri::async_runtime::spawn_blocking(move || std::fs::read(path))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
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
            let _ = app.emit(
                "job-progress",
                Progress {
                    id: jid.clone(),
                    percent,
                    file: file.to_string(),
                },
            );
        };
        let out_file = PathBuf::from(out_file);
        let _dest_lock = match out_file.parent() {
            Some(p) => Some(destlock::acquire(
                p,
                || cb(0, destlock::WAIT_MESSAGE),
                &|| j.is_cancelled(&jid),
            )?),
            None => None,
        };
        archive::create(
            &exe,
            &srcs,
            &out_file,
            &format,
            opt_pw(&password),
            level,
            &j,
            &jid,
            &mut cb,
        )
    })
    .await
    .map_err(|e| e.to_string());
    registry.finish(&id);
    res?
}

#[tauri::command]
async fn god_extracted(
    app: AppHandle,
    jobs: State<'_, Jobs>,
    id: String,
    token: String,
    delete_iso: bool,
) -> Result<god::ExtractedGod, String> {
    let registry = jobs.inner().clone();
    registry.register(&id);
    let j = registry.clone();
    let jid = id.clone();
    let res = tauri::async_runtime::spawn_blocking(move || {
        let mut cb = |percent: u32, file: &str| {
            let _ = app.emit(
                "job-progress",
                Progress {
                    id: jid.clone(),
                    percent,
                    file: file.to_string(),
                },
            );
        };
        god::convert_registered(&token, delete_iso, &j, &jid, &mut cb)
    })
    .await
    .map_err(|e| e.to_string());
    registry.finish(&id);
    res?
}

#[tauri::command]
async fn dlc_install(
    grants: State<'_, safecopy::Grants>,
    path: String,
    libraries: Vec<String>,
    move_file: bool,
    game_path: Option<String>,
) -> Result<dlc::DlcResult, String> {
    let path = grants.check(&path)?;
    let game = game_path
        .filter(|s| !s.trim().is_empty())
        .map(|s| grants.check(&s))
        .transpose()?;
    // only libraries the user picked/approved (or extraction outputs); others are ignored
    let libs: Vec<PathBuf> = libraries
        .iter()
        .filter(|s| !s.trim().is_empty())
        .filter_map(|l| grants.check(l).ok())
        .collect();
    tauri::async_runtime::spawn_blocking(move || {
        dlc::install_dlc(&path, &libs, move_file, game.as_deref())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
fn dlc_match_games(libraries: Vec<String>, title_id: String) -> Vec<String> {
    let libs: Vec<PathBuf> = libraries
        .iter()
        .filter(|s| !s.trim().is_empty())
        .map(PathBuf::from)
        .collect();
    dlc::find_games(&libs, &title_id)
        .into_iter()
        .map(|p| p.display().to_string())
        .collect()
}

#[tauri::command]
async fn library_scan(libraries: Vec<String>) -> Result<Vec<library::Game>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        library::scan(
            &libraries
                .iter()
                .filter(|s| !s.trim().is_empty())
                .map(PathBuf::from)
                .collect::<Vec<_>>(),
        )
    })
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
async fn copy_to_usb(
    app: AppHandle,
    jobs: State<'_, Jobs>,
    grants: State<'_, safecopy::Grants>,
    id: String,
    game: String,
    target: String,
) -> Result<library::CopyResult, String> {
    let (game, target) = (grants.check(&game)?, grants.check(&target)?);
    let registry = jobs.inner().clone();
    registry.register(&id);
    let j = registry.clone();
    let jid = id.clone();
    let res = tauri::async_runtime::spawn_blocking(move || {
        let mut cb = |percent: u32, file: &str| {
            let _ = app.emit(
                "job-progress",
                Progress {
                    id: jid.clone(),
                    percent,
                    file: file.to_string(),
                },
            );
        };
        let _dest_lock = destlock::acquire(&target, || cb(0, destlock::WAIT_MESSAGE), &|| {
            j.is_cancelled(&jid)
        })?;
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
static FTP_HOLDER: Mutex<Option<(String, String)>> = Mutex::new(None);

pub struct FtpLockGuard {
    _inner: std::sync::MutexGuard<'static, ()>,
}

impl Drop for FtpLockGuard {
    fn drop(&mut self) {
        *FTP_HOLDER.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

pub(crate) fn ftp_lock(subsystem: &str, detail: &str) -> Result<FtpLockGuard, String> {
    match FTP_BUSY.try_lock() {
        Ok(g) => {
            *FTP_HOLDER.lock().unwrap_or_else(|e| e.into_inner()) =
                Some((subsystem.to_string(), detail.to_string()));
            Ok(FtpLockGuard { _inner: g })
        }
        Err(std::sync::TryLockError::Poisoned(p)) => {
            *FTP_HOLDER.lock().unwrap_or_else(|e| e.into_inner()) =
                Some((subsystem.to_string(), detail.to_string()));
            Ok(FtpLockGuard {
                _inner: p.into_inner(),
            })
        }
        Err(_) => {
            let msg = FTP_HOLDER
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
                .map(|(s, d)| format!("Unpack is already talking to the console for {s} ({d}). Wait for that to finish (Xbox FTP allows one connection)."))
                .unwrap_or_else(|| "Unpack is already talking to the console. Wait for that to finish (Xbox FTP allows one connection).".into());
            Err(msg)
        }
    }
}

#[derive(Serialize)]
struct FtpLockStatus {
    busy: bool,
    subsystem: String,
    detail: String,
}

#[tauri::command]
fn ftp_lock_status() -> FtpLockStatus {
    let busy = FTP_BUSY.try_lock().is_err();
    let (subsystem, detail) = FTP_HOLDER
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_else(|| (String::new(), String::new()));
    FtpLockStatus {
        busy,
        subsystem,
        detail,
    }
}

pub fn ftp_console(host: &str, port: u16, user: &str, pass: &str) -> Result<ftp::Console, String> {
    let user: String = if user.is_empty() {
        "xboxftp".into()
    } else {
        user.chars().take(64).collect()
    };
    let pass: String = pass.chars().take(128).collect();
    Ok(ftp::Console {
        addr: ftp::check_target(host, port)?,
        user,
        pass,
        resume_dir: ftp::resume_dir(),
    })
}

/// Connect, log in, list `path` (root lists drives). Used by Test connection and the browser.
pub fn ftp_list_blocking(con: ftp::Console, path: String) -> Result<Vec<ftp::Entry>, String> {
    let _g = ftp_lock("Send to Xbox", "browse")?;
    let segs = ftp::split_remote(&path)?;
    let mut f = ftp::Ftp::connect(con.addr, &con.user, &con.pass)?;
    let r = f.list(&segs);
    f.quit();
    r
}

#[tauri::command]
async fn ftp_list(
    host: String,
    port: u16,
    user: String,
    pass: String,
    path: String,
) -> Result<Vec<ftp::Entry>, String> {
    let con = ftp_console(&host, port, &user, &pass)?;
    tauri::async_runtime::spawn_blocking(move || ftp_list_blocking(con, path))
        .await
        .map_err(|e| e.to_string())?
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
    let (def, contents_only) = ftp::destination(
        &a.kind,
        &a.drive,
        src,
        a.title_id.as_deref(),
        a.custom.as_deref(),
    )?;
    match a.dest.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(p) => Ok((ftp::custom_dest(p)?, contents_only)),
        None => Ok((def, contents_only)),
    }
}

#[tauri::command]
fn ftp_dest(args: FtpSendArgs) -> Result<FtpDest, String> {
    // Only path text is computed here; no file is read, so no grant is needed.
    ftp_dest_for(&args, Path::new(&args.src)).map(|(d, c)| FtpDest {
        path: ftp::join_remote(&d),
        contents_only: c,
    })
}

/// Shared by the app and the test bridge: `src` must already be grant-checked.
pub fn ftp_send_blocking(
    a: &FtpSendArgs,
    src: &Path,
    progress: &mut dyn FnMut(u32, &str),
    cancelled: &dyn Fn() -> bool,
) -> Result<ftp::SendResult, String> {
    let con = ftp_console(&a.host, a.port, &a.user, &a.pass)?;
    let (dest, contents_only) = ftp_dest_for(a, src)?;
    let _g = ftp_lock("Send to Xbox", "upload")?;
    ftp::send(
        &con,
        src,
        &dest,
        contents_only,
        a.parallel,
        progress,
        cancelled,
    )
}

#[tauri::command]
async fn ftp_send(
    app: AppHandle,
    jobs: State<'_, Jobs>,
    grants: State<'_, safecopy::Grants>,
    id: String,
    args: FtpSendArgs,
) -> Result<ftp::SendResult, String> {
    let src = grants.check(&args.src)?;
    let registry = jobs.inner().clone();
    registry.register(&id);
    let j = registry.clone();
    let jid = id.clone();
    let res = tauri::async_runtime::spawn_blocking(move || {
        let mut cb = |percent: u32, file: &str| {
            let _ = app.emit(
                "job-progress",
                Progress {
                    id: jid.clone(),
                    percent,
                    file: file.to_string(),
                },
            );
        };
        ftp_send_blocking(&args, &src, &mut cb, &|| j.is_cancelled(&jid))
    })
    .await
    .map_err(|e| e.to_string());
    registry.finish(&id);
    res?
}

#[tauri::command]
async fn mod_identify(
    builtin: State<'_, mods::BuiltinSet>,
    grants: State<'_, safecopy::Grants>,
    paths: Vec<String>,
    hints: Option<Vec<String>>,
    learned: Option<Vec<modnames::Learned>>,
) -> Result<Vec<Result<mods::ModInfo, String>>, String> {
    let hints = hints.unwrap_or_default();
    let checked: Vec<Result<PathBuf, String>> = paths
        .iter()
        .map(|p| mods::check_mod_path(&grants, p))
        .collect();
    let learned: Vec<modnames::Learned> = mods::merge_learned(
        mods::merge_learned(
            learned.unwrap_or_default().into_iter().take(1000).collect(),
            &builtin.learned,
        ),
        &catalog::learned(),
    );
    tauri::async_runtime::spawn_blocking(move || {
        checked
            .into_iter()
            .enumerate()
            .map(|(i, p)| {
                mods::identify_learned(
                    &p?,
                    hints.get(i).map(|s| s.as_str()).filter(|s| !s.is_empty()),
                    &learned,
                )
            })
            .collect()
    })
    .await
    .map_err(|e| e.to_string())
}

#[derive(Serialize)]
struct ModNameKey {
    key: String,
    tid: String,
    game: String,
}

/// Mod catalog (see catalog.rs). One command; `cmd` picks the action. `id` = progress job id.
#[tauri::command]
async fn catalog(
    app: AppHandle,
    jobs: State<'_, Jobs>,
    cmd: String,
    args: Option<serde_json::Value>,
    id: Option<String>,
) -> Result<serde_json::Value, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("catalog");
    let ctx = catalog::install::Ctx {
        store: catalog::Store::new(dir),
        exe: archive::find_7za(),
        tuning: Default::default(),
    };
    let jid = id.unwrap_or_else(|| format!("catalog-{cmd}"));
    let registry = jobs.inner().clone();
    registry.register(&jid);
    let j = registry.clone();
    let jid2 = jid.clone();
    let app2 = app.clone();
    let res = tauri::async_runtime::spawn_blocking(move || {
        let grants = app2.state::<safecopy::Grants>();
        let mut cb = |percent: u32, file: &str| {
            let _ = app.emit(
                "job-progress",
                Progress {
                    id: jid2.clone(),
                    percent,
                    file: file.to_string(),
                },
            );
        };
        catalog::call(
            &ctx,
            &cmd,
            args.unwrap_or(serde_json::json!({})),
            &|p| grants.check(p),
            &mut cb,
            &|| j.is_cancelled(&jid2),
        )
    })
    .await
    .map_err(|e| e.to_string());
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
async fn mod_copy(
    app: AppHandle,
    jobs: State<'_, Jobs>,
    grants: State<'_, safecopy::Grants>,
    id: String,
    path: String,
    target: String,
    mode: String,
) -> Result<mods::ModCopyResult, String> {
    let (path, target) = (grants.check(&path)?, grants.check(&target)?);
    let registry = jobs.inner().clone();
    registry.register(&id);
    let j = registry.clone();
    let jid = id.clone();
    let res = tauri::async_runtime::spawn_blocking(move || {
        let mut cb = |percent: u32, file: &str| {
            let _ = app.emit(
                "job-progress",
                Progress {
                    id: jid.clone(),
                    percent,
                    file: file.to_string(),
                },
            );
        };
        let _dest_lock = destlock::acquire(&target, || cb(0, destlock::WAIT_MESSAGE), &|| {
            j.is_cancelled(&jid)
        })?;
        mods::copy_mod(&path, &target, &mode, &mut cb, &|| j.is_cancelled(&jid))
    })
    .await
    .map_err(|e| e.to_string());
    registry.finish(&id);
    res?
}

#[tauri::command]
async fn dlc_scan_drives(grants: State<'_, safecopy::Grants>) -> Result<Vec<String>, String> {
    let roots = tauri::async_runtime::spawn_blocking(dlc::scan_content_roots)
        .await
        .map_err(|e| e.to_string())?;
    roots.iter().for_each(|r| grants.grant(Path::new(r)));
    Ok(roots)
}

/// Native pickers run from Rust so the picked paths can be granted (Overseer R5 L2).
#[tauri::command]
async fn pick_path(
    app: AppHandle,
    grants: State<'_, safecopy::Grants>,
    kind: String,
    title: String,
) -> Result<Vec<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    let a = app.clone();
    let picked: Vec<PathBuf> = tauri::async_runtime::spawn_blocking(move || {
        let d = a.dialog().file().set_title(&title);
        let v = if kind == "folder" {
            d.blocking_pick_folder().map(|p| vec![p])
        } else {
            d.blocking_pick_files()
        };
        v.unwrap_or_default()
            .into_iter()
            .filter_map(|p| p.into_path().ok())
            .collect()
    })
    .await
    .map_err(|e| e.to_string())?;
    picked.iter().for_each(|p| grants.grant(p));
    Ok(picked.iter().map(|p| p.display().to_string()).collect())
}

#[allow(clippy::too_many_arguments)] // Tauri command surface: one arg per invoke parameter
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
            let base = app
                .path()
                .app_data_dir()
                .map_err(|e| e.to_string())?
                .join("Downloads");
            std::fs::create_dir_all(&base).map_err(|e| e.to_string())?;
            grants.grant(&base);
            base
        }
    };
    let expected_sha256 = download::parse_expected_sha256(expected_sha256.as_deref())?;
    let opts = download::DownloadOpts {
        max_bytes: max_bytes
            .filter(|n| *n > 0)
            .unwrap_or(download::DEFAULT_MAX_BYTES),
        expected_sha256,
    };
    let registry = jobs.inner().clone();
    registry.register(&id);
    let j = registry.clone();
    let jid = id.clone();
    let res = tauri::async_runtime::spawn_blocking(move || {
        let mut cb = |percent: u32, file: &str| {
            let _ = app.emit(
                "job-progress",
                Progress {
                    id: jid.clone(),
                    percent,
                    file: file.to_string(),
                },
            );
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
    let base = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("Downloads");
    std::fs::create_dir_all(&base).map_err(|e| e.to_string())?;
    grants.grant(&base);
    Ok(base.display().to_string())
}

#[tauri::command]
fn list_archives_in_folder(
    grants: State<'_, safecopy::Grants>,
    path: String,
) -> Result<Vec<String>, String> {
    let root = grants.check(&path)?;
    download::list_archives(&root)
}

#[tauri::command]
fn notify_done(app: AppHandle, title: String, body: String) -> Result<(), String> {
    use tauri_plugin_notification::NotificationExt;
    app.notification()
        .builder()
        .title(title)
        .body(body)
        .show()
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn set_taskbar_progress(app: AppHandle, pct: Option<u64>) -> Result<(), String> {
    let Some(w) = app.get_webview_window("main") else {
        return Ok(());
    };
    #[cfg(windows)]
    {
        use tauri::window::{ProgressBarState, ProgressBarStatus};
        if let Some(p) = pct {
            let _ = w.set_progress_bar(ProgressBarState {
                status: Some(ProgressBarStatus::Normal),
                progress: Some(p.min(100)),
            });
        } else {
            let _ = w.set_progress_bar(ProgressBarState {
                status: Some(ProgressBarStatus::None),
                progress: None,
            });
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (w, pct);
    }
    Ok(())
}

#[tauri::command]
async fn pick_save(
    app: AppHandle,
    grants: State<'_, safecopy::Grants>,
    title: String,
    default_path: Option<String>,
) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    let a = app.clone();
    let picked = tauri::async_runtime::spawn_blocking(move || {
        let mut d = a.dialog().file().set_title(&title);
        if let Some(p) = default_path {
            d = d.set_file_name(p);
        }
        d.blocking_save_file().and_then(|p| p.into_path().ok())
    })
    .await
    .map_err(|e| e.to_string())?;
    if let Some(ref p) = picked {
        if let Some(parent) = p.parent() {
            grants.grant(parent);
        }
    }
    Ok(picked.map(|p| p.display().to_string()))
}

#[tauri::command]
async fn iso_info(path: String) -> Result<god::IsoInfo, String> {
    tauri::async_runtime::spawn_blocking(move || god::iso_info(&PathBuf::from(path)))
        .await
        .map_err(|e| e.to_string())?
}

/// Other discs of the same game found next to `path` (same folder, Disc/CD/DVD n naming), so the
/// page can offer to convert them together. Read-only; only inside a granted folder.
#[tauri::command]
async fn iso_companions(
    grants: State<'_, safecopy::Grants>,
    path: String,
) -> Result<Vec<String>, String> {
    let p = grants.check(&path)?;
    tauri::async_runtime::spawn_blocking(move || {
        discs::companions(&p).map(|v| v.into_iter().map(|x| x.display().to_string()).collect())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Missing-disc check: where every disc of this ISO's game is (queued, next to it, in the extract
/// queue, already converted, or missing). Read-only; paths outside granted folders are ignored.
#[tauri::command]
async fn iso_disc_status(
    grants: State<'_, safecopy::Grants>,
    path: String,
    queued: Vec<String>,
    extracting: Vec<String>,
    dest: Option<String>,
) -> Result<discs::DiscStatus, String> {
    let p = grants.check(&path)?;
    let dest = dest.and_then(|d| grants.check(&d).ok());
    let q: Vec<PathBuf> = queued
        .iter()
        .filter_map(|x| grants.check(x).ok())
        .take(256)
        .collect();
    let ex: Vec<String> = extracting
        .into_iter()
        .take(256)
        .map(|n| archive_display_name(&n))
        .collect();
    tauri::async_runtime::spawn_blocking(move || discs::disc_status(&p, &q, &ex, dest.as_deref()))
        .await
        .map_err(|e| e.to_string())
}

/// Just the file name of an archive path (never opened; used for name matching only).
fn archive_display_name(n: &str) -> String {
    n.rsplit(['/', '\\']).next().unwrap_or(n).to_string()
}

#[tauri::command]
async fn iso_to_god(
    app: AppHandle,
    jobs: State<'_, Jobs>,
    grants: State<'_, safecopy::Grants>,
    id: String,
    path: String,
    dest: String,
    title: Option<String>,
) -> Result<god::GodResult, String> {
    let dest = grants.check(&dest)?;
    let registry = jobs.inner().clone();
    registry.register(&id);
    let j = registry.clone();
    let jid = id.clone();
    let res = tauri::async_runtime::spawn_blocking(move || {
        let mut cb = |percent: u32, file: &str| {
            let _ = app.emit(
                "job-progress",
                Progress {
                    id: jid.clone(),
                    percent,
                    file: file.to_string(),
                },
            );
        };
        god::convert_to_dest(
            &PathBuf::from(path),
            &dest,
            title.as_deref(),
            &j,
            &jid,
            &mut cb,
        )
    })
    .await
    .map_err(|e| e.to_string());
    registry.finish(&id);
    res?
}

#[tauri::command]
async fn iso_to_xex(
    app: AppHandle,
    jobs: State<'_, Jobs>,
    grants: State<'_, safecopy::Grants>,
    id: String,
    path: String,
    dest: String,
    title: Option<String>,
) -> Result<iso_extract::ExtractResult, String> {
    let dest = grants.check(&dest)?;
    let registry = jobs.inner().clone();
    registry.register(&id);
    let j = registry.clone();
    let jid = id.clone();
    let res = tauri::async_runtime::spawn_blocking(move || {
        let mut cb = |percent: u32, file: &str| {
            let _ = app.emit(
                "job-progress",
                Progress {
                    id: jid.clone(),
                    percent,
                    file: file.to_string(),
                },
            );
        };
        // Manual: dest is parent; claim named folder inside.
        iso_extract::extract_named(
            &PathBuf::from(path),
            &dest,
            title.as_deref(),
            &j,
            &jid,
            &mut cb,
        )
    })
    .await
    .map_err(|e| e.to_string());
    registry.finish(&id);
    res?
}

#[tauri::command]
async fn xex_extracted(
    app: AppHandle,
    jobs: State<'_, Jobs>,
    id: String,
    token: String,
    delete_iso: bool,
) -> Result<iso_extract::ExtractedXex, String> {
    let registry = jobs.inner().clone();
    registry.register(&id);
    let j = registry.clone();
    let jid = id.clone();
    let res = tauri::async_runtime::spawn_blocking(move || {
        let mut cb = |percent: u32, file: &str| {
            let _ = app.emit(
                "job-progress",
                Progress {
                    id: jid.clone(),
                    percent,
                    file: file.to_string(),
                },
            );
        };
        iso_extract::extract_registered(&token, delete_iso, &j, &jid, &mut cb)
    })
    .await
    .map_err(|e| e.to_string());
    registry.finish(&id);
    res?
}

/// One-time notices left by the first-start migration (R19 L5); empty after the first call.
#[tauri::command]
fn migration_notices() -> Vec<String> {
    migrate::take_notices()
}

#[tauri::command]
fn cancel_job(jobs: State<'_, Jobs>, id: String) -> bool {
    jobs.cancel(&id)
}

#[tauri::command]
fn error_help(message: String) -> hints::ErrorHelp {
    hints::help_for(&message)
}

#[tauri::command]
fn preflight_local(
    dest: String,
    paths: Vec<String>,
    list_cap: Option<u64>,
    auto_iso: Option<String>,
) -> Result<preflight::PreflightReport, String> {
    let dest = PathBuf::from(dest);
    let cap = list_cap.unwrap_or(64 << 20);
    let pbs: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();
    let need = preflight::estimate_local_need(&pbs).min(cap);
    let fat_blocks = preflight::fat_blocks(
        &dest,
        &pbs,
        seven_zip().ok().as_deref(),
        auto_iso.as_deref().unwrap_or("off"),
    );
    Ok(preflight::check_local_dest(&dest, need, &fat_blocks))
}

#[tauri::command]
async fn lan_discover(
    host: Option<String>,
    port: Option<u16>,
) -> Result<Vec<lan::DiscoveredConsole>, String> {
    // One scan at a time; runs off the main thread so the window stays responsive.
    static SCANNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if SCANNING.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return Err("A network scan is already running.".into());
    }
    let port = port.unwrap_or(21);
    let r = tauri::async_runtime::spawn_blocking(move || {
        let base = host
            .and_then(|h| h.trim().parse().ok())
            .or_else(|| {
                lan::local_ipv4()
                    .map(|ip| Ipv4Addr::new(ip.octets()[0], ip.octets()[1], ip.octets()[2], 0))
            })
            .unwrap_or(Ipv4Addr::new(192, 168, 1, 0));
        let skip = lan::local_ipv4();
        lan::discover(base, skip, port)
    })
    .await
    .map_err(|e| e.to_string());
    SCANNING.store(false, std::sync::atomic::Ordering::SeqCst);
    r
}

#[tauri::command]
async fn usb_prepare(
    grants: State<'_, safecopy::Grants>,
    root: String,
) -> Result<usbprep::UsbPrepReport, String> {
    // Only a folder picked in Unpack; the scan for >4 GB files runs off the main thread.
    let root = grants.check(&root)?;
    tauri::async_runtime::spawn_blocking(move || usbprep::prepare_usb(&root))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn god_verify_only(path: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || god::verify_readonly_folder(&PathBuf::from(path)))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn library_index(
    app: AppHandle,
    jobs: State<'_, Jobs>,
    libraries: Vec<String>,
    allow_cover_network: bool,
    max_age_secs: Option<u64>,
    scan_id: Option<String>,
) -> Result<library::LibraryIndex, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("library-cache");
    let libs: Vec<PathBuf> = libraries
        .iter()
        .filter(|s| !s.trim().is_empty())
        .map(PathBuf::from)
        .collect();
    let scan_id = scan_id.unwrap_or_default();
    if !scan_id.is_empty() {
        jobs.register(&scan_id);
    }
    let registry = jobs.inner().clone();
    let jid = scan_id.clone();
    let app_emit = app.clone();
    let res = tauri::async_runtime::spawn_blocking(move || {
        let mut emit = |prog: library::ScanProgress| {
            if jid.is_empty() {
                return;
            }
            let mut p = prog;
            p.id = jid.clone();
            let _ = app_emit.emit("library-scan-progress", p);
        };
        let mut progress: Option<&mut dyn FnMut(library::ScanProgress)> = Some(&mut emit);
        let cancelled = || !jid.is_empty() && registry.is_cancelled(&jid);
        library::index_cached_with_progress(
            &libs,
            &dir,
            allow_cover_network,
            max_age_secs.unwrap_or(3600),
            &mut progress,
            &cancelled,
        )
    })
    .await
    .map_err(|e| e.to_string());
    if !scan_id.is_empty() {
        jobs.finish(&scan_id);
    }
    res?
}

#[tauri::command]
async fn ftp_mkdir(
    host: String,
    port: u16,
    user: String,
    pass: String,
    path: String,
) -> Result<(), String> {
    let con = ftp_console(&host, port, &user, &pass)?;
    tauri::async_runtime::spawn_blocking(move || {
        let _g = ftp_lock("Send to Xbox", "new folder")?;
        let segs = ftp::split_remote(&path)?;
        if segs.len() < 2 {
            return Err("Pick a folder inside a drive, not the drive root.".into());
        }
        if segs
            .last()
            .map(|s| s.eq_ignore_ascii_case("launch.ini"))
            .unwrap_or(false)
        {
            return Err("launch.ini can't be created here.".into());
        }
        let mut f = ftp::Ftp::connect(con.addr, &con.user, &con.pass)?;
        f.mkdirs(&segs, &mut std::collections::HashSet::new())?;
        f.quit();
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn ftp_rename(
    host: String,
    port: u16,
    user: String,
    pass: String,
    from: String,
    to: String,
) -> Result<(), String> {
    let con = ftp_console(&host, port, &user, &pass)?;
    tauri::async_runtime::spawn_blocking(move || {
        let _g = ftp_lock("Send to Xbox", "rename")?;
        let (a, b) = (ftp::split_remote(&from)?, ftp::split_remote(&to)?);
        if a.len() < 2 || b.len() < 2 {
            return Err("Can't rename a drive root.".into());
        }
        for s in [&a, &b] {
            if let Some(m) = ftp::protected_remote(s) {
                return Err(m);
            }
        }
        for s in [&a, &b] {
            if s.iter().any(|x| x.eq_ignore_ascii_case("launch.ini")) {
                return Err("launch.ini can't be renamed from here.".into());
            }
        }
        let mut f = ftp::Ftp::connect(con.addr, &con.user, &con.pass)?;
        f.rename(&a, &b)?;
        f.quit();
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn ftp_delete(
    host: String,
    port: u16,
    user: String,
    pass: String,
    path: String,
) -> Result<(), String> {
    let con = ftp_console(&host, port, &user, &pass)?;
    tauri::async_runtime::spawn_blocking(move || {
        let _g = ftp_lock("Send to Xbox", "delete")?;
        let segs = ftp::split_remote(&path)?;
        if segs.len() < 2 {
            return Err("Can't delete a drive root.".into());
        }
        if let Some(m) = ftp::protected_remote(&segs) {
            return Err(m);
        }
        if segs.iter().any(|x| x.eq_ignore_ascii_case("launch.ini")) {
            return Err("launch.ini can't be deleted from Unpack.".into());
        }
        let mut f = ftp::Ftp::connect(con.addr, &con.user, &con.pass)?;
        if !f.dele(&segs)? {
            return Err("The console refused to delete that file (or it wasn't there).".into());
        }
        f.quit();
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[derive(serde::Deserialize)]
struct SavesBackupArgs {
    host: String,
    port: u16,
    user: String,
    pass: String,
    drive: String,
    profile: String,
    title_id: String,
    dest: String,
}

#[tauri::command]
async fn saves_backup(
    grants: State<'_, safecopy::Grants>,
    args: SavesBackupArgs,
) -> Result<saves::SaveBackupResult, String> {
    let dest = grants.check(&args.dest)?;
    let con = ftp_console(&args.host, args.port, &args.user, &args.pass)?;
    let remote = saves::profile_save_remote(&args.drive, &args.profile, &args.title_id)?;
    tauri::async_runtime::spawn_blocking(move || {
        let _g = ftp_lock("Send to Xbox", "save backup")?;
        saves::backup_folder(&con, &remote, &dest, &mut |_, f| {
            let _ = f;
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

#[derive(serde::Deserialize)]
struct SavesRestoreArgs {
    host: String,
    port: u16,
    user: String,
    pass: String,
    drive: String,
    profile: String,
    title_id: String,
    src: String,
}

#[tauri::command]
async fn saves_restore(
    grants: State<'_, safecopy::Grants>,
    args: SavesRestoreArgs,
) -> Result<saves::SaveRestoreResult, String> {
    let src = grants.check(&args.src)?;
    let con = ftp_console(&args.host, args.port, &args.user, &args.pass)?;
    let remote = saves::profile_save_remote(&args.drive, &args.profile, &args.title_id)?;
    tauri::async_runtime::spawn_blocking(move || {
        let _g = ftp_lock("Send to Xbox", "save restore")?;
        saves::restore_folder(&con, &remote, &src, &mut |_, f| {
            let _ = f;
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

#[derive(serde::Deserialize)]
struct SavesRecoverConsole {
    host: String,
    port: u16,
    user: String,
    pass: String,
}

/// R22 L(a): at startup, repair any save restore that was interrupted on a saved console.
#[tauri::command]
async fn saves_recover(consoles: Vec<SavesRecoverConsole>) -> Result<Vec<String>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut out = vec![];
        for k in consoles.iter().take(10) {
            let Ok(con) = ftp_console(&k.host, k.port, &k.user, &k.pass) else {
                continue;
            };
            if !saves::has_pending(&con) {
                continue;
            }
            let Ok(_g) = ftp_lock("Send to Xbox", "save restore repair") else {
                continue;
            };
            if let Ok(m) = saves::recover_pending(&con) {
                out.extend(m);
            }
        }
        out
    })
    .await
    .map_err(|e| e.to_string())
}

#[derive(serde::Deserialize)]
struct ScreenshotsListArgs {
    host: String,
    port: u16,
    user: String,
    pass: String,
    drive: String,
}

#[tauri::command]
async fn screenshots_list(args: ScreenshotsListArgs) -> Result<Vec<saves::ScreenshotFile>, String> {
    let con = ftp_console(&args.host, args.port, &args.user, &args.pass)?;
    let drive = args.drive;
    tauri::async_runtime::spawn_blocking(move || {
        let _g = ftp_lock("Send to Xbox", "screenshot list")?;
        saves::list_screenshots(&con, &drive)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[derive(serde::Deserialize)]
struct ScreenshotsPullArgs {
    host: String,
    port: u16,
    user: String,
    pass: String,
    shots: Vec<saves::ScreenshotFile>,
    dest: String,
}

#[tauri::command]
async fn screenshots_pull(
    grants: State<'_, safecopy::Grants>,
    args: ScreenshotsPullArgs,
) -> Result<saves::SaveBackupResult, String> {
    let dest = grants.check(&args.dest)?;
    let con = ftp_console(&args.host, args.port, &args.user, &args.pass)?;
    let shots = args.shots;
    tauri::async_runtime::spawn_blocking(move || {
        let _g = ftp_lock("Send to Xbox", "screenshot pull")?;
        saves::pull_screenshots(&con, &shots, &dest, &mut |_, f| {
            let _ = f;
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
fn save_text_file(
    grants: State<'_, safecopy::Grants>,
    path: String,
    text: String,
) -> Result<String, String> {
    safecopy::write_export_text(&grants, &path, &text)
}

#[tauri::command]
async fn check_release(current: String, opt_in: bool) -> Result<download::ReleaseInfo, String> {
    if !opt_in {
        return Ok(download::ReleaseInfo {
            available: false,
            latest: current,
            url: String::new(),
        });
    }
    tauri::async_runtime::spawn_blocking(move || download::check_github_release(&current))
        .await
        .map_err(|e| e.to_string())?
}

pub fn run() {
    // Before Tauri or the WebView touch their folders: carry data over from the old identifier.
    for d in migrate::run_startup() {
        eprintln!("migrated app data into {}", d.display());
    }
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
            if let Some(req) = parse_args(
                &argv.into_iter().skip(1).collect::<Vec<_>>(),
                std::path::Path::new(&cwd),
            ) {
                // Queue it too, in case the page hasn't attached its listener yet.
                app.state::<PendingLaunch>()
                    .0
                    .lock()
                    .unwrap()
                    .push(req.clone());
                let _ = app.emit("launch-request", ());
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_opener::init())
        .manage(pending)
        .manage(Jobs::default())
        .setup(|app| {
            let dir = app
                .path()
                .app_data_dir()
                .unwrap_or_else(|_| std::env::temp_dir().join("Unpack"));
            let grants = safecopy::Grants::with_file(dir.join("grants.json"));
            ftp::set_resume_dir(dir.join("ftp-resume"));
            // Built-in menus (ships empty): validated once; their files are granted for this
            // session so Copy to USB / Send to Xbox work on them. Read-only in the UI.
            let menus = app
                .path()
                .resource_dir()
                .map(|d| d.join("menus"))
                .map(|d| mods::load_builtin(&d))
                .unwrap_or_default();
            for p in &menus.problems {
                eprintln!("built-in menus: skipped {p}");
            }
            for m in &menus.menus {
                grants.grant_session(Path::new(&m.path));
            }
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
                        if safecopy::is_link(&m) {
                            continue;
                        }
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
        .invoke_handler(tauri::generate_handler![
            native_info,
            file_info,
            list_archive,
            read_entry,
            extract_archive,
            extract_entry,
            read_file,
            cancel_job,
            take_launch_requests,
            create_archive,
            iso_info,
            iso_companions,
            iso_disc_status,
            iso_to_god,
            iso_to_xex,
            god_extracted,
            xex_extracted,
            god_verify_only,
            dlc_install,
            dlc_match_games,
            dlc_scan_drives,
            library_scan,
            library_index,
            copy_to_usb,
            mod_identify,
            mod_name_key,
            mod_copy,
            game_search,
            pick_path,
            pick_save,
            download_url,
            download_folder,
            list_archives_in_folder,
            notify_done,
            set_taskbar_progress,
            ftp_list,
            ftp_send,
            ftp_dest,
            ftp_mkdir,
            ftp_rename,
            ftp_delete,
            ftp_lock_status,
            saves_backup,
            saves_restore,
            saves_recover,
            screenshots_list,
            screenshots_pull,
            builtin_menus,
            catalog,
            discord_set_activity,
            discord_set_enabled,
            migration_notices,
            error_help,
            preflight_local,
            lan_discover,
            save_text_file,
            usb_prepare,
            check_release
        ])
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

    /// Bully case: a 7 GB ISO + a small .txt bound for a FAT32 drive with Auto ISO → GOD.
    /// The .txt lands on the destination, the ISO waits in staging and points its output at the
    /// destination, and the staging folder is gone once the ISO has been taken for conversion.
    #[test]
    fn fat_stage_moves_small_files_and_cleans_up_staging() {
        let t = crate::testutil::TmpDir::new("cs31-fat-stage");
        let dest = t.path().join("Content").join("0000000000000000");
        let root = t.path().join("tmp").join(fatstage::STAGE_DIR);
        std::fs::create_dir_all(&dest).unwrap();
        let dir = archive::claim_dir(
            &{
                std::fs::create_dir_all(&root).unwrap();
                root.clone()
            },
            "job-1",
        )
        .unwrap();
        let out = dir.join("Bully (USA)");
        std::fs::create_dir_all(&out).unwrap();
        let iso = out.join("Bully (USA).iso");
        std::fs::File::create(&iso)
            .unwrap()
            .set_len(7_000_000_000)
            .unwrap(); // sparse
        std::fs::write(out.join("readme.txt"), b"hi").unwrap();
        let isos = vec![iso.display().to_string()];
        let tokens = god::register_extracted(&isos, &out, true);
        let res = archive::ExtractResult {
            output: out.display().to_string(),
            skipped: false,
            warnings: vec![],
            files: 2,
            isos,
            merged: false,
            iso_tokens: tokens.clone(),
            dlcs: vec![],
            created: true,
            nested: 0,
            inner_left: 0,
        };
        let st = FatStage {
            dir: dir.clone(),
            root: root.clone(),
            isos: vec!["Bully (USA).iso".into()],
        };
        let r = finish_fat_stage(Ok(res), &st, &dest).unwrap();
        // 1.7.3: bound for a Content profile, the readme is skipped (no stray folder next to
        // Content) and the summary says so.
        let outside = t.path().join("Bully (USA)");
        assert!(!outside.exists(), "no leftover folder next to Content");
        assert_eq!(std::fs::read_dir(&dest).unwrap().count(), 0, "profile stays clean");
        assert_eq!(PathBuf::from(&r.output), dest);
        assert!(r.warnings.iter().any(|w| w.contains("Skipped 1 extra file") && w.contains("readme.txt")), "{:?}", r.warnings);
        assert!(iso.exists() && god::stage_in_use(&dir));
        assert!(
            r.warnings.iter().any(|w| w.contains("temporary")),
            "{:?}",
            r.warnings
        );
        let (_, root2, del, _, _, guard) = god::take_extracted_token(&r.iso_tokens[0]).unwrap();
        assert!(!del);
        assert_eq!(god::out_parent_for(&root2, &guard), dest);
        drop(guard); // conversion finished, failed or was cancelled
        assert!(!iso.exists() && !dir.exists(), "staging must be removed");
    }

    /// 1.7.3: a real game file next to the big ISO keeps the safe placement (next to Content);
    /// only the extras are dropped.
    #[test]
    fn fat_stage_keeps_game_files_drops_extras() {
        let t = crate::testutil::TmpDir::new("cs32-fat-extras");
        let dest = t.path().join("Content").join("0000000000000000");
        std::fs::create_dir_all(&dest).unwrap();
        let root = t.path().join("tmp").join(fatstage::STAGE_DIR);
        std::fs::create_dir_all(&root).unwrap();
        let dir = archive::claim_dir(&root, "job-9").unwrap();
        let out = dir.join("Game");
        std::fs::create_dir_all(out.join("art")).unwrap();
        std::fs::create_dir_all(out.join("Tools")).unwrap();
        let iso = out.join("Game.iso");
        std::fs::File::create(&iso).unwrap().set_len(5_000_000_000).unwrap();
        std::fs::write(out.join("Tools").join("default.xex"), b"XEX2").unwrap();
        std::fs::write(out.join("Tools").join("readme.txt"), b"keep").unwrap();
        std::fs::write(out.join("Vimm's Lair.txt"), b"x").unwrap();
        std::fs::write(out.join("art").join("cover.JPG"), b"x").unwrap();
        let isos = vec![iso.display().to_string()];
        let tokens = god::register_extracted(&isos, &out, true);
        let res = archive::ExtractResult {
            output: out.display().to_string(),
            skipped: false,
            warnings: vec![],
            files: 4,
            isos,
            merged: false,
            iso_tokens: tokens.clone(),
            dlcs: vec![],
            created: true,
            nested: 0,
            inner_left: 0,
        };
        let st = FatStage { dir: dir.clone(), root, isos: vec!["Game.iso".into()] };
        let r = finish_fat_stage(Ok(res), &st, &dest).unwrap();
        let outside = t.path().join("Game");
        assert_eq!(std::fs::read(outside.join("Tools").join("default.xex")).unwrap(), b"XEX2");
        assert_eq!(std::fs::read(outside.join("Tools").join("readme.txt")).unwrap(), b"keep", "R33: inside a game folder nothing goes");
        assert!(!outside.join("Vimm's Lair.txt").exists() && !outside.join("art").exists());
        assert!(r.warnings.iter().any(|w| w.contains("Skipped 2 extra files")), "{:?}", r.warnings);
        let (_, _, _, _, _, guard) = god::take_extracted_token(&tokens[0]).unwrap();
        drop(guard);
        assert!(!dir.exists());
    }

    #[test]
    fn fat_stage_plain_folder_dest_keeps_files_in_dest() {
        assert_eq!(leftover_parent(Path::new("/x/Games")), PathBuf::from("/x/Games"));
        assert_eq!(leftover_parent(Path::new("/usb/Content/0000000000000000")), PathBuf::from("/usb"));
        assert_eq!(leftover_parent(Path::new("/usb/Content")), PathBuf::from("/usb"));
    }

    #[test]
    fn fat_stage_failure_removes_staging() {
        let t = crate::testutil::TmpDir::new("cs31-fat-stage-fail");
        let dir = t.path().join("job-2");
        std::fs::create_dir_all(dir.join("x")).unwrap();
        let st = FatStage {
            dir: dir.clone(),
            root: t.path().into(),
            isos: vec![],
        };
        assert!(finish_fat_stage(Err("CANCELLED".into()), &st, t.path()).is_err());
        assert!(!dir.exists());
    }
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
        let r = parse_args(
            &[
                "--extract-here".into(),
                "--".into(),
                "-dash-launch-test.zip".into(),
            ],
            &d,
        )
        .unwrap();
        assert_eq!((r.action.as_str(), r.paths.len()), ("extract-here", 1));
        assert!(parse_args(&["-dash-launch-test.zip".into()], &d).is_none());
        let _ = std::fs::remove_file(dash);
        let _ = std::fs::remove_file(&f);
    }
}
