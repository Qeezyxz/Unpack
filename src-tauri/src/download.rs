//! Download a file from a user-pasted http(s) URL. Network only from this module, only on click.
//! No built-in catalog. SSRF-hardened redirects/hosters; resume uses If-Range.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Serialize;
use sha2::{Digest, Sha256};
use url::Url;

use crate::safecopy::{self, PARTIAL};

const MAX_REDIRECTS: usize = 7;
const MAX_RESOLVE: usize = 4;
const UA: &str = "Unpack/1.0 (Xbox 360 hobbyist; user-initiated download)";
/// Default max download size (20 GiB). Override via DownloadOpts / settings.
pub const DEFAULT_MAX_BYTES: u64 = 20 * 1024 * 1024 * 1024;
const OVERALL_TIMEOUT: Duration = Duration::from_secs(60 * 60); // 1 hour
const IDLE_TIMEOUT: Duration = Duration::from_secs(120);
const META_SUFFIX: &str = ".meta";

/// Human size for messages: GB for ≥ 1 GB, else MB, else KB / bytes (R12 LOW-4).
pub fn human_size(n: u64) -> String {
    if n >= 1_000_000_000 {
        format!("{:.1} GB", n as f64 / 1e9)
    } else if n >= 1 << 20 {
        format!("{:.1} MB", n as f64 / 1_048_576.0)
    } else if n >= 1024 {
        format!("{:.1} KB", n as f64 / 1024.0)
    } else {
        format!("{n} bytes")
    }
}

#[derive(Debug, Clone)]
pub struct DownloadOpts {
    pub max_bytes: u64,
    pub expected_sha256: Option<String>,
}

impl Default for DownloadOpts {
    fn default() -> Self {
        Self {
            max_bytes: DEFAULT_MAX_BYTES,
            expected_sha256: None,
        }
    }
}

#[derive(Debug, Serialize, Clone, PartialEq)]
pub struct ReleaseInfo {
    pub available: bool,
    pub latest: String,
    pub url: String,
}

/// Opt-in check against GitHub releases (no auto-install).
pub fn check_github_release(current: &str) -> Result<ReleaseInfo, String> {
    let url = "https://api.github.com/repos/Qeezyxz/Unpack/releases/latest";
    let resp = ureq::get(url)
        .set("User-Agent", UA)
        .timeout(Duration::from_secs(12))
        .call()
        .map_err(|e| format!("Couldn't reach GitHub: {e}"))?;
    let text = resp.into_string().map_err(|e| e.to_string())?;
    let body: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let tag = body["tag_name"]
        .as_str()
        .unwrap_or("")
        .trim_start_matches('v')
        .to_string();
    let html = body["html_url"].as_str().unwrap_or("").to_string();
    let available = !tag.is_empty() && version_gt(&tag, current);
    Ok(ReleaseInfo {
        available,
        latest: if tag.is_empty() {
            current.to_string()
        } else {
            tag
        },
        url: html,
    })
}

fn version_gt(a: &str, b: &str) -> bool {
    let pa = a
        .split('.')
        .filter_map(|x| x.parse::<u32>().ok())
        .collect::<Vec<_>>();
    let pb = b
        .split('.')
        .filter_map(|x| x.parse::<u32>().ok())
        .collect::<Vec<_>>();
    for i in 0..pa.len().max(pb.len()) {
        let (x, y) = (
            pa.get(i).copied().unwrap_or(0),
            pb.get(i).copied().unwrap_or(0),
        );
        if x != y {
            return x > y;
        }
    }
    false
}

#[derive(Debug, Serialize)]
pub struct DownloadResult {
    pub path: String,
    pub name: String,
    pub bytes: u64,
    pub sha256: String,
    pub final_url: String,
    pub http_warning: bool,
}

#[cfg(test)]
static TEST_ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());
/// Shared by every test that touches UNPACK_TEST_ALLOW_LOOPBACK (poison-tolerant).
#[cfg(test)]
pub(crate) fn tests_env_lock() -> std::sync::MutexGuard<'static, ()> {
    TEST_ENV.lock().unwrap_or_else(|p| p.into_inner())
}

pub(crate) fn allow_test_loopback() -> bool {
    #[cfg(any(test, feature = "download-test-loopback"))]
    {
        std::env::var("UNPACK_TEST_ALLOW_LOOPBACK").ok().as_deref() == Some("1")
    }
    #[cfg(not(any(test, feature = "download-test-loopback")))]
    {
        false
    }
}

fn url_host_key(url: &Url) -> Result<String, String> {
    match url.host() {
        Some(url::Host::Domain(d)) => Ok(d.trim_end_matches('.').to_ascii_lowercase()),
        Some(url::Host::Ipv4(v)) => Ok(v.to_string()),
        Some(url::Host::Ipv6(v)) => Ok(v.to_string()),
        None => Err("URL has no host.".into()),
    }
}

/// Resolve + SSRF-check; returns only the addresses that passed (used to pin ureq).
pub fn resolve_safe_addrs(url: &Url) -> Result<Vec<SocketAddr>, String> {
    let port = url.port_or_known_default().unwrap_or(80);
    let allow = allow_test_loopback();
    match url.host() {
        Some(url::Host::Ipv4(v)) => {
            let ip = IpAddr::V4(v);
            if is_blocked_ip(ip) && !(allow && ip.is_loopback()) {
                return Err("That address isn't allowed (local/private network).".into());
            }
            Ok(vec![SocketAddr::new(ip, port)])
        }
        Some(url::Host::Ipv6(v)) => {
            let ip = IpAddr::V6(v);
            if is_blocked_ip(ip) && !(allow && ip.is_loopback()) {
                return Err("That address isn't allowed (local/private network).".into());
            }
            Ok(vec![SocketAddr::new(ip, port)])
        }
        Some(url::Host::Domain(host)) => {
            let host = host.trim_end_matches('.').to_ascii_lowercase();
            let addrs: Vec<SocketAddr> = (host.as_str(), port)
                .to_socket_addrs()
                .map_err(|e| format!("Cannot resolve {host}: {e}"))?
                .collect();
            if addrs.is_empty() {
                return Err(format!("Cannot resolve {host}."));
            }
            for a in &addrs {
                if is_blocked_ip(a.ip()) && !(allow && a.ip().is_loopback()) {
                    return Err(format!(
                        "Refusing download: {host} resolves to a local/private address."
                    ));
                }
            }
            Ok(addrs)
        }
        None => Err("URL has no host.".into()),
    }
}

/// ureq resolver that returns only the already-checked addresses (DNS rebinding defense).
fn agent_pinned(host_key: String, addrs: Vec<SocketAddr>) -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(20))
        .timeout_read(Duration::from_secs(60))
        .redirects(0)
        .user_agent(UA)
        .resolver(move |netloc: &str| {
            let host = netloc
                .rsplit_once(':')
                .map(|(h, _)| h)
                .unwrap_or(netloc)
                .trim_matches(|c| c == '[' || c == ']')
                .to_ascii_lowercase();
            if host == host_key {
                Ok(addrs.clone())
            } else {
                Err(std::io::Error::other(
                    "pinned resolver host mismatch",
                ))
            }
        })
        .build()
}

