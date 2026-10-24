//! Plain-English fix-it hints for common errors (shown beside the technical message).

use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ErrorHelp {
    pub message: String,
    pub hint: String,
}

/// Map a raw error string to a short fix-it hint. Empty hint when nothing specific applies.
pub fn help_for(raw: &str) -> ErrorHelp {
    let msg = raw.trim();
    let lower = msg.to_ascii_lowercase();
    let hint = if lower.contains("fat32") || lower.contains("4 gb") || lower.contains("4gb") {
        "FAT32 can't store a single file over 4 GB. Split the archive, use NTFS on the PC, or send to the console's internal drive (Hdd1) instead of a FAT32 USB stick."
    } else if lower.contains("free space")
        || lower.contains("not enough") && lower.contains("space")
    {
        "Free up disk space on that drive, pick another folder, or send fewer files at once."
    } else if lower.contains("ftp")
        && (lower.contains("refused") || lower.contains("connect") || lower.contains("timed out"))
    {
        "Check the console IP, make sure FTP is on in Aurora/XeXMenu/DashLaunch, and that this PC is on the same Wi‑Fi or LAN."
    } else if lower.contains("password") || lower.contains("530") || lower.contains("login") {
        "Default FTP login is usually xboxftp / xboxftp. If you changed it in DashLaunch, use that user and password."
    } else if lower.contains("already talking to the console")
        || lower.contains("ftp allows one connection")
    {
        "Wait for the other Send to Xbox or catalog job to finish — the console only allows one FTP session at a time."
    } else if lower.contains("truncated") || lower.contains("incomplete") {
        "Re-download or re-copy the archive/ISO; the file on disk is smaller than it should be."
    } else if lower.contains("not an xbox") || lower.contains("not a title update") {
        "This file isn't the right type for that action. Pick an Xbox 360 GOD folder, DLC/TU package, or disc ISO."
    } else if lower.contains("grant") || lower.contains("wasn't picked") {
        "Use Unpack's Browse/Add buttons so the path is picked in the app — pasted paths aren't allowed for safety."
    } else if lower.contains("launch.ini") {
        "Don't delete launch.ini on the console. Use Mod catalog uninstall or restore a backup from Unpack's journal."
    } else {
        ""
    };
    ErrorHelp {
        message: msg.to_string(),
        hint: hint.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fat32_gets_a_hint() {
        let h = help_for("FAT32 limit: file is 5 GB");
        assert!(h.hint.contains("4 GB"));
    }

    #[test]
    fn ftp_busy_gets_a_hint() {
        let h = help_for("Unpack is already talking to the console.");
        assert!(!h.hint.is_empty());
    }
}
