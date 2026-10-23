//! Catalog tests. Dummy fixtures only: made-up IDs, fake "XEX2"/STFS headers, local servers.
use super::install::*;
use super::target::Target;
use super::*;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

const TID: &str = "4D5307E6"; // a real TitleID shape; the mods are invented

fn tmp(n: &str) -> crate::testutil::TmpDir { crate::testutil::TmpDir::canonical(&format!("cat-{n}")) }

fn sha(b: &[u8]) -> String { use sha2::{Digest, Sha256}; Sha256::digest(b).iter().map(|x| format!("{x:02x}")).collect() }

fn dummy_xex(tag: &str) -> Vec<u8> { let mut v = b"XEX2".to_vec(); v.extend(format!("dummy plugin {tag}").as_bytes()); v.resize(4096, 7); v }

fn dummy_stfs(content_type: u32, tid: &str, name: &str) -> Vec<u8> {
    let mut b = vec![0u8; 0x1000];
    b[..4].copy_from_slice(b"LIVE");
    b[0x344..0x348].copy_from_slice(&content_type.to_be_bytes());
    b[0x360..0x364].copy_from_slice(&u32::from_str_radix(tid, 16).unwrap().to_be_bytes());
    for (i, u) in name.encode_utf16().enumerate() { b[0x411 + i * 2..0x413 + i * 2].copy_from_slice(&u.to_be_bytes()); }
    b
}

#[derive(Default)]
struct Route { body: Vec<u8>, fail_first: usize, status: Option<u16>, delay_ms: u64 }

#[derive(Clone, Default)]
struct Http { port: u16, routes: Arc<Mutex<HashMap<String, Route>>>, hits: Arc<Mutex<HashMap<String, usize>>>, cur: Arc<AtomicUsize>, max: Arc<AtomicUsize> }

impl Http {
    fn start() -> Http {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut h = Http::default();
        h.port = l.local_addr().unwrap().port();
        let h2 = h.clone();
        std::thread::spawn(move || for c in l.incoming().flatten() {
            let h = h2.clone();
            std::thread::spawn(move || {
                let mut c = c;
                let mut buf = vec![0u8; 8192];
                let n = c.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
                let now = h.cur.fetch_add(1, Ordering::SeqCst) + 1;
                h.max.fetch_max(now, Ordering::SeqCst);
                let hit = { let mut m = h.hits.lock().unwrap(); let e = m.entry(path.clone()).or_insert(0); *e += 1; *e };
                let (status, body, delay) = {
                    let r = h.routes.lock().unwrap();
                    match r.get(&path) {
                        None => (404, b"no".to_vec(), 0),
                        Some(rt) if hit <= rt.fail_first => (503, b"busy".to_vec(), rt.delay_ms),
                        Some(rt) => (rt.status.unwrap_or(200), rt.body.clone(), rt.delay_ms),
                    }
                };
                std::thread::sleep(std::time::Duration::from_millis(delay));
                let _ = c.write_all(format!("HTTP/1.1 {status} X\r\nContent-Length: {}\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n", body.len()).as_bytes());
                let _ = c.write_all(&body);
                h.cur.fetch_sub(1, Ordering::SeqCst);
            });
        });
        h
    }
    fn put(&self, path: &str, body: Vec<u8>) -> String { self.route(path, Route { body, ..Default::default() }) }
    fn route(&self, path: &str, r: Route) -> String { self.routes.lock().unwrap().insert(path.into(), r); format!("http://127.0.0.1:{}{path}", self.port) }
    fn hits(&self, path: &str) -> usize { *self.hits.lock().unwrap().get(path).unwrap_or(&0) }
}

