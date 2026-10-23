# Unpack

> **Work in Progress — Still being tested**
>
> This program is under active development. Features may change, break, or be incomplete. Use at your own risk. Not ready for everyday use yet.

Unpack is a Windows desktop tool for extracting archives (ZIP, RAR, 7Z, and more), converting Xbox 360 ISOs to Games on Demand or extracted XEX folders, managing a game library, installing DLC/title updates, handling mod menus, and sending files to a modded Xbox 360 over the local network.

## Status

- **WIP / Testing** — Newest features are still being reviewed and tested.
- Source is being organized in this repo.
- Installer and portable builds exist but are not yet released here.
- Expect bugs. Back up your files.

## VirusTotal

Installer scan: [VirusTotal report](https://www.virustotal.com/gui/file/06d10c80350857f733c4a1bf864e799c68f0d5e4d6423410dc4264934fd0f2f8)

Currently flagged only by Trapmine (`Malicious.high.ml.score`). This is a common machine-learning false positive on unsigned NSIS/Tauri installers. All other engines report clean.

## Install (when ready)

**Option A (recommended): the installer**
1. Run `Unpack_1.0.0_x64-setup.exe`.
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
