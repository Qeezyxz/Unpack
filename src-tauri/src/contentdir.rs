//! Console Content profile folders (`…\Content\<16 hex>`, e.g. `Content\0000000000000000`).
//!
//! When the output folder is such a profile folder, games go in only as their real GOD layout
//! `<TitleID>\<ContentType>\<MediaID>` (+ `.data`), never inside a wrapper like "Game (GOD)".
//! Picking the `Content` folder itself, or a folder (a drive root) that holds
//! `Content\<16 hex>`, counts too. Names are compared case-insensitively.

use std::fs;
use std::path::{Path, PathBuf};

fn is_hex(s: &str, n: usize) -> bool {
    s.len() == n && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// A 16-hex profile folder name (`0000000000000000`).
pub fn is_profile_name(s: &str) -> bool {
    is_hex(s, 16)
}

/// An 8-hex TitleID / content-type folder name.
pub fn is_id8(s: &str) -> bool {
    is_hex(s, 8)
}

fn name_of(p: &Path) -> Option<String> {
    p.file_name().map(|n| n.to_string_lossy().into_owned())
}

fn real_dir(p: &Path) -> bool {
    fs::symlink_metadata(p).is_ok_and(|m| m.is_dir())
}

/// Child folder of `dir` whose name matches `pred` (case-insensitive by construction), preferring
/// an exact `want` name. Links are never followed.
fn child_dir(dir: &Path, want: &str, pred: impl Fn(&str) -> bool) -> Option<PathBuf> {
    let mut found: Vec<PathBuf> = fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| pred(&e.file_name().to_string_lossy()) && real_dir(&e.path()))
        .map(|e| e.path())
        .collect();
    found.sort();
    found
        .iter()
        .find(|p| name_of(p).is_some_and(|n| n.eq_ignore_ascii_case(want)))
        .or_else(|| (found.len() == 1).then(|| &found[0]))
        .cloned()
}

/// Is `dest` itself a `Content\<16 hex>` profile folder?
pub fn is_profile_dir(dest: &Path) -> bool {
    name_of(dest).is_some_and(|n| is_profile_name(&n))
        && dest
            .parent()
            .and_then(name_of)
            .is_some_and(|n| n.eq_ignore_ascii_case("content"))
}

/// The profile folder games should go into for output folder `dest`, if `dest` is a console
/// Content layout:
/// - `…\Content\<16 hex>` itself;
/// - the `Content` folder (its `0000000000000000`, or its only profile folder; created as
///   `0000000000000000` when it has none);
/// - a folder (typically a drive root) holding `Content\<16 hex>` (existing only).
pub fn resolve(dest: &Path) -> Option<PathBuf> {
    if is_profile_dir(dest) {
        return Some(dest.to_path_buf());
    }
    const ZERO: &str = "0000000000000000";
    if name_of(dest).is_some_and(|n| n.eq_ignore_ascii_case("content")) {
        return Some(child_dir(dest, ZERO, is_profile_name).unwrap_or_else(|| dest.join(ZERO)));
    }
    let content = child_dir(dest, "Content", |n| n.eq_ignore_ascii_case("content"))?;
    child_dir(&content, ZERO, is_profile_name)
}

/// Is `p` a GOD title folder: an 8-hex name holding an 8-hex content-type folder with at least
/// one entry in it (links never followed)?
pub fn is_title_tree(p: &Path) -> bool {
    if !real_dir(p) || !name_of(p).is_some_and(|n| is_id8(&n)) {
        return false;
    }
    fs::read_dir(p).is_ok_and(|rd| {
        rd.flatten().any(|ct| {
            is_id8(&ct.file_name().to_string_lossy())
                && real_dir(&ct.path())
                && fs::read_dir(ct.path()).is_ok_and(|mut r| r.next().is_some())
        })
    })
}

