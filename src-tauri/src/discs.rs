//! Multi-disc games.
//!
//! Two jobs for this module:
//! * name hints (`Game (Disc 2).iso`) and "other discs next to this one" lookups, and
//! * a session registry that remembers which output folder the first disc of a game went to, so
//!   disc 2 joins it instead of getting a "-2" folder of its own.
//!
//! GOD discs merge by construction (one `<TitleID>` folder, one MediaID per disc); an extracted
//! XEX folder needs an overlay in which a later disc never replaces disc 1's default.xex/xbe.
//! The registry never holds paths from the page: folders are only ever ones Rust created itself
//! (`claim_dir`) during this run.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Kind {
    God,
    Xex,
}

/// Redump / dump names: `Game (Disc 2).iso`, `Game-disk1.iso`, `Game CD2.iso`.
pub fn disc_hint_from_name(path: &Path) -> Option<u8> {
    let stem = path
        .file_stem()
        .or_else(|| path.file_name())
        .map(|n| n.to_string_lossy().to_ascii_lowercase())?;
    for label in ["disc", "disk", "dvd", "(cd", " cd", "-cd", "_cd"] {
        if let Some(n) = digit_after_label(&stem, label) {
            return Some(n);
        }
    }
    None
}

fn digit_after_label(haystack: &str, label: &str) -> Option<u8> {
    let mut start = 0;
    while let Some(pos) = haystack[start..].find(label) {
        let abs = start + pos + label.len();
        let rest =
            haystack[abs..].trim_start_matches([' ', '-', '_', '.', '#']);
        if let Some(ch) = rest.chars().next() {
            if let Some(d) = ch.to_digit(10) {
                if (1..=9).contains(&d) {
                    return Some(d as u8);
                }
            }
        }
        start = abs;
    }
    None
}