struct Env { _g: std::sync::MutexGuard<'static, ()> }
fn env() -> Env { let g = crate::download::tests_env_lock(); std::env::set_var("UNPACK_TEST_ALLOW_LOOPBACK", "1"); Env { _g: g } }
impl Drop for Env { fn drop(&mut self) { std::env::remove_var("UNPACK_TEST_ALLOW_LOOPBACK"); } }

fn keys() -> (String, String) { keygen().unwrap() }

fn signed(v: &serde_json::Value, secret: &str) -> (Vec<u8>, Vec<u8>) {
    let b = serde_json::to_vec_pretty(v).unwrap();
    let s = sign(&b, secret).unwrap();
    (b, s.into_bytes())
}

fn plugin_entry(id: &str, ver: &str, url: &str, body: &[u8]) -> serde_json::Value {
    serde_json::json!({
        "id": id, "game": {"title_id": TID, "name": "Halo 3", "aliases": []},
        "mod": {"name": format!("Dummy {id}"), "author": "test", "version": ver},
        "redistribute": true, "source_page": "https://example.invalid/page",
        "files": [{"url": url, "size": body.len(), "sha256": sha(body), "install": {"handler": "plugin", "dashlaunch": {}}}]
    })
}

fn manifest(entries: Vec<serde_json::Value>, ver: u64) -> serde_json::Value { serde_json::json!({"schema": 1, "catalog_version": ver, "entries": entries}) }

fn ctx(store: &Path) -> Ctx { Ctx { store: Store::new(store.to_path_buf()), exe: crate::archive::find_7za(), tuning: Tuning { concurrency: 2, backoff: vec![std::time::Duration::from_millis(20), std::time::Duration::from_millis(20)] } } }

fn load(v: &serde_json::Value) -> Loaded { let (s, p) = keys(); let (b, sig) = signed(v, &s); load_bytes(&b, &sig, &p, "test").unwrap() }

const INI: &str = "; DashLaunch\r\n[Paths]\r\nDefault = Usb:\\Aurora\\Aurora.xex\r\n\r\n[Plugins]\r\nplugin1 = Usb:\\Plugins\\main.xex\r\nplugin2 = Usb:\\Plugins\\other.xex\r\n\r\n[Settings]\r\nnohud = true\r\n";

// ---- signature + schema ----

#[test]
fn signature_is_checked_before_parsing() {
    let (s, p) = keys();
    let garbage = b"{ this is not json";
    let sig = sign(garbage, &s).unwrap();
    // Valid signature, garbage content: now (and only now) the parser complains.
    assert!(load_bytes(garbage, sig.as_bytes(), &p, "t").unwrap_err().contains("isn't valid"));
    // Bad signature over garbage: the signature error comes first, the JSON is never read.
    let (s2, _) = keys();
    let bad = sign(garbage, &s2).unwrap();
    assert!(load_bytes(garbage, bad.as_bytes(), &p, "t").unwrap_err().contains("signature"));
    // One byte changed after signing.
    let (mut b, sig) = signed(&manifest(vec![], 1), &s);
    let n = b.len() - 2; b[n] = b' ';
    assert!(load_bytes(&b, &sig, &p, "t").unwrap_err().contains("signature"));
    // Malformed key / signature text
    assert!(load_bytes(&b, b"zz", &p, "t").is_err());
    assert!(load_bytes(&b, &sig, "1234", "t").is_err());
}

#[test]
fn entries_are_validated_and_bad_ones_skipped() {
    let _e = env();
    let body = dummy_xex("a");
    let good = plugin_entry("good.one", "1.0", "https://example.invalid/a.xex", &body);
    let mut no_sha = good.clone(); no_sha["id"] = "no-sha".into(); no_sha["files"][0].as_object_mut().unwrap().remove("sha256");
    let mut ftp_url = good.clone(); ftp_url["id"] = "ftp-url".into(); ftp_url["files"][0]["url"] = "ftp://example.invalid/a.xex".into();
    let mut escape = good.clone(); escape["id"] = "escape".into(); escape["files"][0]["install"]["dest_path"] = "../x".into();
    let mut root = good.clone(); root["id"] = "root".into(); root["files"][0]["install"]["dest_path"] = "".into(); root["files"][0]["install"]["handler"] = "trainer".into(); root["files"][0]["install"]["dest_path"] = "launch.ini".into(); root["files"][0]["install"].as_object_mut().unwrap().remove("dashlaunch");
    let mut slot1 = good.clone(); slot1["id"] = "slot1".into(); slot1["files"][0]["install"]["dashlaunch"]["slot"] = 1.into();
    let mut unknown = good.clone(); unknown["id"] = "unknown".into(); unknown["surprise"] = true.into();
    let mut bad_id = good.clone(); bad_id["id"] = "Bad ID!".into();
    let mut dup = good.clone(); dup["mod"]["name"] = "dup".into();
    let mut tu_elsewhere = good.clone(); tu_elsewhere["id"] = "tu-elsewhere".into(); tu_elsewhere["files"][0]["install"] = serde_json::json!({"handler": "tu", "dest_path": "Plugins"});
    let paid_fix = { let mut p = good.clone(); p["id"] = "paid".into(); p["paid"] = true.into(); p };
    let l = load(&manifest(vec![good, no_sha, ftp_url, escape, root, slot1, unknown, bad_id, dup, tu_elsewhere, paid_fix], 3));
    let ids: Vec<&str> = l.entries.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(ids, vec!["good.one", "paid"], "{:?}", l.problems);
    assert_eq!(l.problems.len(), 9, "{:?}", l.problems);
    assert!(l.problems.iter().any(|p| p.contains("no-sha") && p.contains("sha256")));
    assert!(l.problems.iter().any(|p| p.contains("slot1") && p.contains("reserved")));
    assert!(l.problems.iter().any(|p| p.contains("duplicate")));
    let paid = &l.entries[1];
    assert!(paid.files.is_empty() && !paid.installable(), "paid entries never carry downloads");
    // unsupported schema
    let (s, p) = keys();
    let (b, sig) = signed(&serde_json::json!({"schema": 2, "catalog_version": 1, "entries": []}), &s);
    assert!(load_bytes(&b, &sig, &p, "t").unwrap_err().contains("schema"));
}

#[test]
fn https_required_outside_tests() {
    // Without the loopback test switch, plain http is refused.
    let _g = crate::download::tests_env_lock();
    std::env::remove_var("UNPACK_TEST_ALLOW_LOOPBACK");
    assert!(url_ok("http://example.invalid/a").is_err());
    assert!(url_ok("https://user:pw@example.invalid/a").is_err());
    assert!(url_ok("https://example.invalid/a").is_ok());
}

#[test]
fn dest_paths_are_confined_per_handler() {
    assert_eq!(dest_segments("plugin", None, TID, "x").unwrap(), vec!["Plugins"]);
    assert_eq!(dest_segments("tu", None, TID, "x").unwrap().join("/"), format!("Content/0000000000000000/{TID}/000B0000"));
    assert_eq!(dest_segments("xex", None, TID, "My: Mod?").unwrap(), vec!["Games", "My_ Mod_"]);
    assert!(dest_segments("xex", Some("Games/Thing"), TID, "x").is_ok());
    for (h, p) in [("xex", "Plugins/x"), ("plugin", "Games/x"), ("dlc", "Content/0000000000000000/4D5307E6/000B0000"), ("trainer", "Trainers"), ("patch", "/"), ("xex", "Games/../.."), ("xex", "Games/a:b")] {
        assert!(dest_segments(h, Some(p), TID, "x").is_err(), "{h} {p}");
    }
}

#[test]
fn rollback_is_refused_and_unconfigured_catalog_is_off() {
    let d = tmp("rollback");
    let st = Store::new(d.to_path_buf());
    assert!(refresh(&st, &|p| Ok(PathBuf::from(p)), &|| false).unwrap_err().contains("No catalog"));
    st.check_rollback("k", 5).unwrap();
    st.check_rollback("k", 5).unwrap();
    assert!(st.check_rollback("k", 4).unwrap_err().contains("rollback"));
    st.check_rollback("other", 1).unwrap();
}

#[test]
fn config_needs_a_key_and_https() {
    let _g = crate::download::tests_env_lock();
    std::env::remove_var("UNPACK_TEST_ALLOW_LOOPBACK");
    let d = tmp("config");
    let st = Store::new(d.to_path_buf());
    let (_, p) = keys();
    assert!(st.set_config(Config { source: "remote".into(), url: "https://example.invalid/c.json".into(), ..Default::default() }).unwrap_err().contains("public key"));
    assert!(st.set_config(Config { source: "remote".into(), url: "http://example.invalid/c.json".into(), pubkey: p.clone(), ..Default::default() }).is_err());
    assert!(st.set_config(Config { source: "remote".into(), url: "https://example.invalid/c.json".into(), pubkey: "00".into(), ..Default::default() }).is_err());
    let c = st.set_config(Config { source: "remote".into(), url: "https://example.invalid/c.json".into(), pubkey: p.to_uppercase(), ..Default::default() }).unwrap();
    assert_eq!(c.pubkey, p);
    assert_eq!(st.config(), c);
}

#[test]
fn local_catalog_file_needs_grant_and_signature() {
    let _e = env();
    let d = tmp("localcat");
    let (s, p) = keys();
    let (b, sig) = signed(&manifest(vec![], 7), &s);
    let f = d.join("catalog.json");
    std::fs::write(&f, &b).unwrap();
    let st = Store::new(d.join("store"));
    st.set_config(Config { source: "local".into(), local_file: f.display().to_string(), pubkey: p.clone(), ..Default::default() }).unwrap();
    let granted = |x: &str| if Path::new(x) == f { Ok(f.clone()) } else { Err("not picked".to_string()) };
    assert!(refresh(&st, &granted, &|| false).unwrap_err().contains(".sig"));
    std::fs::write(d.join("catalog.json.sig"), &sig).unwrap();
    assert_eq!(refresh(&st, &granted, &|| false).unwrap().catalog_version, 7);
    assert!(refresh(&st, &|_| Err("not picked".to_string()), &|| false).unwrap_err().contains("not picked"));
}

#[test]
fn remote_catalog_through_the_downloader() {
    let _e = env();
    let h = Http::start();
    let (s, p) = keys();
    let (b, sig) = signed(&manifest(vec![], 2), &s);
    let url = h.put("/c.json", b);
    h.put("/c.json.sig", sig);
    let d = tmp("remote");
    let st = Store::new(d.to_path_buf());
    st.set_config(Config { source: "remote".into(), url: url.clone(), pubkey: p, ..Default::default() }).unwrap();
    assert_eq!(refresh(&st, &|x| Ok(PathBuf::from(x)), &|| false).unwrap().catalog_version, 2);
    // tampered remote copy
    h.put("/c.json", b"{\"schema\":1,\"catalog_version\":9,\"entries\":[]}".to_vec());
    assert!(refresh(&st, &|x| Ok(PathBuf::from(x)), &|| false).unwrap_err().contains("signature"));
}

#[test]
fn catalog_aliases_and_names_feed_detection() {
    let _e = env();
    let mut e = plugin_entry("alias.test", "1", "https://example.invalid/q.xex", &dummy_xex("q"));
    e["game"]["aliases"] = serde_json::json!(["zqxgame"]);
    e["mod"]["name"] = "Wobbletron Menu".into();
    let l = load(&manifest(vec![e], 1));
    activate(&l);
    let g = crate::gamedb::guess(&["zqxgame stuff"], 3);
    assert_eq!(g.first().map(|g| g.title_id.as_str()), Some(TID));
    assert!(learned().iter().any(|x| x.key == "wobbletron" && x.tid == TID));
    activate(&Loaded::default());
    assert!(crate::gamedb::guess(&["zqxgame stuff"], 3).first().map_or(true, |g| g.title_id != TID));
}

// ---- launch.ini ----

#[test]
fn ini_roundtrip_is_lossless_and_slots_skip_1() {
    let i = ini::Ini::parse(INI.as_bytes()).unwrap();
    assert_eq!(i.serialize(), INI.as_bytes());
    let lf = INI.replace("\r\n", "\n");
    assert_eq!(ini::Ini::parse(lf.as_bytes()).unwrap().serialize(), lf.as_bytes());
    assert_eq!(ini::choose_slot(&i, "Usb:\\Plugins\\new.xex", None, None).unwrap(), (3, None));
    // already present: reuse
    assert_eq!(ini::choose_slot(&i, "usb:\\plugins\\OTHER.xex", None, None).unwrap().0, 2);
    // requested slot occupied -> ask
    assert!(ini::choose_slot(&i, "Usb:\\Plugins\\new.xex", Some(2), None).unwrap_err().starts_with("SLOT_OCCUPIED:2:"));
    assert_eq!(ini::choose_slot(&i, "Usb:\\Plugins\\new.xex", Some(2), Some(2)).unwrap(), (2, Some("Usb:\\Plugins\\other.xex".into())));
    assert!(ini::choose_slot(&i, "x", None, Some(1)).is_err(), "plugin1 is never written");
    let mut full = i.clone();
    for n in 3..=5 { full.set("Plugins", &format!("plugin{n}"), &format!("Usb:\\p{n}.xex")); }
    assert!(ini::choose_slot(&full, "Usb:\\new.xex", None, None).unwrap_err().starts_with("SLOT_OCCUPIED:5:"));
    // empty value counts as free
    let e = ini::Ini::parse(b"[Plugins]\nplugin1 = a\nplugin2 =\n").unwrap();
    assert_eq!(ini::choose_slot(&e, "b", None, None).unwrap().0, 2);
    // a section that's missing is added; comments survive
    let mut n = ini::Ini::parse(b"; only a comment\n").unwrap();
    n.set("Plugins", "plugin2", "Hdd:\\x.xex");
    assert_eq!(String::from_utf8(n.serialize()).unwrap(), "; only a comment\n\n[Plugins]\nplugin2 = Hdd:\\x.xex\n");
}

#[test]
fn ini_validation_catches_collateral_changes_and_bad_files() {
    let i = ini::Ini::parse(INI.as_bytes()).unwrap();
    let mut a = i.clone();
    a.set("Plugins", "plugin3", "Usb:\\x.xex");
    ini::validate_change(&i, &a.serialize(), "plugin3", Some("Usb:\\x.xex")).unwrap();
    a.set("Settings", "nohud", "false");
    assert!(ini::validate_change(&i, &a.serialize(), "plugin3", Some("Usb:\\x.xex")).is_err());
    assert!(ini::Ini::parse(&[0xff, 0xfe, 0x00]).is_err());
    assert!(ini::Ini::parse(&vec![b'a'; ini::INI_MAX + 1]).is_err());
    assert!(ini::Ini::parse(b"a\0b").is_err());
}

// ---- install / uninstall on a local drive ----

fn local(root: &Path) -> Target { Target::Local { root: root.to_path_buf() } }

#[test]
fn plugin_install_edits_launch_ini_and_uninstall_restores() {
    let _e = env();
    let h = Http::start();
    let body = dummy_xex("p");
    let url = h.put("/dl/wobble.xex", body.clone());
    let l = load(&manifest(vec![plugin_entry("wobble", "1.0", &url, &body)], 1));
    let d = tmp("plugin");
    let usb = d.join("usb"); std::fs::create_dir_all(&usb).unwrap();
    std::fs::write(usb.join("launch.ini"), INI).unwrap();
    let c = ctx(&d.join("store"));
    let t = local(&usb);
    let pf = preflight(&c.store, &l, &l.entries[0], &t);
    assert!(pf.ok && pf.state == "new", "{pf:?}");
    let r = install(&c, &l, "wobble", &t, &InstallOpts::default(), &mut |_, _| {}, &|| false).unwrap();
    assert_eq!(r.ini.as_deref(), Some("plugin3 = Usb:\\Plugins\\wobble.xex"));
    assert_eq!(std::fs::read(usb.join("Plugins/wobble.xex")).unwrap(), body);
    let after = std::fs::read_to_string(usb.join("launch.ini")).unwrap();
    assert_eq!(after, INI.replace("plugin2 = Usb:\\Plugins\\other.xex\r\n", "plugin2 = Usb:\\Plugins\\other.xex\r\nplugin3 = Usb:\\Plugins\\wobble.xex\r\n"));
    let baks: Vec<_> = std::fs::read_dir(&usb).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.starts_with("launch.ini.unpack-") && n.ends_with(".bak")).collect();
    assert_eq!(baks.len(), 1);
    assert_eq!(std::fs::read_to_string(usb.join(&baks[0])).unwrap(), INI);
    assert!(std::fs::read_dir(d.join("store/lkg")).unwrap().count() == 1, "last-known-good kept");
    let j = c.store.journal().unwrap();
    let rec = j.find(&t.key(), "wobble").unwrap();
    assert!(rec.complete && rec.files.len() == 1 && rec.files[0].sha256 == sha(&body) && rec.ini[0].key == "plugin3");
    assert_eq!(states(&c.store, &l, &t.key()).unwrap()[0].state, "installed");
    assert!(preflight(&c.store, &l, &l.entries[0], &t).blockers.iter().any(|b| b.contains("already installed")));
    assert!(std::fs::read_dir(d.join("store/stage")).unwrap().count() == 0, "staging cleaned up");
    let u = uninstall(&c, Some(&l), "wobble", &t, false).unwrap();
    assert_eq!((u.removed, u.kept.len()), (1, 0));
    assert!(!usb.join("Plugins").exists(), "empty folder Unpack made is tidied");
    assert_eq!(std::fs::read_to_string(usb.join("launch.ini")).unwrap(), INI);
    assert!(c.store.journal().unwrap().installs.is_empty());
}

#[test]
fn occupied_requested_slot_needs_confirmation_before_download() {
    let _e = env();
    let h = Http::start();
    let body = dummy_xex("s");
    let url = h.put("/s.xex", body.clone());
    let mut e = plugin_entry("slotty", "1", &url, &body);
    e["files"][0]["install"]["dashlaunch"]["slot"] = 2.into();
    let l = load(&manifest(vec![e], 1));
    let d = tmp("slot");
    let usb = d.join("usb"); std::fs::create_dir_all(&usb).unwrap();
    std::fs::write(usb.join("launch.ini"), INI).unwrap();
    let c = ctx(&d.join("store"));
    let t = local(&usb);
    let err = install(&c, &l, "slotty", &t, &InstallOpts::default(), &mut |_, _| {}, &|| false).unwrap_err();
    assert!(err.starts_with("SLOT_OCCUPIED:2:"), "{err}");
    assert_eq!(h.hits("/s.xex"), 0, "nothing downloaded before the user decided");
    assert_eq!(std::fs::read_to_string(usb.join("launch.ini")).unwrap(), INI);
    install(&c, &l, "slotty", &t, &InstallOpts { confirm_slot: Some(2), ..Default::default() }, &mut |_, _| {}, &|| false).unwrap();
    let ini = ini::Ini::parse(&std::fs::read(usb.join("launch.ini")).unwrap()).unwrap();
    assert_eq!(ini.get("Plugins", "plugin2").as_deref(), Some("Usb:\\Plugins\\s.xex"));
    // uninstall puts the user's old plugin2 back
    uninstall(&c, Some(&l), "slotty", &t, false).unwrap();
    assert_eq!(std::fs::read_to_string(usb.join("launch.ini")).unwrap(), INI);
}

