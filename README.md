# Unpack

Unpack **1.1.0** is a Windows (and Linux-dev) desktop tool for extracting archives (ZIP, RAR, 7Z, and more), converting Xbox 360 ISOs to Games on Demand or extracted XEX folders, managing a game library, installing DLC/title updates, handling mod menus, and sending files to a modded Xbox 360 over the local network.

Use at your own risk. Back up `launch.ini` and anything you convert or copy. See `DISCLAIMER.txt`.

## Build from source

```bash
npm install
npx tauri dev      # development window
npx tauri build    # installer / portable (on Windows)
```

Needs Node 22, Rust 1.90+ (`rust-toolchain.toml`), and the WebView2 runtime on Windows. On Linux, install the Tauri WebKitGTK packages (`libwebkit2gtk-4.1-dev`, `librsvg2-dev`). Put `7za.exe` next to `Unpack.exe` on Windows, or install p7zip / 7-Zip and optionally set `UNPACK_7ZA`.

ISO conversion uses a vendored copy of [iso2god-rs](https://github.com/iliazeus/iso2god-rs) (MIT). The TitleID library is that crate’s ~4,900-name list.

## Status

- **1.1.0** — first source-complete release in this repo. The unpublished 1.0.0 installer is a separate build.
- Build from source with `npx tauri build` on Windows for an installer / portable zip.
- Keep backups. This is a hobbyist offline tool.

## Known Bugs

- **Send to Xbox (fixed in 1.1.0):** Older builds could delete `launch.ini` if a transfer was interrupted. 1.1.0 skips `launch.ini` unless you opt in, and replacements use rename instead of delete-then-upload. Keep a backup anyway.

## VirusTotal

- Installer: [VirusTotal report](https://www.virustotal.com/gui/file/06d10c80350857f733c4a1bf864e799c68f0d5e4d6423410dc4264934fd0f2f8)
- Portable: [VirusTotal report](https://www.virustotal.com/gui/file/1d6a4c0551bef9fc413fce2a773cd2a4752ee7ba86f754fa57af420691571b84)

Currently flagged only by Trapmine (`Malicious.high.ml.score`). This is a common machine-learning false positive on unsigned NSIS/Tauri builds. All other engines report clean.

## Install

Windows builds are attached to the [1.1.0 release](https://github.com/Qeezyxz/Unpack/releases/tag/v1.1.0).

**Option A (recommended): the installer**
1. Download and run `Unpack_1.1.0_x64-setup.exe`.
2. Windows may show "Windows protected your PC" because the file isn't signed. Click **More info**, then **Run anyway**.
3. The installer doesn't need admin rights. It installs only for your user account.

**Option B: portable (no install)**
Unzip `Unpack-portable.zip` anywhere and run `Unpack.exe`. Keep `7za.exe` and `WebView2Loader.dll` in the same folder.

Requires the WebView2 runtime (already included on Windows 10/11).

## Features

- Archive extraction and creation (ZIP, RAR, 7Z, TAR, etc.) via bundled 7-Zip
- Batch processing
- Xbox 360 ISO → Games on Demand or extracted XEX folder, with verification
- Game Library (~4,900 TitleIDs)
- USB copy with FAT32 checks and verification
- Auto-detect and place DLC / Title Updates
- Mod Menus management
- Link downloader with resume and hash check
- Homebrew Tools section
- Send to Xbox (local network only, to Aurora / XeXMenu / DashLaunch)
- Notifications, drag-and-drop, keyboard shortcuts
- Installer + portable + cleanup tool

See `DISCLAIMER.txt` and `LICENSE` for terms.

## License

Non-commercial. See [LICENSE](LICENSE).
