//! DashLaunch `launch.ini`: a lossless line-based parser. Only the keys Unpack changes are
//! touched; every other line (comments, spacing, unknown sections) is written back as it was.

pub const INI_MAX: usize = 64 * 1024;
pub const PLUGIN_SECTION: &str = "Plugins";
/// plugin1 is reserved (users put their main plugin there); Unpack only uses 2..=5.
pub const FIRST_SLOT: u8 = 2;
pub const LAST_SLOT: u8 = 5;

#[derive(Debug, Clone, PartialEq)]
enum Line {
    Section(String),
    Kv { key: String, value: String },
    Other,
}

#[derive(Debug, Clone)]
pub struct Ini {
    raw: Vec<String>,
    kind: Vec<Line>,
    crlf: bool,
    trailing_newline: bool,
}

fn classify(l: &str) -> Line {
    let t = l.trim();
    if t.is_empty() || t.starts_with(';') || t.starts_with('#') {
        return Line::Other;
    }
    if t.starts_with('[') && t.ends_with(']') {
        return Line::Section(t[1..t.len() - 1].trim().to_string());
    }
    match t.split_once('=') {
        Some((k, v)) => {
            // DashLaunch allows trailing ";" comments after values.
            let v = v.split(';').next().unwrap_or("").trim();
            Line::Kv { key: k.trim().to_string(), value: v.to_string() }
        }
        None => Line::Other,
    }
}

impl Ini {
    pub fn parse(bytes: &[u8]) -> Result<Ini, String> {
        if bytes.len() > INI_MAX {
            return Err("launch.ini is larger than 64 KB; not editing it.".into());
        }
        let text = std::str::from_utf8(bytes).map_err(|_| "launch.ini isn't plain text; not editing it.".to_string())?;
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        if text.contains('\0') {
            return Err("launch.ini contains NUL bytes; not editing it.".into());
        }
        let crlf = text.contains("\r\n") || !text.contains('\n');
        let trailing_newline = text.ends_with('\n') || text.is_empty();
        let raw: Vec<String> = if text.is_empty() { vec![] } else { text.trim_end_matches(['\r', '\n']).split('\n').map(|l| l.trim_end_matches('\r').to_string()).collect() };
        let kind = raw.iter().map(|l| classify(l)).collect();
        Ok(Ini { raw, kind, crlf, trailing_newline })
    }

    pub fn serialize(&self) -> Vec<u8> {
        let nl = if self.crlf { "\r\n" } else { "\n" };
        let mut s = self.raw.join(nl);
        if self.trailing_newline && !self.raw.is_empty() {
            s.push_str(nl);
        }
        s.into_bytes()
    }

    /// (section, key, value) for every key line, in order; for validation and tests.
    pub fn entries(&self) -> Vec<(String, String, String)> {
        let mut sec = String::new();
        let mut out = vec![];
        for k in &self.kind {
            match k {
                Line::Section(s) => sec = s.clone(),
                Line::Kv { key, value } => out.push((sec.to_ascii_lowercase(), key.to_ascii_lowercase(), value.clone())),
                Line::Other => {}
            }
        }
        out
    }

    fn find(&self, section: &str, key: &str) -> Option<usize> {
        let mut sec = String::new();
        for (i, k) in self.kind.iter().enumerate() {
            match k {
                Line::Section(s) => sec = s.clone(),
                Line::Kv { key: kk, .. } if sec.eq_ignore_ascii_case(section) && kk.eq_ignore_ascii_case(key) => return Some(i),
                _ => {}
            }
        }
        None
    }

    pub fn get(&self, section: &str, key: &str) -> Option<String> {
        self.find(section, key).and_then(|i| match &self.kind[i] { Line::Kv { value, .. } => Some(value.clone()), _ => None })
    }