#[test]
fn changed_files_are_kept_on_uninstall() {
    let _e = env();
    let h = Http::start();
    let body = dummy_xex("k");
    let url = h.put("/k.xex", body.clone());
    let mut e = plugin_entry("keepme", "1", &url, &body);
    e["files"][0]["install"].as_object_mut().unwrap().remove("dashlaunch");
    let l = load(&manifest(vec![e], 1));
    let d = tmp("keep");
    let usb = d.join("usb"); std::fs::create_dir_all(&usb).unwrap();
    let c = ctx(&d.join("store"));
    let t = local(&usb);
    install(&c, &l, "keepme", &t, &InstallOpts::default(), &mut |_, _| {}, &|| false).unwrap();
    std::fs::write(usb.join("Plugins/k.xex"), b"user edited").unwrap();
    let u = uninstall(&c, Some(&l), "keepme", &t, false).unwrap();
    assert_eq!((u.removed, u.kept), (0, vec!["Plugins/k.xex".to_string()]));
    assert_eq!(std::fs::read(usb.join("Plugins/k.xex")).unwrap(), b"user edited");
}

#[test]
fn existing_different_file_is_never_overwritten() {
    let _e = env();
    let h = Http::start();
    let body = dummy_xex("o");
    let url = h.put("/o.xex", body.clone());
    let mut e = plugin_entry("over", "1", &url, &body);
    e["files"][0]["install"].as_object_mut().unwrap().remove("dashlaunch");
    let l = load(&manifest(vec![e], 1));
    let d = tmp("over");
    let usb = d.join("usb"); std::fs::create_dir_all(usb.join("Plugins")).unwrap();
    std::fs::write(usb.join("Plugins/o.xex"), b"someone else's").unwrap();
    let c = ctx(&d.join("store"));
    let err = install(&c, &l, "over", &local(&usb), &InstallOpts::default(), &mut |_, _| {}, &|| false).unwrap_err();
    assert!(err.contains("different content"), "{err}");
    assert_eq!(std::fs::read(usb.join("Plugins/o.xex")).unwrap(), b"someone else's");
}

#[test]
fn update_replaces_the_old_version() {
    let _e = env();
    let h = Http::start();
    let (b1, b2) = (dummy_xex("v1"), dummy_xex("v2-longer"));
    let u1 = h.put("/v1/up.xex", b1.clone());
    let u2 = h.put("/v2/up2.xex", b2.clone());
    let d = tmp("update");
    let usb = d.join("usb"); std::fs::create_dir_all(&usb).unwrap();
    std::fs::write(usb.join("launch.ini"), INI).unwrap();
    let c = ctx(&d.join("store"));
    let t = local(&usb);
    let l1 = load(&manifest(vec![plugin_entry("up", "1.0", &u1, &b1)], 1));
    install(&c, &l1, "up", &t, &InstallOpts::default(), &mut |_, _| {}, &|| false).unwrap();
    let l2 = load(&manifest(vec![plugin_entry("up", "2.0", &u2, &b2)], 2));
    assert_eq!(states(&c.store, &l2, &t.key()).unwrap()[0].state, "update");
    let r = install(&c, &l2, "up", &t, &InstallOpts::default(), &mut |_, _| {}, &|| false).unwrap();
    assert_eq!(r.updated_from.as_deref(), Some("1.0"));
    assert!(!usb.join("Plugins/up.xex").exists() && usb.join("Plugins/up2.xex").exists());
    let ini = ini::Ini::parse(&std::fs::read(usb.join("launch.ini")).unwrap()).unwrap();
    assert_eq!(ini.get("Plugins", "plugin3").as_deref(), Some("Usb:\\Plugins\\up2.xex"));
    assert_eq!(ini.plugin_slots().iter().filter(|s| s.is_some()).count(), 3);
    assert_eq!(c.store.journal().unwrap().find(&t.key(), "up").unwrap().version, "2.0");
}

// ---- downloads ----

#[test]
fn mirror_fallback_and_backoff() {
    let _e = env();
    let h = Http::start();
    let body = dummy_xex("m");
    // primary: wrong bytes (hash mismatch -> straight to the next mirror, no retry)
    let primary = h.put("/p/m.xex", dummy_xex("tampered"));
    // mirror 1: busy twice, then fine (retried with backoff)
    let m1 = h.route("/m1/m.xex", Route { body: body.clone(), fail_first: 2, ..Default::default() });
    let mut e = plugin_entry("mirror", "1", &primary, &body);
    e["files"][0]["mirrors"] = serde_json::json!([m1]);
    e["files"][0]["install"].as_object_mut().unwrap().remove("dashlaunch");
    let l = load(&manifest(vec![e], 1));
    let d = tmp("mirror");
    let out = download_all(&l.entries[0], &d.join("dl"), &ctx(&d).tuning, &mut |_, _| {}, &|| false).unwrap();
    assert_eq!(std::fs::read(&out[0]).unwrap(), body);
    assert_eq!(h.hits("/p/m.xex"), 1);
    assert_eq!(h.hits("/m1/m.xex"), 3);
    // all sources bad -> one clear error naming them
    let mut e2 = plugin_entry("allbad", "1", &h.put("/x/a.xex", b"nope".to_vec()), &body);
    e2["files"][0]["mirrors"] = serde_json::json!([format!("http://127.0.0.1:{}/missing.xex", h.port)]);
    let l2 = load(&manifest(vec![e2], 1));
    let err = download_all(&l2.entries[0], &d.join("dl2"), &ctx(&d).tuning, &mut |_, _| {}, &|| false).unwrap_err();
    assert!(err.contains("Couldn't download"), "{err}");
}

#[test]
fn downloads_are_capped_in_parallel() {
    let _e = env();
    let h = Http::start();
    let mut files = vec![];
    for k in 0..5 {
        let b = dummy_xex(&format!("c{k}"));
        let u = h.route(&format!("/c/f{k}.bin"), Route { body: b.clone(), delay_ms: 150, ..Default::default() });
        files.push(serde_json::json!({"url": u, "size": b.len(), "sha256": sha(&b), "install": {"handler": "trainer"}}));
    }
    let e = serde_json::json!({"id": "many", "game": {"title_id": TID, "name": "Halo 3"}, "mod": {"name": "Many", "version": "1"}, "redistribute": true, "files": files});
    let l = load(&manifest(vec![e], 1));
    let d = tmp("cap");
    let out = download_all(&l.entries[0], &d.join("dl"), &ctx(&d).tuning, &mut |_, _| {}, &|| false).unwrap();
    assert_eq!(out.len(), 5);
    let max = h.max.load(Ordering::SeqCst);
    assert!(max >= 1 && max <= 2, "max parallel {max}");
}

#[test]
fn cancel_stops_downloads() {
    let _e = env();
    let h = Http::start();
    let b = dummy_xex("z");
    let u = h.route("/z.bin", Route { body: b.clone(), fail_first: 100, ..Default::default() });
    let e = serde_json::json!({"id": "cancel", "game": {"title_id": TID, "name": "Halo 3"}, "mod": {"name": "C", "version": "1"}, "redistribute": true, "files": [{"url": u, "size": b.len(), "sha256": sha(&b), "install": {"handler": "trainer"}}]});
    let l = load(&manifest(vec![e], 1));
    let d = tmp("cancel");
    let mut c = ctx(&d);
    c.tuning.backoff = vec![std::time::Duration::from_secs(30)];
    let t0 = std::time::Instant::now();
    let flag = std::sync::atomic::AtomicBool::new(false);
    let err = std::thread::scope(|s| {
        s.spawn(|| { std::thread::sleep(std::time::Duration::from_millis(300)); flag.store(true, Ordering::SeqCst); });
        download_all(&l.entries[0], &d.join("dl"), &c.tuning, &mut |_, _| {}, &|| flag.load(Ordering::SeqCst)).unwrap_err()
    });
    assert_eq!(err, crate::archive::ERR_CANCELLED);
    assert!(t0.elapsed() < std::time::Duration::from_secs(5), "backoff wait is interruptible");
}

// ---- staging guards + handlers ----

#[cfg(unix)]
#[test]
fn staged_links_are_refused() {
    let d = tmp("links");
    std::fs::write(d.join("a.xex"), b"XEX2").unwrap();
    std::os::unix::fs::symlink("/etc/passwd", d.join("evil")).unwrap();
    assert!(super::target::walk_stage(&d).unwrap_err().contains("link"));
}

#[test]
fn staged_bad_names_are_refused() {
    let d = tmp("names");
    std::fs::write(d.join("ok.xex"), b"XEX2").unwrap();
    std::fs::write(d.join("x".repeat(50)), b"1").unwrap();
    assert!(super::target::walk_stage(&d).is_err());
}

/// A stored zip (no compression) with arbitrary entry names, built by hand.
fn zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let (mut out, mut cd) = (vec![], vec![]);
    for (name, data) in entries {
        let crc = crc32fast::hash(data);
        let off = out.len() as u32;
        let hdr = |sig: u32, central: bool| {
            let mut h = sig.to_le_bytes().to_vec();
            if central { h.extend(20u16.to_le_bytes()); }
            h.extend(20u16.to_le_bytes()); h.extend(0u16.to_le_bytes()); h.extend(0u16.to_le_bytes()); h.extend(0u32.to_le_bytes());
            h.extend(crc.to_le_bytes()); h.extend((data.len() as u32).to_le_bytes()); h.extend((data.len() as u32).to_le_bytes());
            h.extend((name.len() as u16).to_le_bytes()); h.extend(0u16.to_le_bytes());
            if central { h.extend(0u16.to_le_bytes()); h.extend(0u16.to_le_bytes()); h.extend(0u16.to_le_bytes()); h.extend(0u32.to_le_bytes()); h.extend(off.to_le_bytes()); }
            h.extend(name.as_bytes());
            h
        };
        out.extend(hdr(0x04034b50, false));
        out.extend(*data);
        cd.extend(hdr(0x02014b50, true));
    }
    let cd_off = out.len() as u32;
    out.extend(&cd);
    out.extend(0x06054b50u32.to_le_bytes()); out.extend([0u8; 4]);
    out.extend((entries.len() as u16).to_le_bytes()); out.extend((entries.len() as u16).to_le_bytes());
    out.extend((cd.len() as u32).to_le_bytes()); out.extend(cd_off.to_le_bytes()); out.extend(0u16.to_le_bytes());
    out
}

#[test]
fn archive_plugin_with_zip_slip_entry_stays_inside() {
    if crate::archive::find_7za().is_none() { eprintln!("7za missing; skipped"); return; }
    let _e = env();
    let h = Http::start();
    let plugin = dummy_xex("zip");
    let z = zip(&[("Wob/wob.xex", &plugin), ("../../escape.txt", b"evil"), ("Wob/readme.txt", b"hi")]);
    let u = h.put("/z/wob.zip", z.clone());
    let mut e = plugin_entry("zipped", "1", &u, &z);
    e["files"][0]["install"]["dashlaunch"]["file"] = "Wob/wob.xex".into();
    let l = load(&manifest(vec![e], 1));
    let d = tmp("zipslip");
    let usb = d.join("usb"); std::fs::create_dir_all(&usb).unwrap();
    std::fs::write(usb.join("launch.ini"), INI).unwrap();
    let c = ctx(&d.join("store"));
    let r = install(&c, &l, "zipped", &local(&usb), &InstallOpts::default(), &mut |_, _| {}, &|| false);
    // Either 7-Zip drops the traversal entry or it lands inside Plugins; never outside.
    assert!(!d.join("escape.txt").exists() && !usb.join("escape.txt").exists() && !d.join("store/escape.txt").exists());
    let r = r.unwrap();
    assert_eq!(r.ini.as_deref(), Some("plugin3 = Usb:\\Plugins\\Wob\\wob.xex"));
    assert_eq!(std::fs::read(usb.join("Plugins/Wob/wob.xex")).unwrap(), plugin);
}

