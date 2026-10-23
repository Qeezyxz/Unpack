# Unpack Project Rules

This is a Windows desktop application for managing Xbox 360 games, DLC, title updates, mods, and homebrew.

## Key Context
- Uses real 7-Zip for archive extraction (ZIP, RAR, 7Z, etc.)
- Handles Xbox 360 ISOs → Games on Demand or extracted XEX folders
- Built-in TitleID database (~4900 entries)
- Network transfer feature ("Send to Xbox") talks only to local network devices running Aurora, XeXMenu, or DashLaunch
- Has installer, portable build, and cleanup/uninstaller

## Preferences
- Prefer clear, maintainable code
- Add comments for non-obvious Xbox 360 / Aurora / FAT32 logic
- Keep the Library, DLC matching, and Send to Xbox features robust
- Verify file operations (especially copies and ISO conversions) before deleting originals