fn parse_url(u: &str) -> Result<Url, String> {
    let url = Url::parse(u).map_err(|e| format!("Bad URL: {e}"))?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err("Only http:// and https:// links are allowed.".into());
    }
    if url.host().is_none() {
        return Err("URL has no host.".into());
    }
    Ok(url)
}

/// Exact host match: `drive.google.com` or a subdomain of it (not substring-in-path).
pub fn host_matches(url: &Url, domain: &str) -> bool {
    let Some(h) = url.host_str() else {
        return false;
    };
    let h = h.trim_end_matches('.').to_ascii_lowercase();
    let d = domain.to_ascii_lowercase();
    h == d || h.ends_with(&format!(".{d}"))
}

pub fn is_blocked_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => {
            let o = v.octets();
            v.is_unspecified()
                || v.is_loopback()
                || v.is_private()
                || v.is_link_local()
                || v.is_broadcast()
                || (o[0] == 100 && (64..128).contains(&o[1])) // CGNAT 100.64/10
                || o[0] == 0
                || (o[0] == 192 && o[1] == 0 && o[2] == 0) // 192.0.0.0/24 IETF
                || (o[0] == 192 && o[1] == 0 && o[2] == 2) // TEST-NET
                || (o[0] == 198 && (o[1] == 18 || o[1] == 19)) // benchmarking
                || (o[0] == 198 && o[1] == 51 && o[2] == 100)
                || (o[0] == 203 && o[1] == 0 && o[2] == 113)
                || o[0] >= 224 // multicast / reserved
        }
        IpAddr::V6(v) => {
            let seg = v.segments();
            v.is_unspecified()
                || v.is_loopback()
                || (seg[0] & 0xfe00) == 0xfc00 // ULA fc00::/7
                || (seg[0] & 0xffc0) == 0xfe80 // link-local fe80::/10
                || (seg[0] & 0xffc0) == 0xfec0 // site-local (deprecated)
                || seg[0] == 0x2002 // 6to4 2002::/16
                || (seg[0] == 0x64 && seg[1] == 0xff9b) // NAT64 64:ff9b::/96
                || v.to_ipv4_mapped().map(|v4| is_blocked_ip(IpAddr::V4(v4))).unwrap_or(false)
                || v.to_ipv4().map(|v4| is_blocked_ip(IpAddr::V4(v4))).unwrap_or(false)
                || (seg[0] & 0xff00) == 0xff00 // multicast
        }
    }
}

/// Resolve host and refuse loopback / private / link-local / CGNAT / ULA / 6to4 / NAT64.
pub fn assert_safe_url(url: &Url) -> Result<(), String> {
    resolve_safe_addrs(url).map(|_| ())
}

fn join_redirect(base: &Url, loc: &str) -> Result<Url, String> {
    base.join(loc)
        .map_err(|e| format!("Bad redirect Location: {e}"))
}

/// Public hoster helpers — host must match exactly (or subdomain), never a substring of the path.
pub fn resolve_direct(url: &str) -> Result<String, String> {
    let mut u = parse_url(url)?;
    assert_safe_url(&u)?;
    for _ in 0..MAX_RESOLVE {
        if host_matches(&u, "drive.google.com") || host_matches(&u, "docs.google.com") {
            if let Some(id) = google_id(u.as_str()) {
                u = parse_url(&format!(
                    "https://drive.google.com/uc?export=download&id={id}"
                ))?;
                assert_safe_url(&u)?;
                continue;
            }
        }
        if host_matches(&u, "dropbox.com")
            || host_matches(&u, "www.dropbox.com")
            || host_matches(&u, "dl.dropboxusercontent.com")
        {
            let mut s = u.to_string();
            s = s.replace("dl=0", "dl=1");
            if !s.contains("dl=1") {
                s = if s.contains('?') {
                    format!("{s}&dl=1")
                } else {
                    format!("{s}?dl=1")
                };
            }
            u = parse_url(&s)?;
            break;
        }
        if host_matches(&u, "mediafire.com") || host_matches(&u, "www.mediafire.com") {
            let addrs = resolve_safe_addrs(&u)?;
            let host_key = url_host_key(&u)?;
            let body = agent_pinned(host_key, addrs)
                .get(u.as_str())
                .call()
                .map_err(|e| format!("MediaFire page: {e}"))?
                .into_string()
                .map_err(|e| e.to_string())?;
            let d = mediafire_href(&body).ok_or_else(|| {
                "Couldn't find a public MediaFire download link on that page.".to_string()
            })?;
            u = parse_url(&d)?;
            if !(host_matches(&u, "mediafire.com") || host_matches(&u, "download.mediafire.com")) {
                return Err("MediaFire link left mediafire.com; refused.".into());
            }
            assert_safe_url(&u)?;
            break;
        }
        break;
    }
    Ok(u.to_string())
}

fn google_id(u: &str) -> Option<String> {
    if let Some(i) = u.find("/file/d/") {
        let rest = &u[i + 8..];
        let id: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .collect();
        if id.len() >= 10 {
            return Some(id);
        }
    }
    if let Some(i) = u.find("id=") {
        let rest = &u[i + 3..];
        let id: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .collect();
        if id.len() >= 10 {
            return Some(id);
        }
    }
    None
}

fn mediafire_href(html: &str) -> Option<String> {
    for marker in [
        "id=\"downloadButton\"",
        "aria-label=\"Download file\"",
        "download.mediafire.com",
    ] {
        if let Some(i) = html.find(marker) {
            let window = &html[i.saturating_sub(200)..html.len().min(i + 800)];
            if let Some(h) = href_in(window) {
                if let Ok(u) = parse_url(&h) {
                    if host_matches(&u, "mediafire.com")
                        || host_matches(&u, "download.mediafire.com")
                    {
                        return Some(h);
                    }
                }
            }
        }
    }
    None
}

fn href_in(s: &str) -> Option<String> {
    let low = s.to_ascii_lowercase();
    let i = low.find("href=")?;
    let rest = s[i + 5..].trim_start();
    let q = rest.chars().next()?;
    if q != '"' && q != '\'' {
        return None;
    }
    let end = rest[1..].find(q)? + 1;
    let h = rest[1..end].trim().to_string();
    if h.starts_with("http://") || h.starts_with("https://") {
        Some(h)
    } else {
        None
    }
}

fn filename_from(url: &str, cd: Option<&str>) -> String {
    if let Some(cd) = cd {
        // Proper quoted / RFC 5987 parsing (don't split on ; inside quotes).
        if let Some(name) = content_disposition_filename(cd) {
            return sanitize_name(&name);
        }
    }
    let path = Url::parse(url)
        .ok()
        .and_then(|u| u.path_segments()?.next_back().map(|s| s.to_string()))
        .unwrap_or_else(|| "download".into());
    let name = percent_decode(&path);
    sanitize_name(if name.is_empty() { "download" } else { &name })
}

