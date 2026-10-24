//! Mod catalog: a signed JSON manifest the user points Unpack at (remote URL or local file).
//! Unpack ships NO catalog and no entries; the catalog owner publishes one and its public key.
//!
//! Trust model: the Ed25519 signature is checked over the exact downloaded bytes with the key
//! pinned in Unpack's config *before* the JSON is parsed. Every file must carry a SHA-256.
//! Downloads go through `download` (SSRF rules); installs are staged, verified, and moved into
//! place only under folders allowed for their handler. See HANDOFF "Mod catalog".

pub mod ini;
pub mod install;
pub mod target;

use std::path::{Path, PathBuf};

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};

pub const SCHEMA: u32 = 1;
pub const MANIFEST_MAX: u64 = 4 << 20;
pub const SIG_MAX: u64 = 1024;
pub const ENTRIES_MAX: usize = 2000;
pub const FILES_MAX: usize = 50;
pub const FILE_MAX: u64 = 20 << 30;
/// catalog_version must stay below this (2^53: exact in JavaScript, and far from u64::MAX so an
/// owner can't lock out every later version by mistake; R12 LOW-5).
pub const MAX_CATALOG_VERSION: u64 = 1 << 53;
pub const HANDLERS: &[&str] = &["xex", "plugin", "tu", "dlc", "trainer", "patch"];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema: u32,
    pub catalog_version: u64,
    pub entries: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Game {
    pub title_id: String,
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ModMeta {
    pub name: String,
    #[serde(default)]
    pub author: String,
    pub version: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Compat {
    /// Title update numbers the mod works with (0 = no TU). Empty = any.
    #[serde(default)]
    pub tu: Vec<u32>,
    /// Minimum DashLaunch version, informational (can't be read reliably from the target).
    #[serde(default)]
    pub dash_min: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Dash {
    /// Preferred plugin slot (2..=5). None = lowest free.
    #[serde(default)]
    pub slot: Option<u8>,
    /// Plugin file relative to dest_path (needed when the download is an archive).
    #[serde(default)]
    pub file: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Install {
    pub handler: String,
    /// Target hint: "hdd", "usb" or "any" (default).
    #[serde(default)]
    pub dest: Option<String>,
    /// Folder on the target drive, relative ("Plugins", "Games/My Mod"). Default per handler.
    #[serde(default)]
    pub dest_path: Option<String>,
    #[serde(default)]
    pub dashlaunch: Option<Dash>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CFile {
    pub url: String,
    #[serde(default)]
    pub mirrors: Vec<String>,
    pub size: u64,
    pub sha256: String,
    pub install: Install,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub id: String,
    pub game: Game,
    #[serde(rename = "mod")]
    pub mod_: ModMeta,
    #[serde(default)]
    pub compat: Compat,
    #[serde(default)]
    pub redistribute: bool,
    #[serde(default)]
    pub paid: bool,
    #[serde(default)]
    pub source_page: Option<String>,
    #[serde(default)]
    pub requires: Vec<String>,
    #[serde(default)]
    pub conflicts: Vec<String>,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub files: Vec<CFile>,
}

impl Entry {
    /// Only an entry that may be redistributed and isn't paid can be downloaded/installed.
    pub fn installable(&self) -> bool {
        !self.paid && self.redistribute && !self.files.is_empty()
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Loaded {
    pub catalog_version: u64,
    pub entries: Vec<Entry>,
    /// Entries that were skipped and why.
    pub problems: Vec<String>,
    /// "remote" | "local"
    pub source: String,
}

fn hex_lower(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if !s.len().is_multiple_of(2) || !s.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

pub fn parse_pubkey(hex: &str) -> Result<VerifyingKey, String> {
    let b = unhex(hex)
        .filter(|b| b.len() == 32)
        .ok_or("The catalog public key must be 64 hex characters (Ed25519).")?;
    VerifyingKey::from_bytes(&b.try_into().unwrap())
        .map_err(|_| "That isn't a valid Ed25519 public key.".to_string())
}

pub fn parse_signature(text: &[u8]) -> Result<Signature, String> {
    let t = std::str::from_utf8(text).map_err(|_| "The signature file isn't text.".to_string())?;
    let b = unhex(t)
        .filter(|b| b.len() == 64)
        .ok_or("The signature must be 128 hex characters (Ed25519).")?;
    Ok(Signature::from_bytes(&b.try_into().unwrap()))
}

/// Verify `sig` over the exact `bytes`. Nothing is parsed before this succeeds.
pub fn verify(bytes: &[u8], sig: &[u8], pubkey_hex: &str) -> Result<(), String> {
    let key = parse_pubkey(pubkey_hex)?;
    let sig = parse_signature(sig)?;
    key.verify_strict(bytes, &sig).map_err(|_| {
        "The catalog's signature doesn't match the pinned public key. Nothing was loaded."
            .to_string()
    })
}

/// For the signing tool: a new keypair (secret seed hex, public key hex).
pub fn keygen() -> Result<(String, String), String> {
    let mut seed = [0u8; 32];
    getrandom::getrandom(&mut seed).map_err(|e| format!("No randomness available: {e}"))?;
    let k = SigningKey::from_bytes(&seed);
    Ok((hex_lower(&seed), hex_lower(k.verifying_key().as_bytes())))
}

pub fn signing_key(secret_hex: &str) -> Result<SigningKey, String> {
    let b = unhex(secret_hex)
        .filter(|b| b.len() == 32)
        .ok_or("The secret key must be 64 hex characters.")?;
    Ok(SigningKey::from_bytes(&b.try_into().unwrap()))
}

pub fn public_of(secret_hex: &str) -> Result<String, String> {
    Ok(hex_lower(
        signing_key(secret_hex)?.verifying_key().as_bytes(),
    ))
}

pub fn sign(bytes: &[u8], secret_hex: &str) -> Result<String, String> {
    Ok(hex_lower(&signing_key(secret_hex)?.sign(bytes).to_bytes()))
}

// ---- validation ----

fn clean(s: &str, max: usize) -> String {
    s.trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(max)
        .collect()
}

fn valid_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars().next().unwrap().is_ascii_alphanumeric()
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
}

/// https only (plain http only for the loopback test servers).
pub fn url_ok(u: &str) -> Result<(), String> {
    let p = url::Url::parse(u).map_err(|_| format!("\"{}\" isn't a valid URL", clean(u, 80)))?;
    if p.scheme() == "https" || (p.scheme() == "http" && crate::download::allow_test_loopback()) {
        if p.host_str().is_none_or(|h| h.is_empty())
            || !p.username().is_empty()
            || p.password().is_some()
        {
            return Err(format!(
                "\"{}\" must be a plain https link without a login",
                clean(u, 80)
            ));
        }
        return Ok(());
    }
    Err(format!("\"{}\" must be an https:// link", clean(u, 80)))
}

pub fn sha_ok(s: &str) -> bool {
    s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// Folder segments for a handler's dest_path, checked against FATX rules and the folders that
/// handler may write into. Never the drive root (launch.ini lives there).
pub fn dest_segments(
    handler: &str,
    dest_path: Option<&str>,
    tid: &str,
    mod_name: &str,
) -> Result<Vec<String>, String> {
    let content = |kind: &str| {
        vec![
            "Content".to_string(),
            "0000000000000000".into(),
            tid.to_string(),
            kind.into(),
        ]
    };
    let default: Vec<String> = match handler {
        "xex" => vec!["Games".into(), crate::ftp::fatx_name(mod_name)],
        "plugin" => vec!["Plugins".into()],
        "tu" => content("000B0000"),
        "dlc" => content("00000002"),
        "trainer" => vec!["Trainers".into(), tid.into()],
        "patch" => vec!["Patches".into(), tid.into()],
        _ => {
            return Err(format!(
                "unknown install handler \"{}\"",
                clean(handler, 20)
            ))
        }
    };
    let segs = match dest_path.map(str::trim).filter(|s| !s.is_empty()) {
        None => default.clone(),
        Some(p) => crate::ftp::split_remote(&p.replace('\\', "/"))?,
    };
    for s in &segs {
        crate::ftp::check_segment(s)?;
    }
    let allowed: &[&str] = match handler {
        "xex" => &["Games", "Apps"],
        "plugin" => &["Plugins"],
        "trainer" => &["Trainers"],
        "patch" => &["Patches", "Games"],
        _ => &[],
    };
    let ok = if allowed.is_empty() {
        segs.iter()
            .map(|s| s.to_ascii_uppercase())
            .eq(default.iter().map(|s| s.to_ascii_uppercase()))
    } else {
        segs.first()
            .is_some_and(|f| allowed.iter().any(|a| a.eq_ignore_ascii_case(f)))
            && (segs.len() >= 2 || handler == "plugin")
    };
    if !ok || segs.len() > 12 {
        return Err(format!(
            "dest_path \"{}\" isn't allowed for {handler} (allowed: {})",
            crate::ftp::join_remote(&segs),
            if allowed.is_empty() {
                crate::ftp::join_remote(&default)
            } else {
                allowed.join(", ")
            }
        ));
    }
    Ok(segs)
}

pub fn check_entry(e: &Entry) -> Result<Entry, String> {
    let mut e = e.clone();
    if !valid_id(&e.id) {
        return Err(format!(
            "id \"{}\" must be lowercase letters, digits, . _ - (max 64)",
            clean(&e.id, 70)
        ));
    }
    let tid = e.game.title_id.trim().to_ascii_uppercase();
    if crate::modnames::parse_tid(&tid).is_none() {
        return Err(format!(
            "game.title_id \"{}\" isn't a valid TitleID",
            clean(&e.game.title_id, 20)
        ));
    }
    e.game.title_id = tid.clone();
    e.game.name = clean(&e.game.name, 120);
    if e.game.name.is_empty() {
        e.game.name =
            crate::gamedb::name(u32::from_str_radix(&tid, 16).unwrap()).unwrap_or_default();
    }
    if e.game.aliases.len() > 20 {
        return Err("more than 20 game aliases".into());
    }
    e.game.aliases = e
        .game
        .aliases
        .iter()
        .map(|a| clean(a, 60))
        .filter(|a| a.chars().filter(|c| c.is_alphanumeric()).count() >= 2)
        .collect();
    e.mod_.name = clean(&e.mod_.name, 120);
    e.mod_.author = clean(&e.mod_.author, 80);
    e.mod_.version = clean(&e.mod_.version, 40);
    if e.mod_.name.is_empty() || e.mod_.version.is_empty() {
        return Err("mod.name and mod.version are required".into());
    }
    e.notes = clean(&e.notes, 2000);
    if let Some(d) = &e.compat.dash_min {
        e.compat.dash_min = Some(clean(d, 20)).filter(|s| !s.is_empty());
    }
    if e.compat.tu.len() > 100 || e.compat.tu.iter().any(|t| *t > 999) {
        return Err("compat.tu must be up to 100 numbers 0–999".into());
    }
    for r in e.requires.iter().chain(&e.conflicts) {
        if !valid_id(r) || r == &e.id {
            return Err(format!(
                "requires/conflicts id \"{}\" is invalid",
                clean(r, 70)
            ));
        }
    }
    if e.requires.len() > 20 || e.conflicts.len() > 50 {
        return Err("too many requires/conflicts".into());
    }
    match &e.source_page {
        Some(p) => url_ok(p).map_err(|m| format!("source_page: {m}"))?,
        None if e.paid || !e.redistribute => {
            return Err("paid or non-redistributable entries need a source_page".into())
        }
        None => {}
    }
    if e.files.len() > FILES_MAX {
        return Err(format!("more than {FILES_MAX} files"));
    }
    let mut dests = std::collections::HashSet::new();
    for (k, f) in e.files.iter_mut().enumerate() {
        url_ok(&f.url).map_err(|m| format!("file {k}: {m}"))?;
        if f.mirrors.len() > 5 {
            return Err(format!("file {k}: more than 5 mirrors"));
        }
        for m in &f.mirrors {
            url_ok(m).map_err(|m| format!("file {k} mirror: {m}"))?;
        }
        if f.size == 0 || f.size > FILE_MAX {
            return Err(format!("file {k}: size must be 1 byte – 20 GB"));
        }
        let sha = f.sha256.trim().to_ascii_lowercase();
        if !sha_ok(&sha) {
            return Err(format!("file {k}: sha256 is required (64 hex characters)"));
        }
        f.sha256 = sha;
        if !HANDLERS.contains(&f.install.handler.as_str()) {
            return Err(format!(
                "file {k}: unknown handler \"{}\"",
                clean(&f.install.handler, 20)
            ));
        }
        match f.install.dest.as_deref() {
            None | Some("any") | Some("hdd") | Some("usb") => {}
            Some(d) => {
                return Err(format!(
                    "file {k}: dest \"{}\" must be hdd, usb or any",
                    clean(d, 20)
                ))
            }
        }
        dest_segments(
            &f.install.handler,
            f.install.dest_path.as_deref(),
            &tid,
            &e.mod_.name,
        )
        .map_err(|m| format!("file {k}: {m}"))?;
        if let Some(d) = &f.install.dashlaunch {
            if f.install.handler != "plugin" {
                return Err(format!(
                    "file {k}: dashlaunch is only for the plugin handler"
                ));
            }
            if let Some(s) = d.slot {
                if !(ini::FIRST_SLOT..=ini::LAST_SLOT).contains(&s) {
                    return Err(format!(
                        "file {k}: dashlaunch.slot must be 2–5 (plugin1 is reserved)"
                    ));
                }
            }
            if let Some(p) = &d.file {
                for s in crate::ftp::split_remote(&p.replace('\\', "/"))? {
                    crate::ftp::check_segment(&s)?;
                }
            }
        }
        if !dests.insert((
            f.install
                .dest_path
                .clone()
                .unwrap_or_default()
                .to_lowercase(),
            file_name_of(&f.url)?.to_lowercase(),
        )) {
            return Err(format!("file {k}: two files would land on the same name"));
        }
    }
    if e.paid {
        // Paid entries only ever open their source page.
        e.files.clear();
    }
    Ok(e)
}

/// The name a downloaded (non-archive) file gets: the URL's last path segment, FATX-checked.
pub fn file_name_of(u: &str) -> Result<String, String> {
    let p = url::Url::parse(u).map_err(|e| e.to_string())?;
    let last = p
        .path_segments()
        .and_then(|mut s| s.next_back())
        .unwrap_or("")
        .to_string();
    let name = crate::download::sanitize_name(&percent(&last));
    crate::ftp::check_segment(&name)
        .map_err(|m| format!("file name from {}: {m}", clean(u, 80)))?;
    Ok(name)
}

fn percent(s: &str) -> String {
    url::form_urlencoded::parse(s.replace('+', "%2B").as_bytes())
        .map(|(k, v)| {
            if v.is_empty() {
                k.into_owned()
            } else {
                format!("{k}={v}")
            }
        })
        .collect::<Vec<_>>()
        .join("&")
}

pub fn is_archive_name(n: &str) -> bool {
    let n = n.to_ascii_lowercase();
    n.ends_with(".zip") || n.ends_with(".7z") || n.ends_with(".rar")
}

/// Verify, then parse and validate. Invalid entries are skipped and listed in `problems`.
pub fn load_bytes(
    bytes: &[u8],
    sig: &[u8],
    pubkey_hex: &str,
    source: &str,
) -> Result<Loaded, String> {
    if bytes.len() as u64 > MANIFEST_MAX {
        return Err(format!(
            "The catalog is larger than the {} limit.",
            crate::download::human_size(MANIFEST_MAX)
        ));
    }
    if sig.len() as u64 > SIG_MAX {
        return Err(format!(
            "The signature file is larger than the {} limit.",
            crate::download::human_size(SIG_MAX)
        ));
    }
    verify(bytes, sig, pubkey_hex)?;
    let text =
        std::str::from_utf8(bytes).map_err(|_| "The catalog isn't UTF-8 JSON.".to_string())?;
    let m: Manifest = serde_json::from_str(text.strip_prefix('\u{feff}').unwrap_or(text))
        .map_err(|e| format!("The catalog isn't valid: {e}"))?;
    if m.schema != SCHEMA {
        return Err(format!(
            "Catalog schema {} isn't supported (this Unpack reads schema {SCHEMA}).",
            m.schema
        ));
    }
    if m.catalog_version >= MAX_CATALOG_VERSION {
        return Err(format!(
            "catalog_version {} is too large (the limit is {}).",
            m.catalog_version,
            MAX_CATALOG_VERSION - 1
        ));
    }
    let mut out = Loaded {
        catalog_version: m.catalog_version,
        source: source.into(),
        ..Default::default()
    };
    let mut ids = std::collections::HashSet::new();
    for (i, v) in m.entries.into_iter().enumerate() {
        if out.entries.len() >= ENTRIES_MAX {
            out.problems.push(format!(
                "more than {ENTRIES_MAX} entries; the rest are ignored"
            ));
            break;
        }
        let label = v
            .get("id")
            .and_then(|x| x.as_str())
            .map(|s| clean(s, 64))
            .unwrap_or_else(|| format!("#{i}"));
        let e: Entry = match serde_json::from_value(v) {
            Ok(e) => e,
            Err(err) => {
                out.problems.push(format!("entry {label}: {err}"));
                continue;
            }
        };
        match check_entry(&e) {
            Ok(e) if !ids.insert(e.id.clone()) => {
                out.problems.push(format!("entry {label}: duplicate id"))
            }
            Ok(e) => out.entries.push(e),
            Err(err) => out.problems.push(format!("entry {label}: {err}")),
        }
    }
    Ok(out)
}

/// Feed a loaded catalog into game guessing (aliases) and known mod names (learned keys).
pub fn learned_from(l: &Loaded) -> Vec<crate::modnames::Learned> {
    let mut v: Vec<crate::modnames::Learned> = vec![];
    for e in &l.entries {
        if let Ok((key, tid, _)) = crate::modnames::learn_key(&e.mod_.name, &e.game.title_id) {
            if !v.iter().any(|x| x.key == key) {
                v.push(crate::modnames::Learned { key, tid });
            }
        }
    }
    v
}

pub fn aliases_from(l: &Loaded) -> Vec<(u32, String, Vec<String>)> {
    l.entries
        .iter()
        .filter(|e| !e.game.aliases.is_empty())
        .map(|e| {
            (
                u32::from_str_radix(&e.game.title_id, 16).unwrap(),
                e.game.name.clone(),
                e.game.aliases.clone(),
            )
        })
        .collect()
}

static LEARNED: std::sync::RwLock<Vec<crate::modnames::Learned>> =
    std::sync::RwLock::new(Vec::new());
pub fn learned() -> Vec<crate::modnames::Learned> {
    LEARNED.read().unwrap_or_else(|p| p.into_inner()).clone()
}

/// Make a freshly verified catalog active for detection.
pub fn activate(l: &Loaded) {
    *LEARNED.write().unwrap_or_else(|p| p.into_inner()) = learned_from(l);
    crate::gamedb::set_extra_aliases(aliases_from(l));
}

// ---- config + store ----

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    /// "remote" | "local" | "" (off)
    pub source: String,
    pub url: String,
    /// Defaults to url + ".sig".
    pub sig_url: String,
    pub local_file: String,
    /// Ed25519 public key (64 hex) from the catalog owner.
    pub pubkey: String,
}

impl Config {
    pub fn validated(mut self) -> Result<Config, String> {
        self.source = match self.source.as_str() {
            "remote" | "local" | "" => self.source,
            _ => return Err("source must be remote or local".into()),
        };
        self.url = self.url.trim().to_string();
        self.sig_url = self.sig_url.trim().to_string();
        self.local_file = self.local_file.trim().to_string();
        self.pubkey = self.pubkey.trim().to_ascii_lowercase();
        if !self.url.is_empty() {
            url_ok(&self.url)?;
        }
        if !self.sig_url.is_empty() {
            url_ok(&self.sig_url)?;
        }
        if !self.pubkey.is_empty() {
            parse_pubkey(&self.pubkey)?;
        }
        if self.source == "remote" && self.url.is_empty() {
            return Err("Enter the catalog URL.".into());
        }
        if self.source == "local" && self.local_file.is_empty() {
            return Err("Pick the catalog file.".into());
        }
        if !self.source.is_empty() && self.pubkey.is_empty() {
            return Err(
                "Enter the catalog owner's public key. Unpack never loads an unsigned catalog."
                    .into(),
            );
        }
        Ok(self)
    }
}

/// Unpack's own folder for the catalog: config, journal, backups, staging.
#[derive(Debug, Clone)]
pub struct Store {
    pub dir: PathBuf,
}

impl Store {
    pub fn new(dir: PathBuf) -> Store {
        Store { dir }
    }
    fn read_json<T: for<'a> Deserialize<'a> + Default>(&self, name: &str) -> T {
        std::fs::read(self.dir.join(name))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }
    /// Like read_json, but a file that exists and doesn't parse is an error (R12 LOW-2): a damaged
    /// journal or rollback record must never be treated as empty. A copy is kept aside as
    /// `<name>.bad-<stamp>` (once per distinct content) and the original is left in place, so every
    /// later catalog action keeps refusing until the user removes or fixes it.
    fn read_json_strict<T: for<'a> Deserialize<'a> + Default>(
        &self,
        name: &str,
        what: &str,
        removing: &str,
    ) -> Result<T, String> {
        let p = self.dir.join(name);
        let b = match std::fs::read(&p) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(T::default()),
            Err(e) => return Err(format!("Cannot read the catalog {what} ({}): {e}. Catalog installs are off until this is fixed.", p.display())),
        };
        match serde_json::from_slice(&b) {
            Ok(v) => Ok(v),
            Err(_) => {
                let sha = target::sha256_bytes(&b);
                let exists = std::fs::read_dir(&self.dir).ok().is_some_and(|rd| {
                    rd.flatten().any(|d| {
                        let n = d.file_name().to_string_lossy().to_string();
                        n.starts_with(&format!("{name}.bad-"))
                            && std::fs::read(d.path())
                                .map(|x| target::sha256_bytes(&x) == sha)
                                .unwrap_or(false)
                    })
                });
                let mut kept = String::new();
                if !exists {
                    let bad = self
                        .dir
                        .join(format!("{name}.bad-{}", install::unique_stamp()));
                    if std::fs::write(&bad, &b).is_ok() {
                        kept = format!(" A copy was saved as {}.", bad.display());
                    }
                }
                Err(format!("The catalog {what} ({}) is damaged.{kept} Catalog actions are off until you fix or remove that file ({removing}).", p.display()))
            }
        }
    }
    /// Create a file that must not exist yet (backups: never replaced; R12 LOW-3).
    pub fn write_new_file(&self, name: &str, bytes: &[u8]) -> Result<(), String> {
        use std::io::Write;
        let p = self.dir.join(name);
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
        }
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&p)
            .map_err(|e| format!("Cannot save {name}: {e}"))?;
        f.write_all(bytes)
            .and_then(|_| f.sync_all())
            .map_err(|e| format!("Cannot save {name}: {e}"))
    }
    pub fn write_atomic(&self, name: &str, bytes: &[u8]) -> Result<(), String> {
        let p = self.dir.join(name);
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
        }
        let tmp = p.with_extension("tmp");
        std::fs::write(&tmp, bytes).map_err(|e| format!("Cannot save {name}: {e}"))?;
        std::fs::rename(&tmp, &p).map_err(|e| format!("Cannot save {name}: {e}"))
    }
    pub fn config(&self) -> Config {
        self.read_json("config.json")
    }
    pub fn set_config(&self, c: Config) -> Result<Config, String> {
        let c = c.validated()?;
        self.write_atomic("config.json", &serde_json::to_vec_pretty(&c).unwrap())?;
        Ok(c)
    }
    /// Highest catalog_version accepted per public key (rollback protection).
    fn seen(&self) -> Result<std::collections::BTreeMap<String, u64>, String> {
        self.read_json_strict(
            "seen.json",
            "rollback record",
            "removing it resets rollback protection, so an older catalog could load once",
        )
    }
    pub fn check_rollback(&self, pubkey: &str, version: u64) -> Result<(), String> {
        let s = self.seen()?;
        if version >= MAX_CATALOG_VERSION {
            return Err(format!(
                "catalog_version {version} is too large (the limit is {}).",
                MAX_CATALOG_VERSION - 1
            ));
        }
        match s.get(pubkey) {
            Some(&v) if version < v => Err(format!("This catalog (version {version}) is older than one already loaded (version {v}). Not loading it (rollback protection).")),
            _ => {
                let mut s = s;
                s.insert(pubkey.into(), version);
                self.write_atomic("seen.json", &serde_json::to_vec(&s).unwrap())
            }
        }
    }
    pub fn journal(&self) -> Result<install::Journal, String> {
        self.read_json_strict(
            "journal.json",
            "install journal",
            "removing it forgets what Unpack installed",
        )
    }
    pub fn save_journal(&self, j: &install::Journal) -> Result<(), String> {
        self.write_atomic("journal.json", &serde_json::to_vec_pretty(j).unwrap())
    }
    pub fn stage_dir(&self, label: &str) -> Result<PathBuf, String> {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let k = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let d = self.dir.join("stage").join(format!(
            "{}-{n}-{k}",
            label
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
                .take(40)
                .collect::<String>()
        ));
        std::fs::create_dir_all(&d).map_err(|e| format!("Cannot create a staging folder: {e}"))?;
        Ok(d)
    }
}

/// Fetch a small file (catalog / signature) through the downloader (SSRF rules apply).
pub fn fetch_small(
    store: &Store,
    url: &str,
    max: u64,
    cancelled: &dyn Fn() -> bool,
) -> Result<Vec<u8>, String> {
    url_ok(url)?;
    let d = store.stage_dir("fetch")?;
    let r = crate::download::download_with(
        url,
        &d,
        &crate::download::DownloadOpts {
            max_bytes: max,
            expected_sha256: None,
        },
        &mut |_, _| {},
        cancelled,
    );
    let out = r.and_then(|r| std::fs::read(&r.path).map_err(|e| e.to_string()));
    let _ = std::fs::remove_dir_all(&d);
    let too_big = || {
        format!(
            "{url} is larger than the {} limit for catalog files. Nothing was loaded.",
            crate::download::human_size(max)
        )
    };
    let b = out.map_err(|e| {
        if e.contains("size limit") {
            too_big()
        } else {
            e
        }
    })?;
    if b.len() as u64 > max {
        return Err(too_big());
    }
    Ok(b)
}

/// Load the configured catalog. `check_local` grant-checks the local file path.
pub fn refresh(
    store: &Store,
    check_local: &dyn Fn(&str) -> Result<PathBuf, String>,
    cancelled: &dyn Fn() -> bool,
) -> Result<Loaded, String> {
    let c = store.config();
    if c.pubkey.is_empty() {
        return Err("No catalog is set up. Enter the catalog URL (or file) and the owner's public key first.".into());
    }
    let (bytes, sig) = match c.source.as_str() {
        "remote" => {
            let sig_url = if c.sig_url.is_empty() {
                format!("{}.sig", c.url)
            } else {
                c.sig_url.clone()
            };
            (
                fetch_small(store, &c.url, MANIFEST_MAX, cancelled)?,
                fetch_small(store, &sig_url, SIG_MAX, cancelled)?,
            )
        }
        "local" => {
            let p = check_local(&c.local_file)?;
            let read = |p: &Path, max: u64| -> Result<Vec<u8>, String> {
                let m = std::fs::symlink_metadata(p)
                    .map_err(|e| format!("Cannot open {}: {e}", p.display()))?;
                if crate::safecopy::is_link(&m) || !m.is_file() {
                    return Err(format!("{} isn't a regular file.", p.display()));
                }
                if m.len() > max {
                    return Err(format!(
                        "{} is larger than the {} limit for catalog files.",
                        p.display(),
                        crate::download::human_size(max)
                    ));
                }
                std::fs::read(p).map_err(|e| e.to_string())
            };
            let mut sp = p.clone().into_os_string();
            sp.push(".sig");
            (
                read(&p, MANIFEST_MAX)?,
                read(Path::new(&sp), SIG_MAX).map_err(|_| {
                    "The catalog file needs its signature next to it (same name + .sig)."
                        .to_string()
                })?,
            )
        }
        _ => return Err("No catalog is set up.".into()),
    };
    let l = load_bytes(&bytes, &sig, &c.pubkey, &c.source)?;
    store.check_rollback(&c.pubkey, l.catalog_version)?;
    Ok(l)
}

#[cfg(test)]
mod tests;

// ---- command dispatcher (shared by the Tauri app and the test bridge) ----

static LOADED: std::sync::Mutex<Option<Loaded>> = std::sync::Mutex::new(None);

pub fn current() -> Option<Loaded> {
    LOADED.lock().unwrap_or_else(|p| p.into_inner()).clone()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TargetArgs {
    kind: String,
    #[serde(default)]
    path: String,
    #[serde(default)]
    host: String,
    #[serde(default)]
    port: Option<u16>,
    #[serde(default)]
    user: String,
    #[serde(default)]
    pass: String,
    #[serde(default)]
    drive: String,
}

fn target_from(
    v: &serde_json::Value,
    check_grant: &dyn Fn(&str) -> Result<PathBuf, String>,
) -> Result<target::Target, String> {
    let a: TargetArgs =
        serde_json::from_value(v.clone()).map_err(|e| format!("bad target: {e}"))?;
    match a.kind.as_str() {
        "local" => {
            let root = check_grant(&a.path)?;
            if !root.is_dir() {
                return Err("Pick a drive or folder as the target.".into());
            }
            Ok(target::Target::Local { root })
        }
        "console" => {
            if !crate::ftp::is_drive(&a.drive) {
                return Err("Pick a console drive (like Hdd1 or Usb0).".into());
            }
            Ok(target::Target::Console {
                con: crate::ftp_console(&a.host, a.port.unwrap_or(21), &a.user, &a.pass)?,
                drive: a.drive,
            })
        }
        _ => Err("Pick where to install (a drive on this PC or the console).".into()),
    }
}

fn entry_of<'a>(l: &'a Loaded, id: &str) -> Result<&'a Entry, String> {
    l.entries
        .iter()
        .find(|e| e.id == id)
        .ok_or_else(|| "That entry isn't in the loaded catalog. Refresh the catalog.".to_string())
}

static CATALOG_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
#[allow(dead_code)] // held in catalog tests to exercise catalog_lock_wait while blocked
pub(crate) fn catalog_lock() -> std::sync::MutexGuard<'static, ()> {
    CATALOG_LOCK.lock().unwrap_or_else(|p| p.into_inner())
}
pub const CATALOG_WAIT_MESSAGE: &str = "Waiting for another catalog install to finish…";

/// The catalog lock, waited for visibly and cancellably (R16 L2): while another job holds it the
/// job shows `CATALOG_WAIT_MESSAGE` and a Cancel ends the wait with `ERR_CANCELLED`.
pub(crate) fn catalog_lock_wait(
    progress: &mut dyn FnMut(u32, &str),
    cancelled: &dyn Fn() -> bool,
) -> Result<std::sync::MutexGuard<'static, ()>, String> {
    let mut told = false;
    loop {
        match CATALOG_LOCK.try_lock() {
            Ok(g) => return Ok(g),
            Err(std::sync::TryLockError::Poisoned(p)) => return Ok(p.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) => {}
        }
        if cancelled() {
            return Err(crate::archive::ERR_CANCELLED.into());
        }
        if !told {
            progress(0, CATALOG_WAIT_MESSAGE);
            told = true;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// `cmd`: config_get | config_set | refresh | current | states | preflight | install | uninstall | journal
pub fn call(
    ctx: &install::Ctx,
    cmd: &str,
    a: serde_json::Value,
    check_grant: &dyn Fn(&str) -> Result<PathBuf, String>,
    progress: &mut dyn FnMut(u32, &str),
    cancelled: &dyn Fn() -> bool,
) -> Result<serde_json::Value, String> {
    use serde_json::{json, to_value};
    let s = |k: &str| a[k].as_str().unwrap_or("").to_string();
    let loaded = || current().ok_or_else(|| "Load the catalog first (Refresh).".to_string());
    // One catalog-wide lock for every target (R15 M1): install/uninstall/preflight do a
    // read-modify-write of launch.ini and journal.json, so local targets need it too.
    // Order: CATALOG_LOCK first, then the FTP lock for console targets.
    let console_lock = |t: &target::Target,
                        progress: &mut dyn FnMut(u32, &str)|
     -> Result<
        (
            std::sync::MutexGuard<'static, ()>,
            Option<crate::FtpLockGuard>,
        ),
        String,
    > {
        let g = catalog_lock_wait(progress, cancelled)?;
        let f = if t.is_console() {
            Some(crate::ftp_lock("Mod catalog", cmd)?)
        } else {
            None
        };
        Ok((g, f))
    };
    match cmd {
        "config_get" => Ok(to_value(ctx.store.config()).unwrap()),
        "config_set" => {
            let c: Config = serde_json::from_value(a["config"].clone())
                .map_err(|e| format!("bad config: {e}"))?;
            if c.source == "local" {
                check_grant(c.local_file.trim())?;
            }
            let c = ctx.store.set_config(c)?;
            Ok(to_value(c).unwrap())
        }
        "refresh" => {
            let l = refresh(&ctx.store, check_grant, cancelled)?;
            activate(&l);
            *LOADED.lock().unwrap_or_else(|p| p.into_inner()) = Some(l.clone());
            Ok(to_value(l).unwrap())
        }
        "current" => Ok(current()
            .map(|l| to_value(l).unwrap())
            .unwrap_or(json!(null))),
        "states" => {
            let l = loaded()?;
            let t = target_from(&a["target"], check_grant)?;
            Ok(to_value(install::states(&ctx.store, &l, &t.key())?).unwrap())
        }
        "preflight" => {
            let l = loaded()?;
            let t = target_from(&a["target"], check_grant)?;
            let _g = console_lock(&t, &mut *progress)?;
            Ok(to_value(install::preflight(
                &ctx.store,
                &l,
                entry_of(&l, &s("id"))?,
                &t,
            ))
            .unwrap())
        }
        "install" => {
            let l = loaded()?;
            let t = target_from(&a["target"], check_grant)?;
            let opts: install::InstallOpts =
                serde_json::from_value(a["opts"].clone()).unwrap_or_default();
            let _g = console_lock(&t, &mut *progress)?;
            Ok(to_value(install::install(
                ctx,
                &l,
                &s("id"),
                &t,
                &opts,
                progress,
                cancelled,
            )?)
            .unwrap())
        }
        "uninstall" => {
            let l = current();
            let t = target_from(&a["target"], check_grant)?;
            let _g = console_lock(&t, &mut *progress)?;
            Ok(to_value(install::uninstall(ctx, l.as_ref(), &s("id"), &t, false)?).unwrap())
        }
        "journal" => Ok(to_value(ctx.store.journal()?).unwrap()),
        _ => Err(format!("unknown catalog command {cmd}")),
    }
}
