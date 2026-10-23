# Unpack

This GitHub repository is the **complete source code** for Unpack. It is public / open source: you can clone it, read it, change it, and build it yourself.

Unpack is a Windows (and Linux-dev) desktop tool for extracting archives (ZIP, RAR, 7Z, and more), converting Xbox 360 ISOs to Games on Demand or extracted XEX folders, managing a game library, installing DLC/title updates, handling mod menus, and sending files to a modded Xbox 360 over the local network.

Use at your own risk. Back up `launch.ini` and anything you convert or copy. See `DISCLAIMER.txt`.

## What’s in this repo

The `main` branch is the whole program, not an installer-only dump.

```
src/                      UI (HTML, CSS, JavaScript)
src-tauri/src/            Rust backend (extract, convert, USB, FTP, library)
src-tauri/vendor/iso2god  Vendored ISO → GOD engine (MIT)
.github/workflows/        Tests and the Windows installer build
LICENSE                   Terms for using and sharing the source
```

Current version: **1.4.0**.

## Build from source

```bash
npm install
npx tauri dev      # development window
npx tauri build    # installer / portable (on Windows)
```

Needs Node 22, Rust 1.90+ (`rust-toolchain.toml`), and the WebView2 runtime on Windows. On Linux, install the Tauri WebKitGTK packages (`libwebkit2gtk-4.1-dev`, `librsvg2-dev`). Put `7za.exe` next to `Unpack.exe` on Windows, or install p7zip / 7-Zip and optionally set `UNPACK_7ZA`.

ISO conversion uses a vendored copy of [iso2god-rs](https://github.com/iliazeus/iso2god-rs) (MIT). The TitleID library starts from that crate’s list and adds the Xbox 360 / original Xbox TitleID CSVs so conversions keep a real name (or the TitleID) instead of collapsing to `(unknown)`.

## Optional: Windows installer

If you just want to run Unpack, Windows builds are attached to the [latest GitHub Release](https://github.com/Qeezyxz/Unpack/releases/latest). The source on `main` is still the real project.

**Installer:** download `Unpack_1.4.0_x64-setup.exe`. Windows may show "Windows protected your PC" because the file isn't signed — **More info**, then **Run anyway**. No admin rights; it installs for your user account.

**Portable:** unzip `Unpack-portable.zip` and run `Unpack.exe`. Keep `7za.exe` and `WebView2Loader.dll` in the same folder.

Requires WebView2 (already on Windows 10/11).

Unsigned NSIS / Tauri builds are often flagged only by Trapmine (`Malicious.high.ml.score`). That is a common machine-learning false positive. Previous 1.3.0 reports: [installer](https://www.virustotal.com/gui/file/06d10c80350857f733c4a1bf864e799c68f0d5e4d6423410dc4264934fd0f2f8), [portable](https://www.virustotal.com/gui/file/1d6a4c0551bef9fc413fce2a773cd2a4752ee7ba86f754fa57af420691571b84).

## Features

- Archive extraction and creation (ZIP, RAR, 7Z, TAR, etc.) via bundled 7-Zip
- Opens zip/rar/7z files found inside an extracted folder (optional; on by default)
- Two jobs at once by default (set 1–4 in Settings — click the Unpack logo)
- Xbox 360 ISO → Games on Demand or extracted XEX folder, with verification. Disc 1 + Disc 2 merge into one playable folder
- Game Library (TitleID catalog for Xbox 360 and original Xbox)
- USB copy with FAT32 checks and verification
- Auto-detect and place DLC / Title Updates
- Mod Menus and Homebrew Tools
- Send to Xbox (local network only, to Aurora / XeXMenu / DashLaunch). If Wi-Fi drops, Send again — it can continue the leftover `.part` file
- Discord Rich Presence (optional; talks only to the local Discord client, application ID `1562281261504929922`)
- Installer + portable builds from this same source

## Status

- **1.4.0** — Safer copies and Send to Xbox, nested archives, jobs-at-once setting
- **1.3.0** — Original dark UI and Unpack mark
- **1.2.0** — Discord Rich Presence
- **1.1.1** — Full TitleID lists and Disc 1 / Disc 2 merge

Older builds could delete `launch.ini` if a transfer was interrupted. Current source skips `launch.ini` unless you opt in, writes a `.part` file first, and only renames over the real file when the copy is complete.

## License

The full source on `main` is public. You may copy, modify, and share it under [LICENSE](LICENSE) (hobbyist / non-commercial: don’t sell Unpack). Third-party notices: [THIRD-PARTY.md](THIRD-PARTY.md).