#[test]
fn tu_handler_checks_the_package() {
    let _e = env();
    let h = Http::start();
    let dlc = dummy_stfs(crate::dlc::CT_DLC, TID, "Map Pack");
    let u = h.put("/tu/TU_ABC", dlc.clone());
    let e = serde_json::json!({"id": "tu-x", "game": {"title_id": TID, "name": "Halo 3"}, "mod": {"name": "TU", "version": "1"}, "redistribute": true, "files": [{"url": u, "size": dlc.len(), "sha256": sha(&dlc), "install": {"handler": "tu"}}]});
    let l = load(&manifest(vec![e], 1));
    let d = tmp("tuh");
    let usb = d.join("usb"); std::fs::create_dir_all(&usb).unwrap();
    let c = ctx(&d.join("store"));
    let err = install(&c, &l, "tu-x", &local(&usb), &InstallOpts::default(), &mut |_, _| {}, &|| false).unwrap_err();
    assert!(err.contains("not a title update"), "{err}");
    assert!(!usb.join("Content").exists());
}

// ---- preflight ----

#[test]
fn tu_compat_against_detected_title_updates() {
    let _e = env();
    let d = tmp("tucompat");
    let usb = d.join("usb");
    let tu_dir = usb.join(format!("Content/0000000000000000/{TID}/000B0000"));
    std::fs::create_dir_all(&tu_dir).unwrap();
    let st = Store::new(d.join("store"));
    let t = local(&usb);
    let mk = |tus: serde_json::Value| { let mut e = plugin_entry("tuc", "1", "https://example.invalid/t.xex", &dummy_xex("t")); e["compat"] = serde_json::json!({"tu": tus, "dash_min": "3.21"}); load(&manifest(vec![e], 1)) };
    // nothing installed
    let l = mk(serde_json::json!([18]));
    assert!(preflight(&st, &l, &l.entries[0], &t).blockers.iter().any(|b| b.contains("no title update")));
    let l0 = mk(serde_json::json!([0, 18]));
    assert!(preflight(&st, &l0, &l0.entries[0], &t).blockers.is_empty());
    std::fs::write(tu_dir.join("TU_X"), dummy_stfs(crate::dlc::CT_TITLE_UPDATE, TID, "Title Update 18")).unwrap();
    let p = preflight(&st, &l, &l.entries[0], &t);
    assert!(p.blockers.is_empty() && p.detected_tus == vec![18], "{p:?}");
    assert!(p.info.iter().any(|i| i.contains("DashLaunch 3.21")));
    let l17 = mk(serde_json::json!([17]));
    assert!(preflight(&st, &l17, &l17.entries[0], &t).blockers.iter().any(|b| b.contains("TU18")));
    // a TU whose number can't be read -> warning, not a guess
    std::fs::remove_file(tu_dir.join("TU_X")).unwrap();
    std::fs::write(tu_dir.join("TU_Y"), dummy_stfs(crate::dlc::CT_TITLE_UPDATE, TID, "Update")).unwrap();
    let p = preflight(&st, &l17, &l17.entries[0], &t);
    assert!(p.blockers.is_empty() && p.warnings.iter().any(|w| w.contains("couldn't tell")), "{p:?}");
    assert_eq!(tu_number("Title Update #5"), Some(5));
    assert_eq!(tu_number("Update"), None);
    assert_eq!(tu_number("Tutorial 2"), None);
    assert_eq!(tu_number("TU 12"), Some(12));
    assert_eq!(tu_number("Halo 3 TU3"), Some(3));
}

#[test]
fn requires_conflicts_and_paid_are_checked_against_the_journal() {
    let _e = env();
    let d = tmp("deps");
    let usb = d.join("usb"); std::fs::create_dir_all(&usb).unwrap();
    let st = Store::new(d.join("store"));
    let t = local(&usb);
    let mut a = plugin_entry("base.lib", "1", "https://example.invalid/a.xex", &dummy_xex("a"));
    a["conflicts"] = serde_json::json!(["rival"]);
    let mut b = plugin_entry("addon", "1", "https://example.invalid/b.xex", &dummy_xex("b"));
    b["requires"] = serde_json::json!(["base.lib"]);
    let rival = plugin_entry("rival", "1", "https://example.invalid/r.xex", &dummy_xex("r"));
    let mut paid = plugin_entry("paid.one", "1", "https://example.invalid/p.xex", &dummy_xex("p"));
    paid["paid"] = true.into();
    let l = load(&manifest(vec![a, b, rival, paid], 1));
    let e = |id: &str| l.entries.iter().find(|e| e.id == id).unwrap();
    assert!(preflight(&st, &l, e("addon"), &t).blockers.iter().any(|x| x.contains("Needs") && x.contains("base.lib")));
    let mut j = st.journal().unwrap();
    j.installs.push(Installed { entry_id: "base.lib".into(), version: "1".into(), target: t.key(), complete: true, ..Default::default() });
    st.save_journal(&j).unwrap();
    assert!(preflight(&st, &l, e("addon"), &t).blockers.is_empty());
    // the installed entry lists the new one as a conflict (reverse direction)
    assert!(preflight(&st, &l, e("rival"), &t).blockers.iter().any(|x| x.contains("Conflicts")));
    // other targets are independent
    let other = d.join("usb2"); std::fs::create_dir_all(&other).unwrap();
    assert!(preflight(&st, &l, e("rival"), &local(&other)).blockers.is_empty());
    assert!(preflight(&st, &l, e("paid.one"), &t).blockers.iter().any(|x| x.contains("paid")));
    let c = Ctx { store: st, exe: None, tuning: Tuning::default() };
    assert!(install(&c, &l, "paid.one", &t, &InstallOpts::default(), &mut |_, _| {}, &|| false).unwrap_err().contains("paid"));
    // uninstall refuses while something needs it
    let mut j = c.store.journal().unwrap();
    j.installs.push(Installed { entry_id: "addon".into(), version: "1".into(), target: t.key(), name: "Addon".into(), complete: true, ..Default::default() });
    c.store.save_journal(&j).unwrap();
    assert!(uninstall(&c, Some(&l), "base.lib", &t, false).unwrap_err().contains("needs this"));
}

#[test]
fn fat_4gb_limit_on_the_console() {
    let _e = env();
    let mut e = plugin_entry("huge", "1", "https://example.invalid/h.bin", b"x");
    e["files"][0]["size"] = (5u64 << 30).into();
    e["files"][0]["install"] = serde_json::json!({"handler": "trainer"});
    let l = load(&manifest(vec![e], 1));
    let d = tmp("fat");
    let st = Store::new(d.join("store"));
    let t = Target::Console { con: crate::ftp::tests::con(1), drive: "Hdd1".into() };
    let p = preflight(&st, &l, &l.entries[0], &t);
    assert!(p.blockers.iter().any(|b| b.contains("4 GB")), "{p:?}");
}

#[test]
fn warnings_need_the_users_ok() {
    let _e = env();
    let h = Http::start();
    let body = dummy_xex("w");
    let u = h.put("/w.xex", body.clone());
    let l = load(&manifest(vec![plugin_entry("warn", "1", &u, &body)], 1));
    let d = tmp("warn");
    let usb = d.join("usb"); std::fs::create_dir_all(&usb).unwrap(); // no launch.ini
    let c = ctx(&d.join("store"));
    let t = local(&usb);
    let err = install(&c, &l, "warn", &t, &InstallOpts::default(), &mut |_, _| {}, &|| false).unwrap_err();
    assert!(err.starts_with("CONFIRM_WARNINGS:") && err.contains("launch.ini"), "{err}");
    let r = install(&c, &l, "warn", &t, &InstallOpts { accept_warnings: true, ..Default::default() }, &mut |_, _| {}, &|| false).unwrap();
    assert!(r.ini.is_none() && r.notes.iter().any(|n| n.contains("DashLaunch yourself")));
    assert!(!usb.join("launch.ini").exists(), "never creates a launch.ini");
}

// ---- console via the fake Aurora FTP server ----

#[test]
fn console_install_and_uninstall_over_ftp() {
    let _e = env();
    let h = Http::start();
    let body = dummy_xex("ftp");
    let u = h.put("/f/ftpplug.xex", body.clone());
    let l = load(&manifest(vec![plugin_entry("ftpplug", "1", &u, &body)], 1));
    let f = crate::ftp::tests::fake(true, true);
    f.fs.lock().unwrap().files.insert("/Hdd1/launch.ini".into(), INI.as_bytes().to_vec());
    let d = tmp("console");
    let c = ctx(&d.join("store"));
    let t = Target::Console { con: crate::ftp::tests::con(f.port), drive: "Hdd1".into() };
    let pf = preflight(&c.store, &l, &l.entries[0], &t);
    assert!(pf.ok && pf.info.iter().any(|i| i.contains("Free space")), "{pf:?}");
    let r = install(&c, &l, "ftpplug", &t, &InstallOpts::default(), &mut |_, _| {}, &|| false).unwrap();
    assert_eq!(r.ini.as_deref(), Some("plugin3 = Hdd:\\Plugins\\ftpplug.xex"));
    {
        let fs = f.fs.lock().unwrap();
        assert_eq!(fs.files["/Hdd1/Plugins/ftpplug.xex"], body);
        let ini = String::from_utf8(fs.files["/Hdd1/launch.ini"].clone()).unwrap();
        assert!(ini.contains("plugin3 = Hdd:\\Plugins\\ftpplug.xex\r\n") && ini.contains("plugin1 = Usb:\\Plugins\\main.xex"));
        assert!(fs.files.iter().any(|(k, v)| k.starts_with("/Hdd1/launch.ini.unpack-") && k.ends_with(".bak") && v == INI.as_bytes()));
        assert!(!fs.files.keys().any(|k| k.contains(".new") || k.ends_with(".part")));
    }
    let un = uninstall(&c, Some(&l), "ftpplug", &t, false).unwrap();
    assert_eq!(un.removed, 1);
    let fs = f.fs.lock().unwrap();
    assert!(!fs.files.contains_key("/Hdd1/Plugins/ftpplug.xex"));
    assert_eq!(fs.files["/Hdd1/launch.ini"], INI.as_bytes());
}

#[test]
fn console_tu_detection_reads_headers_over_ftp() {
    let _e = env();
    let f = crate::ftp::tests::fake(true, true);
    {
        let mut fs = f.fs.lock().unwrap();
        for p in ["/Hdd1/Content", "/Hdd1/Content/0000000000000000", &format!("/Hdd1/Content/0000000000000000/{TID}"), &format!("/Hdd1/Content/0000000000000000/{TID}/000B0000")] { fs.dirs.insert(p.to_string()); }
        let mut big = dummy_stfs(crate::dlc::CT_TITLE_UPDATE, TID, "Title Update 7");
        big.resize(3 << 20, 0); // only the header is read
        fs.files.insert(format!("/Hdd1/Content/0000000000000000/{TID}/000B0000/TU_7"), big);
    }
    let t = Target::Console { con: crate::ftp::tests::con(f.port), drive: "Hdd1".into() };
    assert_eq!(detect_tus(&t, TID).unwrap(), (vec![7], 0));
}

#[test]
fn stamp_format() {
    assert_eq!(stamp(0), "19700101-000000Z");
    assert_eq!(stamp(1_792_528_380), "20261020-203300Z");
}

