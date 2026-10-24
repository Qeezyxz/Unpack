//! User-built "Known mod names": normalized mod name -> TitleID. Starts empty; nothing bundled.
//! Learned when the user confirms a mod's game; matched before the generic game-alias guess.
//! A header TitleID always wins (handled by the caller).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Learned {
    pub key: String,
    pub tid: String,
}

/// Noise words: menu words, release/site tags, file extensions.
const NOISE: &[&str] = &[
    "menu",
    "menus",
    "mod",
    "mods",
    "modmenu",
    "plugin",
    "xex",
    "zip",
    "rar",
    "7z",
    "final",
    "release",
    "beta",
    "free",
    "paid",
    "by",
    "www",
    "com",
    "net",
    "org",
    "se7ensins",
    "nextgenupdate",
    "ngu",
    "xbox",
    "360",
    "x360",
    "xbox360",
    "rgh",
    "jtag",
    "tu",
    "the",
    "update",
    "latest",
    "new",
    "fixed",
];

/// Generic mod words: never distinctive on their own, stripped from keys.
const GENERIC: &[&str] = &[
    "aimbot",
    "aim",
    "bot",
    "godmode",
    "god",
    "hack",
    "hacks",
    "hacked",
    "cheat",
    "cheats",
    "trainer",
    "unlocker",
    "unlock",
    "unlocks",
    "unlockall",
    "all",
    "esp",
    "wallhack",
    "recovery",
    "lobby",
    "lobbies",
    "tool",
    "tools",
    "infection",
    "infections",
    "prestige",
    "spawner",
    "money",
    "drop",
    "mods",
    "patch",
    "patched",
    "edition",
    "pack",
    "plugins",
    "script",
    "scripts",
    "gsc",
    "sprx",
    "loader",
    "private",
    "public",
    "pro",
    "lite",
    "max",
    "ultimate",
    "premium",
    "vip",
    "official",
    "custom",
    "online",
    "offline",
    "zombies",
    "multiplayer",
    "mp",
    "sp",
    "campaign",
    "v",
    "x",
    "and",
    "of",
    "a",
    "for",
    "in",
    "with",
];
/// A key needs at least this many letters/digits to be distinctive.
const MIN_KEY_CHARS: usize = 3;

fn is_version(w: &str) -> bool {
    let b = w.as_bytes();
    (b.len() >= 2 && b[0] == b'v' && w[1..].chars().all(|c| c.is_ascii_digit()))
        || w.chars().all(|c| c.is_ascii_digit())
}

fn strip_seq(ws: &mut Vec<String>, seq: &[String]) {
    let n = seq.len();
    if n == 0 {
        return;
    }
    let mut i = 0;
    while i + n <= ws.len() {
        if ws[i..i + n] == *seq {
            ws.drain(i..i + n);
        } else {
            i += 1;
        }
    }
}

/// Normalize a mod file/folder/archive name. If `tid` is given, that game's name and aliases are
/// removed too (so "call-of-duty-black-ops-ii-jiggy-menu-v4-5" -> "jiggy").
pub fn normalize(name: &str, tid: Option<u32>) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let mut ws = crate::gamedb::words(base);
    if let Some(t) = tid {
        let mut names: Vec<Vec<String>> = vec![];
        if let Some(n) = crate::gamedb::name(t) {
            names.push(crate::gamedb::words(&n));
        }
        if let Some(a) = crate::gamedb::ALIASES.iter().find(|a| a.0 == t) {
            names.push(crate::gamedb::words(a.1));
            names.extend(a.2.iter().map(|x| crate::gamedb::words(x)));
        }
        names.sort_by_key(|n| std::cmp::Reverse(n.len()));
        for n in &names {
            strip_seq(&mut ws, n);
        }
    }
    ws.retain(|w| !is_generic(w) && !is_version(w));
    ws.join(" ")
}

