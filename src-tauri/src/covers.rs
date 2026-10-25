//! Library cover art: prefer local game/Aurora files; optional opt-in HTTP source.

use std::collections::HashMap;
use std::fs;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct CoverInfo {
    pub path: String,
    pub source: String,
}

/// How long to remember a failed network fetch before trying again.
const FAIL_TTL_SECS: u64 = 6 * 3600;

const UNITY_BASE: &str = "https://xboxunity.net/Resources/Lib";

/// Largest cover image I accept from the network or the cache (after downscale).
const MAX_IMG_BYTES: u64 = 512 * 1024;

const MAX_DECODE_DIM: u32 = 4096;
const COVER_MAX_W: u32 = 300;
const COVER_MAX_H: u32 = 420;
const JPEG_QUALITY: u8 = 80;

static FAILURE_LOCK: Mutex<()> = Mutex::new(());

/// True when the bytes start like a PNG or JPEG.
pub fn is_image(b: &[u8]) -> bool {
    b.starts_with(b"\x89PNG\r\n\x1a\n") || b.starts_with(&[0xFF, 0xD8, 0xFF])
}

fn rename_denied_retryable(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::PermissionDenied || e.raw_os_error() == Some(5)
}

fn dest_already_valid_image(dest: &Path) -> bool {
    let mut head = [0u8; 8];
    fs::File::open(dest)
        .and_then(|mut f| f.read_exact(&mut head))
        .is_ok_and(|_| is_image(&head))
}

fn rename_with_retry_using<F>(mut rename: F, tmp: &Path, dest: &Path) -> std::io::Result<()>
where
    F: FnMut(&Path, &Path) -> std::io::Result<()>,
{
    let mut last: Option<std::io::Error> = None;
    for attempt in 0..5 {
        match rename(tmp, dest) {
            Ok(()) => return Ok(()),
            Err(e) if rename_denied_retryable(&e) && attempt < 4 => {
                last = Some(e);
                thread::sleep(Duration::from_millis(20 * (attempt as u64 + 1)));
            }
            Err(e) => return Err(e),
        }
    }
    Err(last.unwrap_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::PermissionDenied, "rename failed after retries")
    }))
}

fn rename_with_retry(tmp: &Path, dest: &Path) -> std::io::Result<()> {
    rename_with_retry_using(|a, b| fs::rename(a, b), tmp, dest)
}

