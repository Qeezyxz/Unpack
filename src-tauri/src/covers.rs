//! Library cover art: prefer local game/Aurora files; optional opt-in HTTP source.

use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct CoverInfo {
    pub path: String,
    pub source: String,
}

/// How long to remember a failed network fetch before trying again.
const FAIL_TTL_SECS: u64 = 6 * 3600;

const UNITY_BASE: &str = "http://xboxunity.net/Resources/Lib";

fn cache_path(cache_dir: &Path, title_id: &str) -> PathBuf {
    cache_dir.join(format!("{}.png", title_id.to_ascii_uppercase()))
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
    let _ = fs::write(failures_path(cache_dir), serde_json::to_vec(map).unwrap_or_default());
}

fn failure_active(cache_dir: &Path, title_id: &str) -> bool {
    let tid = title_id.to_ascii_uppercase();
    let map = read_failures(cache_dir);
    map.get(&tid)
        .is_some_and(|t| now_secs().saturating_sub(*t) < FAIL_TTL_SECS)
}

fn note_failure(cache_dir: &Path, title_id: &str) {
    let tid = title_id.to_ascii_uppercase();
    let mut map = read_failures(cache_dir);
    map.insert(tid, now_secs());
    // Drop stale entries so the file stays small.
    map.retain(|_, t| now_secs().saturating_sub(*t) < FAIL_TTL_SECS);
    write_failures(cache_dir, &map);
}

fn clear_failure(cache_dir: &Path, title_id: &str) {
    let tid = title_id.to_ascii_uppercase();
    let mut map = read_failures(cache_dir);
    if map.remove(&tid).is_some() {
        write_failures(cache_dir, &map);
    }
}

/// Path to a cached PNG if it already exists on disk.
pub fn cached_file(cache_dir: &Path, title_id: &str) -> Option<PathBuf> {
    let p = cache_path(cache_dir, title_id);
    if p.is_file() {
        Some(p)
    } else {
        None
    }
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

/// Copy local cover into cache if present.
pub fn cache_local(game_path: &Path, title_id: &str, cache_dir: &Path) -> Option<CoverInfo> {
    let src = local_cover(game_path, title_id)?;
    fs::create_dir_all(cache_dir).ok()?;
    let dest = cache_path(cache_dir, title_id);
    if fs::copy(&src, &dest).is_ok() {
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

fn download_unity_cover(title_id: &str) -> Option<Vec<u8>> {
    let tid = title_id.to_ascii_uppercase();
    let info_url = format!("{UNITY_BASE}/CoverInfo.php?titleid={tid}");
    let info_resp = ureq::get(&info_url)
        .timeout(std::time::Duration::from_secs(8))
        .call();
    let info_resp = match info_resp {
        Ok(r) if r.status() == 200 => r,
        _ => return None,
    };
    let mut body = vec![];
    if info_resp.into_reader().read_to_end(&mut body).is_err() {
        return None;
    }
    let info: UnityCoverInfo = serde_json::from_slice(&body).ok()?;
    let cid = pick_cover_id(&info.covers)?;
    let img_url = format!("{UNITY_BASE}/Cover.php?cid={cid}&size=large");
    let img_resp = match ureq::get(&img_url)
        .timeout(std::time::Duration::from_secs(12))
        .call()
    {
        Ok(r) if r.status() == 200 => r,
        _ => return None,
    };
    let mut img = vec![];
    if img_resp.into_reader().read_to_end(&mut img).is_err() || img.len() < 200 {
        return None;
    }
    Some(img)
}

/// Opt-in fetch from XboxUnity (CoverInfo + Cover.php) by TitleID (fails gracefully offline).
pub fn fetch_opt_in(title_id: &str, cache_dir: &Path, allow_network: bool) -> Option<CoverInfo> {
    if !allow_network {
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
        Some(b) => b,
        None => {
            note_failure(cache_dir, title_id);
            return None;
        }
    };
    fs::create_dir_all(cache_dir).ok()?;
    let dest = cache_path(cache_dir, title_id);
    if fs::write(&dest, &body).is_ok() {
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

/// Local cover first, then cached PNG, then optional network fetch.
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
    let info = fetch_opt_in(title_id, cache_dir, allow_network)?;
    Some(info)
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