/// Fold leetspeak and a plural "s" so "aimb0t", "aimbots" and "hax" variants compare as generic.
fn fold(w: &str) -> String {
    let t: String = w
        .chars()
        .map(|c| match c {
            '0' => 'o',
            '1' => 'i',
            '3' => 'e',
            '4' => 'a',
            '5' => 's',
            '7' => 't',
            '@' => 'a',
            '$' => 's',
            _ => c,
        })
        .collect();
    let t = t
        .strip_suffix("es")
        .filter(|x| x.len() >= 4 && GENERIC.contains(x))
        .map(String::from)
        .or_else(|| {
            t.strip_suffix('s')
                .filter(|x| x.len() >= 3)
                .map(String::from)
                .filter(|x| GENERIC.contains(&x.as_str()))
        })
        .unwrap_or(t);
    t
}
fn is_generic(w: &str) -> bool {
    let f = fold(w);
    GENERIC.contains(&w)
        || NOISE.contains(&w)
        || GENERIC.contains(&f.as_str())
        || NOISE.contains(&f.as_str())
        || ["hax", "haxx", "h4x", "aimbt", "godmod"].contains(&f.as_str())
}
/// Space-stripped forms of every game name and alias ("blackops", "codbo2", "gtav"), plus
/// common shorthand, cached.
fn squashed_game_names() -> &'static std::collections::HashSet<String> {
    static S: std::sync::OnceLock<std::collections::HashSet<String>> = std::sync::OnceLock::new();
    S.get_or_init(|| {
        let mut set = std::collections::HashSet::new();
        let mut add = |ws: Vec<String>| {
            // every contiguous run of words, squashed: "call of duty black ops" → "blackops", "callofduty", …
            for i in 0..ws.len() {
                for j in i + 1..=ws.len().min(i + 6) {
                    let s = ws[i..j].concat();
                    if s.len() >= 3 {
                        set.insert(s);
                    }
                }
            }
        };
        for (_, n, al) in crate::gamedb::ALIASES {
            add(crate::gamedb::words(n));
            for a in al.iter() {
                add(crate::gamedb::words(a));
            }
        }
        for (_, n) in iso2god::game_list::all() {
            add(crate::gamedb::words(n));
        }
        for x in ["blops", "bops", "cod", "codmw", "gta", "rdr2", "mc"] {
            set.insert(x.into());
        }
        set
    })
}

fn contains_words(hay: &[String], k: &[String]) -> bool {
    !k.is_empty()
        && k.len() <= hay.len()
        && (0..=hay.len() - k.len()).any(|i| hay[i..i + k.len()] == *k)
}

/// Why a key can't be used, if it can't: too short, generic, or part of a known game's name/alias
/// (that would let one mod name re-file every mod of that game).
pub fn key_problem(key: &str) -> Option<String> {
    let k: Vec<String> = key
        .split(' ')
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();
    if k.concat().chars().count() < MIN_KEY_CHARS {
        return Some("Nothing distinctive left after removing the game, version and generic words (menu, aimbot, trainer…).".into());
    }
    if k.iter().all(|w| is_generic(w)) {
        return Some(format!("“{key}” is a generic mod word, not a mod name."));
    }
    let squashed = k.concat();
    if squashed_game_names().contains(&squashed) {
        return Some(format!(
            "“{key}” is a game's name or shorthand, not a mod name."
        ));
    }
    for (_, n, al) in crate::gamedb::ALIASES {
        if contains_words(&crate::gamedb::words(n), &k)
            || al.iter().any(|a| crate::gamedb::words(a) == k)
        {
            return Some(format!(
                "“{key}” is part of a game's name ({n}), not a mod name."
            ));
        }
    }
    for (_, n) in iso2god::game_list::all() {
        if contains_words(&crate::gamedb::words(n), &k) {
            return Some(format!(
                "“{key}” is part of a game's name ({n}), not a mod name."
            ));
        }
    }
    None
}

pub fn parse_tid(t: &str) -> Option<u32> {
    let t = t.trim();
    if t.len() != 8 {
        return None;
    }
    u32::from_str_radix(t, 16)
        .ok()
        .filter(|&v| v != 0 && crate::gamedb::name(v).is_some())
}

/// Validate and compute a key for learning. Errors on unknown TitleID or empty key.
pub fn learn_key(name: &str, tid: &str) -> Result<(String, String, String), String> {
    let t = parse_tid(tid).ok_or_else(|| format!("{tid} isn't a known Xbox 360 TitleID."))?;
    let key: String = normalize(name, Some(t)).chars().take(80).collect();
    if let Some(why) = key_problem(&key) {
        return Err(why);
    }
    Ok((
        key,
        format!("{t:08X}"),
        crate::gamedb::name(t).unwrap_or_default(),
    ))
}

