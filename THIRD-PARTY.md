# Third-party notices

Full license texts for the bundled binaries are in `licenses/` (they are installed with the app).

## iso2god-rs

Vendored at `src-tauri/vendor/iso2god` from [iliazeus/iso2god-rs](https://github.com/iliazeus/iso2god-rs) (crate version 1.8.0 in this tree; Unpack carries a few local changes, listed in `HANDOFF.md`).

MIT License. Copyright (c) 2023 Ilia Pozdnyakov. See `src-tauri/vendor/iso2god/LICENSE` and `licenses/iso2god-rs-LICENSE.txt`.

The built-in TitleID list (5,908 titles) is that crate's `game_list/titles.jsonl` plus the Xbox 360 and original Xbox TitleID CSVs in the same folder, merged by `merge_gamelists.py` (existing names win; new IDs are appended).

## 7-Zip

`7za.exe` © Igor Pavlov, GNU LGPL with BSD-3 parts. Source: <https://www.7-zip.org>. See `licenses/7-Zip-License.txt` and `licenses/7-Zip-readme.txt`.

## Browser fallback engines (`src/vendor/`)

- fflate (MIT)
- 7z-wasm (LGPL, includes the unRAR license)
- node-unrar-js (MIT)

Fonts: Fraunces and Outfit (SIL OFL).

## Discord Rich Presence

The `discord-rich-presence` crate (MIT) talks to the local Discord app over its IPC pipe. Unpack makes no network connection for it.

## Tauri

Built with [Tauri 2](https://tauri.app) (MIT / Apache-2.0).