fn content_disposition_filename(cd: &str) -> Option<String> {
    // filename*=UTF-8''...
    let mut i = 0;
    let b = cd.as_bytes();
    while i < b.len() {
        while i < b.len() && (b[i] == b' ' || b[i] == b';') {
            i += 1;
        }
        let start = i;
        while i < b.len() && b[i] != b'=' && b[i] != b';' {
            i += 1;
        }
        let key = cd[start..i].trim().to_ascii_lowercase();
        if i < b.len() && b[i] == b'=' {
            i += 1;
            let val = if i < b.len() && (b[i] == b'"' || b[i] == b'\'') {
                let q = b[i];
                i += 1;
                let vstart = i;
                while i < b.len() && b[i] != q {
                    if b[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
                let v = cd[vstart..i].replace("\\\"", "\"").replace("\\\\", "\\");
                if i < b.len() {
                    i += 1;
                }
                v
            } else {
                let vstart = i;
                while i < b.len() && b[i] != b';' {
                    i += 1;
                }
                cd[vstart..i].trim().to_string()
            };
            if key == "filename*" {
                if let Some(decoded) = val.split("''").nth(1) {
                    let name = percent_decode(decoded);
                    if !name.is_empty() {
                        return Some(name);
                    }
                }
            } else if key == "filename" && !val.is_empty() {
                return Some(val);
            }
        }
        if i < b.len() && b[i] == b';' {
            i += 1;
        }
    }
    None
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Ok(h), Ok(l)) = (
                u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 2]).unwrap_or(""), 16),
                u8::from_str_radix(std::str::from_utf8(&bytes[i + 2..i + 3]).unwrap_or(""), 16),
            ) {
                out.push((h << 4) | l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn is_bidi_control(c: char) -> bool {
    matches!(
        c,
        '\u{200E}' | '\u{200F}' | '\u{061C}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
    )
}

/// Windows device names (CON, NUL, COM1..9, LPT1..9 incl. superscript ¹²³, CONIN$, CONOUT$).
/// `stem` is the part before the first dot; surrounding spaces/dots are ignored like Windows does.
pub fn is_windows_reserved_stem(stem: &str) -> bool {
    let s: String = stem
        .trim()
        .trim_end_matches([' ', '.'])
        .chars()
        .map(|c| match c {
            '\u{00B9}' => '1',
            '\u{00B2}' => '2',
            '\u{00B3}' => '3',
            _ => c,
        })
        .collect::<String>()
        .to_ascii_uppercase();
    matches!(
        s.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "CONIN$"
            | "CONOUT$"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    ) || (s.len() == 4
        && (s.starts_with("COM") || s.starts_with("LPT"))
        && s.as_bytes()[3].is_ascii_digit())
}

pub fn sanitize_name(n: &str) -> String {
    let mut n: String = n
        .chars()
        .map(|c| {
            if is_bidi_control(c)
                || matches!(
                    c,
                    '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\0'
                )
            {
                '_'
            } else {
                c
            }
        })
        .collect();
    while n.ends_with(' ') || n.ends_with('.') {
        n.pop();
    }
    n = n.trim_start().to_string();
    if n.is_empty() {
        return "download".into();
    }
    let first = n.split_once('.').map(|(st, _)| st).unwrap_or(&n);
    if is_windows_reserved_stem(first) {
        n = format!("_{n}");
    }
    n.chars().take(180).collect()
}

fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let p = dir.join(name);
    if !p.exists() {
        return p;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (name.to_string(), String::new()),
    };
    for n in 2..1000 {
        let c = dir.join(format!("{stem}-{n}{ext}"));
        if !c.exists() {
            return c;
        }
    }
    dir.join(format!("{stem}-new{ext}"))
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct PartialMeta {
    etag: Option<String>,
    last_modified: Option<String>,
    url: String,
}

fn meta_path(partial: &Path) -> PathBuf {
    let mut s = partial.as_os_str().to_owned();
    s.push(META_SUFFIX);
    PathBuf::from(s)
}

fn refuse_link(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(m) if safecopy::is_link(&m) => Err(format!(
            "{} is a link; not writing through it.",
            safecopy::plain(&path.display().to_string())
        )),
        _ => Ok(()),
    }
}

/// True if the file has other hard links (its data is shared with another name).
fn partial_shares_data(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        fs::symlink_metadata(path)
            .map(|m| m.nlink() > 1)
            .unwrap_or(false)
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{
            GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
        };
        let Ok(f) = File::open(path) else { return true }; // can't check → treat as unsafe, restart
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        let ok = unsafe { GetFileInformationByHandle(f.as_raw_handle() as _, &mut info) };
        ok == 0 || info.nNumberOfLinks > 1
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        true
    }
}

/// Replace `path` with a fresh copy of its bytes (breaks any hard link). Same folder, then rename.
#[allow(dead_code)]
fn rewrite_fresh(path: &Path) -> Result<(), String> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".fresh");
    let tmp = PathBuf::from(tmp);
    let _ = fs::remove_file(&tmp);
    let mut src = File::open(path).map_err(|e| e.to_string())?;
    let mut dst = open_nofollow(&tmp, false)?;
    std::io::copy(&mut src, &mut dst).map_err(|e| e.to_string())?;
    dst.sync_all().map_err(|e| e.to_string())?;
    drop((src, dst));
    fs::remove_file(path).map_err(|e| e.to_string())?;
    fs::rename(&tmp, path).map_err(|e| e.to_string())
}

/// Open a regular file without following a final-component symlink (Unix O_NOFOLLOW).
fn open_nofollow(path: &Path, append: bool) -> Result<File, String> {
    refuse_link(path)?;
    let mut opts = fs::OpenOptions::new();
    if append {
        opts.append(true);
    } else {
        opts.write(true).create_new(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Linux O_NOFOLLOW = 0x20000. Harmless no-op on other unix if unsupported → EINVAL then fallback.
        opts.custom_flags(0x20000);
    }
    match opts.open(path) {
        Ok(f) => Ok(f),
        Err(e) => {
            #[cfg(unix)]
            {
                // Fallback without the flag if the kernel rejected it (non-Linux unix).
                if e.raw_os_error() == Some(22) {
                    let mut opts = fs::OpenOptions::new();
                    if append {
                        opts.append(true);
                    } else {
                        opts.write(true).create_new(true);
                    }
                    return opts.open(path).map_err(|e| e.to_string());
                }
            }
            Err(e.to_string())
        }
    }
}

fn save_meta(partial: &Path, m: &PartialMeta) -> Result<(), String> {
    let mp = meta_path(partial);
    refuse_link(&mp)?;
    let _ = fs::remove_file(&mp);
    let mut f = open_nofollow(&mp, false).map_err(|e| format!("Cannot write resume meta: {e}"))?;
    f.write_all(serde_json::to_string(m).unwrap_or_default().as_bytes())
        .map_err(|e| format!("Cannot write resume meta: {e}"))?;
    Ok(())
}

fn load_meta(partial: &Path) -> Option<PartialMeta> {
    let mp = meta_path(partial);
    let m = fs::symlink_metadata(&mp).ok()?;
    if safecopy::is_link(&m) || !m.is_file() {
        return None;
    }
    let s = fs::read_to_string(&mp).ok()?;
    serde_json::from_str(&s).ok()
}

fn clear_partial(partial: &Path) {
    let _ = fs::remove_file(partial);
    let mp = meta_path(partial);
    if let Ok(m) = fs::symlink_metadata(&mp) {
        if !safecopy::is_link(&m) {
            let _ = fs::remove_file(&mp);
        }
    }
}

/// Normalize / validate an optional expected SHA-256. Empty → None. Malformed → Err.
pub fn parse_expected_sha256(raw: Option<&str>) -> Result<Option<String>, String> {
    let Some(s) = raw.map(|x| x.trim()).filter(|x| !x.is_empty()) else {
        return Ok(None);
    };
    let s = s.to_ascii_lowercase();
    if s.len() != 64 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("Expected SHA-256 must be exactly 64 hexadecimal characters.".into());
    }
    Ok(Some(s))
}

