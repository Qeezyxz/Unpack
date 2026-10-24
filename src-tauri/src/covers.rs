//! Library cover art: prefer local game/Aurora files; optional opt-in HTTP source.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct CoverInfo {
    pub path: String,
    pub source: String,
}

fn cache_path(cache_dir: &Path, title_id: &str) -> PathBuf {
    cache_dir.join(format!("{}.png", title_id.to_ascii_uppercase()))
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

/// Opt-in fetch from a public cover source by TitleID (fails gracefully offline).
pub fn fetch_opt_in(title_id: &str, cache_dir: &Path, allow_network: bool) -> Option<CoverInfo> {
    if !allow_network {
        return None;
    }
    let tid = title_id.to_ascii_uppercase();
    let url = format!("https://xboxunity.net/Media/Covers/{tid}.png");
    let resp = match ureq::get(&url)
        .timeout(std::time::Duration::from_secs(8))
        .call()
    {
        Ok(r) if r.status() == 200 => r,
        _ => return None,
    };
    let mut body = vec![];
    if resp.into_reader().read_to_end(&mut body).is_err() || body.len() < 200 {
        return None;
    }
    fs::create_dir_all(cache_dir).ok()?;
    let dest = cache_path(cache_dir, title_id);
    if fs::write(&dest, &body).is_ok() {
        Some(CoverInfo {
            path: dest.display().to_string(),
            source: "xboxunity".into(),
        })
    } else {
        None
    }
}
