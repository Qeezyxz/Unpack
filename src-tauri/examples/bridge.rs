//! TEST HARNESS ONLY (not shipped): serves ../src over HTTP and exposes the same
//! command names as the Tauri app, backed by the real `archive` module, so the
//! frontend can be driven end-to-end in headless Chrome on Linux.
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tiny_http::{Header, Response, Server};
use unpack_lib::archive::{self, Collision, ExtractOptions, Jobs, Layout};

fn app_csp() -> String {
    let conf: serde_json::Value = serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
    conf["app"]["security"]["csp"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| format!("{k} {}", v.as_str().unwrap()))
        .collect::<Vec<_>>()
        .join("; ")
}

fn main() {
    // Headless UI tests use loopback FTP fixtures and local HTTP download servers.
    std::env::set_var("UNPACK_TEST_ALLOW_LOOPBACK", "1");
    unpack_lib::ftp::set_resume_dir(std::env::temp_dir().join("unpack-bridge-ftp-resume"));
    let root = PathBuf::from(std::env::args().nth(1).unwrap_or("../src".into()));
    let port = std::env::var("UNPACK_BRIDGE_PORT").unwrap_or_else(|_| "18790".into());
    let server = Arc::new(Server::http(format!("127.0.0.1:{port}")).unwrap());
    let events: Arc<Mutex<Vec<Value>>> = Default::default();
    let jobs = Jobs::default();
    let exe = archive::find_7za().expect("7za");
    eprintln!("bridge on http://127.0.0.1:{port}");
    for mut req in server.incoming_requests() {
        let url = req.url().to_string();
        let events = events.clone();
        let jobs = jobs.clone();
        let exe = exe.clone();
        let root = root.clone();
        std::thread::spawn(move || {
            if let Some(cmd) = url.strip_prefix("/invoke/") {
                let mut body = String::new();
                req.as_reader().read_to_string(&mut body).ok();
                let a: Value = serde_json::from_str(&body).unwrap_or(json!({}));
                let s = |k: &str| a[k].as_str().unwrap_or("").to_string();
                let pw = a["password"].as_str().map(|s| s.to_string());
                let res: Result<Value, String> = match cmd {
                    "native_info" => Ok(json!({"available": true, "path": exe.display().to_string()})),
                    "file_info" => std::fs::metadata(s("path")).map_err(|e| e.to_string()).map(|m| {
                        json!({"path": s("path"), "name": PathBuf::from(s("path")).file_name().unwrap().to_string_lossy(), "size": m.len(), "is_dir": m.is_dir()})
                    }),
                    "list_archive" => archive::list(&exe, &PathBuf::from(s("path")), pw.as_deref()).map(|l| serde_json::to_value(l).unwrap()),
                    "read_entry" => archive::read_entry(&exe, &PathBuf::from(s("path")), &s("entry"), pw.as_deref(), 4 << 20).map(|b| json!(b)),
                    "cancel_job" => Ok(json!(jobs.cancel(&s("id")))),
                    "extract_archive" => {
                        let id = s("id");
                        jobs.register(&id);
                        let (ap, dp) = (PathBuf::from(s("path")), PathBuf::from(s("dest")));
                        let opts = ExtractOptions {
                            archive: &ap, dest_parent: &dp, password: pw.as_deref(),
                            collision: Collision::parse(&s("collision")),
                            layout: if s("layout") == "here" { Layout::Here } else { Layout::Folder },
                            skip_junk: a["skipJunk"].as_bool().unwrap_or(true),
                            nested: a["nested"].as_bool().unwrap_or(false),
                        };
                        let r = archive::extract(&exe, &opts, &jobs, &id, &mut |p, f| {
                            events.lock().unwrap().push(json!({"event":"job-progress","payload":{"id": id, "percent": p, "file": f}}));
                        });
                        if let (Ok(res), Some(true)) = (&r, a["sealIsos"].as_bool()) { unpack_lib::god::seal_tokens(&res.iso_tokens); }
                        jobs.finish(&id);
                        if let Ok(res) = &r { if res.created && !res.skipped { GRANTS.grant_session(std::path::Path::new(&res.output)); } }
                        r.map(|r| serde_json::to_value(r).unwrap())
                    }
                    other => extra(other, &a, &exe, &jobs, &events),
                };
                let (code, body) = match res {
                    Ok(v) => (200, v.to_string()),
                    Err(e) => (500, json!(e).to_string()),
                };
                let _ = req.respond(
                    Response::from_string(body)
                        .with_status_code(code)
                        .with_header(
                            Header::from_bytes("content-type", "application/json").unwrap(),
                        ),
                );
            } else if let Some(since) = url.strip_prefix("/events?since=") {
                let n: usize = since.parse().unwrap_or(0);
                let ev = events.lock().unwrap();
                let slice: Vec<Value> = ev.iter().skip(n).cloned().collect();
                let _ = req.respond(Response::from_string(
                    json!({"next": ev.len(), "events": slice}).to_string(),
                ));
            } else {
                let path = url.split('?').next().unwrap();
                let path = if path == "/" { "/index.html" } else { path };
                let f = root.join(path.trim_start_matches('/'));
                let ct = match f.extension().and_then(|e| e.to_str()) {
                    Some("html") => "text/html",
                    Some("js") => "text/javascript",
                    Some("css") => "text/css",
                    Some("wasm") => "application/wasm",
                    Some("woff2") => "font/woff2",
                    _ => "application/octet-stream",
                };
                match std::fs::read(&f) {
                    Ok(b) => {
                        // serve the page under the exact CSP from tauri.conf.json (no unsafe-eval)
                        let resp = Response::from_data(b)
                            .with_header(Header::from_bytes("content-type", ct).unwrap());
                        let resp = if ct == "text/html" {
                            resp.with_header(
                                Header::from_bytes("content-security-policy", app_csp()).unwrap(),
                            )
                        } else {
                            resp
                        };
                        let _ = req.respond(resp);
                    }
                    Err(_) => {
                        let _ = req.respond(Response::empty(404));
                    }
                }
            }
        });
    }
}

