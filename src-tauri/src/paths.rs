use std::path::{Path, PathBuf};

/// Xbox 360 STFS / GOD content type folder names under Content/0000000000000000/{TITLEID}/
pub fn content_type_folder(content_type: u32) -> String {
    format!("{content_type:08X}")
}

pub fn content_dest(root: &Path, title_id: u32, content_type: u32) -> PathBuf {
    root.join("Content")
        .join("0000000000000000")
        .join(format!("{title_id:08X}"))
        .join(content_type_folder(content_type))
}

pub fn god_content_type() -> u32 {
    0x00007000
}

#[allow(dead_code)]
pub fn dlc_content_type() -> u32 {
    0x00000002
}

#[allow(dead_code)]
pub fn title_update_content_type() -> u32 {
    0x000B0000
}

#[allow(dead_code)]
pub fn arcade_content_type() -> u32 {
    0x000D0000
}

#[allow(dead_code)]
pub fn theme_content_type() -> u32 {
    0x00030000
}

pub fn content_type_label(content_type: u32) -> &'static str {
    match content_type {
        0x00000001 => "Saved Game",
        0x00000002 => "DLC",
        0x00000003 => "Trailer",
        0x00000004 => "Theme (original)",
        0x00010000 => "Profile",
        0x00020000 => "Gamer Picture",
        0x00030000 => "Theme",
        0x00040000 => "Cache",
        0x00050000 => "Xbox Original",
        0x00070000 => "Installed Game",
        0x00090000 => "Avatar Item",
        0x000B0000 => "Title Update",
        0x000D0000 => "XBLA",
        0x00007000 => "Games on Demand",
        _ => "Content",
    }
}

pub fn is_protected_filename(name: &str) -> bool {
    Path::new(name)
        .file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.eq_ignore_ascii_case("launch.ini"))
        .unwrap_or(false)
}

pub fn sanitize_xbox_folder_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '-',
            c if c.is_control() => '-',
            c => c,
        })
        .collect();
    let trimmed = cleaned.trim().trim_matches('.');
    // Cut to length first: a cut can leave a trailing space or dot, which
    // Windows will not keep on a folder name.
    let short: String = trimmed.chars().take(42).collect();
    let short = short.trim_end_matches(['.', ' ']);
    if short.is_empty() {
        "Game".into()
    } else if is_windows_device_name(short) {
        format!("_{short}")
    } else {
        short.to_string()
    }
}

/// `CON`, `NUL`, `COM1` and friends cannot be folder names on Windows.
fn is_windows_device_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit()
            && stem.as_bytes()[3] != b'0')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_layout_matches_aurora() {
        let dest = content_dest(Path::new("/media/usb"), 0x41560817, dlc_content_type());
        assert_eq!(
            dest,
            Path::new("/media/usb/Content/0000000000000000/41560817/00000002")
        );
        assert_eq!(content_type_label(0x000B0000), "Title Update");
    }

    #[test]
    fn launch_ini_is_protected() {
        assert!(is_protected_filename("launch.ini"));
        assert!(is_protected_filename("/Hdd1/launch.ini"));
        assert!(is_protected_filename("LAUNCH.INI"));
        assert!(!is_protected_filename("launch.ini.bak"));
        assert!(!is_protected_filename("default.xex"));
    }

    #[test]
    fn sanitizes_illegal_folder_names() {
        assert_eq!(sanitize_xbox_folder_name("Halo 3"), "Halo 3");
        assert_eq!(sanitize_xbox_folder_name("A:B/C"), "A-B-C");
        assert_eq!(sanitize_xbox_folder_name("   "), "Game");
        assert_eq!(sanitize_xbox_folder_name("Con"), "_Con");
        assert_eq!(sanitize_xbox_folder_name("com1"), "_com1");
        assert_eq!(sanitize_xbox_folder_name("Console Wars"), "Console Wars");
        let long = format!("{} tail", "a".repeat(41));
        assert_eq!(sanitize_xbox_folder_name(&long), "a".repeat(41));
        assert_eq!(sanitize_xbox_folder_name("Halo: Reach."), "Halo- Reach");
    }
}
