use crate::paths;
use serde::Serialize;
use std::fs::File;
use std::io::Read;
use std::path::Path;

#[derive(Debug, Clone, Serialize)]
pub struct StfsInfo {
    pub magic: String,
    pub title_id: String,
    pub title_id_num: u32,
    pub content_type: u32,
    pub content_label: String,
    pub display_name: String,
}

pub fn looks_like_stfs(bytes: &[u8]) -> bool {
    if bytes.len() < 4 {
        return false;
    }
    matches!(&bytes[0..4], b"CON " | b"LIVE" | b"PIRS")
}

pub fn parse_stfs_bytes(bytes: &[u8]) -> Result<StfsInfo, String> {
    if bytes.len() < 0x430 {
        return Err("file is too small to be an STFS package".into());
    }
    if !looks_like_stfs(bytes) {
        return Err("not a CON/LIVE/PIRS package".into());
    }
    let magic = String::from_utf8_lossy(&bytes[0..4]).trim().to_string();
    let content_type = u32::from_be_bytes(bytes[0x344..0x348].try_into().unwrap());
    let title_id_num = u32::from_be_bytes(bytes[0x360..0x364].try_into().unwrap());
    let display_name = utf16be_zstring(&bytes[0x411..0x411.min(bytes.len()).saturating_add(0x80)]);
    Ok(StfsInfo {
        magic,
        title_id: format!("{title_id_num:08X}"),
        title_id_num,
        content_type,
        content_label: paths::content_type_label(content_type).to_string(),
        display_name,
    })
}

pub fn parse_stfs_file(path: &Path) -> Result<StfsInfo, String> {
    let mut file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut buf = vec![0u8; 0x800];
    let n = file.read(&mut buf).map_err(|e| e.to_string())?;
    buf.truncate(n);
    parse_stfs_bytes(&buf)
}

fn utf16be_zstring(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_be_bytes([c[0], c[1]]))
        .take_while(|u| *u != 0)
        .collect();
    String::from_utf16(&units)
        .unwrap_or_default()
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_package(title_id: u32, content_type: u32, name: &str) -> Vec<u8> {
        let mut buf = vec![0u8; 0x500];
        buf[0..4].copy_from_slice(b"LIVE");
        buf[0x344..0x348].copy_from_slice(&content_type.to_be_bytes());
        buf[0x360..0x364].copy_from_slice(&title_id.to_be_bytes());
        for (i, unit) in name.encode_utf16().enumerate() {
            let bytes = unit.to_be_bytes();
            buf[0x411 + i * 2] = bytes[0];
            buf[0x411 + i * 2 + 1] = bytes[1];
        }
        buf
    }

    #[test]
    fn parses_dlc_header() {
        let buf = sample_package(0x4D5307E6, 0x00000002, "Halo 3 DLC");
        let info = parse_stfs_bytes(&buf).unwrap();
        assert_eq!(info.magic, "LIVE");
        assert_eq!(info.title_id, "4D5307E6");
        assert_eq!(info.content_type, 0x00000002);
        assert_eq!(info.content_label, "DLC");
        assert_eq!(info.display_name, "Halo 3 DLC");
    }

    #[test]
    fn rejects_random_bytes() {
        assert!(parse_stfs_bytes(&[0u8; 0x500]).is_err());
    }
}
