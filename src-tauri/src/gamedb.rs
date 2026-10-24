//! Offline Xbox 360 game reference: TitleID -> name (the ~4,900-entry MIT-licensed list that
//! ships with iso2god-rs) plus a hand-written table of official names and common aliases for
//! popular titles. Used to name a mod's game and to guess it from file/folder/archive names.
//! No network lookups, and no data about specific mods.

use serde::Serialize;

/// (TitleID, official name, aliases). Aliases are matched as whole words after normalising.
pub const ALIASES: &[(u32, &str, &[&str])] = &[
    (
        0x415607E6,
        "Call of Duty 4: Modern Warfare",
        &["cod4", "cod 4", "mw1", "cod4 mw", "modern warfare 1"],
    ),
    (
        0x4156081C,
        "Call of Duty: World at War",
        &["waw", "cod5", "cod waw", "world at war"],
    ),
    (
        0x41560817,
        "Call of Duty: Modern Warfare 2",
        &["mw2", "cod6", "codmw2", "modern warfare 2"],
    ),
    (
        0x41560855,
        "Call of Duty: Black Ops",
        &["bo1", "black ops 1", "cod7", "codbo1"],
    ),
    (
        0x415608CB,
        "Call of Duty: Modern Warfare 3",
        &["mw3", "cod8", "modern warfare 3"],
    ),
    (
        0x415608C3,
        "Call of Duty: Black Ops II",
        &["bo2", "black ops 2", "cod9", "codbo2"],
    ),
    (
        0x415608FC,
        "Call of Duty: Ghosts",
        &["cod ghosts", "ghosts"],
    ),
    (
        0x41560914,
        "Call of Duty: Advanced Warfare",
        &["aw", "cod aw", "advanced warfare"],
    ),
    (
        0x4156091D,
        "Call of Duty: Black Ops III",
        &["bo3", "black ops 3", "codbo3"],
    ),
    (
        0x545407F2,
        "Grand Theft Auto IV",
        &["gta iv", "gta4", "gtaiv", "gta 4"],
    ),
    (
        0x545408A7,
        "Grand Theft Auto V",
        &["gta v", "gta5", "gtav", "gta 5"],
    ),
    (0x5454082B, "Red Dead Redemption", &["rdr", "rdr1"]),
    (0x4D5307E6, "Halo 3", &["h3"]),
    (0x4D530877, "Halo 3: ODST", &["odst", "h3 odst"]),
    (0x4D53085B, "Halo: Reach", &["reach", "halo reach"]),
    (
        0x4D5309B1,
        "Halo: Combat Evolved Anniversary",
        &["hcea", "halo ce", "halo 1"],
    ),
    (0x4D530919, "Halo 4", &["h4"]),
    (0x4D530808, "Halo Wars", &[]),
    (0x4D5307D5, "Gears of War", &["gow1", "gears 1"]),
    (0x4D53082D, "Gears of War 2", &["gow2", "gears 2"]),
    (0x4D5308AB, "Gears of War 3", &["gow3", "gears 3"]),
    (0x4D530A26, "Gears of War: Judgment", &["gears judgment"]),
    (
        0x584111F7,
        "Minecraft: Xbox 360 Edition",
        &["minecraft", "mc360"],
    ),
    (0x545407E7, "Borderlands", &["bl1"]),
    (0x5454087C, "Borderlands 2", &["bl2"]),
    (
        0x545408B4,
        "Borderlands: The Pre-Sequel",
        &["tps", "pre sequel"],
    ),
    (0x415608F8, "Destiny", &[]),
    (0x425307D5, "Fallout 3", &["fo3"]),
    (0x425307E0, "Fallout: New Vegas", &["fnv", "new vegas"]),
    (0x45410950, "Battlefield 3", &["bf3"]),
    (0x454109BA, "Battlefield 4", &["bf4"]),
    (0x454108D4, "Left 4 Dead 2", &["l4d2"]),
    (0x45410912, "Portal 2", &[]),
    (0x454108E6, "Skate 3", &[]),
    (
        0x5451086D,
        "Saints Row: The Third",
        &["sr3", "saints row 3"],
    ),
    (0x4D5309C9, "Forza Horizon", &["fh1"]),
    (0x4D530AA4, "Forza Horizon 2", &["fh2"]),
    (0x4D530910, "Forza Motorsport 4", &["fm4"]),
    (
        0x425307E6,
        "The Elder Scrolls V: Skyrim",
        &["skyrim", "tes5"],
    ),
    (
        0x555308C2,
        "Assassin's Creed IV: Black Flag",
        &["ac4", "black flag"],
    ),
];

/// Extra aliases from a verified mod catalog: (TitleID, name, aliases). Replaced on each load.
static EXTRA: std::sync::RwLock<Vec<(u32, String, Vec<String>)>> =
    std::sync::RwLock::new(Vec::new());
pub fn set_extra_aliases(v: Vec<(u32, String, Vec<String>)>) {
    *EXTRA.write().unwrap_or_else(|p| p.into_inner()) = v;
}
pub fn extra_aliases() -> Vec<(u32, String, Vec<String>)> {
    EXTRA.read().unwrap_or_else(|p| p.into_inner()).clone()
}