static DISCORD: Mutex<Vec<Value>> = Mutex::new(Vec::new());
static LAUNCH: Mutex<Vec<Value>> = Mutex::new(Vec::new());
// Same path-grant gate as the app (Overseer R5 L2). The page's pickers are faked by the test shim,
// which calls __test_grant with what it "picked".
static GRANTS: std::sync::LazyLock<unpack_lib::safecopy::Grants> =
    std::sync::LazyLock::new(Default::default);
static PICKED: std::sync::LazyLock<std::sync::Mutex<unpack_lib::picked_folders::PickedFolders>> =
    std::sync::LazyLock::new(|| {
        let d = std::env::temp_dir().join("unpack-bridge-picked");
        std::sync::Mutex::new(unpack_lib::picked_folders::PickedFolders::load(d))
    });
static LAST_SCAN: std::sync::LazyLock<unpack_lib::picked_folders::LastScannedRoots> =
    std::sync::LazyLock::new(unpack_lib::picked_folders::LastScannedRoots::default);
// Same as the app's setup: built-in menus from UNPACK_TEST_MENUS (fixtures) or resources/menus.
static BUILTIN: std::sync::LazyLock<unpack_lib::mods::BuiltinSet> =
    std::sync::LazyLock::new(|| {
        let d = std::env::var("UNPACK_TEST_MENUS")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("resources/menus"));
        let s = unpack_lib::mods::load_builtin(&d);
        for m in &s.menus {
            GRANTS.grant_session(std::path::Path::new(&m.path));
        }
        s
    });