/// Write via a temp file in the same folder, then rename, so readers never see a torn file.
fn write_atomic(dest: &Path, data: &[u8]) -> std::io::Result<()> {
    static N: AtomicU64 = AtomicU64::new(0);
    let name = dest
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();
    let tmp = dest.with_file_name(format!(
        ".{name}.{}.{:?}.{}.tmp",
        std::process::id(),
        std::thread::current().id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    fs::write(&tmp, data)?;
    match rename_with_retry(&tmp, dest) {
        Ok(()) => Ok(()),
        Err(e) => {
            if dest_already_valid_image(dest) {
                let _ = fs::remove_file(&tmp);
                Ok(())
            } else {
                let _ = fs::remove_file(&tmp);
                Err(e)
            }
        }
    }
}

/// Downscale to at most 300×420 and re-encode as JPEG (~quality 80). Rejects decompression bombs.
pub fn prepare_cover_bytes(raw: &[u8]) -> Option<Vec<u8>> {
    if !is_image(raw) {
        return None;
    }
    let (w, h) = image::ImageReader::new(Cursor::new(raw))
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()?;
    if w > MAX_DECODE_DIM || h > MAX_DECODE_DIM {
        return None;
    }
    let img = image::load_from_memory(raw).ok()?;
    let scaled = if w > COVER_MAX_W || h > COVER_MAX_H {
        img.thumbnail(COVER_MAX_W, COVER_MAX_H)
    } else {
        img
    };
    let mut out = Vec::new();
    let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, JPEG_QUALITY);
    enc.encode_image(&scaled).ok()?;
    (out.len() as u64 <= MAX_IMG_BYTES).then_some(out)
}

/// Why a download produced no cover: the network itself failed, or this title has none.
enum Miss {
    Network,
    Title,
}

fn agent() -> ureq::Agent {
    // No redirects: a redirect could point at another host.
    ureq::AgentBuilder::new()
        .redirects(0)
        .timeout(std::time::Duration::from_secs(12))
        .build()
}

fn read_capped(r: ureq::Response, cap: u64) -> Option<Vec<u8>> {
    let mut b = vec![];
    r.into_reader().take(cap + 1).read_to_end(&mut b).ok()?;
    (b.len() as u64 <= cap).then_some(b)
}

/// A TitleID is exactly 8 hex digits; anything else never touches the disk or a URL.
pub fn valid_tid(title_id: &str) -> bool {
    title_id.len() == 8 && title_id.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Cover file as a `data:` URL so the webview can show it under `img-src data:`.
pub fn data_url(path: &Path) -> Option<String> {
    let f = fs::File::open(path).ok()?;
    let mut buf = vec![];
    f.take(MAX_IMG_BYTES + 1).read_to_end(&mut buf).ok()?;
    if buf.len() as u64 > MAX_IMG_BYTES || !is_image(&buf) {
        return None;
    }
    let mime = if buf.starts_with(&[0xFF, 0xD8]) {
        "image/jpeg"
    } else {
        "image/png"
    };
    Some(format!("data:{mime};base64,{}", b64(&buf)))
}

fn b64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if c.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

fn cache_path(cache_dir: &Path, title_id: &str) -> PathBuf {
    cache_dir.join(format!("{}.jpg", title_id.to_ascii_uppercase()))
}

fn failures_path(cache_dir: &Path) -> PathBuf {
    cache_dir.join("cover-failures.json")
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn read_failures(cache_dir: &Path) -> HashMap<String, u64> {
    let raw = fs::read(failures_path(cache_dir)).ok();
    raw.and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn write_failures(cache_dir: &Path, map: &HashMap<String, u64>) {
    fs::create_dir_all(cache_dir).ok();
    let _ = write_atomic(&failures_path(cache_dir), &serde_json::to_vec(map).unwrap_or_default());
}

fn failure_active(cache_dir: &Path, title_id: &str) -> bool {
    let tid = title_id.to_ascii_uppercase();
    let map = read_failures(cache_dir);
    map.get(&tid)
        .is_some_and(|t| now_secs().saturating_sub(*t) < FAIL_TTL_SECS)
}

fn note_failure(cache_dir: &Path, title_id: &str) {
    let _g = FAILURE_LOCK.lock().unwrap();
    let tid = title_id.to_ascii_uppercase();
    let mut map = read_failures(cache_dir);
    map.insert(tid, now_secs());
    map.retain(|_, t| now_secs().saturating_sub(*t) < FAIL_TTL_SECS);
    write_failures(cache_dir, &map);
}

fn clear_failure(cache_dir: &Path, title_id: &str) {
    let _g = FAILURE_LOCK.lock().unwrap();
    let tid = title_id.to_ascii_uppercase();
    let mut map = read_failures(cache_dir);
    if map.remove(&tid).is_some() {
        write_failures(cache_dir, &map);
    }
}

/// Path to a cached cover if it already exists on disk.
pub fn cached_file(cache_dir: &Path, title_id: &str) -> Option<PathBuf> {
    if !valid_tid(title_id) {
        return None;
    }
    let p = cache_path(cache_dir, title_id);
    let mut head = [0u8; 8];
    let ok = fs::File::open(&p)
        .and_then(|mut f| f.read_exact(&mut head))
        .is_ok()
        && is_image(&head);
    if ok {
        Some(p)
    } else {
        let _ = fs::remove_file(&p);
        None
    }
}

/// Forget every remembered fetch failure (Refresh does this).
pub fn clear_failures(cache_dir: &Path) {
    let _ = fs::remove_file(failures_path(cache_dir));
}

/// Look beside the game for `cover.png`, `boxart.png`, or Aurora `GameData/<tid>.png`.
pub fn local_cover(game_path: &Path, title_id: &str) -> Option<PathBuf> {
    for name in ["cover.png", "boxart.png", "BoxArt.png"] {
        let p = game_path.join(name);
        if p.is_file() {
            return Some(p);
        }
    }
    let aurora = game_path
        .join("GameData")
        .join(format!("{}.png", title_id.to_ascii_uppercase()));
    if aurora.is_file() {
        return Some(aurora);
    }
    None
}

fn cache_prepared(dest: &Path, raw: &[u8]) -> bool {
    match prepare_cover_bytes(raw) {
        Some(data) => write_atomic(dest, &data).is_ok(),
        None => false,
    }
}

/// Copy local cover into cache if present.
pub fn cache_local(game_path: &Path, title_id: &str, cache_dir: &Path) -> Option<CoverInfo> {
    if !valid_tid(title_id) {
        return None;
    }
    let src = local_cover(game_path, title_id)?;
    fs::create_dir_all(cache_dir).ok()?;
    let dest = cache_path(cache_dir, title_id);
    let mut data = vec![];
    fs::File::open(&src)
        .ok()?
        .take(8 * 1024 * 1024 + 1)
        .read_to_end(&mut data)
        .ok()?;
    if cache_prepared(&dest, &data) {
        return Some(CoverInfo {
            path: dest.display().to_string(),
            source: "local".into(),
        });
    }
    None
}

#[derive(Debug, Clone, serde::Deserialize)]
struct UnityCoverEntry {
    #[serde(rename = "CoverID")]
    cover_id: String,
    #[serde(rename = "Official")]
    official: Option<String>,
    #[serde(rename = "Rating")]
    rating: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct UnityCoverInfo {
    #[serde(rename = "Covers")]
    covers: Vec<UnityCoverEntry>,
}

fn pick_cover_id(entries: &[UnityCoverEntry]) -> Option<String> {
    let mut sorted = entries.to_vec();
    sorted.sort_by(|a, b| {
        let oa = a.official.as_deref() == Some("1");
        let ob = b.official.as_deref() == Some("1");
        let ra: i32 = a
            .rating
            .as_deref()
            .and_then(|r| r.parse().ok())
            .unwrap_or(0);
        let rb: i32 = b
            .rating
            .as_deref()
            .and_then(|r| r.parse().ok())
            .unwrap_or(0);
        ob.cmp(&oa).then(rb.cmp(&ra))
    });
    sorted.first().map(|c| c.cover_id.clone())
}

fn download_unity_cover(title_id: &str) -> Result<Vec<u8>, Miss> {
    let tid = title_id.to_ascii_uppercase();
    let ag = agent();
    let get = |url: &str| match ag.get(url).call() {
        Ok(r) if r.status() == 200 => Ok(r),
        Ok(_) | Err(ureq::Error::Status(..)) => Err(Miss::Title),
        Err(_) => Err(Miss::Network),
    };
    let info_resp = get(&format!("{UNITY_BASE}/CoverInfo.php?titleid={tid}"))?;
    let body = read_capped(info_resp, 256 * 1024).ok_or(Miss::Network)?;
    let info: UnityCoverInfo = serde_json::from_slice(&body).map_err(|_| Miss::Title)?;
    let cid = pick_cover_id(&info.covers).ok_or(Miss::Title)?;
    if cid.is_empty() || cid.len() > 12 || !cid.bytes().all(|b| b.is_ascii_digit()) {
        return Err(Miss::Title);
    }
    let img_resp = get(&format!("{UNITY_BASE}/Cover.php?cid={cid}&size=large"))?;
    let ct = img_resp.content_type().to_ascii_lowercase();
    if ct != "image/png" && ct != "image/jpeg" && ct != "image/jpg" {
        return Err(Miss::Title);
    }
    let img = read_capped(img_resp, 8 * 1024 * 1024).ok_or(Miss::Title)?;
    if !is_image(&img) {
        return Err(Miss::Title);
    }
    Ok(img)
}

/// Opt-in fetch from XboxUnity (CoverInfo + Cover.php) by TitleID (fails gracefully offline).
pub fn fetch_opt_in(title_id: &str, cache_dir: &Path, allow_network: bool) -> Option<CoverInfo> {
    if !allow_network || !valid_tid(title_id) {
        return None;
    }
    if failure_active(cache_dir, title_id) {
        return None;
    }
    if let Some(p) = cached_file(cache_dir, title_id) {
        return Some(CoverInfo {
            path: p.display().to_string(),
            source: "cache".into(),
        });
    }
    let body = match download_unity_cover(title_id) {
        Ok(b) => b,
        Err(Miss::Title) => {
            note_failure(cache_dir, title_id);
            return None;
        }
        Err(Miss::Network) => return None,
    };
    fs::create_dir_all(cache_dir).ok()?;
    let dest = cache_path(cache_dir, title_id);
    if cache_prepared(&dest, &body) {
        clear_failure(cache_dir, title_id);
        Some(CoverInfo {
            path: dest.display().to_string(),
            source: "xboxunity".into(),
        })
    } else {
        note_failure(cache_dir, title_id);
        None
    }
}

/// Local cover first, then cached file, then optional network fetch.
pub fn resolve_cover(
    game_path: &Path,
    title_id: &str,
    cache_dir: &Path,
    allow_network: bool,
) -> Option<CoverInfo> {
    if let Some(info) = cache_local(game_path, title_id, cache_dir) {
        clear_failure(cache_dir, title_id);
        return Some(info);
    }
    if let Some(p) = cached_file(cache_dir, title_id) {
        return Some(CoverInfo {
            path: p.display().to_string(),
            source: "cache".into(),
        });
    }
    fetch_opt_in(title_id, cache_dir, allow_network)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::ImageEncoder;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn failure_cache_expires() {
        let d = crate::testutil::TmpDir::new("covers-fail");
        let cache = d.join("cache");
        note_failure(&cache, "415608C3");
        assert!(failure_active(&cache, "415608C3"));
        let mut map = read_failures(&cache);
        map.insert("415608C3".into(), now_secs() - FAIL_TTL_SECS - 1);
        write_failures(&cache, &map);
        assert!(!failure_active(&cache, "415608C3"));
    }

    #[test]
    fn rejects_bad_title_ids_and_encodes() {
        let d = crate::testutil::TmpDir::new("covers-tid");
        assert!(!valid_tid("../../x"));
        assert!(!valid_tid("415608C3/"));
        assert!(valid_tid("415608c3"));
        assert!(cached_file(&d.join("c"), "..\\..\\a").is_none());
        assert!(fetch_opt_in("../etc/x", &d.join("c"), true).is_none());
        assert_eq!(b64(b"Man"), "TWFu");
        assert_eq!(b64(b"Ma"), "TWE=");
        assert_eq!(b64(b"M"), "TQ==");
    }

    #[test]
    fn bad_cache_files_are_dropped_and_good_ones_served() {
        let d = crate::testutil::TmpDir::new("covers-magic");
        let c = d.join("c");
        fs::create_dir_all(&c).unwrap();
        fs::write(cache_path(&c, "415608C3"), vec![b'<'; 500]).unwrap();
        assert!(cached_file(&c, "415608C3").is_none());
        assert!(!cache_path(&c, "415608C3").exists());
        let img = image::RgbaImage::from_pixel(32, 40, image::Rgba([9, 9, 9, 255]));
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(img.as_raw(), 32, 40, image::ExtendedColorType::Rgba8)
            .unwrap();
        let jpeg = prepare_cover_bytes(&png).unwrap();
        write_atomic(&cache_path(&c, "415608C3"), &jpeg).unwrap();
        assert!(cached_file(&c, "415608C3").is_some());
        assert!(data_url(&cache_path(&c, "415608C3"))
            .unwrap()
            .starts_with("data:image/jpeg;base64,"));
        note_failure(&c, "11111111");
        clear_failures(&c);
        assert!(!failure_active(&c, "11111111"));
    }

    #[test]
    fn pick_prefers_official_cover() {
        let entries = vec![
            UnityCoverEntry {
                cover_id: "1".into(),
                official: Some("0".into()),
                rating: Some("5".into()),
            },
            UnityCoverEntry {
                cover_id: "2".into(),
                official: Some("1".into()),
                rating: Some("3".into()),
            },
        ];
        assert_eq!(pick_cover_id(&entries).as_deref(), Some("2"));
    }

    #[test]
    fn big_png_becomes_small_jpeg() {
        let img = image::RgbaImage::from_pixel(2000, 2800, image::Rgba([40, 80, 120, 255]));
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(img.as_raw(), 2000, 2800, image::ExtendedColorType::Rgba8)
            .unwrap();
        assert!(png.len() > 20_000);
        let out = prepare_cover_bytes(&png).unwrap();
        assert!(out.starts_with(&[0xFF, 0xD8, 0xFF]));
        assert!(out.len() < 50_000);
        let decoded = image::load_from_memory(&out).unwrap();
        assert!(decoded.width() <= COVER_MAX_W && decoded.height() <= COVER_MAX_H);
    }

    #[test]
    fn oversize_png_header_rejected_before_decode() {
        let mut png = vec![
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 0x0D, 0x49, 0x48, 0x44, 0x52,
        ];
        png.extend_from_slice(&8192u32.to_be_bytes());
        png.extend_from_slice(&8192u32.to_be_bytes());
        png.extend_from_slice(&[8, 2, 0, 0, 0, 0, 0, 0, 0]);
        assert!(prepare_cover_bytes(&png).is_none());
    }

    #[test]
    fn decompression_bomb_rejected() {
        let img = image::RgbaImage::from_pixel(5000, 5000, image::Rgba([0, 0, 0, 255]));
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(img.as_raw(), 5000, 5000, image::ExtendedColorType::Rgba8)
            .unwrap();
        assert!(prepare_cover_bytes(&png).is_none());
    }

    #[test]
    fn write_atomic_concurrent_valid_images() {
        let d = crate::testutil::TmpDir::new("covers-atomic");
        let dest = d.join("c/415608C3.jpg");
        fs::create_dir_all(dest.parent().unwrap()).unwrap();
        let png = {
            let img = image::RgbaImage::from_pixel(64, 80, image::Rgba([1, 2, 3, 255]));
            let mut b = Vec::new();
            image::codecs::png::PngEncoder::new(&mut b)
                .write_image(img.as_raw(), 64, 80, image::ExtendedColorType::Rgba8)
                .unwrap();
            b
        };
        let dest = Arc::new(dest);
        let png = Arc::new(png);
        thread::scope(|s| {
            for _ in 0..4 {
                s.spawn(|| {
                    for _ in 0..50 {
                        let data = prepare_cover_bytes(png.as_slice()).unwrap();
                        write_atomic(dest.as_ref(), &data).unwrap();
                    }
                });
            }
        });
        let final_bytes = fs::read(dest.as_ref()).unwrap();
        assert!(is_image(&final_bytes));
        assert!(prepare_cover_bytes(&final_bytes).is_some());
        let left: Vec<_> = fs::read_dir(d.join("c"))
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(left.is_empty());
    }

    #[test]
    fn dest_already_valid_image_checks_magic() {
        let d = crate::testutil::TmpDir::new("covers-valid");
        let jpg = d.join("x.jpg");
        fs::write(&jpg, [0xFF, 0xD8, 0xFF, 0x00, 0, 0, 0, 0]).unwrap();
        assert!(dest_already_valid_image(&jpg));
        let txt = d.join("y.jpg");
        fs::write(&txt, b"not an image").unwrap();
        assert!(!dest_already_valid_image(&txt));
    }

    #[test]
    fn rename_with_retry_retries_permission_denied() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let d = crate::testutil::TmpDir::new("covers-rename");
        let tmp = d.join("part.tmp");
        let dest = d.join("cover.jpg");
        fs::write(&tmp, b"x").unwrap();
        fs::write(&dest, [0xFF, 0xD8, 0xFF, 0x00, 0, 0, 0, 0]).unwrap();
        static TRIES: AtomicU32 = AtomicU32::new(0);
        let try_rename = |from: &Path, to: &Path| {
            let n = TRIES.fetch_add(1, Ordering::SeqCst);
            if n < 2 {
                return Err(std::io::Error::from_raw_os_error(5));
            }
            fs::rename(from, to)
        };
        rename_with_retry_using(try_rename, &tmp, &dest).unwrap();
        assert!(TRIES.load(Ordering::SeqCst) >= 3);
    }

    #[test]
    fn note_failure_keeps_all_concurrent_entries() {
        let d = crate::testutil::TmpDir::new("covers-fail-par");
        let cache = Arc::new(d.join("cache"));
        thread::scope(|s| {
            for i in 0..16 {
                let c = Arc::clone(&cache);
                s.spawn(move || {
                    note_failure(c.as_ref(), &format!("{:08X}", i));
                });
            }
        });
        let map = read_failures(cache.as_ref());
        assert_eq!(map.len(), 16);
    }
}