/// The file name with the disc marker removed, lower-cased: `Halo (Disc 2).iso` and `halo disc1.iso` both give `halo`.
pub fn stem_without_disc(path: &Path) -> String {
    let mut s = path
        .file_stem()
        .map(|n| n.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    for n in 1..=9u8 {
        for pat in [
            format!("(disc {n})"),
            format!("(disk {n})"),
            format!("(dvd {n})"),
            format!("(cd {n})"),
            format!("(disc{n})"),
            format!("(disk{n})"),
            format!("-disc{n}"),
            format!("_disc{n}"),
            format!("-disk{n}"),
            format!("_disk{n}"),
            format!(" disc {n}"),
            format!(" disk {n}"),
            format!(" disc{n}"),
            format!(" disk{n}"),
            format!(" cd{n}"),
            format!(" cd {n}"),
            format!("-d{n}"),
            format!("_d{n}"),
        ] {
            s = s.replace(&pat, "");
        }
    }
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn is_iso_path(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str(),
        "iso" | "000" | "bin"
    )
}

/// Is this ISO (by its own header numbers or by its file name) one disc of several?
pub fn looks_multi_disc(path: &Path, disc_number: u8, disc_count: u8) -> bool {
    disc_count > 1 || disc_number > 1 || disc_hint_from_name(path).is_some()
}

/// Disc number to use for ordering: the header's, else the name's, else 1.
pub fn disc_number_of(path: &Path, header: u8) -> u8 {
    if header > 0 {
        header
    } else {
        disc_hint_from_name(path).filter(|n| *n > 0).unwrap_or(1)
    }
}

/// Other discs of the same game in the folder next to `path`: same title ID, or the same file name
/// apart from the disc marker. Sorted by disc number. `path` itself is not included.
pub fn companions(path: &Path) -> Result<Vec<PathBuf>, String> {
    let parent = path.parent().ok_or("That ISO has no folder.")?;
    let me = crate::god::iso_info(path).ok();
    let siblings: Vec<PathBuf> = std::fs::read_dir(parent)
        .map_err(|e| format!("Cannot read {}: {e}", parent.display()))?
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .map(|e| e.path())
        .filter(|p| is_iso_path(p) && p != path)
        .collect();
    let inspect = siblings.len() <= 32;
    let mut found: Vec<(u8, PathBuf)> = Vec::new();
    for sib in siblings {
        let info = if inspect { crate::god::iso_info(&sib).ok() } else { None };
        if same_game(path, me.as_ref(), &sib, info.as_ref()) {
            let n = disc_number_of(&sib, info.map(|i| i.disc_number).unwrap_or(0));
            found.push((n, sib));
        }
    }
    found.sort();
    Ok(found.into_iter().map(|(_, p)| p).collect())
}

// ---------- missing-disc check ----------

/// Where one disc of a multi-disc game is.
#[derive(Debug, Clone, serde::Serialize, PartialEq)]
pub struct DiscSlot {
    pub disc: u8,
    /// "this" | "queued" | "next_to_it" | "extract_queue" | "converted" | "missing"
    pub state: String,
    pub path: Option<String>,
}

/// Every disc of the game `path` belongs to, and where each one is.
#[derive(Debug, Clone, serde::Serialize, PartialEq)]
pub struct DiscStatus {
    pub title_id: Option<String>,
    pub disc: u8,
    pub count: u8,
    pub slots: Vec<DiscSlot>,
    pub missing: Vec<u8>,
    /// "Disc 1 of 2: queued · Disc 2 of 2: this one", empty for a single-disc game.
    pub line: String,
}

/// Are `a` and `b` discs of the same game? Same title ID (and one looks multi-disc), or the same
/// file name apart from the disc marker when the title IDs don't contradict it (R15 M3).
fn same_game(
    a: &Path,
    ai: Option<&crate::god::IsoInfo>,
    b: &Path,
    bi: Option<&crate::god::IsoInfo>,
) -> bool {
    if let (Some(x), Some(y)) = (ai, bi) {
        if x.title_id != y.title_id {
            return false;
        }
        if looks_multi_disc(a, x.disc_number, x.disc_count) || looks_multi_disc(b, y.disc_number, y.disc_count) {
            return true;
        }
    }
    let s = stem_without_disc(a);
    !s.is_empty() && s == stem_without_disc(b)
}

/// Disc numbers of this game already converted during this run (session registry).
pub fn converted_discs(title_id: Option<&str>, path: &Path) -> Vec<u8> {
    let keys = keys_for(title_id, path);
    let title = title_of(&keys);
    let reg = lock();
    let mut v: Vec<u8> = reg
        .iter()
        .filter(|((_, _, k), set)| {
            keys.contains(k)
                && set.dir.is_dir()
                && !matches!((&set.title, &title), (Some(a), Some(b)) if a != b)
        })
        .flat_map(|(_, set)| set.discs.iter().copied())
        .collect();
    v.sort();
    v.dedup();
    v
}

/// Disc numbers of GOD packages already in `<dest>/<TitleID>/00007000` (read from each CON
/// header, offset 0x366; at most 64 headers, 0x400 bytes each, read-only).
pub fn converted_in_folder(dest: &Path, title_id: &str) -> Vec<u8> {
    use std::io::Read;
    if title_id.len() != 8 || !title_id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return vec![];
    }
    let dir = dest.join(title_id.to_ascii_uppercase()).join("00007000");
    let Ok(rd) = std::fs::read_dir(&dir) else { return vec![] };
    let mut v = vec![];
    for e in rd.flatten().take(64) {
        let Ok(ft) = e.file_type() else { continue };
        if !ft.is_file() || e.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let mut buf = [0u8; 0x368];
        let ok = std::fs::File::open(e.path()).and_then(|mut f| f.read_exact(&mut buf)).is_ok();
        // R22 L(c): same TitleID (header 0x360) and a disc number 1..=9 only.
        let tid = u32::from_str_radix(title_id, 16).unwrap_or(0).to_be_bytes();
        if ok && buf.starts_with(b"LIVE") && buf[0x360..0x364] == tid && (1..=9).contains(&buf[0x366].max(1)) {
            v.push(buf[0x366].max(1));
        }
    }
    v.sort();
    v.dedup();
    v
}