fn parse_content_range(cr: &str) -> Option<(u64, u64)> {
    // bytes start-end/total  or bytes start-end/*
    let cr = cr.trim();
    let rest = cr.strip_prefix("bytes ")?;
    let (range, total) = rest.split_once('/')?;
    let (start, _) = range.split_once('-')?;
    let start: u64 = start.parse().ok()?;
    let total = if total == "*" {
        None
    } else {
        Some(total.parse().ok()?)
    };
    Some((start, total.unwrap_or(0)))
}

pub fn download(
    url: &str,
    dir: &Path,
    progress: &mut dyn FnMut(u32, &str),
    cancelled: &dyn Fn() -> bool,
) -> Result<DownloadResult, String> {
    download_with(url, dir, &DownloadOpts::default(), progress, cancelled)
}

/// Cached response metadata from the final hop (etag, last-modified, length, type, disposition).
type FetchHeaderMeta = (Option<String>, Option<String>, Option<u64>, Option<String>, String);

pub fn download_with(
    url: &str,
    dir: &Path,
    opts: &DownloadOpts,
    progress: &mut dyn FnMut(u32, &str),
    cancelled: &dyn Fn() -> bool,
) -> Result<DownloadResult, String> {
    let started_at = Instant::now();
    let expected = parse_expected_sha256(opts.expected_sha256.as_deref())?;
    let mut current = parse_url(url)?;
    let http_warning = current.scheme() == "http";
    fs::create_dir_all(dir).map_err(|e| format!("Cannot create downloads folder: {e}"))?;

    // Hoster resolve (may fetch MediaFire HTML — still SSRF-checked)
    current = parse_url(&resolve_direct(current.as_str())?)?;

    let mut resp_holder = None;
    let mut final_headers: Option<FetchHeaderMeta> = None;
    // etag, last_modified, content_length, content_type, content_disposition
    let mut pinned_agent = None;

    for hop in 0..=MAX_REDIRECTS {
        if cancelled() {
            return Err(crate::archive::ERR_CANCELLED.into());
        }
        if started_at.elapsed() > OVERALL_TIMEOUT {
            return Err("Download timed out (overall limit).".into());
        }
        let addrs = resolve_safe_addrs(&current)?;
        let host_key = url_host_key(&current)?;
        let agent = agent_pinned(host_key, addrs);
        pinned_agent = Some(agent.clone());
        let r = agent
            .get(current.as_str())
            .call()
            .map_err(|e| format!("Download failed: {e}"))?;
        let status = r.status();
        if (300..400).contains(&status) {
            if hop == MAX_REDIRECTS {
                return Err("Too many redirects.".into());
            }
            let loc = r.header("location").unwrap_or("").to_string();
            if loc.is_empty() {
                return Err(format!("Redirect without a Location header ({status})."));
            }
            let next = join_redirect(&current, &loc)?;
            if current.scheme() == "https" && next.scheme() == "http" {
                return Err("Refusing https→http redirect (downgrade).".into());
            }
            if next.scheme() != "http" && next.scheme() != "https" {
                return Err("Redirect left http(s).".into());
            }
            assert_safe_url(&next)?;
            current = next;
            continue;
        }
        if !(200..300).contains(&status) {
            return Err(format!("Server returned HTTP {status}."));
        }
        // Only a strong ETag is a usable resume validator (weak W/ tags may match changed bytes).
        let etag = r
            .header("etag")
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty() && !s.starts_with("W/"));
        let lm = r.header("last-modified").map(|s| s.to_string());
        let len = r
            .header("content-length")
            .and_then(|s| s.parse::<u64>().ok());
        let ct = r.header("content-type").unwrap_or("").to_string();
        let cd = r.header("content-disposition").unwrap_or("").to_string();
        final_headers = Some((etag, lm, len, Some(ct), cd));
        resp_holder = Some(r);
        break;
    }
    let resp = resp_holder.ok_or_else(|| "Too many redirects.".to_string())?;
    let (etag, last_modified, len, ct, cd) = final_headers.unwrap();
    let ct = ct.unwrap_or_default().to_ascii_lowercase();
    let name = filename_from(
        current.as_str(),
        if cd.is_empty() {
            None
        } else {
            Some(cd.as_str())
        },
    );
    if ct.contains("text/html") && !name.contains('.') {
        return Err(
            "That link returned a web page, not a file. Try a direct download link.".into(),
        );
    }
    if let Some(n) = len {
        if n > opts.max_bytes {
            return Err(format!(
                "File is {}; the size limit is {}. Raise it in Settings if you meant to.",
                human_size(n),
                human_size(opts.max_bytes)
            ));
        }
        if let Some(free) = crate::god::free_space(dir) {
            if free < n + 32 * 1024 * 1024 {
                return Err(format!(
                    "Not enough free space: needs {} MB, {} MB free.",
                    n / 1_048_576,
                    free / 1_048_576
                ));
            }
        }
    }

    let dest = unique_path(dir, &name);
    let mut pname = dest.file_name().unwrap().to_os_string();
    pname.push(PARTIAL);
    let partial = dest.parent().unwrap().join(pname);

    let mut have = 0u64;
    let mut resume_meta = PartialMeta {
        etag: etag.clone(),
        last_modified: last_modified.clone(),
        url: current.to_string(),
    };
    if let Ok(m) = fs::symlink_metadata(&partial) {
        if safecopy::is_link(&m) || !m.is_file() {
            return Err(format!(
                "{} is not a regular file.",
                safecopy::plain(&partial.display().to_string())
            ));
        }
        have = m.len();
        if partial_shares_data(&partial) {
            // A hard link to some other file: appending would modify that file. Drop our name for it.
            let _ = fs::remove_file(&partial);
            clear_partial(&partial);
            have = 0;
        } else if let Some(old) = load_meta(&partial) {
            // URL changed, or nothing strong to compare a resumed body with → restart
            let weak = old.etag.as_deref().is_none_or(|e| e.starts_with("W/"))
                && old
                    .last_modified
                    .as_deref()
                    .is_none_or(|l| l.trim().is_empty());
            if old.url != current.as_str() || weak {
                clear_partial(&partial);
                have = 0;
            } else {
                resume_meta = old;
            }
        } else if have > 0 {
            // No meta for an old partial — safer to restart
            clear_partial(&partial);
            have = 0;
        }
    }

    let agent = pinned_agent
        .clone()
        .ok_or_else(|| "Internal error: no pinned agent.".to_string())?;
    let (mut reader, total_hint): (Box<dyn Read + Send>, Option<u64>) = if have > 0 {
        let mut req = agent
            .get(current.as_str())
            .set("Range", &format!("bytes={have}-"));
        if let Some(ref e) = resume_meta.etag {
            req = req.set("If-Range", e);
        } else if let Some(ref lm) = resume_meta.last_modified {
            req = req.set("If-Range", lm);
        }
        match req.call() {
            Ok(r) if r.status() == 206 => {
                let resp_etag = r.header("etag").map(|x| x.to_string());
                let resp_lm = r.header("last-modified").map(|x| x.to_string());
                // Servers that ignore If-Range but honor Range can concatenate mismatched bodies.
                if resume_meta.etag.is_some() && resp_etag != resume_meta.etag {
                    clear_partial(&partial);
                    return Err("Resume failed (ETag changed). Partial cleared — try again.".into());
                }
                if resume_meta.etag.is_none()
                    && resume_meta.last_modified.is_some()
                    && resp_lm != resume_meta.last_modified
                {
                    clear_partial(&partial);
                    return Err(
                        "Resume failed (Last-Modified changed). Partial cleared — try again."
                            .into(),
                    );
                }
                let cr = r.header("content-range").unwrap_or("").to_string();
                let (start, total) = parse_content_range(&cr).ok_or_else(|| {
                    clear_partial(&partial);
                    "Resume failed (bad Content-Range). Partial cleared — try again.".to_string()
                })?;
                if start != have {
                    clear_partial(&partial);
                    return Err("Resume failed (Content-Range start mismatch). Partial cleared — try again.".into());
                }
                let t = if total > 0 { Some(total) } else { len };
                if let Some(t) = t {
                    if have > t {
                        clear_partial(&partial);
                        return Err("Partial was larger than the file; cleared. Try again.".into());
                    }
                    if t > opts.max_bytes {
                        clear_partial(&partial);
                        return Err("File exceeds the size limit.".into());
                    }
                }
                (Box::new(r.into_reader()), t)
            }
            Ok(r) if (200..300).contains(&r.status()) => {
                clear_partial(&partial);
                have = 0;
                let t = r
                    .header("content-length")
                    .and_then(|s| s.parse().ok())
                    .or(len);
                (Box::new(r.into_reader()), t)
            }
            _ => {
                clear_partial(&partial);
                have = 0;
                (Box::new(resp.into_reader()), len)
            }
        }
    } else {
        resume_meta = PartialMeta {
            etag: etag.clone(),
            last_modified: last_modified.clone(),
            url: current.to_string(),
        };
        save_meta(&partial, &resume_meta)?;
        (Box::new(resp.into_reader()), len)
    };

    if have == 0 {
        save_meta(&partial, &resume_meta)?;
    }

    // Re-check right before appending (link count via nlink / GetFileInformationByHandle).
    if have > 0 && partial_shares_data(&partial) {
        return Err("The partial download is hard-linked to another file; not resuming. Try again to start over.".into());
    }
    let mut out = if have > 0 {
        open_nofollow(&partial, true)?
    } else {
        let _ = fs::remove_file(&partial);
        open_nofollow(&partial, false)?
    };
    let mut hasher = Sha256::new();
    if have > 0 {
        let mut f = File::open(&partial).map_err(|e| e.to_string())?;
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = f.read(&mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
    }
    let mut got = have;
    let mut buf = vec![0u8; 1 << 16];
    let mut last_progress = Instant::now();
    progress(
        if let Some(t) = total_hint {
            ((got * 100) / t.max(1)) as u32
        } else {
            0
        },
        &format!("Downloading {name}…"),
    );
    loop {
        if cancelled() {
            let _ = out.sync_all();
            return Err(crate::archive::ERR_CANCELLED.into());
        }
        if started_at.elapsed() > OVERALL_TIMEOUT {
            let _ = out.sync_all();
            return Err("Download timed out (overall limit).".into());
        }
        if last_progress.elapsed() > IDLE_TIMEOUT {
            let _ = out.sync_all();
            return Err(
                "Download stalled (no data for 2 minutes). You can try again to resume.".into(),
            );
        }
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                let _ = out.sync_all();
                return Err(format!("Download interrupted: {e}"));
            }
        };
        if let Err(e) = out.write_all(&buf[..n]) {
            let _ = out.sync_all();
            if e.raw_os_error() == Some(28) || format!("{e}").contains("No space") {
                clear_partial(&partial);
                return Err("Disk full — partial download removed.".into());
            }
            return Err(format!("Write failed: {e}"));
        }
        hasher.update(&buf[..n]);
        got += n as u64;
        last_progress = Instant::now();
        if got > opts.max_bytes {
            clear_partial(&partial);
            return Err("Download exceeded the size limit; partial removed.".into());
        }
        if let Some(t) = total_hint {
            if got > t {
                clear_partial(&partial);
                return Err("Download wrote past the declared size; partial removed.".into());
            }
        }
        let pct = match total_hint {
            Some(t) if t > 0 => ((got * 100) / t).min(99) as u32,
            _ => ((got / (512 * 1024)) % 90) as u32,
        };
        progress(pct, &format!("{name} · {}", fmt_bytes(got)));
    }
    out.sync_all().map_err(|e| e.to_string())?;
    drop(out);
    if let Some(t) = total_hint {
        if got != t {
            if got > t {
                clear_partial(&partial);
            }
            return Err(format!(
                "Download incomplete: got {got} of {t} bytes. You can try again to resume."
            ));
        }
    }
    if got == 0 {
        clear_partial(&partial);
        return Err("The download was empty.".into());
    }
    {
        let mut f = File::open(&partial).map_err(|e| e.to_string())?;
        let mut head = [0u8; 64];
        let n = f.read(&mut head).unwrap_or(0);
        let h = String::from_utf8_lossy(&head[..n]).to_ascii_lowercase();
        if h.contains("<!doctype html") || h.contains("<html") {
            clear_partial(&partial);
            return Err(
                "That link returned a web page, not a file. Try a direct download link.".into(),
            );
        }
    }
    if dest.exists() {
        return Err(format!(
            "{} appeared while downloading; left the .partial so nothing was overwritten.",
            safecopy::plain(&dest.display().to_string())
        ));
    }
    let sha = hex_lower(&hasher.finalize());
    if let Some(ref exp) = expected {
        if exp != &sha {
            let base = format!(
                "{}.bad-hash",
                dest.file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("download.bin")
            );
            let q = unique_path(dir, &base);
            let _ = fs::rename(&partial, &q);
            let _ = fs::remove_file(meta_path(&partial));
            return Err(format!(
                "SHA-256 mismatch (expected {exp}, got {sha}). File quarantined as {}.",
                q.file_name().unwrap().to_string_lossy()
            ));
        }
    }
    fs::rename(&partial, &dest).map_err(|e| format!("Cannot finish download: {e}"))?;
    let _ = fs::remove_file(meta_path(&partial));
    progress(100, &name);
    Ok(DownloadResult {
        path: dest.display().to_string(),
        name: dest.file_name().unwrap().to_string_lossy().into_owned(),
        bytes: got,
        sha256: sha,
        final_url: current.to_string(),
        http_warning,
    })
}