pub fn name(tid: u32) -> Option<String> {
    ALIASES
        .iter()
        .find(|a| a.0 == tid)
        .map(|a| a.1.to_string())
        .or_else(|| iso2god::game_list::find_title_by_id(tid))
}

/// lowercase words; roman numerals II..V -> digits; "&" -> "and"
pub fn words(s: &str) -> Vec<String> {
    let mut out = vec![];
    let lower = s.to_lowercase().replace('&', " and ");
    // split camel-ish joins like "BO2_Menu" -> "bo2 menu"; keep letter+digit tokens together
    for w in lower.split(|c: char| !c.is_alphanumeric()) {
        if w.is_empty() {
            continue;
        }
        out.push(
            match w {
                "ii" => "2",
                "iii" => "3",
                "iv" => "4",
                "v" => "5",
                "vi" => "6",
                _ => w,
            }
            .to_string(),
        );
    }
    out
}

/// Whole-word sequence match that isn't the prefix of a sequel ("black ops" in "black ops 2").
fn contains_seq(hay: &[String], needle: &[String]) -> bool {
    let n = needle.len();
    n > 0
        && n <= hay.len()
        && (0..=hay.len() - n).any(|i| {
            hay[i..i + n] == *needle
                && hay.get(i + n).map_or(true, |nx| {
                    nx.parse::<u32>().is_err()
                        || needle.last().map_or(false, |l| l.parse::<u32>().is_ok())
                })
        })
}
/// Score for an exact name hit: one-word names ("Black", "Prey") are weak evidence.
fn exact_score(nw: usize) -> u32 {
    if nw <= 1 {
        70
    } else {
        86 + nw.min(10) as u32
    }
}

#[derive(Debug, Serialize, Clone, PartialEq)]
pub struct Guess {
    pub title_id: String,
    pub name: String,
    /// 0..=100
    pub confidence: u32,
    pub reason: String,
}

const STOP: &[&str] = &[
    "the", "of", "and", "a", "edition", "game", "xbox", "360", "x360", "menu", "mod", "mods",
    "plugin", "xex", "con", "live", "pirs", "dlc", "tu", "rgh", "jtag", "v1", "v2", "final", "by",
];

/// Rank games for free text (file name, folder, archive name). Best first, at most `max`.
pub fn guess(texts: &[&str], max: usize) -> Vec<Guess> {
    let hay: Vec<String> = texts.iter().flat_map(|t| words(t)).collect();
    if hay.is_empty() {
        return vec![];
    }
    let mut best: std::collections::HashMap<u32, (u32, String, String)> = Default::default();
    let mut put = |tid: u32, score: u32, name: String, why: String| {
        let e = best.entry(tid).or_insert((0, name.clone(), String::new()));
        if score > e.0 {
            *e = (score, name, why);
        }
    };
    for (tid, nm, aliases) in ALIASES {
        for a in aliases.iter() {
            let aw = words(a);
            if contains_seq(&hay, &aw) {
                // short aliases ("aw", "h3") are weaker evidence than "black ops 2"
                let s = if a.len() <= 2 { 72 } else { 92 };
                put(
                    *tid,
                    s + aw.len() as u32,
                    nm.to_string(),
                    format!("matched “{a}”"),
                );
            }
        }
        {
            let nw = words(nm);
            if contains_seq(&hay, &nw) {
                put(
                    *tid,
                    exact_score(nw.len()) + 2,
                    nm.to_string(),
                    "full name".into(),
                );
            }
        }
    }
    // Catalog aliases (verified catalog only): same scoring as the built-in alias table.
    for (tid, nm, aliases) in extra_aliases() {
        let nm = name(tid).unwrap_or(nm);
        for a in &aliases {
            let aw = words(a);
            if contains_seq(&hay, &aw) {
                let s = if a.len() <= 2 { 72 } else { 92 };
                put(
                    tid,
                    s + aw.len() as u32,
                    nm.clone(),
                    format!("catalog alias “{a}”"),
                );
            }
        }
    }
    // Whole list: all significant words of the name appear in order-free form.
    let hs: std::collections::HashSet<&str> = hay.iter().map(|s| s.as_str()).collect();
    for (tid, nm) in iso2god::game_list::all() {
        let nw: Vec<String> = words(nm)
            .into_iter()
            .filter(|w| !STOP.contains(&w.as_str()))
            .collect();
        if nw.is_empty() || nw.iter().all(|w| w.len() < 3 && w.parse::<u32>().is_err()) {
            continue;
        }
        let hit = nw.iter().filter(|w| hs.contains(w.as_str())).count();
        if hit == 0 {
            continue;
        }
        let cover = hit * 100 / nw.len();
        if cover < 60 {
            continue;
        }
        let exact = contains_seq(&hay, &words(nm));
        let s = if exact {
            exact_score(words(nm).len())
        } else {
            (cover as u32 * 85 / 100).min(85) + nw.len().min(4) as u32
        }; // longer names = more specific
        let canon = name(*tid).unwrap_or_else(|| nm.to_string());
        put(
            *tid,
            s.min(99),
            canon,
            if exact {
                "full name".into()
            } else {
                format!("{hit}/{} name words", nw.len())
            },
        );
    }
    let mut v: Vec<Guess> = best
        .into_iter()
        .map(|(t, (c, n, r))| Guess {
            title_id: format!("{t:08X}"),
            name: n,
            confidence: c.min(99),
            reason: r,
        })
        .collect();
    // one row per name (the list repeats names for regional/demo IDs): keep the alias-table one
    v.sort_by(|a, b| {
        b.confidence
            .cmp(&a.confidence)
            .then(
                ALIASES
                    .iter()
                    .any(|x| format!("{:08X}", x.0) == b.title_id)
                    .cmp(&ALIASES.iter().any(|x| format!("{:08X}", x.0) == a.title_id)),
            )
            .then(a.title_id.cmp(&b.title_id))
    });
    let mut seen = std::collections::HashSet::new();
    v.retain(|g| seen.insert(g.name.to_lowercase()));
    v.truncate(max);
    v
}