#[test]
fn dispatcher_end_to_end_local() {
    let _e = env();
    let h = Http::start();
    let body = dummy_xex("disp");
    let u = h.put("/d/disp.xex", body.clone());
    let (s, p) = keys();
    let (b, sig) = signed(&manifest(vec![plugin_entry("disp", "1", &u, &body)], 4), &s);
    let cu = h.put("/cat.json", b);
    h.put("/cat.json.sig", sig);
    let d = tmp("disp");
    let usb = d.join("usb"); std::fs::create_dir_all(&usb).unwrap();
    std::fs::write(usb.join("launch.ini"), INI).unwrap();
    let c = ctx(&d.join("store"));
    let usb_s = usb.display().to_string();
    let grant = |x: &str| if x == usb_s { Ok(usb.clone()) } else { Err(format!("{x} wasn't picked")) };
    let call = |cmd: &str, a: serde_json::Value| super::call(&c, cmd, a, &grant, &mut |_, _| {}, &|| false);
    call("config_set", serde_json::json!({"config": {"source": "remote", "url": cu, "pubkey": p}})).unwrap();
    let l = call("refresh", serde_json::json!({})).unwrap();
    assert_eq!(l["entries"][0]["id"], "disp");
    let target = serde_json::json!({"kind": "local", "path": usb_s});
    assert_eq!(call("states", serde_json::json!({"target": target})).unwrap()[0]["state"], "new");
    call("install", serde_json::json!({"id": "disp", "target": target})).unwrap();
    assert_eq!(call("states", serde_json::json!({"target": target})).unwrap()[0]["state"], "installed");
    assert!(call("install", serde_json::json!({"id": "disp", "target": {"kind": "local", "path": "/elsewhere"}})).unwrap_err().contains("wasn't picked"));
    call("uninstall", serde_json::json!({"id": "disp", "target": target})).unwrap();
    assert_eq!(call("journal", serde_json::json!({})).unwrap()["installs"].as_array().unwrap().len(), 0);
}

// ---------------- Round 12 regressions ----------------

fn console_plugin_setup(tag: &str) -> (Http, Loaded, crate::ftp::tests::Fake, crate::testutil::TmpDir, Vec<u8>) {
    let h = Http::start();
    let body = dummy_xex(tag);
    let u = h.put(&format!("/f/{tag}.xex"), body.clone());
    let l = load(&manifest(vec![plugin_entry(tag, "1", &u, &body)], 1));
    let f = crate::ftp::tests::fake(true, true);
    f.fs.lock().unwrap().files.insert("/Hdd1/launch.ini".into(), INI.as_bytes().to_vec());
    (h, l, f, tmp(tag), body)
}

#[test]
fn r12_high1_failed_console_rename_puts_launch_ini_back() {
    let _e = env();
    let (_h, l, f, d, _) = console_plugin_setup("rnfail");
    // The new file's RNTO into place fails once; renaming the original back succeeds.
    { let mut fs = f.fs.lock().unwrap(); fs.rnto_fail_to = Some("/Hdd1/launch.ini".into()); fs.rnto_fail_times = 1; }
    let c = ctx(&d.join("store"));
    let t = Target::Console { con: crate::ftp::tests::con(f.port), drive: "Hdd1".into() };
    let err = install(&c, &l, "rnfail", &t, &InstallOpts::default(), &mut |_, _| {}, &|| false).unwrap_err();
    assert!(err.contains("was put back"), "{err}");
    let fs = f.fs.lock().unwrap();
    assert_eq!(fs.files["/Hdd1/launch.ini"], INI.as_bytes(), "original launch.ini restored");
    assert!(!fs.files.keys().any(|k| k.contains("launch.ini.old") || k.contains(".new")), "{:?}", fs.files.keys().collect::<Vec<_>>());
    // DELE of launch.ini never happened before the new file was in place.
    let first_dele = fs.log.iter().position(|x| x.starts_with("DELE") && x.ends_with("/launch.ini"));
    assert!(first_dele.is_none(), "launch.ini was deleted: {:?}", fs.log);
}

#[test]
fn r12_high1_rename_back_fails_too_original_is_reuploaded() {
    let _e = env();
    let (_h, l, f, d, _) = console_plugin_setup("rnfail2");
    { let mut fs = f.fs.lock().unwrap(); fs.rnto_fail_to = Some("/Hdd1/launch.ini".into()); fs.rnto_fail_times = usize::MAX; }
    let c = ctx(&d.join("store"));
    let t = Target::Console { con: crate::ftp::tests::con(f.port), drive: "Hdd1".into() };
    let err = install(&c, &l, "rnfail2", &t, &InstallOpts::default(), &mut |_, _| {}, &|| false).unwrap_err();
    assert!(err.contains("was put back"), "{err}");
    let fs = f.fs.lock().unwrap();
    assert_eq!(fs.files["/Hdd1/launch.ini"], INI.as_bytes());
    assert!(!fs.files.keys().any(|k| k.contains("launch.ini.old")));
}

#[test]
fn r12_high1_console_swap_success_leaves_no_old_copy() {
    let _e = env();
    let (_h, l, f, d, _) = console_plugin_setup("swapok");
    f.fs.lock().unwrap().files.insert("/Hdd1/launch.ini.old".into(), b"user file".to_vec());
    let c = ctx(&d.join("store"));
    let t = Target::Console { con: crate::ftp::tests::con(f.port), drive: "Hdd1".into() };
    install(&c, &l, "swapok", &t, &InstallOpts::default(), &mut |_, _| {}, &|| false).unwrap();
    let fs = f.fs.lock().unwrap();
    assert!(String::from_utf8_lossy(&fs.files["/Hdd1/launch.ini"]).contains("plugin3 = Hdd:\\Plugins\\swapok.xex"));
    assert_eq!(fs.files["/Hdd1/launch.ini.old"], b"user file", "an existing .old is never touched");
    assert!(!fs.files.contains_key("/Hdd1/launch.ini.old-2"));
    let aside = fs.log.iter().position(|x| x.contains("RNTO") && x.contains("launch.ini.old-2")).expect("moved aside first");
    let newin = fs.log.iter().rposition(|x| x.starts_with("RNTO") && x.ends_with("/launch.ini")).unwrap();
    assert!(aside < newin);
}

#[test]
fn r12_high1_local_swap_failure_restores_original() {
    let d = tmp("localswap");
    let usb = d.join("usb"); std::fs::create_dir_all(&usb).unwrap();
    std::fs::write(usb.join("launch.ini"), INI).unwrap();
    let t = local(&usb);
    super::target::fail::LOCAL_SWAP.with(|c| c.set(true));
    let err = t.replace(&["launch.ini".into()], b"new content").unwrap_err();
    super::target::fail::LOCAL_SWAP.with(|c| c.set(false));
    assert!(err.contains("was put back"), "{err}");
    assert_eq!(std::fs::read_to_string(usb.join("launch.ini")).unwrap(), INI);
    let names: Vec<String> = std::fs::read_dir(&usb).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(names, vec!["launch.ini".to_string()], "no .old / .unpack-new left");
    t.replace(&["launch.ini".into()], b"new content").unwrap();
    assert_eq!(std::fs::read(usb.join("launch.ini")).unwrap(), b"new content");
    assert_eq!(std::fs::read_dir(&usb).unwrap().count(), 1);
}

#[test]
fn r12_medium1_shared_file_survives_other_mods_uninstall() {
    let _e = env();
    let h = Http::start();
    let body = dummy_xex("shared");
    let u = h.put("/s/shared.xex", body.clone());
    let mut a = plugin_entry("moda", "1", &u, &body);
    let mut b = plugin_entry("modb", "1", &u, &body);
    a["mod"]["name"] = "Mod A".into(); b["mod"]["name"] = "Mod B".into();
    let l = load(&manifest(vec![a, b], 1));
    let d = tmp("shared");
    let usb = d.join("usb"); std::fs::create_dir_all(&usb).unwrap();
    std::fs::write(usb.join("launch.ini"), INI).unwrap();
    let c = ctx(&d.join("store"));
    let t = local(&usb);
    install(&c, &l, "moda", &t, &InstallOpts::default(), &mut |_, _| {}, &|| false).unwrap();
    let rb = install(&c, &l, "modb", &t, &InstallOpts::default(), &mut |_, _| {}, &|| false).unwrap();
    assert!(rb.ini.unwrap().contains("already points"));
    // Uninstall B: the file and A's launch.ini line stay.
    let u = uninstall(&c, Some(&l), "modb", &t, false).unwrap();
    assert_eq!(u.removed, 0);
    assert!(u.kept.iter().any(|k| k.contains("used by Mod A")), "{u:?}");
    assert_eq!(std::fs::read(usb.join("Plugins/shared.xex")).unwrap(), body);
    assert!(std::fs::read_to_string(usb.join("launch.ini")).unwrap().contains("plugin3 = Usb:\\Plugins\\shared.xex"));
    // Reverse order: install B again, uninstall A first; the line A added stays because B uses it.
    install(&c, &l, "modb", &t, &InstallOpts::default(), &mut |_, _| {}, &|| false).unwrap();
    let ua = uninstall(&c, Some(&l), "moda", &t, false).unwrap();
    assert!(ua.ini.as_deref().unwrap_or("").contains("used by Mod B"), "{ua:?}");
    assert!(usb.join("Plugins/shared.xex").exists());
    assert!(std::fs::read_to_string(usb.join("launch.ini")).unwrap().contains("plugin3 = Usb:\\Plugins\\shared.xex"));
    // Last user removes it.
    let ub = uninstall(&c, Some(&l), "modb", &t, false).unwrap();
    assert_eq!(ub.removed, 1);
    assert!(!usb.join("Plugins/shared.xex").exists());
}

#[test]
fn r12_low1_unpack_cap() {
    assert_eq!(unpack_cap(1000), 256 << 20);
    assert_eq!(unpack_cap(1 << 30), 4 << 30);
    assert_eq!(unpack_cap(10 << 30), UNPACK_ABS_MAX);
    assert_eq!(unpack_cap(u64::MAX), UNPACK_ABS_MAX);
}

#[test]
fn r12_low1_zip_bomb_is_refused_before_unpacking() {
    let Some(exe) = crate::archive::find_7za() else { eprintln!("7za missing; skipped"); return; };
    let _e = env();
    let d = tmp("bomb");
    let src = d.join("src"); std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("bomb.xex"), dummy_xex("bomb")).unwrap();
    let f = std::fs::File::create(src.join("zeros.bin")).unwrap();
    f.set_len(300 << 20).unwrap(); drop(f);
    let zp = d.join("bomb.zip");
    let st = std::process::Command::new(&exe).arg("a").arg("-tzip").arg("-mx1").arg("-bd").arg(&zp).arg(src.join("bomb.xex")).arg(src.join("zeros.bin")).stdout(std::process::Stdio::null()).status().unwrap();
    assert!(st.success());
    let z = std::fs::read(&zp).unwrap();
    assert!(z.len() < 4 << 20, "compresses small: {}", z.len());
    let h = Http::start();
    let u = h.put("/b/bomb.zip", z.clone());
    let mut e = plugin_entry("bomb", "1", &u, &z);
    e["files"][0]["install"]["dashlaunch"]["file"] = "bomb.xex".into();
    let l = load(&manifest(vec![e], 1));
    let usb = d.join("usb"); std::fs::create_dir_all(&usb).unwrap();
    let c = ctx(&d.join("store"));
    let err = install(&c, &l, "bomb", &local(&usb), &InstallOpts { accept_warnings: true, ..Default::default() }, &mut |_, _| {}, &|| false).unwrap_err();
    assert!(err.contains("would unpack to") && err.contains("Not unpacked"), "{err}");
    assert!(!usb.join("Plugins").exists());
    assert_eq!(std::fs::read_dir(d.join("store/stage")).unwrap().count(), 0);
}