/// First learned entry whose key words appear in order within any of the texts. Longest key wins.
pub fn lookup<'a>(learned: &'a [Learned], texts: &[&str]) -> Option<&'a Learned> {
    let hays: Vec<Vec<String>> = texts
        .iter()
        .map(|t| {
            normalize(t, None)
                .split(' ')
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect()
        })
        .collect();
    let mut best: Option<&Learned> = None;
    for l in learned {
        // Stored/imported keys are re-checked: a bad key never auto-detects anything.
        if parse_tid(&l.tid).is_none() || key_problem(&l.key).is_some() {
            continue;
        }
        let k: Vec<String> = l
            .key
            .split(' ')
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect();
        if k.is_empty() {
            continue;
        }
        let hit = hays.iter().any(|h| {
            k.len() <= h.len() && (0..=h.len() - k.len()).any(|i| h[i..i + k.len()] == *k)
        });
        if hit && best.map_or(true, |b| k.len() > b.key.split(' ').count()) {
            best = Some(l);
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn learn_jiggy_then_detect_new_version() {
        let (key, tid, game) =
            learn_key("call-of-duty-black-ops-ii-jiggy-menu-v4-5", "415608C3").unwrap();
        assert_eq!(key, "jiggy");
        assert_eq!(tid, "415608C3");
        assert!(game.contains("Black Ops II"));
        let l = vec![Learned { key, tid }];
        assert_eq!(
            lookup(&l, &["Jiggy Menu v5.0.zip"]).unwrap().tid,
            "415608C3"
        );
        assert_eq!(lookup(&l, &["JIGGY_v6"]).unwrap().tid, "415608C3");
        assert!(lookup(&l, &["Other Menu v5.0"]).is_none());
    }
    #[test]
    fn rejects_bad_tid_and_empty_key() {
        assert!(learn_key("x menu", "ZZZZZZZZ").is_err());
        assert!(learn_key("x menu", "00000000").is_err());
        assert!(learn_key("x menu", "FFFFFFF0").is_err());
        assert!(
            learn_key("BO2 Menu v3", "415608C3").is_err(),
            "only game+menu+version words"
        );
    }
    #[test]
    fn longest_key_wins() {
        let l = vec![
            Learned {
                key: "jiggy".into(),
                tid: "4D5307E6".into(),
            },
            Learned {
                key: "jiggy reloaded".into(),
                tid: "415608C3".into(),
            },
        ];
        assert_eq!(lookup(&l, &["Jiggy Reloaded v2"]).unwrap().tid, "415608C3");
    }
    #[test]
    fn r8_game_names_aliases_and_generic_words_refused() {
        // Overseer case 1: learning "Black Ops II" for Halo 3 would make the key "black ops"
        assert!(learn_key("Black Ops II Menu v3", "4D5307E6").is_err());
        // case 3: "aimbot" is generic
        assert!(learn_key("MW2 Aimbot", "4D5307E6").is_err());
        assert!(learn_key("Aimbot", "4D5307E6").is_err());
        for w in [
            "trainer",
            "god mode hack",
            "esp wallhack",
            "gta",
            "mw2",
            "reach",
            "ab",
        ] {
            assert!(learn_key(w, "4D5307E6").is_err(), "{w}");
        }
        // generic words are stripped but a distinctive name survives
        assert_eq!(
            learn_key("Jiggy Aimbot Menu v4", "415608C3").unwrap().0,
            "jiggy"
        );
        // R9: space-stripped game names / shorthand and plural/leet generic words
        for w in [
            "blackops",
            "blops",
            "aimbots",
            "aimb0t",
            "AimB0ts",
            "callofduty",
            "gtav",
            "haloreach",
            "godmodes",
            "trainers",
        ] {
            assert!(learn_key(w, "4D5307E6").is_err(), "{w}");
            assert!(key_problem(&normalize(w, None)).is_some(), "stored {w}");
        }
        assert_eq!(
            learn_key("Jiggy Aimb0ts v2", "415608C3").unwrap().0,
            "jiggy"
        );
        // imported/stored bad keys never match
        let bad = vec![
            Learned {
                key: "black ops".into(),
                tid: "4D5307E6".into(),
            },
            Learned {
                key: "aimbot".into(),
                tid: "4D5307E6".into(),
            },
        ];
        assert!(lookup(&bad, &["Black Ops II Menu v3.xex"]).is_none());
        assert!(lookup(&bad, &["MW2 Aimbot.xex"]).is_none());
    }
}