/// GOD title folders in an extracted tree: at its top level, or one wrapper folder down
/// (`Bo3game (GOD)/415608C3/00007000/…`). Sorted.
pub fn find_title_trees(root: &Path) -> Vec<PathBuf> {
    let mut out = vec![];
    let Ok(rd) = fs::read_dir(root) else {
        return out;
    };
    for e in rd.flatten() {
        let p = e.path();
        if is_title_tree(&p) {
            out.push(p);
        } else if real_dir(&p) {
            if let Ok(inner) = fs::read_dir(&p) {
                out.extend(
                    inner
                        .flatten()
                        .map(|x| x.path())
                        .filter(|x| is_title_tree(x)),
                );
            }
        }
    }
    out.sort();
    out
}

/// Amber hint for non-GOD content extracted into a Content profile folder.
pub const NOT_GOD_HINT: &str = "This doesn't look like a GOD game (no <TitleID>\\<type> folders inside), so it was extracted as is. Aurora and the dashboard only list games inside Content\\0000000000000000 as <TitleID>\\00007000\\… folders.";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TmpDir;

    #[test]
    fn resolves_profile_content_and_root() {
        let t = TmpDir::new("cdir");
        let root = t.path().join("usb");
        let prof = root.join("content").join("0000000000000000");
        fs::create_dir_all(&prof).unwrap();
        assert_eq!(resolve(&prof), Some(prof.clone()));
        assert_eq!(resolve(&root.join("content")), Some(prof.clone()));
        assert_eq!(resolve(&root), Some(prof.clone()));
        let other = t.path().join("Games");
        fs::create_dir_all(&other).unwrap();
        assert_eq!(resolve(&other), None);
        // An 8-hex or wrong-parent folder isn't a profile.
        let fake = t.path().join("x").join("0000000000000000");
        fs::create_dir_all(&fake).unwrap();
        assert_eq!(resolve(&fake), None);
        // An empty Content folder gets the default profile.
        let c2 = t.path().join("Content");
        fs::create_dir_all(&c2).unwrap();
        assert_eq!(resolve(&c2), Some(c2.join("0000000000000000")));
    }

    #[test]
    fn finds_title_trees_at_root_and_under_one_wrapper() {
        let t = TmpDir::new("cdir2");
        let a = t
            .path()
            .join("Bo3game (GOD)")
            .join("415608C3")
            .join("00007000");
        fs::create_dir_all(&a).unwrap();
        fs::write(a.join("ABCD1234"), b"x").unwrap();
        let b = t.path().join("4D5307E6").join("00007000");
        fs::create_dir_all(&b).unwrap();
        fs::write(b.join("DEADBEEF"), b"x").unwrap();
        fs::create_dir_all(t.path().join("4D530000").join("00007000")).unwrap(); // empty: not a game
        fs::create_dir_all(
            t.path()
                .join("a")
                .join("b")
                .join("415608C4")
                .join("00007000")
                .join("x"),
        )
        .unwrap(); // too deep
        let f = find_title_trees(t.path());
        let names: Vec<_> = f.iter().map(|p| name_of(p).unwrap()).collect();
        assert_eq!(names, vec!["4D5307E6", "415608C3"]);
    }
}

/// The folder that holds the `Content` folder of profile folder `profile`
/// (`X:\\Content\\0000000000000000` gives `X:\\`), used for anything that is not a GOD package.
pub fn outside_content(profile: &Path) -> Option<PathBuf> {
    let content = profile.parent()?;
    if !name_of(content).is_some_and(|n| n.eq_ignore_ascii_case("content")) {
        return None;
    }
    content.parent().map(Path::to_path_buf)
}

#[cfg(test)]
mod outside_tests {
    use super::*;
    #[test]
    fn outside_content_is_the_folder_holding_content() {
        let p = Path::new("/usb/Content/0000000000000000");
        assert_eq!(outside_content(p), Some(PathBuf::from("/usb")));
        assert_eq!(outside_content(Path::new("/usb/Games/0000000000000000")), None);
    }
}
