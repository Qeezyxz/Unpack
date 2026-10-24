# Unpack Project Rules

This is a Windows desktop application for managing Xbox 360 games, DLC, title updates, mods, and homebrew.

## Key Context
- Uses real 7-Zip for archive extraction (ZIP, RAR, 7Z, etc.)
- Handles Xbox 360 ISOs → Games on Demand or extracted XEX folders
- Built-in TitleID database (5,908 entries)
- Network transfer feature ("Send to Xbox") talks only to local network devices running Aurora, XeXMenu, or DashLaunch
- Has installer, portable build, and cleanup/uninstaller

## Preferences
- Prefer clear, maintainable code
- Add comments for non-obvious Xbox 360 / Aurora / FAT32 logic
- Keep the Library, DLC matching, and Send to Xbox features robust
- Verify file operations (especially copies and ISO conversions) before deleting originals
- The code base is the independently reviewed Unpack source (see HANDOFF.md for the security model). Keep path grants, no-write-through-links, `.partial`/`.part` + verify, and "network only from Rust, only on click" intact when adding features.
- Every command added to `src-tauri/src/lib.rs` must also exist in `src-tauri/examples/bridge.rs` so the UI suites in `tests/` cover it.