/// A guess is "confident" when it's strong and clearly ahead of the runner-up.
pub fn confident(g: &[Guess]) -> bool {
    match g {
        [a, ..]
            if a.confidence >= 85
                && g.get(1).map_or(true, |b| a.confidence >= b.confidence + 8) =>
        {
            true
        }
        _ => false,
    }
}

#[derive(Debug, Serialize)]
pub struct Entry {
    pub title_id: String,
    pub name: String,
    pub aliases: Vec<String>,
}

/// For the game picker: search by name/alias/TitleID.
pub fn search(q: &str, max: usize) -> Vec<Entry> {
    let ql = q.trim().to_lowercase();
    let qa = words(q);
    let alias_of = |t: u32| {
        ALIASES
            .iter()
            .find(|a| a.0 == t)
            .map(|a| a.2.iter().map(|s| s.to_string()).collect())
            .unwrap_or_default()
    };
    let mut out: Vec<Entry> = vec![];
    let mut push = |t: u32, n: String| {
        if !out.iter().any(|e: &Entry| e.name.eq_ignore_ascii_case(&n)) && out.len() < max {
            out.push(Entry {
                title_id: format!("{t:08X}"),
                name: n,
                aliases: alias_of(t),
            })
        }
    };
    for (t, n, al) in ALIASES {
        if ql.is_empty()
            || n.to_lowercase().contains(&ql)
            || al.iter().any(|a| words(a) == qa)
            || format!("{t:08x}") == ql
        {
            push(*t, n.to_string());
        }
    }
    if ql.is_empty() {
        return out;
    }
    for (t, n) in iso2god::game_list::all() {
        if n.to_lowercase().contains(&ql)
            || format!("{t:08x}") == ql
            || (!qa.is_empty() && contains_seq(&words(n), &qa))
        {
            push(*t, name(*t).unwrap_or(n.to_string()));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    fn top(t: &[&str]) -> Guess {
        guess(t, 5).into_iter().next().expect("a guess")
    }
    #[test]
    fn aliases_and_names() {
        assert_eq!(top(&["BO2_Menu_v3.xex"]).title_id, "415608C3");
        assert_eq!(top(&["MW2 10th prestige"]).title_id, "41560817");
        assert_eq!(top(&["GTA V Mod Menu"]).title_id, "545408A7");
        assert_eq!(top(&["gta5_trainer.xex"]).title_id, "545408A7");
        assert_eq!(top(&["Grand Theft Auto IV patch"]).title_id, "545407F2");
        assert_eq!(
            top(&["menus", "Call of Duty Black Ops II"]).title_id,
            "415608C3"
        );
        assert_eq!(top(&["Halo Reach stuff"]).title_id, "4D53085B");
        assert_eq!(top(&["Borderlands 2 save editor"]).title_id, "5454087C");
        assert_eq!(top(&["Skate 3"]).title_id, "454108E6");
        assert!(confident(&guess(&["BO2_Menu_v3.xex"], 5)));
        assert!(confident(&guess(&["GTA V Mod Menu"], 5)));
    }
    #[test]
    fn unknown_or_vague_is_not_confident() {
        assert!(!confident(&guess(&["plugin.xex"], 5)));
        assert!(!confident(&guess(&["menu_final2.xex"], 5)));
        assert!(
            !confident(&guess(&["Black Ops"], 5)) || top(&["Black Ops"]).title_id == "41560855"
        );
        assert!(guess(&[""], 5).is_empty());
    }
    #[test]
    fn names_and_search() {
        assert_eq!(
            name(0x415608C3).as_deref(),
            Some("Call of Duty: Black Ops II")
        );
        assert!(name(0x4D530A81).is_some(), "falls back to the full list");
        assert!(search("bo2", 10).iter().any(|e| e.title_id == "415608C3"));
        assert!(search("4d5307e6", 10).iter().any(|e| e.name == "Halo 3"));
        assert!(search("forza", 20).len() >= 3);
        assert!(iso2god::game_list::all().len() > 4000);
    }
}