#[test]
fn r12_low2_corrupt_journal_and_seen_are_not_empty() {
    let _e = env();
    let h = Http::start();
    let body = dummy_xex("cj");
    let u = h.put("/c/cj.xex", body.clone());
    let l = load(&manifest(vec![plugin_entry("cj", "1", &u, &body)], 1));
    let d = tmp("corrupt");
    let usb = d.join("usb"); std::fs::create_dir_all(&usb).unwrap();
    let c = ctx(&d.join("store"));
    std::fs::create_dir_all(d.join("store")).unwrap();
    std::fs::write(d.join("store/journal.json"), b"{corrupt").unwrap();
    let t = local(&usb);
    let e = c.store.journal().unwrap_err();
    assert!(e.contains("damaged") && e.contains("journal.json"), "{e}");
    assert!(states(&c.store, &l, &t.key()).is_err());
    assert!(preflight(&c.store, &l, &l.entries[0], &t).blockers.iter().any(|b| b.contains("damaged")));
    assert!(install(&c, &l, "cj", &t, &InstallOpts::default(), &mut |_, _| {}, &|| false).unwrap_err().contains("damaged"));
    assert!(uninstall(&c, Some(&l), "cj", &t, false).unwrap_err().contains("damaged"));
    assert_eq!(std::fs::read(d.join("store/journal.json")).unwrap(), b"{corrupt", "never overwritten");
    let bad: Vec<_> = std::fs::read_dir(d.join("store")).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().starts_with("journal.json.bad-")).collect();
    assert_eq!(bad.len(), 1, "one copy kept aside, not one per call");
    assert!(!usb.join("Plugins").exists());
    // seen.json: the rollback floor is kept (refused), not reset.
    std::fs::write(d.join("store/seen.json"), b"{corrupt").unwrap();
    let err = c.store.check_rollback(&"ab".repeat(32), 1).unwrap_err();
    assert!(err.contains("damaged"), "{err}");
    assert_eq!(std::fs::read(d.join("store/seen.json")).unwrap(), b"{corrupt");
}

#[test]
fn r12_low3_backup_names_are_unique_within_a_second() {
    let a: std::collections::HashSet<String> = (0..200).map(|_| unique_stamp()).collect();
    assert_eq!(a.len(), 200);
    let _e = env();
    let h = Http::start();
    let body = dummy_xex("bk");
    let u = h.put("/k/bk.xex", body.clone());
    let l = load(&manifest(vec![plugin_entry("bk", "1", &u, &body)], 1));
    let d = tmp("backups");
    let usb = d.join("usb"); std::fs::create_dir_all(&usb).unwrap();
    std::fs::write(usb.join("launch.ini"), INI).unwrap();
    let c = ctx(&d.join("store"));
    let t = local(&usb);
    install(&c, &l, "bk", &t, &InstallOpts::default(), &mut |_, _| {}, &|| false).unwrap();
    uninstall(&c, Some(&l), "bk", &t, false).unwrap();
    let backups: Vec<Vec<u8>> = std::fs::read_dir(d.join("store/backups")).unwrap().flatten().map(|e| std::fs::read(e.path()).unwrap()).collect();
    assert_eq!(backups.len(), 2, "install + uninstall each keep their own copy");
    assert!(backups.iter().any(|b| b == INI.as_bytes()), "the original survives");
}

#[test]
fn r12_low4_size_messages_name_the_real_limit() {
    let _e = env();
    let (s, p) = keys();
    let big = vec![b' '; (MANIFEST_MAX + 1) as usize];
    let e = load_bytes(&big, &sign(&big, &s).unwrap().into_bytes(), &p, "t").unwrap_err();
    assert!(e.contains("4.0 MB"), "{e}");
    let h = Http::start();
    let u = h.put("/big.json", big);
    let d = tmp("bigcat");
    let st = Store::new(d.join("store"));
    let e = fetch_small(&st, &u, MANIFEST_MAX, &|| false).unwrap_err();
    assert!(e.contains("4.0 MB limit for catalog files") && !e.contains("Settings") && !e.contains("GB"), "{e}");
    assert_eq!(crate::download::human_size(1024), "1.0 KB");
    assert_eq!(crate::download::human_size(20 << 30), "21.5 GB");
}

#[test]
fn r12_low5_catalog_version_is_capped() {
    let (s, p) = keys();
    for v in [MAX_CATALOG_VERSION, u64::MAX] {
        let (b, sig) = signed(&manifest(vec![], v), &s);
        assert!(load_bytes(&b, &sig, &p, "t").unwrap_err().contains("too large"));
    }
    let (b, sig) = signed(&manifest(vec![], MAX_CATALOG_VERSION - 1), &s);
    assert!(load_bytes(&b, &sig, &p, "t").is_ok());
}

// ---------------- Round 13 regressions ----------------

use crate::ftp::tests::Fault;

fn console_files(f: &crate::ftp::tests::Fake) -> Vec<String> { f.fs.lock().unwrap().files.keys().filter(|k| k.starts_with("/Hdd1/launch.ini") && !k.ends_with(".bak")).cloned().collect() }

/// Run one console install with `faults`, then return (result, files left).
fn console_with_faults(tag: &str, faults: Vec<Fault>) -> (Result<Report, String>, crate::ftp::tests::Fake, Ctx, Target, Loaded, crate::testutil::TmpDir, Http) {
    let (h, l, f, d, _) = console_plugin_setup(tag);
    f.fs.lock().unwrap().faults = faults;
    let c = ctx(&d.join("store"));
    let t = Target::Console { con: crate::ftp::tests::con(f.port), drive: "Hdd1".into() };
    let r = install(&c, &l, tag, &t, &InstallOpts::default(), &mut |_, _| {}, &|| false);
    (r, f, c, t, l, d, h)
}

#[test]
fn r13_medium1_drop_at_each_swap_step_never_loses_launch_ini() {
    let _e = env();
    let new_line = |tag: &str| format!("plugin3 = Hdd:\\Plugins\\{tag}.xex");
    // a) drop on the aside rename: nothing moved, launch.ini untouched, upload tidied.
    let (r, f, _c, _t, _l, _d, _h) = console_with_faults("dropa", vec![Fault::new("RNTO", "/Hdd1/launch.ini.old", true, 1)]);
    let e = r.unwrap_err();
    assert!(e.contains("unchanged"), "{e}");
    assert_eq!(f.fs.lock().unwrap().files["/Hdd1/launch.ini"], INI.as_bytes());
    assert_eq!(console_files(&f), vec!["/Hdd1/launch.ini".to_string()]);
    // b) drop on the new-file rename: original moved aside; a fresh connection puts it back.
    let (r, f, _c, _t, _l, _d, _h) = console_with_faults("dropb", vec![Fault::new("RNTO", "/Hdd1/launch.ini", true, 1)]);
    let e = r.unwrap_err();
    assert!(e.contains("was put back"), "{e}");
    assert_eq!(f.fs.lock().unwrap().files["/Hdd1/launch.ini"], INI.as_bytes());
    assert_eq!(console_files(&f), vec!["/Hdd1/launch.ini".to_string()]);
    assert!(f.fs.lock().unwrap().log.iter().filter(|l| l.starts_with("USER")).count() >= 2, "reconnected");
    // c) drop on the size check right after the new file went in: it IS in place, install succeeds.
    let mut fc = Fault::new("SIZE", "/Hdd1/launch.ini", true, 1);
    fc.after = Some("RNTO /Hdd1/launch.ini.old".into());
    let (r, f, _c, _t, _l, _d, _h) = console_with_faults("dropc", vec![fc]);
    r.unwrap();
    assert!(String::from_utf8_lossy(&f.fs.lock().unwrap().files["/Hdd1/launch.ini"]).contains(&new_line("dropc")));
    assert_eq!(console_files(&f), vec!["/Hdd1/launch.ini".to_string()]);
    // d) drop on deleting .old after a good swap: install succeeds; the next run prunes the .old.
    let (r, f, c, t, l, _d, _h) = console_with_faults("dropd", vec![Fault::new("DELE", "/Hdd1/launch.ini.old", true, 1)]);
    r.unwrap();
    assert!(console_files(&f).contains(&"/Hdd1/launch.ini.old".to_string()));
    let pf = preflight(&c.store, &l, &l.entries[0], &t);
    assert_eq!(console_files(&f), vec!["/Hdd1/launch.ini".to_string()], "leftover .old pruned once launch.ini is verified: {pf:?} {:?}", f.fs.lock().unwrap().log.iter().rev().take(30).collect::<Vec<_>>());
    // e) drop during the upload of the new file: launch.ini untouched; strays tidied next run.
    let (r, f, c, t, l, _d, _h) = console_with_faults("drope", vec![Fault::new("STOR", "/Hdd1/launch.ini.new.part", true, 1)]);
    assert!(r.is_err());
    f.fs.lock().unwrap().files.insert("/Hdd1/launch.ini.new.part".into(), b"half".to_vec());
    let _ = preflight(&c.store, &l, &l.entries[0], &t);
    assert_eq!(f.fs.lock().unwrap().files["/Hdd1/launch.ini"], INI.as_bytes());
    assert_eq!(console_files(&f), vec!["/Hdd1/launch.ini".to_string()]);
}

#[test]
fn r13_medium1_refused_rename_back_and_reupload_is_repaired_next_run() {
    let _e = env();
    let (r, f, c, t, l, _d, _h) = console_with_faults("stuck", vec![
        Fault::new("RNTO", "/Hdd1/launch.ini", false, usize::MAX),
        Fault::new("STOR", "/Hdd1/launch.ini", false, usize::MAX),
    ]);
    let e = r.unwrap_err();
    assert!(e.contains("could NOT be put back yet") && e.contains("launch.ini.old") && e.contains("automatically"), "{e}");
    assert!(!f.fs.lock().unwrap().files.contains_key("/Hdd1/launch.ini"));
    assert_eq!(f.fs.lock().unwrap().files["/Hdd1/launch.ini.old"], INI.as_bytes());
    // The console behaves again: the next look at this target restores launch.ini before anything else.
    f.fs.lock().unwrap().faults.clear();
    let pf = preflight(&c.store, &l, &l.entries[0], &t);
    assert!(pf.info.iter().any(|i| i.contains("put back your previous one from launch.ini.old")), "{pf:?}");
    assert!(!pf.warnings.iter().any(|w| w.contains("No launch.ini")), "{pf:?}");
    assert_eq!(f.fs.lock().unwrap().files["/Hdd1/launch.ini"], INI.as_bytes());
    assert_eq!(console_files(&f), vec!["/Hdd1/launch.ini".to_string()]);
    // And the install now goes through normally.
    let r = install(&c, &l, "stuck", &t, &InstallOpts { accept_warnings: true, ..Default::default() }, &mut |_, _| {}, &|| false).unwrap();
    assert_eq!(r.ini.as_deref(), Some("plugin3 = Hdd:\\Plugins\\stuck.xex"));
}

