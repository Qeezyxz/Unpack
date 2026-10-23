# Unpack

Windows tool for extracting, organizing, and transferring Xbox 360 games, DLC, title updates, mods, and homebrew.

## Features

- **Archive extraction** — Opens ZIP, RAR, 7Z and other archives using real 7-Zip. Drop in a pile of archives or folders and extract them all into one folder.
- **ISO conversion** — Automatically converts extracted Xbox 360 ISOs to Games on Demand format or an extracted XEX folder with `default.xex`. Optional delete of original ISO after byte-for-byte verification.
- **Library** — Every extracted/converted game appears with its real name (built-in list of ~4,900 Xbox 360 TitleIDs).
- **USB copy** — Copy a game to a USB drive’s `Games` folder. Respects FAT32 4 GB limit, skips shortcuts/junctions, and verifies every file.
- **DLC & Title Updates** — Auto-detects DLC and title update files by reading headers, matches them to the correct game, and places them in the right folder. Unmatched items are marked “game not found”.
- **Mod Menus** — Organizes menu files by game. Auto-detects using game + alias list plus names you’ve taught it. Learns confirmed menus (e.g. Jiggy Menu) for future versions. Import/export names list. Includes an empty built-in menus folder.
- **Link downloader** — Downloads from URL, resumes, optional SHA-256 check, extracts, detects game, and files it. Import/export saved links list.
- **Homebrew Tools** — Lists Aurora, XeXMenu, DashLaunch, Horizon, save editors, etc. with descriptions and links. Import your own list.
- **Send to Xbox** — Connect over WiFi or cable to Aurora, XeXMenu, or DashLaunch. Browse drives and send games, DLC, updates, plugins, or anything else. Live speed + ETA, resume/cancel, never overwrites a different existing file, home network only.
- **Extras** — Windows notifications on long jobs, drag-and-drop, keyboard shortcuts (e.g. Ctrl+Shift+X for Send to Xbox), custom icons/art, installer + portable version + cleanup tool.

## Requirements

- Windows
- 7-Zip

## Status

Newest features are currently in review. Build will be added once it passes.

## License

TBD