/// Missing-disc check for one ISO. `dest`: the GOD output folder, scanned for discs already there. `queued`: other ISOs already waiting to convert (GOD panel or
/// auto queue); `extracting`: archive names still in the extract queue (matched by name only).
pub fn disc_status(
    path: &Path,
    queued: &[PathBuf],
    extracting: &[String],
    dest: Option<&Path>,
) -> DiscStatus {
    let me = crate::god::iso_info(path).ok();
    let my_disc = me
        .as_ref()
        .map(|i| i.disc_number.max(1))
        .unwrap_or_else(|| disc_number_of(path, 0));
    let mut count = me.as_ref().map(|i| i.disc_count).unwrap_or(1).max(1);
    let multi = me
        .as_ref()
        .map(|i| looks_multi_disc(path, i.disc_number, i.disc_count))
        .unwrap_or_else(|| disc_hint_from_name(path).is_some());
    let title_id = me.as_ref().map(|i| i.title_id.clone());
    let mut found: Vec<(u8, &'static str, Option<String>)> = vec![(my_disc, "this", Some(path.display().to_string()))];
    if multi {
        let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        let mine = canon(path);
        let add = |p: &Path, state: &'static str, found: &mut Vec<(u8, &'static str, Option<String>)>, count: &mut u8| {
            if canon(p) == mine {
                return;
            }
            let info = crate::god::iso_info(p).ok();
            if !same_game(path, me.as_ref(), p, info.as_ref()) {
                return;
            }
            let n = info.as_ref().map(|i| i.disc_number.max(1)).unwrap_or_else(|| disc_number_of(p, 0));
            if let Some(i) = &info {
                *count = (*count).max(i.disc_count);
            }
            found.push((n, state, Some(p.display().to_string())));
        };
        for q in queued {
            add(q, "queued", &mut found, &mut count);
        }
        for c in companions(path).unwrap_or_default() {
            add(&c, "next_to_it", &mut found, &mut count);
        }
        let my_stem = stem_without_disc(path);
        for name in extracting {
            let p = Path::new(name);
            if let Some(n) = disc_hint_from_name(p) {
                if !my_stem.is_empty() && stem_without_disc(p) == my_stem {
                    found.push((n, "extract_queue", Some(name.clone())));
                }
            }
        }
        for n in converted_discs(title_id.as_deref(), path) {
            found.push((n, "converted", None));
        }
        if let (Some(d), Some(t)) = (dest, title_id.as_deref()) {
            for n in converted_in_folder(d, t) {
                found.push((n, "converted", Some(d.join(t).display().to_string())));
            }
        }
    }
    count = count.max(found.iter().map(|f| f.0).max().unwrap_or(1));
    // Best state per disc: this > queued > next_to_it > extract_queue > converted.
    let rank = |s: &str| ["this", "queued", "next_to_it", "extract_queue", "converted"].iter().position(|x| *x == s).unwrap_or(9);
    let mut slots = vec![];
    let mut missing = vec![];
    for d in 1..=count {
        let best = found.iter().filter(|f| f.0 == d).min_by_key(|f| rank(f.1));
        match best {
            Some((_, st, p)) => slots.push(DiscSlot { disc: d, state: st.to_string(), path: p.clone() }),
            None => {
                missing.push(d);
                slots.push(DiscSlot { disc: d, state: "missing".into(), path: None });
            }
        }
    }
    let word = |s: &str| match s {
        "this" => "this one",
        "queued" => "queued",
        "next_to_it" => "found next to it",
        "extract_queue" => "in the extract queue",
        "converted" => "already converted",
        _ => "MISSING",
    };
    let line = if count > 1 {
        slots
            .iter()
            .map(|s| format!("Disc {} of {count}: {}", s.disc, word(&s.state)))
            .collect::<Vec<_>>()
            .join(" · ")
    } else {
        String::new()
    };
    DiscStatus { title_id, disc: my_disc, count, slots, missing, line }
}

// ---------- session registry of output folders ----------

const TTL: Duration = Duration::from_secs(12 * 3600);

struct Set {
    dir: PathBuf,
    discs: Vec<u8>,
    /// Disc whose default.xex/xbe is in the folder (0 = none yet). Lower disc numbers may replace it.
    exe_disc: u8,
    /// Title ID of the discs in this folder, when one was readable. A disc with a different title
    /// ID never joins, even if its file name matches (R15 M3).
    title: Option<String>,
    born: Instant,
}

type Key = (PathBuf, Kind, String);