#[test]
fn r13_medium1_install_repairs_before_reading_and_local_too() {
    let _e = env();
    // Console: launch.ini missing, an Unpack .old present → install restores first, never says "No launch.ini".
    let (_h, l, f, d, _) = console_plugin_setup("rep");
    { let mut fs = f.fs.lock().unwrap(); let b = fs.files.remove("/Hdd1/launch.ini").unwrap(); fs.files.insert("/Hdd1/launch.ini.old-3".into(), b); fs.files.insert("/Hdd1/launch.ini.new.part".into(), b"x".to_vec()); }
    let c = ctx(&d.join("store"));
    let t = Target::Console { con: crate::ftp::tests::con(f.port), drive: "Hdd1".into() };
    let r = install(&c, &l, "rep", &t, &InstallOpts::default(), &mut |_, _| {}, &|| false).unwrap();
    assert!(r.notes.iter().any(|n| n.contains("put back your previous one from launch.ini.old-3")), "{r:?}");
    assert!(!r.notes.iter().any(|n| n.contains("No launch.ini")));
    assert_eq!(r.ini.as_deref(), Some("plugin3 = Hdd:\\Plugins\\rep.xex"));
    assert_eq!(console_files(&f), vec!["/Hdd1/launch.ini".to_string()]);
    // Local: same repair.
    let d2 = tmp("replocal");
    let usb = d2.join("usb"); std::fs::create_dir_all(&usb).unwrap();
    std::fs::write(usb.join("launch.ini.old"), INI).unwrap();
    std::fs::write(usb.join("launch.ini.unpack-new"), "half").unwrap();
    let c2 = ctx(&d2.join("store"));
    let notes = repair_ini(&c2.store, &local(&usb)).unwrap();
    assert_eq!(notes.len(), 1);
    assert_eq!(std::fs::read_to_string(usb.join("launch.ini")).unwrap(), INI);
    assert_eq!(std::fs::read_dir(&usb).unwrap().count(), 1);
    // A user's own .old (not one of Unpack's backups) next to a present launch.ini is left alone.
    std::fs::write(usb.join("launch.ini.old"), "mine").unwrap();
    repair_ini(&c2.store, &local(&usb)).unwrap();
    assert_eq!(std::fs::read_to_string(usb.join("launch.ini.old")).unwrap(), "mine");
}

#[test]
fn r13_low1_ini_line_passes_to_remaining_user() {
    let _e = env();
    let h = Http::start();
    let body = dummy_xex("shared3");
    let u = h.put("/s/shared.xex", body.clone());
    let mut es = vec![];
    for n in ["m1", "m2", "m3"] { let mut e = plugin_entry(n, "1", &u, &body); e["mod"]["name"] = format!("Mod {n}").into(); es.push(e); }
    let l = load(&manifest(es, 1));
    let d = tmp("handover");
    let usb = d.join("usb"); std::fs::create_dir_all(&usb).unwrap();
    std::fs::write(usb.join("launch.ini"), INI).unwrap();
    let c = ctx(&d.join("store"));
    let t = local(&usb);
    for n in ["m1", "m2", "m3"] { install(&c, &l, n, &t, &InstallOpts::default(), &mut |_, _| {}, &|| false).unwrap(); }
    let line = "plugin3 = Usb:\\Plugins\\shared.xex";
    uninstall(&c, Some(&l), "m2", &t, false).unwrap();
    assert!(std::fs::read_to_string(usb.join("launch.ini")).unwrap().contains(line) && usb.join("Plugins/shared.xex").exists());
    let u1 = uninstall(&c, Some(&l), "m1", &t, false).unwrap();
    assert!(u1.ini.unwrap().contains("used by Mod m3"));
    assert!(c.store.journal().unwrap().find(&t.key(), "m3").unwrap().ini.iter().any(|x| x.key == "plugin3"), "ownership handed to m3");
    let u3 = uninstall(&c, Some(&l), "m3", &t, false).unwrap();
    assert_eq!(u3.removed, 1);
    assert!(!usb.join("Plugins/shared.xex").exists());
    assert_eq!(std::fs::read_to_string(usb.join("launch.ini")).unwrap(), INI, "no line left pointing at a missing file");
}

#[test]
fn r13_low2_old_copies_never_lock_out_and_are_pruned() {
    // Name picking never refuses, even with .old … .old-99 taken.
    let mut calls = 0;
    let name = super::target::free_old_name(&[], "launch.ini", |s| { calls += 1; Ok(if calls <= 99 { Some(1) } else { let _ = s; None }) }).unwrap();
    assert!(name[0].starts_with("launch.ini.old-") && name[0].len() <= 42 && super::target::is_old_name(&name[0], "launch.ini"), "{name:?}");
    // Local: 100 old copies of the user's own content; replace still works.
    let d = tmp("manyold");
    let usb = d.join("usb"); std::fs::create_dir_all(&usb).unwrap();
    std::fs::write(usb.join("launch.ini"), INI).unwrap();
    std::fs::write(usb.join("launch.ini.old"), "user").unwrap();
    for k in 2..100 { std::fs::write(usb.join(format!("launch.ini.old-{k}")), "user").unwrap(); }
    local(&usb).replace(&["launch.ini".into()], b"[Plugins]\r\n").unwrap();
    assert_eq!(std::fs::read(usb.join("launch.ini")).unwrap(), b"[Plugins]\r\n");
    // Console: DELE of .old refused twice → two leftovers; once deletes work, the next run prunes both.
    let _e = env();
    let (r, f, c, t, l, _d, _h) = console_with_faults("nodele", vec![Fault::new("DELE", "/Hdd1/launch.ini.old", false, 2)]);
    r.unwrap();
    assert!(console_files(&f).contains(&"/Hdd1/launch.ini.old".to_string()));
    f.fs.lock().unwrap().faults = vec![Fault::new("DELE", "/Hdd1/launch.ini.old", false, usize::MAX), Fault::new("DELE", "/Hdd1/launch.ini.old-2", false, usize::MAX)];
    uninstall(&c, Some(&l), "nodele", &t, false).unwrap();
    assert!(console_files(&f).len() >= 3, "{:?}", console_files(&f));
    f.fs.lock().unwrap().faults.clear();
    let _ = preflight(&c.store, &l, &l.entries[0], &t);
    assert_eq!(console_files(&f), vec!["/Hdd1/launch.ini".to_string()]);
    assert_eq!(f.fs.lock().unwrap().files["/Hdd1/launch.ini"], INI.as_bytes());
}

#[test]
fn r13_low3_messages() {
    let d = tmp("msgs");
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("seen.json"), b"{x").unwrap();
    let st = Store::new(d.to_path_buf());
    let e = st.check_rollback(&"ab".repeat(32), 1).unwrap_err();
    assert!(e.contains("resets rollback protection") && !e.contains("forgets what Unpack installed"), "{e}");
    std::fs::write(d.join("journal.json"), b"{x").unwrap();
    assert!(st.journal().unwrap_err().contains("forgets what Unpack installed"));
    assert_eq!(super::target::short_err_for_test("The console's FTP server can't rename files (550 no). The upload is kept as /Hdd1/launch.ini."), "The console's FTP server can't rename files (550 no)");
    let cap = unpack_cap(1);
    let (a, b) = super::install::two_sizes_for_test(cap + 1, cap);
    assert_eq!((a.as_str(), b.as_str()), ("268435457 bytes", "268435456 bytes"));
    let (a, b) = super::install::two_sizes_for_test(2 << 30, cap);
    assert!(a.contains("GB") && b.contains("MB"));
}

// ---------------- Round 14 regressions + self-tests ----------------

#[test]
fn r14_medium_launch_ini_names_match_case_insensitively() {
    let _e = env();
    // Console: LAUNCH.INI present plus a lower-case .old → present; nothing blocks; .old pruned.
    let (_h, l, f, d, _) = console_plugin_setup("upper");
    { let mut fs = f.fs.lock().unwrap(); let b = fs.files.remove("/Hdd1/launch.ini").unwrap(); fs.files.insert("/Hdd1/LAUNCH.INI".into(), b.clone()); fs.files.insert("/Hdd1/launch.ini.old".into(), b); fs.files.insert("/Hdd1/Launch.ini.New.part".into(), b"x".to_vec()); }
    let c = ctx(&d.join("store"));
    let t = Target::Console { con: crate::ftp::tests::con(f.port), drive: "Hdd1".into() };
    let pf = preflight(&c.store, &l, &l.entries[0], &t);
    assert!(pf.blockers.is_empty() && pf.warnings.is_empty(), "{pf:?}");
    let left: Vec<String> = f.fs.lock().unwrap().files.keys().filter(|k| k.to_lowercase().starts_with("/hdd1/launch.ini") && !k.ends_with(".bak")).cloned().collect();
    assert_eq!(left, vec!["/Hdd1/LAUNCH.INI".to_string()]);
    let r = install(&c, &l, "upper", &t, &InstallOpts::default(), &mut |_, _| {}, &|| false).unwrap();
    assert_eq!(r.ini.as_deref(), Some("plugin3 = Hdd:\\Plugins\\upper.xex"));
    assert!(String::from_utf8_lossy(&f.fs.lock().unwrap().files["/Hdd1/LAUNCH.INI"]).contains("plugin3 = Hdd:\\Plugins\\upper.xex"));
    assert!(!f.fs.lock().unwrap().files.contains_key("/Hdd1/launch.ini"), "no second lower-case file created");
    uninstall(&c, Some(&l), "upper", &t, false).unwrap();
    assert_eq!(f.fs.lock().unwrap().files["/Hdd1/LAUNCH.INI"], INI.as_bytes());
    // Local: mixed-case .old names are recognised.
    assert!(super::target::is_old_name("LAUNCH.INI.OLD-12", "launch.ini") && super::target::is_old_name("Launch.ini.Old", "launch.ini"));
    assert!(!super::target::is_old_name("launch.ini.older", "launch.ini") && !super::target::is_old_name("launch.ini.old-", "launch.ini") && !super::target::is_old_name("launch.in", "launch.ini"));
    let d2 = tmp("upperlocal");
    let usb = d2.join("usb"); std::fs::create_dir_all(&usb).unwrap();
    std::fs::write(usb.join("Launch.Ini.OLD"), INI).unwrap();
    let c2 = ctx(&d2.join("store"));
    assert_eq!(repair_ini(&c2.store, &local(&usb)).unwrap().len(), 1);
    assert_eq!(std::fs::read_to_string(usb.join("launch.ini")).unwrap(), INI);
    // Double period: a failed restore message never ends a clause with "..".
    std::fs::remove_file(usb.join("launch.ini")).unwrap();
    std::fs::write(usb.join("launch.ini.old"), INI).unwrap();
    std::fs::create_dir(usb.join("launch.ini")).unwrap(); // a folder squatting on the name
    let e = repair_ini(&c2.store, &local(&usb)).unwrap_err();
    assert!(!e.contains("..") && e.contains("Rename launch.ini.old to launch.ini yourself"), "{e}");
}