#[allow(unused_variables)]
fn extra(
    cmd: &str,
    a: &Value,
    exe: &std::path::Path,
    jobs: &Jobs,
    events: &Arc<Mutex<Vec<Value>>>,
) -> Result<Value, String> {
    match cmd {
        "take_launch_requests" => Ok(Value::Array(std::mem::take(&mut *LAUNCH.lock().unwrap()))),
        // test hook: simulate a second launch forwarding its arguments
        "__push_launch" => {
            let args: Vec<String> = a["args"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().to_string())
                .collect();
            if let Some(r) = unpack_lib::parse_args(&args, std::path::Path::new("/")) {
                LAUNCH
                    .lock()
                    .unwrap()
                    .push(json!({"action": r.action, "paths": r.paths}));
                events
                    .lock()
                    .unwrap()
                    .push(json!({"event": "launch-request", "payload": null}));
            }
            Ok(json!(true))
        }
        "create_archive" => {
            let id = a["id"].as_str().unwrap().to_string();
            jobs.register(&id);
            let srcs: Vec<PathBuf> = a["sources"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| PathBuf::from(v.as_str().unwrap()))
                .collect();
            let r = archive::create(
                exe,
                &srcs,
                &PathBuf::from(a["outFile"].as_str().unwrap()),
                a["format"].as_str().unwrap(),
                a["password"].as_str(),
                a["level"].as_u64().unwrap_or(5) as u32,
                jobs,
                &id,
                &mut |p, f| {
                    events.lock().unwrap().push(json!({"event":"job-progress","payload":{"id": id, "percent": p, "file": f}}));
                },
            );
            jobs.finish(&id);
            r.map(|v| json!(v))
        }
        "iso_info" => unpack_lib::god::iso_info(&PathBuf::from(a["path"].as_str().unwrap()))
            .map(|v| serde_json::to_value(v).unwrap()),
        "iso_disc_status" => {
            let p = GRANTS.check(a["path"].as_str().unwrap_or(""))?;
            let list = |k: &str| {
                a[k].as_array()
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect::<Vec<_>>()
            };
            let q: Vec<PathBuf> = list("queued")
                .iter()
                .filter_map(|x| GRANTS.check(x).ok())
                .collect();
            let ex: Vec<String> = list("extracting")
                .iter()
                .map(|n| n.rsplit(['/', '\\']).next().unwrap_or(n).to_string())
                .collect();
            Ok(serde_json::to_value(unpack_lib::discs::disc_status(
                &p,
                &q,
                &ex,
                a["dest"]
                    .as_str()
                    .and_then(|d| GRANTS.check(d).ok())
                    .as_deref(),
            ))
            .unwrap())
        }
        "iso_companions" => {
            let p = GRANTS.check(a["path"].as_str().unwrap_or(""))?;
            unpack_lib::discs::companions(&p).map(|v| {
                json!(v
                    .into_iter()
                    .map(|x| x.display().to_string())
                    .collect::<Vec<_>>())
            })
        }
        // Discord is a local IPC the headless harness can't reach: the bridge builds the same
        // text as the app and records it, so tests can assert what would be shown (and that it stops when off).
        "discord_set_activity" => {
            let t = unpack_lib::discord::activity_for(
                a["view"].as_str().unwrap_or(""),
                a["job"].as_str(),
            );
            DISCORD
                .lock()
                .unwrap()
                .push(json!({"call": "activity", "details": t.details, "state": t.state}));
            Ok(Value::Null)
        }
        "discord_set_enabled" => {
            DISCORD.lock().unwrap().push(
                json!({"call": "enabled", "enabled": a["enabled"].as_bool().unwrap_or(false)}),
            );
            Ok(Value::Null)
        }
        // The harness can't run a real migration: tests plant the notice file in a temp folder.
        "paths_are_granted" => {
            let paths = a["paths"]
                .as_array()
                .map(|v| {
                    v.iter()
                        .filter_map(|x| x.as_str())
                        .map(|p| GRANTS.is_granted(p))
                        .collect::<Vec<bool>>()
                })
                .unwrap_or_default();
            Ok(json!(paths))
        }
        "migration_notices" => Ok(json!(std::env::var_os("UNPACK_TEST_NOTICE_DIR")
            .and_then(|d| unpack_lib::migrate::take_notice_in(std::path::Path::new(&d)))
            .into_iter()
            .collect::<Vec<_>>())),
        "__plant_migration_notice" => {
            let d = std::env::var_os("UNPACK_TEST_NOTICE_DIR")
                .ok_or("UNPACK_TEST_NOTICE_DIR not set")?;
            std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
            std::fs::write(
                std::path::Path::new(&d).join(unpack_lib::migrate::NOTICE),
                "C:\\Users\\me\\AppData\\Local\\com.qeezyxz.unpack\\EBWebView.old-version",
            )
            .map_err(|e| e.to_string())?;
            Ok(Value::Null)
        }
        "__discord_calls" => Ok(Value::Array(DISCORD.lock().unwrap().clone())),
        "iso_to_god" => {
            let id = a["id"].as_str().unwrap().to_string();
            jobs.register(&id);
            let mut cb = |p: u32, f: &str| {
                events.lock().unwrap().push(
                    json!({"event":"job-progress","payload":{"id": id, "percent": p, "file": f}}),
                );
            };
            let path = PathBuf::from(a["path"].as_str().unwrap());
            // Same rule as the app (lib.rs iso_to_god): dest must be a folder the user picked.
            let dest = match GRANTS.check(a["dest"].as_str().unwrap_or("")) {
                Ok(d) => d,
                Err(e) => {
                    jobs.finish(&id);
                    return Err(e);
                }
            };
            let r = unpack_lib::god::convert_to_dest(
                &path,
                &dest,
                a["title"].as_str(),
                jobs,
                &id,
                &mut cb,
            );
            jobs.finish(&id);
            r.map(|v| serde_json::to_value(v).unwrap())
        }
        "iso_to_xex" => {
            let id = a["id"].as_str().unwrap().to_string();
            jobs.register(&id);
            let mut cb = |p: u32, f: &str| {
                events.lock().unwrap().push(
                    json!({"event":"job-progress","payload":{"id": id, "percent": p, "file": f}}),
                );
            };
            let path = PathBuf::from(a["path"].as_str().unwrap());
            let dest = match GRANTS.check(a["dest"].as_str().unwrap_or("")) {
                Ok(d) => d,
                Err(e) => {
                    jobs.finish(&id);
                    return Err(e);
                }
            };
            let r = unpack_lib::iso_extract::extract_named(
                &path,
                &dest,
                a["title"].as_str(),
                jobs,
                &id,
                &mut cb,
            );
            jobs.finish(&id);
            if let Ok(ref res) = r {
                GRANTS.grant(std::path::Path::new(&res.output));
            }
            r.map(|v| serde_json::to_value(v).unwrap())
        }
        "xex_extracted" => {
            let id = a["id"].as_str().unwrap().to_string();
            jobs.register(&id);
            let mut cb = |p: u32, f: &str| {
                events.lock().unwrap().push(
                    json!({"event":"job-progress","payload":{"id": id, "percent": p, "file": f}}),
                );
            };
            let r = unpack_lib::iso_extract::extract_registered(
                a["token"].as_str().unwrap_or(""),
                a["deleteIso"].as_bool().unwrap_or(false),
                jobs,
                &id,
                &mut cb,
            );
            jobs.finish(&id);
            if let Ok(ref res) = r {
                GRANTS.grant(std::path::Path::new(&res.output));
            }
            r.map(|v| serde_json::to_value(v).unwrap())
        }
        "__test_grant" => {
            // Mirror the app: folders go through picked-folders; files grant only that path (not the parent).
            for p in a["paths"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .filter_map(|x| x.as_str())
            {
                let path = std::path::PathBuf::from(p);
                if path.is_dir() {
                    let _ = PICKED
                        .lock()
                        .unwrap()
                        .record_dialog_pick(&GRANTS, &path);
                } else {
                    GRANTS.grant(&path);
                }
            }
            Ok(json!(true))
        }
        "record_user_folder" => {
            let p = a["path"].as_str().unwrap_or("");
            LAST_SCAN.require_scanned_root(std::path::Path::new(p))?;
            PICKED
                .lock()
                .unwrap()
                .record_dialog_pick(&GRANTS, std::path::Path::new(p))
                .map(|_| json!(null))
        }
        "session_drop_prepare" | "__test_drop_grant" => {
            let paths: Vec<PathBuf> = a["paths"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .filter_map(|x| x.as_str())
                .map(PathBuf::from)
                .collect();
            let (allowed, refused) =
                unpack_lib::picked_folders::prepare_session_drop_paths(&paths, &GRANTS);
            if let Some(msg) = refused {
                events.lock().unwrap().push(json!({"event":"path-grant-refused","payload": msg}));
            }
            Ok(json!(
                allowed
                    .iter()
                    .map(|p| p.to_string_lossy().to_string())
                    .collect::<Vec<_>>()
            ))
        }
        "dlc_install" => {
            let libs: Vec<PathBuf> = a["libraries"]
                .as_array()
                .map(|v| {
                    v.iter()
                        .filter_map(|x| x.as_str())
                        .filter(|s| !s.is_empty())
                        .filter_map(|l| GRANTS.check(l).ok())
                        .collect()
                })
                .unwrap_or_default();
            {
                let gp = a["gamePath"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .map(|s| GRANTS.check(s))
                    .transpose()?;
                unpack_lib::dlc::install_dlc(
                    &GRANTS.check(a["path"].as_str().unwrap_or(""))?,
                    &libs,
                    a["moveFile"].as_bool().unwrap_or(false),
                    gp.as_deref(),
                )
                .map(|v| serde_json::to_value(v).unwrap())
            }
        }
        "library_scan" => {
            let libs: Vec<PathBuf> = a["libraries"]
                .as_array()
                .map(|v| {
                    v.iter()
                        .filter_map(|x| x.as_str())
                        .filter(|s| !s.is_empty())
                        .map(|s| GRANTS.check(s))
                        .collect::<Result<Vec<_>, _>>()
                })
                .transpose()?
                .unwrap_or_default();
            Ok(serde_json::to_value(unpack_lib::library::scan(&libs)).unwrap())
        }
        "copy_to_usb" => {
            let id = a["id"].as_str().unwrap_or("").to_string();
            jobs.register(&id);
            let j = jobs.clone();
            let r = unpack_lib::library::copy_to_usb(
                &GRANTS.check(a["game"].as_str().unwrap_or(""))?,
                &GRANTS.check(a["target"].as_str().unwrap_or(""))?,
                &mut |p, f| {
                    events.lock().unwrap().push(json!({"event":"job-progress","payload":{"id": id, "percent": p, "file": f}}));
                },
                &|| j.is_cancelled(&id),
            );
            jobs.finish(&id);
            r.map(|v| serde_json::to_value(v).unwrap())
        }
        "mod_identify" => {
            let learned = unpack_lib::mods::merge_learned(
                serde_json::from_value::<Vec<unpack_lib::modnames::Learned>>(a["learned"].clone())
                    .unwrap_or_default(),
                &BUILTIN.learned,
            );
            let learned = unpack_lib::mods::merge_learned(learned, &unpack_lib::catalog::learned());
            let v: Vec<Value> = a["paths"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .filter_map(|x| x.as_str())
                .enumerate()
                .map(|(i, p)| {
                    match unpack_lib::mods::check_mod_path(&GRANTS, p).and_then(|p| {
                        unpack_lib::mods::identify_learned(
                            &p,
                            a["hints"]
                                .get(i)
                                .and_then(|h| h.as_str())
                                .filter(|h| !h.is_empty()),
                            &learned,
                        )
                    }) {
                        Ok(m) => json!({"Ok": m}),
                        Err(e) => json!({"Err": e}),
                    }
                })
                .collect();
            Ok(json!(v))
        }
        "mod_name_key" => unpack_lib::modnames::learn_key(
            a["name"].as_str().unwrap_or(""),
            a["tid"].as_str().unwrap_or(""),
        )
        .map(|(key, tid, game)| json!({"key": key, "tid": tid, "game": game})),
        "game_search" => Ok(serde_json::to_value(unpack_lib::gamedb::search(
            a["q"].as_str().unwrap_or(""),
            40,
        ))
        .unwrap()),
        "mod_copy" => {
            let id = a["id"].as_str().unwrap_or("").to_string();
            jobs.register(&id);
            let j = jobs.clone();
            let r = unpack_lib::mods::copy_mod(
                &GRANTS.check(a["path"].as_str().unwrap_or(""))?,
                &GRANTS.check(a["target"].as_str().unwrap_or(""))?,
                a["mode"].as_str().unwrap_or(""),
                &mut |p, f| {
                    events.lock().unwrap().push(json!({"event":"job-progress","payload":{"id": id, "percent": p, "file": f}}));
                },
                &|| j.is_cancelled(&id),
            );
            jobs.finish(&id);
            r.map(|v| serde_json::to_value(v).unwrap())
        }
        "builtin_menus" => Ok(serde_json::to_value(&*BUILTIN).unwrap()),
        "ftp_list" => {
            let con = unpack_lib::ftp_console(
                a["host"].as_str().unwrap_or(""),
                a["port"].as_u64().unwrap_or(21) as u16,
                a["user"].as_str().unwrap_or(""),
                a["pass"].as_str().unwrap_or(""),
            )?;
            unpack_lib::ftp_list_blocking(con, a["path"].as_str().unwrap_or("/").to_string())
                .map(|v| json!(v))
        }
        "ftp_dest" => {
            let args: unpack_lib::FtpSendArgs =
                serde_json::from_value(a["args"].clone()).map_err(|e| e.to_string())?;
            unpack_lib::ftp_dest_for(&args, std::path::Path::new(&args.src))
                .map(|(d, c)| json!({"path": unpack_lib::ftp::join_remote(&d), "contents_only": c}))
        }
        "saves_backup" => {
            let dest = GRANTS.check(a["dest"].as_str().unwrap_or(""))?;
            let con = unpack_lib::ftp_console(
                a["host"].as_str().unwrap_or(""),
                a["port"].as_u64().unwrap_or(21) as u16,
                a["user"].as_str().unwrap_or(""),
                a["pass"].as_str().unwrap_or(""),
            )?;
            let remote = unpack_lib::saves::profile_save_remote(
                a["drive"].as_str().unwrap_or("Hdd1"),
                a["profile"].as_str().unwrap_or(""),
                a["title_id"].as_str().unwrap_or(""),
            )?;
            unpack_lib::saves::backup_folder(&con, &remote, &dest, &mut |_, _| {})
                .map(|v| serde_json::to_value(v).unwrap())
        }
        "saves_restore" => {
            let src = GRANTS.check(a["src"].as_str().unwrap_or(""))?;
            let con = unpack_lib::ftp_console(
                a["host"].as_str().unwrap_or(""),
                a["port"].as_u64().unwrap_or(21) as u16,
                a["user"].as_str().unwrap_or(""),
                a["pass"].as_str().unwrap_or(""),
            )?;
            let remote = unpack_lib::saves::profile_save_remote(
                a["drive"].as_str().unwrap_or("Hdd1"),
                a["profile"].as_str().unwrap_or(""),
                a["title_id"].as_str().unwrap_or(""),
            )?;
            unpack_lib::saves::restore_folder(&con, &remote, &src, &mut |_, _| {})
                .map(|v| serde_json::to_value(v).unwrap())
        }
        "saves_recover" => {
            let mut out = vec![];
            for k in a["consoles"].as_array().cloned().unwrap_or_default() {
                let Ok(con) = unpack_lib::ftp_console(
                    k["host"].as_str().unwrap_or(""),
                    k["port"].as_u64().unwrap_or(21) as u16,
                    k["user"].as_str().unwrap_or(""),
                    k["pass"].as_str().unwrap_or(""),
                ) else {
                    continue;
                };
                if unpack_lib::saves::has_pending(&con) {
                    out.extend(unpack_lib::saves::recover_pending(&con).unwrap_or_default());
                }
            }
            Ok(json!(out))
        }
        "screenshots_list" => {
            let con = unpack_lib::ftp_console(
                a["host"].as_str().unwrap_or(""),
                a["port"].as_u64().unwrap_or(21) as u16,
                a["user"].as_str().unwrap_or(""),
                a["pass"].as_str().unwrap_or(""),
            )?;
            let drive = a["drive"].as_str().unwrap_or("Hdd1").to_string();
            unpack_lib::saves::list_screenshots(&con, &drive)
                .map(|v| serde_json::to_value(v).unwrap())
        }
        "screenshots_pull" => {
            let dest = GRANTS.check(a["dest"].as_str().unwrap_or(""))?;
            let shots: Vec<unpack_lib::saves::ScreenshotFile> =
                serde_json::from_value(a["shots"].clone()).map_err(|e| e.to_string())?;
            let con = unpack_lib::ftp_console(
                a["host"].as_str().unwrap_or(""),
                a["port"].as_u64().unwrap_or(21) as u16,
                a["user"].as_str().unwrap_or(""),
                a["pass"].as_str().unwrap_or(""),
            )?;
            unpack_lib::saves::pull_screenshots(&con, &shots, &dest, &mut |_, _| {})
                .map(|v| serde_json::to_value(v).unwrap())
        }
        "ftp_send" => {
            let id = a["id"].as_str().unwrap_or("").to_string();
            let args: unpack_lib::FtpSendArgs =
                serde_json::from_value(a["args"].clone()).map_err(|e| e.to_string())?;
            let src = GRANTS.check(&args.src)?;
            jobs.register(&id);
            let j = jobs.clone();
            let r = unpack_lib::ftp_send_blocking(
                &args,
                &src,
                &mut |p, f| {
                    events.lock().unwrap().push(json!({"event":"job-progress","payload":{"id": id, "percent": p, "file": f}}));
                },
                &|| j.is_cancelled(&id),
            );
            jobs.finish(&id);
            r.map(|v| serde_json::to_value(v).unwrap())
        }
        "catalog" => {
            let id = a["id"]
                .as_str()
                .map(String::from)
                .unwrap_or_else(|| format!("catalog-{}", a["cmd"].as_str().unwrap_or("")));
            let dir = std::env::var("UNPACK_TEST_CATALOG_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| std::env::temp_dir().join("unpack-bridge-catalog"));
            let ctx = unpack_lib::catalog::install::Ctx {
                store: unpack_lib::catalog::Store::new(dir),
                exe: unpack_lib::archive::find_7za(),
                tuning: unpack_lib::catalog::install::Tuning {
                    concurrency: 2,
                    backoff: vec![std::time::Duration::from_millis(50)],
                },
            };
            jobs.register(&id);
            let j = jobs.clone();
            let r = unpack_lib::catalog::call(
                &ctx,
                a["cmd"].as_str().unwrap_or(""),
                a["args"].clone(),
                &|p| GRANTS.check(p),
                &mut |p, f| {
                    events.lock().unwrap().push(json!({"event":"job-progress","payload":{"id": id, "percent": p, "file": f}}));
                },
                &|| j.is_cancelled(&id),
            );
            jobs.finish(&id);
            r
        }
        "list_archives_in_folder" => {
            let root = GRANTS.check(a["path"].as_str().unwrap_or(""))?;
            unpack_lib::download::list_archives(&root).map(|v| json!(v))
        }
        "download_folder" => {
            let d = std::env::temp_dir().join("unpack-bridge-downloads");
            std::fs::create_dir_all(&d).ok();
            GRANTS.grant(&d);
            Ok(json!(d.display().to_string()))
        }
        "download_url" => {
            let id = a["id"].as_str().unwrap_or("").to_string();
            jobs.register(&id);
            let j = jobs.clone();
            let dir = match a["destDir"].as_str().filter(|s| !s.is_empty()) {
                Some(d) => GRANTS.check(d)?,
                None => {
                    let d = std::env::temp_dir().join("unpack-bridge-downloads");
                    std::fs::create_dir_all(&d).ok();
                    GRANTS.grant(&d);
                    d
                }
            };
            let url = a["url"].as_str().unwrap_or("").to_string();
            let opts = unpack_lib::download::DownloadOpts {
                max_bytes: a["maxBytes"]
                    .as_u64()
                    .filter(|n| *n > 0)
                    .unwrap_or(unpack_lib::download::DEFAULT_MAX_BYTES),
                expected_sha256: a["expectedSha256"]
                    .as_str()
                    .map(|s| s.to_string())
                    .filter(|s| !s.trim().is_empty()),
            };
            // e2e uses a local HTTP fixture server
            std::env::set_var("UNPACK_TEST_ALLOW_LOOPBACK", "1");
            let r = unpack_lib::download::download_with(
                &url,
                &dir,
                &opts,
                &mut |p, f| {
                    events.lock().unwrap().push(json!({"event":"job-progress","payload":{"id": id, "percent": p, "file": f}}));
                },
                &|| j.is_cancelled(&id),
            );
            jobs.finish(&id);
            if let Ok(ref res) = r {
                GRANTS.grant(std::path::Path::new(&res.path));
            }
            r.map(|v| serde_json::to_value(v).unwrap())
        }
        "pick_save" => {
            let v = a["defaultPath"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .or_else(|| std::env::var("UNPACK_TEST_SAVE").ok());
            if let Some(ref p) = v {
                if let Some(parent) = std::path::Path::new(p).parent() {
                    GRANTS.grant(parent);
                }
                GRANTS.grant(std::path::Path::new(p));
            }
            Ok(json!(v))
        }
        "save_text_file" => unpack_lib::safecopy::write_export_text(
            &GRANTS,
            a["path"].as_str().unwrap_or(""),
            a["text"].as_str().unwrap_or(""),
        )
        .map(|v| json!(v)),
        "error_help" => Ok(serde_json::to_value(unpack_lib::hints::help_for(
            a["message"].as_str().unwrap_or(""),
        ))
        .unwrap()),
        "preflight_local" => {
            let dest = a["dest"].as_str().unwrap_or("");
            let paths: Vec<PathBuf> = a["paths"]
                .as_array()
                .map(|v| {
                    v.iter()
                        .filter_map(|x| x.as_str())
                        .map(PathBuf::from)
                        .collect()
                })
                .unwrap_or_default();
            let need = unpack_lib::preflight::estimate_local_need(&paths);
            Ok(
                serde_json::to_value(unpack_lib::preflight::check_local_dest(
                    &PathBuf::from(dest),
                    need,
                    &unpack_lib::preflight::fat_blocks(
                        &PathBuf::from(dest),
                        &paths,
                        unpack_lib::archive::find_7za().as_deref(),
                        a["autoIso"].as_str().unwrap_or("off"),
                    ),
                ))
                .unwrap(),
            )
        }
        "dlc_match_games" => {
            let libs: Vec<PathBuf> = a["libraries"]
                .as_array()
                .map(|v| {
                    v.iter()
                        .filter_map(|x| x.as_str())
                        .map(PathBuf::from)
                        .collect()
                })
                .unwrap_or_default();
            Ok(json!(unpack_lib::dlc::find_games(
                &libs,
                a["titleId"].as_str().unwrap_or("")
            )
            .into_iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()))
        }
        "ftp_lock_status" => Ok(json!({"busy": false, "subsystem": "", "detail": ""})),
        "library_index" => {
            let libs: Vec<PathBuf> = a["libraries"]
                .as_array()
                .map(|v| {
                    v.iter()
                        .filter_map(|x| x.as_str())
                        .filter(|s| !s.is_empty())
                        .map(|s| GRANTS.check(s))
                        .collect::<Result<Vec<_>, _>>()
                })
                .transpose()?
                .unwrap_or_default();
            let dir = std::env::temp_dir().join("unpack-bridge-lib-cache");
            let max_age = a["maxAgeSecs"].as_u64().unwrap_or(0);
            unpack_lib::library::index_cached(&libs, &dir, max_age)
                .map(|v| serde_json::to_value(v).unwrap())
        }
        "god_verify_only" => unpack_lib::god::verify_readonly_folder(&PathBuf::from(
            a["path"].as_str().unwrap_or(""),
        ))
        .map(|m| json!(m)),
        "check_release" => Ok(serde_json::to_value(unpack_lib::download::ReleaseInfo {
            available: false,
            latest: a["current"].as_str().unwrap_or("0").into(),
            url: String::new(),
        })
        .unwrap()),
        "lan_discover" => Ok(json!([])),
        "usb_prepare" => GRANTS
            .check(a["root"].as_str().unwrap_or(""))
            .and_then(|r| unpack_lib::usbprep::prepare_usb(&r))
            .map(|v| serde_json::to_value(v).unwrap()),
        "notify_done" | "set_taskbar_progress" => Ok(json!(null)),

        "dlc_scan_drives" => {
            let r = std::env::var("UNPACK_TEST_DRIVES")
                .map(|d| unpack_lib::dlc::scan_roots(&[PathBuf::from(d)]))
                .unwrap_or_default();
            LAST_SCAN.set_from_scan(&r);
            Ok(json!(r))
        }
        "god_extracted" => {
            let id = a["id"].as_str().unwrap().to_string();
            jobs.register(&id);
            let mut cb = |p: u32, f: &str| {
                events.lock().unwrap().push(
                    json!({"event":"job-progress","payload":{"id": id, "percent": p, "file": f}}),
                );
            };
            let r = unpack_lib::god::convert_registered(
                a["token"].as_str().unwrap_or(""),
                a["deleteIso"].as_bool().unwrap_or(false),
                jobs,
                &id,
                &mut cb,
            );
            jobs.finish(&id);
            r.map(|v| serde_json::to_value(v).unwrap())
        }
        _ => Err(format!("unknown command {cmd}")),
    }
}