fn registry() -> &'static Mutex<HashMap<Key, Set>> {
    static R: OnceLock<Mutex<HashMap<Key, Set>>> = OnceLock::new();
    R.get_or_init(Default::default)
}

fn lock() -> std::sync::MutexGuard<'static, HashMap<Key, Set>> {
    registry().lock().unwrap_or_else(|e| e.into_inner())
}

fn parent_key(parent: &Path) -> PathBuf {
    std::fs::canonicalize(parent).unwrap_or_else(|_| parent.to_path_buf())
}

/// Registry keys for one ISO: its title ID (when readable) and its file name without the disc marker.
pub fn keys_for(title_id: Option<&str>, path: &Path) -> Vec<String> {
    let mut k = Vec::new();
    if let Some(t) = title_id.filter(|t| !t.is_empty()) {
        k.push(format!("id:{t}"));
    }
    let s = stem_without_disc(path);
    if !s.is_empty() {
        k.push(format!("stem:{s}"));
    }
    k
}

fn title_of(keys: &[String]) -> Option<String> {
    keys.iter()
        .find_map(|k| k.strip_prefix("id:"))
        .map(|t| t.to_ascii_uppercase())
}

/// What `claim` decided.
pub struct Claim {
    pub dir: PathBuf,
    /// true: an earlier disc's folder is being joined; false: a fresh folder.
    pub joined: bool,
}

/// Atomically decide where disc `disc` of a multi-disc game goes. If an earlier disc of the same
/// game (matching title ID or name) put its output in `parent` during this run and that folder
/// still exists, the new disc joins it; otherwise `make_dir` creates a fresh folder and the set is
/// remembered. A disc number that is already in the set starts a new folder (a re-run of the same disc).
pub fn claim(
    parent: &Path,
    kind: Kind,
    keys: &[String],
    disc: u8,
    make_dir: impl FnOnce() -> Result<PathBuf, String>,
) -> Result<Claim, String> {
    let pk = parent_key(parent);
    let mut reg = lock();
    reg.retain(|_, s| s.born.elapsed() < TTL);
    let title = title_of(keys);
    for k in keys {
        let id = (pk.clone(), kind, k.clone());
        let usable = reg
            .get(&id)
            .map(|s| {
                s.dir.is_dir()
                    && !s.discs.contains(&disc)
                    && !matches!((&s.title, &title), (Some(a), Some(b)) if a != b)
            })
            .unwrap_or(false);
        if usable {
            let dir = {
                let s = reg.get_mut(&id).unwrap();
                s.discs.push(disc);
                if s.title.is_none() {
                    s.title = title.clone();
                }
                s.dir.clone()
            };
            for other in keys {
                let oid = (pk.clone(), kind, other.clone());
                if oid != id {
                    if let Some(o) = reg.get_mut(&oid) {
                        if o.dir == dir && !o.discs.contains(&disc) {
                            o.discs.push(disc);
                        }
                    }
                }
            }
            return Ok(Claim { dir, joined: true });
        }
    }
    let dir = make_dir()?;
    for k in keys {
        reg.insert(
            (pk.clone(), kind, k.clone()),
            Set {
                dir: dir.clone(),
                discs: vec![disc],
                exe_disc: 0,
                title: title.clone(),
                born: Instant::now(),
            },
        );
    }
    Ok(Claim { dir, joined: false })
}

/// The job failed: forget `disc` so a retry can join the folder again.
pub fn release(parent: &Path, kind: Kind, keys: &[String], disc: u8) {
    let pk = parent_key(parent);
    let mut reg = lock();
    for k in keys {
        if let Some(s) = reg.get_mut(&(pk.clone(), kind, k.clone())) {
            s.discs.retain(|d| *d != disc);
        }
    }
}

/// The disc whose executable currently sits in the set's folder (0 = none).
pub fn exe_disc(parent: &Path, kind: Kind, keys: &[String]) -> u8 {
    let pk = parent_key(parent);
    let reg = lock();
    keys.iter()
        .filter_map(|k| reg.get(&(pk.clone(), kind, k.clone())))
        .map(|s| s.exe_disc)
        .find(|d| *d > 0)
        .unwrap_or(0)
}