#[test]
fn r14_low1_old_copy_is_checked_before_restoring() {
    let d = tmp("oldcheck");
    let usb = d.join("usb"); std::fs::create_dir_all(&usb).unwrap();
    let c = ctx(&d.join("store"));
    let t = local(&usb);
    let mut rnd = vec![0u8; 3000];
    let mut x = 12345u32;
    for b in rnd.iter_mut() { x = x.wrapping_mul(1103515245).wrapping_add(12345); *b = (x >> 16) as u8; }
    let cases: Vec<(&str, Vec<u8>, &str)> = vec![
        ("empty", vec![], "is empty"),
        ("random", rnd, "not plain text"),
        ("huge", vec![b'a'; 20 << 20], "larger than 64 KB"),
    ];
    for (what, bytes, why) in cases {
        std::fs::write(usb.join("launch.ini.old"), &bytes).unwrap();
        let e = repair_ini(&c.store, &t).unwrap_err();
        assert!(e.contains("launch.ini.old") && e.contains(why) && e.contains("won't put that back"), "{what}: {e}");
        assert!(!usb.join("launch.ini").exists(), "{what}: not restored");
        assert_eq!(std::fs::read(usb.join("launch.ini.old")).unwrap().len(), bytes.len(), "{what}: left alone");
    }
    // With backups for this target, only a copy matching one of them is put back.
    let kh = key_hash(&t.key());
    c.store.write_new_file(&format!("backups/{kh}-20261020-000000Z-000-0-launch.ini"), INI.as_bytes()).unwrap();
    std::fs::write(usb.join("launch.ini.old"), "[Plugins]\r\nplugin2 = Usb:\\evil.xex\r\n").unwrap();
    let e = repair_ini(&c.store, &t).unwrap_err();
    assert!(e.contains("doesn't match any launch.ini Unpack backed up"), "{e}");
    std::fs::write(usb.join("launch.ini.old-2"), INI).unwrap();
    let n = repair_ini(&c.store, &t).unwrap();
    assert!(n[0].contains("launch.ini.old-2"), "{n:?}");
    assert_eq!(std::fs::read_to_string(usb.join("launch.ini")).unwrap(), INI);
    assert!(usb.join("launch.ini.old").exists(), "the non-matching copy is kept for the user");
    // Preflight shows it as a blocker naming the file.
    std::fs::remove_file(usb.join("launch.ini")).unwrap();
    let body = dummy_xex("x");
    let l = load(&manifest(vec![plugin_entry("x", "1", "https://example.invalid/x.xex", &body)], 1));
    let pf = preflight(&c.store, &l, &l.entries[0], &t);
    assert!(pf.blockers.iter().any(|b| b.contains("launch.ini.old")), "{pf:?}");
}

fn usr_dead_after(marker: &str) -> Fault { let mut f = Fault::new("USER", "/xboxftp", true, usize::MAX); f.after = Some(marker.into()); f }

#[test]
fn r14_low23_swap_messages_say_what_happened() {
    let _e = env();
    // Dropped on the aside RNFR and no reconnect: launch.ini was never moved; no .old claimed.
    let (r, f, _c, _t, _l, _d, _h) = console_with_faults("dropaside", vec![
        Fault::new("RNFR", "/Hdd1/launch.ini", true, 1),
        usr_dead_after("RNFR /Hdd1/launch.ini$"),
    ]);
    let e = r.unwrap_err();
    assert!(e.contains("most likely not moved") && !e.contains("could NOT be put back"), "{e} {:?}", f.fs.lock().unwrap().log.iter().rev().take(25).collect::<Vec<_>>());
    assert_eq!(f.fs.lock().unwrap().files["/Hdd1/launch.ini"], INI.as_bytes());
    assert!(!f.fs.lock().unwrap().files.contains_key("/Hdd1/launch.ini.old"));
    // The new file went in; the size check dropped and reconnects fail: say "unverified", not "failed to restore".
    let mut sz = Fault::new("SIZE", "/Hdd1/launch.ini", true, 1);
    sz.after = Some("RNFR /Hdd1/launch.ini.new.part".into());
    let (r, f, c, t, l, _d, _h) = console_with_faults("afternew", vec![sz, usr_dead_after("RNFR /Hdd1/launch.ini.new.part")]);
    let e = r.unwrap_err();
    assert!(e.contains("renamed into place but couldn't be verified") && !e.contains("could NOT be put back"), "{e}");
    assert!(String::from_utf8_lossy(&f.fs.lock().unwrap().files["/Hdd1/launch.ini"]).contains("afternew.xex"));
    // Next connect: the stale .old is pruned and the mod line stays.
    f.fs.lock().unwrap().faults.clear();
    let _ = preflight(&c.store, &l, &l.entries[0], &t);
    assert_eq!(console_files(&f), vec!["/Hdd1/launch.ini".to_string()]);
    assert!(String::from_utf8_lossy(&f.fs.lock().unwrap().files["/Hdd1/launch.ini"]).contains("afternew.xex"));
}

fn shared_mods(n: usize, tag: &str) -> (Http, Loaded, Vec<String>) {
    let h = Http::start();
    let body = dummy_xex(&format!("shared-{tag}"));
    let u = h.put("/s/shared.xex", body.clone());
    let ids: Vec<String> = (1..=n).map(|k| format!("m{k}")).collect();
    let es = ids.iter().map(|id| { let mut e = plugin_entry(id, "1", &u, &body); e["mod"]["name"] = format!("Mod {id}").into(); e }).collect();
    (h, load(&manifest(es, 1)), ids)
}

fn perms(v: &[usize]) -> Vec<Vec<usize>> {
    if v.len() <= 1 { return vec![v.to_vec()]; }
    let mut out = vec![];
    for i in 0..v.len() { let mut rest = v.to_vec(); let x = rest.remove(i); for mut p in perms(&rest) { p.insert(0, x); out.push(p); } }
    out
}

fn handoff_run(l: &Loaded, ids: &[String], inst: &[usize], uninst: &[usize], tag: &str) {
    let d = tmp(tag);
    let usb = d.join("usb"); std::fs::create_dir_all(&usb).unwrap();
    std::fs::write(usb.join("launch.ini"), INI).unwrap();
    let c = ctx(&d.join("store"));
    let t = local(&usb);
    for &k in inst { install(&c, l, &ids[k], &t, &InstallOpts::default(), &mut |_, _| {}, &|| false).unwrap(); }
    let line = "plugin3 = Usb:\\Plugins\\shared.xex";
    for (step, &k) in uninst.iter().enumerate() {
        uninstall(&c, Some(l), &ids[k], &t, false).unwrap();
        let ini = std::fs::read_to_string(usb.join("launch.ini")).unwrap();
        if step + 1 < uninst.len() {
            assert!(usb.join("Plugins/shared.xex").exists() && ini.contains(line), "install {inst:?} uninstall {uninst:?} step {step}: {ini}");
            // Exactly one remaining record owns the line.
            let owners = c.store.journal().unwrap().installs.iter().filter(|i| i.ini.iter().any(|x| x.key == "plugin3")).count();
            assert_eq!(owners, 1, "install {inst:?} uninstall {uninst:?} step {step}");
        } else {
            assert!(!usb.join("Plugins").exists(), "install {inst:?} uninstall {uninst:?}: file removed at the end");
            assert_eq!(ini, INI, "install {inst:?} uninstall {uninst:?}: launch.ini back to the original");
        }
    }
}

#[test]
fn r14_selftest_handoff_every_order_three_mods() {
    let _e = env();
    let (_h, l, ids) = shared_mods(3, "h3");
    for (a, inst) in perms(&[0, 1, 2]).iter().enumerate() {
        for (b, un) in perms(&[0, 1, 2]).iter().enumerate() { handoff_run(&l, &ids, inst, un, &format!("h3-{a}-{b}")); }
    }
}

#[test]
fn r14_selftest_handoff_every_order_four_mods() {
    let _e = env();
    let (_h, l, ids) = shared_mods(4, "h4");
    for (a, inst) in [vec![0, 1, 2, 3], vec![3, 1, 0, 2]].iter().enumerate() {
        for (b, un) in perms(&[0, 1, 2, 3]).iter().enumerate() { handoff_run(&l, &ids, inst, un, &format!("h4-{a}-{b}")); }
    }
}

#[test]
fn r14_selftest_pruning_with_refused_deletes() {
    let _e = env();
    // Every DELE of an .old is refused for two whole install/uninstall rounds; nothing breaks, the
    // leftovers are never restored over a good launch.ini, and they're pruned once deletes work.
    let (_h, l, f, d, _) = console_plugin_setup("nodel");
    let c = ctx(&d.join("store"));
    let t = Target::Console { con: crate::ftp::tests::con(f.port), drive: "Hdd1".into() };
    let refuse = |f: &crate::ftp::tests::Fake| { let mut fs = f.fs.lock().unwrap(); fs.faults = (1..8).map(|k| Fault::new("DELE", &if k == 1 { "/Hdd1/launch.ini.old".to_string() } else { format!("/Hdd1/launch.ini.old-{k}") }, false, usize::MAX)).collect(); };
    refuse(&f);
    for round in 0..2 {
        install(&c, &l, "nodel", &t, &InstallOpts { accept_warnings: true, ..Default::default() }, &mut |_, _| {}, &|| false).unwrap();
        assert!(String::from_utf8_lossy(&f.fs.lock().unwrap().files["/Hdd1/launch.ini"]).contains("nodel.xex"), "round {round}");
        uninstall(&c, Some(&l), "nodel", &t, false).unwrap();
        assert_eq!(f.fs.lock().unwrap().files["/Hdd1/launch.ini"], INI.as_bytes(), "round {round}");
    }
    assert!(console_files(&f).len() >= 3, "{:?}", console_files(&f));
    f.fs.lock().unwrap().faults.clear();
    let _ = preflight(&c.store, &l, &l.entries[0], &t);
    assert_eq!(console_files(&f), vec!["/Hdd1/launch.ini".to_string()]);
    assert_eq!(f.fs.lock().unwrap().files["/Hdd1/launch.ini"], INI.as_bytes());
}

#[test]
fn r14_selftest_repair_racing_a_write() {
    let d = tmp("race");
    let usb = d.join("usb"); std::fs::create_dir_all(&usb).unwrap();
    std::fs::write(usb.join("launch.ini"), INI).unwrap();
    let c = ctx(&d.join("store"));
    let kh = key_hash(&local(&usb).key());
    c.store.write_new_file(&format!("backups/{kh}-20261020-000000Z-000-0-launch.ini"), INI.as_bytes()).unwrap();
    let a = INI.to_string();
    let b = INI.replace("nohud = true", "nohud = false");
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let usb2 = usb.clone();
    let (a2, b2) = (a.clone(), b.clone());
    let writer = std::thread::spawn(move || {
        let t = local(&usb2);
        for k in 0..300 { t.replace(&["launch.ini".into()], if k % 2 == 0 { b2.as_bytes() } else { a2.as_bytes() }).unwrap(); }
    });
    let stop2 = stop.clone();
    let store = c.store.dir.clone();
    let usb3 = usb.clone();
    let repairer = std::thread::spawn(move || {
        let st = Store::new(store);
        let t = local(&usb3);
        let mut restored = 0;
        while !stop2.load(Ordering::SeqCst) {
            let n = repair_ini(&st, &t).unwrap();
            restored += n.len();
        }
        restored
    });
    writer.join().unwrap();
    stop.store(true, Ordering::SeqCst);
    let restored = repairer.join().unwrap();
    assert_eq!(restored, 0, "repair never saw a half-done swap");
    let now = std::fs::read_to_string(usb.join("launch.ini")).unwrap();
    assert!(now == a || now == b);
    let names: Vec<String> = std::fs::read_dir(&usb).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(names, vec!["launch.ini".to_string()]);
}

#[test]
fn r14_low4_retry_classification_ignores_urls() {
    // The flake: a 503 from a server on port 40407 was treated as a permanent 404.
    assert!(!is_permanent("Download failed: http://127.0.0.1:40407/z.bin: status code 503"));
    assert!(!is_permanent("Download failed: https://h.invalid/blocked/404/file.bin: status code 500"));
    assert!(is_permanent("Download failed: http://127.0.0.1:5000/z.bin: status code 404"));
    assert!(is_permanent("Server returned HTTP 410."));
    assert!(is_permanent("SHA-256 mismatch"));
    assert!(is_permanent("Address 10.0.0.1 is blocked (private network)."));
}