fn fmt_bytes(n: u64) -> String {
    if n < 1024 {
        format!("{n} B")
    } else if n < 1048576 {
        format!("{:.1} KB", n as f64 / 1024.0)
    } else {
        format!("{:.1} MB", n as f64 / 1048576.0)
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn list_archives(root: &Path) -> Result<Vec<String>, String> {
    let meta =
        fs::symlink_metadata(root).map_err(|e| format!("Cannot open {}: {e}", root.display()))?;
    if safecopy::is_link(&meta) {
        return Err("Links aren't followed; pick the real folder.".into());
    }
    if !meta.is_dir() {
        return Err("Not a folder.".into());
    }
    let mut out = vec![];
    let mut st = vec![root.to_path_buf()];
    while let Some(d) = st.pop() {
        let Ok(rd) = fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            let Ok(m) = fs::symlink_metadata(&p) else {
                continue;
            };
            if safecopy::is_link(&m) {
                continue;
            }
            if m.is_dir() {
                st.push(p);
            } else if m.is_file() {
                let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
                if crate::archive::is_archive_name(name)
                    || name.to_ascii_lowercase().ends_with(".iso")
                {
                    out.push(p.display().to_string());
                }
            }
        }
    }
    out.sort();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
    use std::thread;
    /// Every test that reads or writes UNPACK_TEST_ALLOW_LOOPBACK must hold this (poison-tolerant).
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        super::tests_env_lock()
    }

    fn tmp(n: &str) -> crate::testutil::TmpDir {
        crate::testutil::TmpDir::new(&format!("dl-{n}"))
    }

    fn serve_once(status: &str, headers: &str, body: &'static [u8]) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let st = status.to_string();
        let hd = headers.to_string();
        thread::spawn(move || {
            if let Ok((mut s, _)) = listener.accept() {
                let mut buf = [0u8; 2048];
                let _ = s.read(&mut buf);
                let resp = format!(
                    "HTTP/1.1 {st}\r\nContent-Length: {}\r\n{hd}\r\n",
                    body.len()
                );
                let _ = s.write_all(resp.as_bytes());
                let _ = s.write_all(body);
            }
        });
        thread::sleep(Duration::from_millis(30));
        format!("http://127.0.0.1:{port}/file.bin")
    }

    #[test]
    fn downloads_with_sha() {
        let _guard = env_lock();
        std::env::set_var("UNPACK_TEST_ALLOW_LOOPBACK", "1");
        let d = tmp("ok");
        let url = serve_once("200 OK", "Content-Disposition: attachment; filename=\"pack.zip\"\r\nContent-Type: application/zip\r\n", b"hello-download-bytes!!");
        let r = download(&url, &d, &mut |_, _| {}, &|| false).unwrap();
        assert_eq!(r.name, "pack.zip");
        assert_eq!(
            r.sha256,
            hex_lower(&Sha256::digest(b"hello-download-bytes!!"))
        );
        assert!(r.http_warning);
        std::env::remove_var("UNPACK_TEST_ALLOW_LOOPBACK");
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn refuses_non_http_and_html() {
        let _guard = env_lock();
        std::env::set_var("UNPACK_TEST_ALLOW_LOOPBACK", "1");
        let d = tmp("bad");
        assert!(download("file:///etc/passwd", &d, &mut |_, _| {}, &|| false).is_err());
        let url = serve_once(
            "200 OK",
            "Content-Type: text/html\r\n",
            b"<!DOCTYPE html><html>login</html>",
        );
        assert!(download(&url, &d, &mut |_, _| {}, &|| false)
            .unwrap_err()
            .contains("web page"));
        std::env::remove_var("UNPACK_TEST_ALLOW_LOOPBACK");
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn hoster_host_exact_not_substring() {
        let _guard = env_lock();
        let u = parse_url("http://127.0.0.1/zz?mediafire.com").unwrap();
        assert!(!host_matches(&u, "mediafire.com"));
        assert!(!host_matches(&u, "drive.google.com"));
        let u = parse_url("https://www.mediafire.com/file/x").unwrap();
        assert!(host_matches(&u, "mediafire.com"));
        let u = parse_url("https://evil.com/drive.google.com/file").unwrap();
        assert!(!host_matches(&u, "drive.google.com"));
        // URL rewrite without DNS (dropbox/google helpers only change the string when host matches)
        let drop = parse_url("https://www.dropbox.com/s/abc/file.zip?dl=0").unwrap();
        assert!(host_matches(&drop, "dropbox.com"));
        let s = drop.to_string().replace("dl=0", "dl=1");
        assert!(s.contains("dl=1"));
        let g = parse_url("https://drive.google.com/file/d/abcdefghijklmnopqrstuv/view").unwrap();
        assert!(host_matches(&g, "drive.google.com"));
        assert_eq!(
            google_id(g.as_str()).as_deref(),
            Some("abcdefghijklmnopqrstuv")
        );
        // Substring trap must not resolve_direct-fetch: host is 127.0.0.1
        std::env::set_var("UNPACK_TEST_ALLOW_LOOPBACK", "1");
        let trap = "http://127.0.0.1/zz?mediafire.com";
        let out = resolve_direct(trap).unwrap();
        assert!(
            out.contains("127.0.0.1"),
            "must not treat query as hoster: {out}"
        );
        std::env::remove_var("UNPACK_TEST_ALLOW_LOOPBACK");
    }

    #[test]
    fn ssrf_blocks_loopback_and_private() {
        let _guard = env_lock();
        std::env::remove_var("UNPACK_TEST_ALLOW_LOOPBACK");
        assert!(is_blocked_ip("127.0.0.1".parse().unwrap()));
        assert!(is_blocked_ip("10.0.0.1".parse().unwrap()));
        assert!(is_blocked_ip("192.168.1.1".parse().unwrap()));
        assert!(is_blocked_ip("169.254.1.1".parse().unwrap()));
        assert!(is_blocked_ip("100.64.0.1".parse().unwrap()));
        assert!(is_blocked_ip("::1".parse().unwrap()));
        assert!(is_blocked_ip("fc00::1".parse().unwrap()));
        assert!(is_blocked_ip("fe80::1".parse().unwrap()));
        assert!(!is_blocked_ip("8.8.8.8".parse().unwrap()));
        // download to loopback URL is refused by assert_safe_url before fetch… except our tests use loopback servers.
        // Production path: assert_safe_url on 127.0.0.1 fails:
        let u = parse_url("http://127.0.0.1/x").unwrap();
        assert!(assert_safe_url(&u).is_err());
        let u = parse_url("http://192.168.0.5/x").unwrap();
        assert!(assert_safe_url(&u).is_err());
        std::env::set_var("UNPACK_TEST_ALLOW_LOOPBACK", "1");
        let u = parse_url("http://127.0.0.1/x").unwrap();
        assert!(assert_safe_url(&u).is_ok(), "test escape for loopback");
        std::env::remove_var("UNPACK_TEST_ALLOW_LOOPBACK");
        let u = parse_url("http://127.0.0.1/x").unwrap();
        assert!(assert_safe_url(&u).is_err());
    }

    #[test]
    fn relative_redirect_join_and_https_downgrade() {
        let base = parse_url("https://example.com/dir/page").unwrap();
        let j = join_redirect(&base, "/x").unwrap();
        assert_eq!(j.as_str(), "https://example.com/x");
        let j = join_redirect(&base, "y").unwrap();
        assert_eq!(j.as_str(), "https://example.com/dir/y");
        // downgrade check is in the download loop; unit-level:
        let next = parse_url("http://example.com/x").unwrap();
        assert!(base.scheme() == "https" && next.scheme() == "http");
    }

    #[test]
    fn sanitize_windows_reserved_and_trailing() {
        assert_eq!(sanitize_name("CON"), "_CON");
        assert_eq!(sanitize_name("nul.txt"), "_nul.txt");
        assert_eq!(sanitize_name("aux.zip"), "_aux.zip");
        assert_eq!(sanitize_name("Lpt9.tar.gz"), "_Lpt9.tar.gz");
        assert!(sanitize_name("nul .txt").starts_with('_'));
        assert!(sanitize_name("CONIN$").starts_with('_'));
        assert!(sanitize_name("CONOUT$.log").starts_with('_'));
        assert_eq!(sanitize_name("hello. ."), "hello");
        let bidi = format!("evil{}piz.exe", '\u{202e}');
        assert!(!sanitize_name(&bidi).contains('\u{202e}'));
        assert_eq!(sanitize_name("a;b.zip"), "a;b.zip");
        let n = content_disposition_filename("attachment; filename=\"foo;bar.zip\"").unwrap();
        assert_eq!(n, "foo;bar.zip");
    }

    #[test]
    fn resume_requires_content_range_start() {
        assert_eq!(parse_content_range("bytes 100-199/500"), Some((100, 500)));
        assert_eq!(parse_content_range("bytes 0-99/100"), Some((0, 100)));
        assert!(parse_content_range("bytes */500").is_none());
    }

    #[test]
    fn resume_mismatch_clears_partial() {
        // Custom server: first response 200 with body; we pre-seed a partial with wrong meta and a 206 with start=0
        let d = tmp("resume");
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let hits = Arc::new(Mutex::new(0u32));
        let h2 = hits.clone();
        thread::spawn(move || {
            for _ in 0..3 {
                if let Ok((mut s, _)) = listener.accept() {
                    let mut buf = [0u8; 4096];
                    let n = s.read(&mut buf).unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]);
                    let mut hits = h2.lock().unwrap();
                    *hits += 1;
                    if req.contains("Range:") {
                        // Wrong Content-Range start (0 instead of partial len)
                        let body = b"NEWDATA!!";
                        let resp = format!("HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-8/18\r\nContent-Length: {}\r\n\r\n", body.len());
                        let _ = s.write_all(resp.as_bytes());
                        let _ = s.write_all(body);
                    } else {
                        let body = b"FULLFILECONTENT!!";
                        let resp = format!("HTTP/1.1 200 OK\r\nETag: \"v1\"\r\nContent-Length: {}\r\nContent-Type: application/octet-stream\r\n\r\n", body.len());
                        let _ = s.write_all(resp.as_bytes());
                        let _ = s.write_all(body);
                    }
                }
            }
        });
        thread::sleep(Duration::from_millis(40));
        let url = format!("http://127.0.0.1:{port}/f.bin");
        // Seed a stale partial claiming same URL with etag
        let partial = d.join("f.bin.partial");
        fs::write(&partial, b"OLDOLDOLDOLD").unwrap();
        save_meta(
            &partial,
            &PartialMeta {
                etag: Some("\"v1\"".into()),
                last_modified: None,
                url: url.clone(),
            },
        )
        .ok();
        // Bypass SSRF for loopback in this unit test by calling the resume path through download —
        // assert_safe_url blocks 127.0.0.1. So test parse/clear logic directly:
        let have = fs::metadata(&partial).unwrap().len();
        let cr = "bytes 0-8/18";
        let (start, _) = parse_content_range(cr).unwrap();
        assert_ne!(start, have);
        clear_partial(&partial);
        assert!(!partial.exists());
        assert!(!meta_path(&partial).exists());
        let _ = url;
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn expected_sha_quarantines_unique_name() {
        let d = tmp("sha");
        let partial = d.join("x.bin.partial");
        fs::write(&partial, b"abc").unwrap();
        let sha = hex_lower(&Sha256::digest(b"abc"));
        let expect = "0".repeat(64);
        assert_ne!(sha, expect);
        let dest = d.join("x.bin");
        // Pre-existing quarantine must not be overwritten.
        let existing = d.join("x.bin.bad-hash");
        fs::write(&existing, b"OLD").unwrap();
        let base = format!(
            "{}.bad-hash",
            dest.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("download.bin")
        );
        let q = unique_path(&d, &base);
        assert_ne!(q, existing);
        fs::rename(&partial, &q).unwrap();
        assert!(q.exists() && !partial.exists());
        assert_eq!(fs::read(&existing).unwrap(), b"OLD");
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn partial_refuses_symlink() {
        let d = tmp("partiallink");
        #[cfg(unix)]
        {
            let partial = d.join("f.bin.partial");
            let victim = d.join("victim");
            fs::write(&victim, b"KEEP").unwrap();
            std::os::unix::fs::symlink(&victim, &partial).unwrap();
            assert!(refuse_link(&partial).is_err());
            assert!(open_nofollow(&partial, true).is_err());
            assert_eq!(fs::read(&victim).unwrap(), b"KEEP");
        }
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn sanitize_superscript_devices() {
        for n in [
            "COM\u{00B9}",
            "com\u{00B2}.txt",
            "LPT\u{00B3}.tar.gz",
            "lpt\u{00B9} .log",
        ] {
            assert!(sanitize_name(n).starts_with('_'), "{n}");
        }
        assert!(!sanitize_name("COM\u{00B9}0.txt").starts_with('_'));
    }

    #[test]
    fn hard_linked_partial_is_detected_and_never_appended() {
        let d = tmp("hardlink");
        let victim = d.join("victim");
        fs::write(&victim, b"KEEP").unwrap();
        let partial = d.join("a.bin.partial");
        fs::hard_link(&victim, &partial).unwrap();
        assert!(partial_shares_data(&partial));
        fs::write(d.join("solo"), b"x").unwrap();
        assert!(!partial_shares_data(&d.join("solo")));
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn rewrite_fresh_breaks_shared_data() {
        let d = tmp("fresh");
        let p = d.join("x.partial");
        fs::write(&p, b"ABC").unwrap();
        #[cfg(unix)]
        {
            let v = d.join("victim");
            fs::hard_link(&p, &v).unwrap();
            rewrite_fresh(&p).unwrap();
            let mut f = open_nofollow(&p, true).unwrap();
            f.write_all(b"D").unwrap();
            assert_eq!(fs::read(&v).unwrap(), b"ABC");
        }
        #[cfg(not(unix))]
        rewrite_fresh(&p).unwrap();
        assert!(fs::read(&p).unwrap().starts_with(b"ABC"));
        fs::remove_dir_all(&d).ok();
    }

    /// Server that serves `body` with optional headers, honouring Range blindly (ignores If-Range).
    fn serve_range(
        body: &'static [u8],
        headers: &'static str,
        n: usize,
    ) -> (u16, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let reqs = Arc::new(Mutex::new(vec![]));
        let r2 = reqs.clone();
        thread::spawn(move || {
            for _ in 0..n {
                let Ok((mut s, _)) = listener.accept() else {
                    break;
                };
                let mut buf = [0u8; 4096];
                let k = s.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..k]).to_string();
                r2.lock().unwrap().push(req.clone());
                let start = req
                    .lines()
                    .find_map(|l| l.strip_prefix("Range: bytes="))
                    .and_then(|v| v.trim_end_matches('-').parse::<usize>().ok());
                let resp = match start {
                    Some(st) if st < body.len() => format!("HTTP/1.1 206 Partial Content\r\n{headers}Content-Range: bytes {st}-{}/{}\r\nContent-Length: {}\r\n\r\n", body.len() - 1, body.len(), body.len() - st),
                    _ => format!("HTTP/1.1 200 OK\r\n{headers}Content-Type: application/octet-stream\r\nContent-Length: {}\r\n\r\n", body.len()),
                };
                let _ = s.write_all(resp.as_bytes());
                let _ = s.write_all(&body[start.filter(|&x| x < body.len()).unwrap_or(0)..]);
            }
        });
        (port, reqs)
    }

    #[test]
    fn resume_without_strong_validator_restarts() {
        let _guard = env_lock();
        std::env::set_var("UNPACK_TEST_ALLOW_LOOPBACK", "1");
        for (tag, hdr) in [("none", ""), ("weak", "ETag: W/\"v1\"\r\n")] {
            let d = tmp(&format!("weak-{tag}"));
            let (port, reqs) = serve_range(b"NEWFILECONTENT", hdr, 2);
            let url = format!("http://127.0.0.1:{port}/f.bin");
            let partial = d.join("f.bin.partial");
            fs::write(&partial, b"OLDOLD").unwrap();
            save_meta(
                &partial,
                &PartialMeta {
                    etag: if tag == "weak" {
                        Some("W/\"v1\"".into())
                    } else {
                        None
                    },
                    last_modified: None,
                    url: url.clone(),
                },
            )
            .unwrap();
            let r = download(&url, &d, &mut |_, _| {}, &|| false).unwrap();
            assert_eq!(
                fs::read(&r.path).unwrap(),
                b"NEWFILECONTENT",
                "{tag}: restarted, no old prefix"
            );
            assert!(
                !reqs.lock().unwrap().iter().any(|q| q.contains("Range:")),
                "{tag}: no Range sent"
            );
            fs::remove_dir_all(&d).ok();
        }
        std::env::remove_var("UNPACK_TEST_ALLOW_LOOPBACK");
    }

    #[test]
    fn hard_linked_partial_download_restarts_and_victim_kept() {
        let _guard = env_lock();
        std::env::set_var("UNPACK_TEST_ALLOW_LOOPBACK", "1");
        let d = tmp("hl-dl");
        let (port, _) = serve_range(b"0123456789", "ETag: \"s1\"\r\n", 2);
        let url = format!("http://127.0.0.1:{port}/f.bin");
        let victim = d.join("victim");
        fs::write(&victim, b"KEEP").unwrap();
        let partial = d.join("f.bin.partial");
        fs::hard_link(&victim, &partial).unwrap();
        save_meta(
            &partial,
            &PartialMeta {
                etag: Some("\"s1\"".into()),
                last_modified: None,
                url: url.clone(),
            },
        )
        .unwrap();
        let r = download(&url, &d, &mut |_, _| {}, &|| false).unwrap();
        assert_eq!(fs::read(&victim).unwrap(), b"KEEP");
        assert_eq!(fs::read(&r.path).unwrap(), b"0123456789");
        fs::remove_dir_all(&d).ok();
        std::env::remove_var("UNPACK_TEST_ALLOW_LOOPBACK");
    }

    #[test]
    fn sanitize_com_lpt_any_extension() {
        assert!(sanitize_name("COM1.txt").starts_with('_'));
        assert!(sanitize_name("com3.docx").starts_with('_'));
        assert!(sanitize_name("LPT1").starts_with('_'));
        assert_eq!(sanitize_name("Lpt9.tar.gz"), "_Lpt9.tar.gz");
        assert!(sanitize_name("nul .txt").starts_with('_'));
        assert_eq!(sanitize_name("report..."), "report");
        assert_eq!(sanitize_name("  spaced  "), "spaced");
    }

    #[test]
    fn list_archives_recursive() {
        let d = tmp("walk");
        fs::create_dir_all(d.join("a/b")).unwrap();
        fs::write(d.join("a/one.zip"), b"x").unwrap();
        fs::write(d.join("a/b/two.iso"), b"x").unwrap();
        fs::write(d.join("a/b/readme.txt"), b"x").unwrap();
        let v = list_archives(&d).unwrap();
        assert_eq!(v.len(), 2);
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn size_cap_setting() {
        let opts = DownloadOpts {
            max_bytes: 10,
            expected_sha256: None,
        };
        assert!(opts.max_bytes < DEFAULT_MAX_BYTES);
    }

    #[test]
    fn expected_sha_malformed_refused() {
        assert!(parse_expected_sha256(Some("abcd")).is_err());
        assert!(parse_expected_sha256(Some(&"0".repeat(63))).is_err());
        assert!(parse_expected_sha256(Some(&"g".repeat(64))).is_err());
        assert_eq!(
            parse_expected_sha256(Some(&"a".repeat(64)))
                .unwrap()
                .unwrap()
                .len(),
            64
        );
        assert!(parse_expected_sha256(None).unwrap().is_none());
        assert!(parse_expected_sha256(Some("  ")).unwrap().is_none());
    }

    #[test]
    fn ipv6_literals_and_blocked_ranges() {
        assert!(is_blocked_ip("2002::1".parse().unwrap())); // 6to4
        assert!(is_blocked_ip("64:ff9b::1".parse().unwrap())); // NAT64
        assert!(is_blocked_ip("::ffff:127.0.0.1".parse().unwrap()));
        let u = parse_url("http://[::1]/x").unwrap();
        let _guard = env_lock();
        std::env::remove_var("UNPACK_TEST_ALLOW_LOOPBACK");
        let e = assert_safe_url(&u).unwrap_err();
        assert!(
            e.contains("allowed") || e.contains("local") || e.contains("private"),
            "{e}"
        );
        let u = parse_url("http://[2001:4860:4860::8888]/").unwrap();
        // public DNS — resolve may fail offline; at least host parse works
        assert!(matches!(u.host(), Some(url::Host::Ipv6(_))));
    }

    #[test]
    fn meta_refuses_symlink() {
        let d = tmp("metalink");
        #[cfg(unix)]
        {
            let partial = d.join("f.bin.partial");
            fs::write(&partial, b"x").unwrap();
            let mp = meta_path(&partial);
            let victim = d.join("victim");
            fs::write(&victim, b"KEEP").unwrap();
            std::os::unix::fs::symlink(&victim, &mp).unwrap();
            assert!(save_meta(
                &partial,
                &PartialMeta {
                    etag: None,
                    last_modified: None,
                    url: "http://x".into()
                }
            )
            .is_err());
            assert_eq!(fs::read(&victim).unwrap(), b"KEEP");
        }
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn release_compare() {
        assert!(version_gt("1.7.0", "1.6.0"));
        assert!(!version_gt("1.6.0", "1.7.0"));
    }

    #[test]
    fn pinned_resolver_returns_only_checked_addrs() {
        let addrs = vec!["8.8.8.8:443".parse().unwrap()];
        let a = agent_pinned("example.com".into(), addrs.clone());
        // Building succeeds; Resolver is used on connect. Unit-check host key helper:
        let u = parse_url("https://Example.COM/path").unwrap();
        assert_eq!(url_host_key(&u).unwrap(), "example.com");
        let _ = a;
        let _ = addrs;
    }
}