pub fn set_exe_disc(parent: &Path, kind: Kind, keys: &[String], disc: u8) {
    let pk = parent_key(parent);
    let mut reg = lock();
    for k in keys {
        if let Some(s) = reg.get_mut(&(pk.clone(), kind, k.clone())) {
            s.exe_disc = disc;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_hints() {
        let h = |n: &str| disc_hint_from_name(Path::new(n));
        assert_eq!(h("Game (Disc 2).iso"), Some(2));
        assert_eq!(h("Game-disk1.iso"), Some(1));
        assert_eq!(h("Game DVD 3.iso"), Some(3));
        assert_eq!(h("Game (CD 2).iso"), Some(2));
        assert_eq!(h("Game disc0.iso"), None);
        assert_eq!(h("Discworld.iso"), None);
        assert_eq!(h("Halo 3.iso"), None);
    }

    #[test]
    fn stems_ignore_the_disc_marker() {
        let s = |n: &str| stem_without_disc(Path::new(n));
        assert_eq!(s("Halo Wars (Disc 1).iso"), "halo wars");
        assert_eq!(s("Halo Wars (Disc 2).iso"), "halo wars");
        assert_eq!(s("halo wars disc2.iso"), "halo wars");
        assert_eq!(s("Halo Wars.iso"), "halo wars");
        assert_eq!(s("Game_disk2.iso"), "game");
    }

    #[test]
    fn multi_disc_detection_and_ordering() {
        assert!(looks_multi_disc(Path::new("a.iso"), 1, 2));
        assert!(looks_multi_disc(Path::new("a.iso"), 2, 1));
        assert!(looks_multi_disc(Path::new("a (Disc 2).iso"), 1, 1));
        assert!(!looks_multi_disc(Path::new("a.iso"), 1, 1));
        assert_eq!(disc_number_of(Path::new("a (Disc 2).iso"), 0), 2);
        assert_eq!(disc_number_of(Path::new("a.iso"), 0), 1);
        assert_eq!(disc_number_of(Path::new("a (Disc 2).iso"), 3), 3);
    }

    fn fresh(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "unpack-discs-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn second_disc_joins_the_first_discs_folder() {
        let parent = fresh("join");
        let keys = keys_for(Some("4D5307E6"), Path::new("Game (Disc 1).iso"));
        let make = |n: &str| {
            let d = parent.join(n);
            std::fs::create_dir_all(&d)
                .map(|_| d)
                .map_err(|e| e.to_string())
        };
        let a = claim(&parent, Kind::God, &keys, 1, || make("Game (GOD)")).unwrap();
        assert!(!a.joined);
        let b = claim(&parent, Kind::God, &keys, 2, || {
            panic!("disc 2 must join, not create")
        })
        .unwrap();
        assert!(b.joined);
        assert_eq!(a.dir, b.dir);
        // the same disc again is a new run: fresh folder
        let c = claim(&parent, Kind::God, &keys, 2, || make("Game (GOD)-2")).unwrap();
        assert!(!c.joined);
        assert_ne!(a.dir, c.dir);
        // a failed job gives its slot back
        release(&parent, Kind::God, &keys, 2);
        // the other kind never mixes in
        let d = claim(&parent, Kind::Xex, &keys, 2, || make("Game")).unwrap();
        assert!(!d.joined);
        let _ = std::fs::remove_dir_all(&parent);
    }

    #[test]
    fn disc_without_a_title_id_joins_by_file_name() {
        let parent = fresh("stem");
        let first = keys_for(Some("0000ABCD"), Path::new("Zed (Disc 1).iso"));
        let second = keys_for(None, Path::new("Zed (Disc 2).iso"));
        let make = || {
            let d = parent.join("Zed");
            std::fs::create_dir_all(&d)
                .map(|_| d)
                .map_err(|e| e.to_string())
        };
        let a = claim(&parent, Kind::Xex, &first, 1, make).unwrap();
        let b = claim(&parent, Kind::Xex, &second, 2, || {
            panic!("should join by name")
        })
        .unwrap();
        assert!(b.joined && a.dir == b.dir);
        set_exe_disc(&parent, Kind::Xex, &first, 1);
        assert_eq!(exe_disc(&parent, Kind::Xex, &second), 1);
        let _ = std::fs::remove_dir_all(&parent);
    }

    #[test]
    fn r15_m3_different_title_ids_never_join_by_name() {
        let parent = fresh("mix");
        let halo = keys_for(Some("4D5307E6"), Path::new("Mix (Disc 1).iso"));
        let quake = keys_for(Some("415607D2"), Path::new("Mix (Disc 2).iso"));
        let mk = |n: &str| {
            let d = parent.join(n);
            std::fs::create_dir_all(&d)
                .map(|_| d)
                .map_err(|e| e.to_string())
        };
        let a = claim(&parent, Kind::Xex, &halo, 1, || mk("Halo 3")).unwrap();
        let b = claim(&parent, Kind::Xex, &quake, 2, || mk("Quake 4")).unwrap();
        assert!(!b.joined && a.dir != b.dir);
        // a disc with no readable title ID may still join by name
        let none = keys_for(None, Path::new("Mix (Disc 3).iso"));
        assert!(
            claim(&parent, Kind::Xex, &none, 3, || panic!("joins"))
                .unwrap()
                .joined
        );
        let _ = std::fs::remove_dir_all(&parent);
    }

    #[test]
    fn a_removed_folder_is_not_joined() {
        let parent = fresh("gone");
        let keys = keys_for(Some("0000BEEF"), Path::new("Q (Disc 1).iso"));
        let d = parent.join("Q (GOD)");
        std::fs::create_dir_all(&d).unwrap();
        claim(&parent, Kind::God, &keys, 1, || Ok(d.clone())).unwrap();
        std::fs::remove_dir_all(&d).unwrap();
        let again = claim(&parent, Kind::God, &keys, 2, || {
            std::fs::create_dir_all(&d).unwrap();
            Ok(d.clone())
        })
        .unwrap();
        assert!(!again.joined);
        let _ = std::fs::remove_dir_all(&parent);
    }

    // ---- missing-disc check ----
    fn iso(dir: &Path, name: &str, tid: u32, disc: (u8, u8)) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let p = dir.join(name);
        crate::god::tests::make_iso_files(&p, tid, 0x1000 + disc.0 as u32, disc, &[("default.xex", name.as_bytes())]);
        p
    }
    fn states(st: &DiscStatus) -> Vec<(u8, String)> {
        st.slots.iter().map(|s| (s.disc, s.state.clone())).collect()
    }

    #[test]
    fn status_header_only_disc_numbers() {
        let d = fresh("st-header");
        let a = iso(&d, "Alpha.iso", 0x5A5A0001, (1, 2));
        iso(&d, "Bravo.iso", 0x5A5A0001, (2, 2)); // no disc marker in either name
        let st = disc_status(&a, &[], &[], None);
        assert_eq!(st.count, 2);
        assert_eq!(states(&st), vec![(1, "this".into()), (2, "next_to_it".into())]);
        assert!(st.missing.is_empty());
        assert_eq!(st.line, "Disc 1 of 2: this one · Disc 2 of 2: found next to it");
    }

    #[test]
    fn status_name_only_markers() {
        let d = fresh("st-name");
        let two = iso(&d, "Gamma (Disc 2).iso", 0x5A5A0002, (1, 1)); // header says 1 of 1
        let st = disc_status(&two, &[], &[], None);
        assert_eq!(st.disc, 2);
        assert_eq!(st.missing, vec![1]);
        assert!(st.line.contains("Disc 1 of 2: MISSING"), "{}", st.line);
        iso(&d, "Gamma (Disc 1).iso", 0x5A5A0002, (1, 1));
        let st = disc_status(&two, &[], &[], None);
        assert!(st.missing.is_empty(), "{st:?}");
    }

    #[test]
    fn status_disc2_alone_present_absent_converted() {
        let d = fresh("st-d2");
        let two = iso(&d, "Delta (Disc 2).iso", 0x5A5A0003, (2, 2));
        // absent
        assert_eq!(disc_status(&two, &[], &[], None).missing, vec![1]);
        // in the extract queue (archive name only)
        let st = disc_status(&two, &[], &["Delta (Disc 1).7z".into()], None);
        assert_eq!(states(&st)[0], (1, "extract_queue".into()));
        // already converted this session
        let out = d.join("out");
        std::fs::create_dir_all(&out).unwrap();
        let o2 = out.clone();
        claim(&d, Kind::God, &keys_for(Some("5A5A0003"), Path::new("Delta (Disc 1).iso")), 1, move || Ok(o2)).unwrap();
        let st = disc_status(&two, &[], &[], None);
        assert_eq!(states(&st)[0], (1, "converted".into()));
        assert!(st.missing.is_empty());
        // present next to it, and queued elsewhere
        let other = fresh("st-d2b");
        let one = iso(&other, "Delta (Disc 1).iso", 0x5A5A0003, (1, 2));
        let st = disc_status(&two, &[one], &[], None);
        assert_eq!(states(&st)[0], (1, "queued".into()));
    }

    #[test]
    fn status_disc1_already_in_the_titleid_folder() {
        let d = fresh("st-tid");
        let two = iso(&d, "Hotel (Disc 2).iso", 0x5A5A0009, (2, 2));
        let one = iso(&d.join("src"), "Hotel (Disc 1).iso", 0x5A5A0009, (1, 2));
        let out = d.join("god");
        std::fs::create_dir_all(&out).unwrap();
        crate::god::convert(&one, &out, None, &crate::archive::Jobs::default(), "st-tid", &mut |_, _| {}).unwrap();
        let st = disc_status(&two, &[], &[], Some(&out));
        assert_eq!(states(&st)[0], (1, "converted".into()), "{st:?}");
        assert!(st.missing.is_empty());
        assert_eq!(disc_status(&two, &[], &[], None).missing, vec![1]);
    }

    #[test]
    fn status_three_disc_game() {
        let d = fresh("st-three");
        iso(&d, "Echo (Disc 1).iso", 0x5A5A0004, (1, 3));
        let two = iso(&d, "Echo (Disc 2).iso", 0x5A5A0004, (2, 3));
        let st = disc_status(&two, &[], &[], None);
        assert_eq!(st.count, 3);
        assert_eq!(st.missing, vec![3]);
        assert!(st.line.ends_with("Disc 3 of 3: MISSING"), "{}", st.line);
        iso(&d, "Echo (Disc 3).iso", 0x5A5A0004, (3, 3));
        assert!(disc_status(&two, &[], &[], None).missing.is_empty());
    }

    #[test]
    fn status_similar_names_different_games_never_group() {
        let d = fresh("st-similar");
        let a = iso(&d, "Foxtrot (Disc 2).iso", 0x5A5A0005, (2, 2));
        iso(&d, "Foxtrot Legends (Disc 1).iso", 0x5A5A0006, (1, 2)); // similar name, other game
        iso(&d, "Foxtrot (Disc 1) [alt].iso", 0x5A5A0007, (1, 2)); // other title ID
        let st = disc_status(&a, &[], &["Foxtrot Legends (Disc 1).7z".into()], None);
        assert_eq!(st.missing, vec![1], "{st:?}");
        assert!(companions(&a).unwrap().is_empty());
    }

    #[test]
    fn status_single_disc_has_no_line() {
        let d = fresh("st-single");
        let a = iso(&d, "Golf.iso", 0x5A5A0008, (1, 1));
        let st = disc_status(&a, &[], &[], None);
        assert_eq!((st.count, st.line.as_str()), (1, ""));
    }

    /// R22 L(c): only packages of the same TitleID with a sane disc number count as converted.
    #[test]
    fn r22_lc_converted_in_folder_checks_title_and_range() {
        let td = crate::testutil::TmpDir::new("discs-r22lc");
        let dir = td.path().join("4D5307E6").join("00007000");
        std::fs::create_dir_all(&dir).unwrap();
        let con = |name: &str, tid: u32, disc: u8| {
            let mut b = vec![0u8; 0x400];
            b[..4].copy_from_slice(b"LIVE");
            b[0x360..0x364].copy_from_slice(&tid.to_be_bytes());
            b[0x366] = disc;
            std::fs::write(dir.join(name), b).unwrap();
        };
        con("AAAA", 0x4D53_07E6, 2);
        con("OTHERTID", 0x1234_5678, 1);
        con("BIG", 0x4D53_07E6, 250);
        assert_eq!(converted_in_folder(td.path(), "4D5307E6"), vec![2]);
    }
}