    pub fn set(&mut self, section: &str, key: &str, value: &str) {
        let line = format!("{key} = {value}");
        if let Some(i) = self.find(section, key) {
            self.raw[i] = line;
            self.kind[i] = classify(&self.raw[i]);
            return;
        }
        // Insert after the last non-blank line of the section, or add the section at the end.
        let mut sec = String::new();
        let mut last_in: Option<usize> = None;
        let mut header: Option<usize> = None;
        for (i, k) in self.kind.iter().enumerate() {
            match k {
                Line::Section(s) => { sec = s.clone(); if s.eq_ignore_ascii_case(section) { header = Some(i); last_in = Some(i); } }
                _ if sec.eq_ignore_ascii_case(section) && !self.raw[i].trim().is_empty() => last_in = Some(i),
                _ => {}
            }
        }
        match (header, last_in) {
            (Some(_), Some(at)) => {
                self.raw.insert(at + 1, line);
                self.kind.insert(at + 1, classify(&self.raw[at + 1]));
            }
            _ => {
                if self.raw.last().map_or(false, |l| !l.trim().is_empty()) {
                    self.raw.push(String::new());
                    self.kind.push(Line::Other);
                }
                self.raw.push(format!("[{section}]"));
                self.kind.push(Line::Section(section.to_string()));
                self.raw.push(line);
                self.kind.push(classify(self.raw.last().unwrap()));
            }
        }
    }

    pub fn remove(&mut self, section: &str, key: &str) {
        if let Some(i) = self.find(section, key) {
            self.raw.remove(i);
            self.kind.remove(i);
        }
    }

    /// Current value of plugin1..plugin5 (index 0 = plugin1). Empty values count as free.
    pub fn plugin_slots(&self) -> [Option<String>; 5] {
        let mut out: [Option<String>; 5] = Default::default();
        for n in 1..=5u8 {
            out[(n - 1) as usize] = self.get(PLUGIN_SECTION, &format!("plugin{n}")).filter(|v| !v.trim().is_empty());
        }
        out
    }
}

/// Where to put `value`. `requested`: the catalog's preferred slot (2..=5). `confirm`: a slot the
/// user explicitly agreed to overwrite. Returns (slot, previous value) or an error that starts
/// with `SLOT_OCCUPIED:` (the UI asks the user and retries with `confirm`).
pub fn choose_slot(ini: &Ini, value: &str, requested: Option<u8>, confirm: Option<u8>) -> Result<(u8, Option<String>), String> {
    let slots = ini.plugin_slots();
    if let Some(n) = slots.iter().position(|v| v.as_deref().map_or(false, |v| v.eq_ignore_ascii_case(value))) {
        return Ok((n as u8 + 1, slots[n].clone())); // already there: no change
    }
    if let Some(c) = confirm {
        if !(FIRST_SLOT..=LAST_SLOT).contains(&c) {
            return Err(format!("plugin{c} can't be used (plugin1 is reserved; use 2–5)."));
        }
        return Ok((c, slots[(c - 1) as usize].clone()));
    }
    if let Some(r) = requested {
        if !(FIRST_SLOT..=LAST_SLOT).contains(&r) {
            return Err(format!("plugin{r} can't be used (plugin1 is reserved; use 2–5)."));
        }
        return match &slots[(r - 1) as usize] {
            None => Ok((r, None)),
            Some(v) => Err(format!("SLOT_OCCUPIED:{r}:plugin{r} is already used by {v}. Replace it?")),
        };
    }
    for n in FIRST_SLOT..=LAST_SLOT {
        if slots[(n - 1) as usize].is_none() {
            return Ok((n, None));
        }
    }
    Err(format!("SLOT_OCCUPIED:{LAST_SLOT}:plugin2–plugin5 are all in use (plugin5 = {}). Replace plugin5?", slots[4].clone().unwrap_or_default()))
}

/// The edited file must parse back to exactly the old keys plus/minus the one change.
pub fn validate_change(before: &Ini, after_bytes: &[u8], key: &str, new_value: Option<&str>) -> Result<(), String> {
    let after = Ini::parse(after_bytes)?;
    let k = key.to_ascii_lowercase();
    let sec = PLUGIN_SECTION.to_ascii_lowercase();
    let strip = |v: Vec<(String, String, String)>| v.into_iter().filter(|(s, kk, _)| !(s == &sec && kk == &k)).collect::<Vec<_>>();
    if strip(before.entries()) != strip(after.entries()) {
        return Err("launch.ini check failed: other settings would change. Nothing was written.".into());
    }
    if after.get(PLUGIN_SECTION, key).as_deref() != new_value {
        return Err("launch.ini check failed: the plugin line didn't come out as expected.".into());
    }
    Ok(())
}
