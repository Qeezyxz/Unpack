# Unpack: desktop edition

**Version 1.7.3.** Use at your own risk: back up `launch.ini` and anything you convert or copy. See [DISCLAIMER.txt](DISCLAIMER.txt). Licensed under the [Unpack Non-Commercial License](LICENSE); third-party notices are in [THIRD-PARTY.md](THIRD-PARTY.md).

I'm Justin, and I build Unpack. This repository is its **complete source code** (Tauri 2: Rust backend, plain HTML/JS page). It's public: you can read it, change it and build it yourself. `HANDOFF.md` describes the architecture, the security model, the build and the tests; `PORT-NOTES.md` explains how I merged the 1.6.0 additions into the reviewed code base.

I made Unpack to open and extract **ZIP, RAR, 7Z** (plus TAR, GZ, BZ2, XZ and more), make **ZIP and 7Z** archives, and convert **Xbox 360 ISOs to Games on Demand**. It looks the same as my web page, but now it's a real Windows program. It works offline, handles huge archives, and adds right-click menu entries in Explorer.

## Install

**Option A (recommended): the installer**
1. Run `Unpack_1.7.3_x64-setup.exe` (from `dist/` after you build it, or from a release you trust).
2. Windows may show "Windows protected your PC" because the file isn't signed. Click **More info**, then **Run anyway**.
3. The installer doesn't need admin rights. It installs only for your user account.

**Option B: portable (no install)**
Unzip `Unpack-portable.zip` anywhere and run `Unpack.exe`. Keep `7za.exe` and `WebView2Loader.dll` in the same folder. The portable copy has **no** right-click menu entries (those come from the installer).

Unpack needs Microsoft's WebView2 runtime. Windows 10 and 11 already have it.

### VirusTotal and false positives

Unsigned NSIS / Tauri builds are often flagged only by Trapmine (`Malicious.high.ml.score`). That is a common machine-learning false positive. Reports for the older 1.3.0 build: [installer](https://www.virustotal.com/gui/file/06d10c80350857f733c4a1bf864e799c68f0d5e4d6423410dc4264934fd0f2f8), [portable](https://www.virustotal.com/gui/file/1d6a4c0551bef9fc413fce2a773cd2a4752ee7ba86f754fa57af420691571b84). Check the file you downloaded against a release you trust, or build it yourself from this source.

### Known bugs and notes

- **Send to Xbox / `launch.ini` (fixed in 1.1.0, hardened since):** older builds could delete `launch.ini` if a transfer was interrupted. Unpack now uploads to a `.part` file, renames it only after the size is checked, and never overwrites a file that is already on the console (a different file at the same place stops the send before anything is written; the DashLaunch catalog install writes a timestamped backup first). USB copy and ISO convert follow the same "finish first, then swap" rule. Keep a backup of your `launch.ini` anyway.
- A full Xbox drive and a dropped Wi-Fi connection are different messages. Both keep the `.part` file so the next send resumes.
- **FAT32 drives (fixed in 1.7.3):** FAT32's 4 GB limit is per file, so I only refuse a single file of 4 GB or more, never a big archive or a big total. A game ISO over 4 GB with Auto ISO → GOD on is extracted to a temporary folder on your PC's drive, converted onto the FAT32 drive, and the temporary copy is deleted afterwards. Your PC's drive needs room for everything in the archive (not just the ISO) plus 256 MiB while that happens; if it doesn't have it, I tell you how much is needed. Other game files land next to the `Content` folder, the same as a normal extract; extras like readme `.txt`, `.nfo` and pictures that sit beside the ISO are skipped when the destination is a `Content` folder (never anything inside a game or app folder).
- **Library (fixed in 1.7.3):** you can set the library folder to a drive root (`D:\`), its `Content` folder, a profile folder (`Content\0000000000000000`) or a single TitleID folder. Refresh always rescans, and a long scan shows progress and can be cancelled.
- **Replace files repair (fixed in 1.7.4):** if Unpack is killed at the very last step of a **Replace files** into a Content folder and you edit the new `.data` files before the next run repairs the package, I keep the new header (it matches the note's fingerprint) and your edited `.data`, and I only delete old `.unpack-old.*` copies that still match what I set aside — I never pair the old header with your edited data.
- **Replace during folder move (fixed in 1.7.4):** if you rename or move the game's type folder (e.g. `<TitleID>\00007000`) while a Replace is running, I stop, roll back only what I can prove is mine, and tell you the folder was moved or renamed.
- The bundled 7-Zip is a Windows build; on Linux the tests use the system 7-Zip (23.01 or newer).

## Features

- Open and extract ZIP, RAR, 7Z, TAR, GZ, BZ2, XZ and more. Create ZIP and 7Z, with optional passwords.
- Batch queue: drop many archives at once. Each one shows its own progress bar and Cancel button, and you can extract them all into one chosen folder. **Jobs at once** (Settings → Extraction, 1–4, default 2); jobs that write the same folder always take turns.
- **Open zips inside zips** (Settings → Extraction, on by default): archives found inside an extracted archive are opened too (a few levels, with size limits). The result line says how many were opened and how many were left closed.
- **Xbox 360 ISO → GOD**, by hand or automatically for ISOs found inside extracted archives. You can optionally delete the extracted ISO after a fully verified convert. **Multi-disc games:** Disc 1 and Disc 2 go into one playable folder in either order (the lowest disc keeps the `default.xex` that launches); Unpack offers other discs it finds next to an ISO. A TitleID that is not in the list still converts, with a warning that shows the hex ID; a lone Disc 2 with no `default.xex` gets a plain message.
- **Discord Rich Presence** (Settings → Discord, **off** by default; I left it for you to turn on if you want it): when on, it shows which screen you are on and how many jobs run, never file names or folders. It only talks to the Discord app on your PC over its local pipe (Discord application ID `1562281261504929922`).
- **Clear errors:** a red **Error** banner with **Copy** and **Dismiss** stays above every dialog until you dismiss it or the next job starts. A failed write to a FAT32 drive explains the 4 GB limit.
- **DLC and title updates:** Xbox 360 packages (CON/LIVE/PIRS) found in extracted archives, or picked with **Install DLC**, go straight into their game. DLC lands in `<TitleID>/00000002` and title updates in `<TitleID>/000B0000`. Nothing is ever overwritten.
- **Library:** your games as tiles, with each game's name read from its own files (offline). Each tile shows the TitleID, size, and DLC/TU counts.
- **Copy to USB:** copies a game with its DLC and title updates to `<USB>\Content\0000000000000000`. It checks free space first and never overwrites.
- **Batch summary** when a batch finishes: counts of extracted, converted, skipped and failed items, DLC installed, and space freed, plus an **Open folder** button.
- **Download from link** (in Mod Menus): paste any http(s) URL and click Download. Progress, cancel and resume (`.partial`). SHA-256 shown. Public Google Drive / Dropbox / MediaFire pages rewritten to a direct link when the pattern is simple. Saved name+URL+notes list. Downloads land in the app Downloads folder (or one you choose); archives are offered to the extract queue; other files can be added to the Mod library. The page itself never goes online — only the Rust download command does, and only when you click.
- **Folder drop:** drop a folder of archives/ISOs to confirm adding them all to the extract queue. Mixing a folder with loose files still opens Create archive.
- **Windows toast + taskbar progress** when a batch, download or USB copy finishes (can turn off in Settings).
- **Mod Menus:** a browser for mod files *you already have* (.xex plugins, CON/LIVE/PIRS packages, folders). A searchable grid with game tiles, filters by game and type, a details panel with notes, and **Copy to USB** (into `\Plugins`, the game's `Content` folder, or as is), with a free-space check and no overwrite. The game comes from the file's header TitleID when it has one; otherwise Unpack guesses from the file, folder and archive names using its offline game list (e.g. `BO2_Menu.xex` → Call of Duty: Black Ops II), shows how sure it is, and asks you only when it isn't sure. Your choices are remembered. For offline / modded-console use: Unpack ships no mods, links or catalog, only goes online for a download, catalog or console you set up yourself, and never touches Xbox Live.
- **Send to Xbox** (Ctrl+Shift+X): send games, DLC, title updates and mods from your PC to a modded Xbox 360 over WiFi or LAN, using the FTP server in Aurora, XeXMenu or DashLaunch (default login `xboxftp` / `xboxftp`, port 21). Enter the console's IP, **Connect / Test** to see all its drives (Hdd1, Usb0, Usb1, UsbMu0…) and pick which one to send to (Hdd1 by default, else the first USB; your choice is remembered per console), browse folders, and save consoles for next time. One-click destinations use the same layout as Copy to USB: GOD games → `Hdd1\Content\0000000000000000\<TitleID>` (with their DLC and TUs), DLC → `…\<TitleID>\00000002`, title updates → `…\<TitleID>\000B0000`, extracted XEX games → `Hdd1\Games\<name>`, plugins → `Plugins`, or any folder you browse to. Picked files are detected automatically (package headers / default.xex). **Send to Xbox** buttons are also on every Library game and Mod Menus item. Live MB/s and ETA, Cancel and **Resume** (uploads go to a `.part` file and continue where they stopped, only if Unpack's own record on your PC shows the file is unchanged and the `.part` is exactly the size it stopped at; otherwise it starts over), remote size checked after each file, existing files never overwritten (same size = skipped, different = stops before sending; if the console's folder listing fails, nothing is sent). Every queue item has an editable **To** folder, pre-filled with the standard place for its type; use **Browse…** to pick it in the console browser, **Reset to default** to go back, the **Last:** chip for your last custom folder of that type, or **Send all to this folder** to point the whole queue at the folder you're viewing. Custom paths must start with a drive (like `/Hdd1`) and follow the Xbox name rules (42 characters, no `" * + , / : ; < = > ? \ |`). Optional **2 at once** uses a second connection when the console allows it and falls back to one otherwise. Only local network addresses (10.x, 172.16–31.x, 192.168.x, 169.254.x) are accepted, and links/junctions on the PC are never followed.
- **Save backup and restore** (on the Xbox screen, over FTP): you can back up a game's save folder to your PC and restore it later. Before a restore touches anything, I upload each new file to a `.part` first; only then is the live file moved into a new `.unpack-save-backup-…` folder on the console and the new one renamed in. Unpack also writes a small restore record on your PC. The record also holds the size and a fingerprint (SHA-256) of every file I upload, and I delete it as soon as the last file is renamed in and its size checked. If the restore is interrupted (WiFi drops, the app or PC is closed), the next start, or the next restore to that console, looks at each file: an original that is missing comes back from the backup folder, a file that still holds exactly what I uploaded is undone (its original comes back, or it's removed if the restore added it), and a file that changed since (you played before I could clean up) is kept as it is, never deleted. If you didn't play in between, the save ends up exactly as it was before the restore. If you did, the save can be a mix of old and new files; I say so plainly, name the files I kept and tell you which backup folder still holds the originals. If the restore had actually finished (every file was in place) and you played before I could note it, nothing is rolled back and the originals stay in the backup folder. A record from an older dev build that has no fingerprints only ever renames backups back into empty places; it never deletes a live file. Backups and screenshots are always written as new files on your PC (never overwriting, never through a link, and a destination swapped for a link mid-transfer stops the transfer).
- **Mod catalog** (Mod Menus → Catalog): browse and install mods from my catalog. Unpack itself ships **no** catalog and no entries: you add the catalog's address (or pick a `catalog.json` file) and the catalog's **public key**. Unpack only loads a catalog whose Ed25519 signature matches that key, and refuses an older catalog than one it already loaded. Pick where to install: a USB drive / folder on this PC, or the console (uses your saved Send to Xbox console and lists every drive it has, e.g. Hdd1, Usb0, Usb1; you pick which one). **Check** runs the safety checks without writing anything: free space, the FAT32 4 GB limit, required and conflicting mods, and whether the game's installed title update (TU) is one the mod supports. **Install** downloads (with mirrors and retries, at most 2 files at once), checks every file's SHA-256, unpacks archives into a staging folder (links and paths that leave the folder are refused), then copies into place; nothing existing is overwritten. Plugins can be added to DashLaunch's `launch.ini`: Unpack writes a timestamped backup first, swaps the new file in without ever removing the old one first (and puts the original back if anything fails, reconnecting if the WiFi dropped; if even that fails, the next time Unpack connects it restores your previous launch.ini automatically), never touches `plugin1`, uses the lowest free slot of `plugin2`–`plugin5`, and asks before replacing a slot that's in use. Installed mods show **Installed** / **Update available** badges; **Uninstall** removes only files that are unchanged since install and not used by another installed mod, and puts the `launch.ini` line back. Archives that would unpack to far more than their size (over 4× / 20 GB) are refused. Paid mods (and ones that can't be redistributed) only get an **Open source page** button. Game aliases and mod names from the catalog also help detect your own files.
- The built-in game list has **5,908 TitleIDs** (Xbox 360 and original Xbox): the iso2god-rs list plus the TitleID CSVs, merged by `src-tauri/vendor/iso2god/src/game_list/merge_gamelists.py`.
- Light, dark and System themes. First-run tips, keyboard shortcuts (press `?`), and tooltips on every button.
- Everything stays on your PC: no network (except the downloads and Send to Xbox you start), no admin rights, per-user only.

## Adding built-in menus

Unpack can show menus shipped with the app as read-only **Built-in** items in Mod Menus. It ships with none. To add your own build: put the file in `src-tauri/resources/menus/` and list it in `menus.json` there:

```json
[{ "name": "Display name", "file": "file.xex", "titleId": "4D5307E6", "version": "1.0", "sha256": "optional" }]
```

The file must be inside that folder (no `..`, no links), the TitleID must be a known game, and if you give a SHA-256 it must match, otherwise the entry is skipped. Built-in names also help auto-detect your own files; names you taught Unpack always win. See HANDOFF.md for details.

## Running a catalog

This is how I run my catalog, and you can run your own the same way. `unpack-catalog-sign.exe` (in the release folder) makes the key and signs the file:

```
unpack-catalog-sign keygen owner.key            # prints the public key you add in Unpack; keep owner.key private
unpack-catalog-sign sign owner.key catalog.json # writes catalog.json.sig (checks the file loads first)
unpack-catalog-sign sign - catalog.json < owner.key   # or read the secret key from stdin
unpack-catalog-sign verify <public key> catalog.json
unpack-catalog-sign check catalog.json          # schema check only
```

The secret key is only read from a file or stdin, never typed on the command line. `keygen` makes the key file readable only by you (on Windows it removes inherited access with `icacls` and warns if it can't; keep the file in a private folder either way). `catalog_version` must stay below 2^53.

Publish `catalog.json` and `catalog.json.sig` side by side (https). Format (schema 1):

```json
{ "schema": 1, "catalog_version": 1, "entries": [{
  "id": "my-plugin", "game": { "title_id": "4D5307E6", "name": "Halo 3", "aliases": ["h3"] },
  "mod": { "name": "My Plugin", "author": "me", "version": "1.0" },
  "compat": { "tu": [0, 18], "dash_min": "3.21" },
  "redistribute": true, "paid": false, "source_page": "https://…",
  "requires": [], "conflicts": [], "notes": "",
  "files": [{ "url": "https://…/my.xex", "mirrors": ["https://…"], "size": 12345, "sha256": "<64 hex>",
              "install": { "handler": "plugin", "dest": "any", "dest_path": "Plugins", "dashlaunch": { "slot": 3 } } }] }] }
```

Handlers: `xex` (Games/ or Apps/), `plugin` (Plugins/), `tu` and `dlc` (the game's `000B0000` / `00000002` folder, package checked), `trainer` (Trainers/), `patch` (Patches/ or Games/). `.zip`/`.7z`/`.rar` downloads are unpacked into `dest_path`; name the plugin inside with `dashlaunch.file`. Raise `catalog_version` with every change. Full rules in HANDOFF.md ("Mod catalog").

## Using it

- **Open archives:** drop them on the window, click the drop area, or press **Ctrl+O**. You can open many at once.
- **Extract** puts the files in the default place. The "Extract to" setting at the top can be:
  - **Next to archive** (default): `Game.zip` becomes a `Game` folder beside it.
  - **A chosen folder**: every archive gets its own folder in there. With "Extract as soon as they open" ticked, archives unpack the moment you drop them.
  - **Ask every time**.
- **Extract to…** always asks for a folder. **Extract here** puts the files right beside the archive, with no extra folder.
- **If it already exists:** *Add a number* (`Game-2`), *Replace files*, or *Skip archive*.
- **Passwords:** Unpack asks when it needs one, and asks again if the password is wrong.
- **Queue:** each archive gets a row with its own progress bar and Cancel button. By default two archives extract at a time and the rest wait (Settings → Jobs at once). **Recent** lists past jobs, and **Show** opens the result in Explorer.
- **Preview:** click a text or image file in the list to see it, or use **Save file** to save just that one file.
- **Create archive:** click the button at the top, or drop files or folders that aren't archives. Pick ZIP (opens anywhere) or 7Z (smaller), choose a compression level, and optionally set a password. With 7Z, the password also hides the file names. Passwords can't contain `"` on Windows (a 7-Zip limitation).
- **ISO → GOD:** click the button, or drop `.iso` files. Unpack reads each game's name (you can edit it), you pick an output folder, and it converts the ISOs one by one with progress and Cancel. Copy the `TitleID` folder it makes (for example `4D5307E6`) to `Content/0000000000000000/` on your console's drive. Only convert games you own.

- **Batch extract + auto ISO → GOD:** set "Extract to" to **A chosen folder**, tick **Auto-convert ISOs to GOD** (on the main screen or in **Settings**, the gear at the top right; it's off by default), then drop or pick many archives at once. Each archive extracts into its own folder inside the chosen folder. Every Xbox 360 / original Xbox `.iso` that came out of an archive is then converted, one at a time, into a new `Game Name (GOD)` folder in that same output folder. Each ISO shows as its own row under its archive, with progress, a Cancel button and a clear result. ISOs that aren't Xbox games are skipped and kept, with a message. An existing `(GOD)` folder is never reused or overwritten; a second one becomes `Game Name (GOD)-2`. This doesn't run with **Extract here**.
  - **Straight into a console Content folder:** I made Unpack treat `Content\0000000000000000` (or the `Content` folder, or a drive that holds one) as special. If you pick it as the output, GOD games go in only as `<TitleID>\<type>\<MediaID>` plus its `<MediaID>.data` folder, with no `Game (GOD)` or archive-name folder around them. Another disc of the same game joins its TitleID folder, and a disc that's already there is skipped. A package's header and its `.data` folder always move together: if only half of a package is in the archive or already on the drive, I leave it alone and tell you (use **Replace files** with a complete package to fix a half one). With **Replace files**, I first write a small note (`.unpack-replace.…json`) next to the package that records which files are the old copy and which are the new one, including a fingerprint of the new copy (the header's full checksum, and for the `.data` files their sizes plus checksums of the first and last MiB of each, which is far faster than hashing gigabytes and still tells copies apart), then set the old header and `.data` aside, move the new pair in (never on top of anything that's already there, and never merging two folders), and only then delete the old one. Only one job at a time places or repairs packages in a given `<TitleID>\<type>` folder. That covers converting an ISO straight into the Content folder as well as extracting an archive there. The rule is enforced with an operating-system lock, so it holds for jobs in another Unpack window, another Windows user session, or another PC using the same drive, and it's released automatically if Unpack is killed; a job that has to wait says so and can be cancelled. On Windows the lock is a small `.unpack-replace.lock` file in that folder that I hold open so it can't be deleted or replaced while a job is running (on Linux I lock the folder itself), and after taking the lock I check it's still the file at that path, retrying if it isn't. Every move into the Content folder is no-overwrite at the operating-system level, so even if something goes wrong, nothing already there is ever written over. If a Replace fails partway, I take back only what still matches the fingerprint of what I moved in, and I never put the old copy back over something; anything I can't prove is mine I leave where it is and tell you (with the two exceptions under Known issues above). If Unpack is killed in the middle, the next time I put that game into the Content folder I read that note and repair the package as a whole: if the new pair is complete it stays, otherwise whatever part of the new copy had arrived is set aside (as `.unpack-new.…`) and the old pair comes back together. That repair is itself safe to interrupt and simply picks up again next time. I only act on a note that is exactly what I write: at most 64 KiB, one package, its own names, a known step. A note that doesn't pass that check, a note I can't read, leftover `.unpack-old.…` files without a note, and half-written notes (`.unpack-replace.….tmp`) that I can't prove are mine, I don't touch; I tell you which files to check. A half-written note that is provably mine is removed. Half-extracted staging folders (`.unpack-tmp-…`) from a killed run are removed the next time I extract into that folder, but only when their lock file is a real file, carries my signature, and nothing is still using it; any I can't prove are mine, I only mention. Files that aren't part of a GOD package stay out of the Content folder, with a note. An archive that holds ISOs is extracted into a folder **next to** the Content folder (e.g. `X:\Game`), never inside it; the GOD game still goes into the Content folder, and with delete-ISO on, the emptied extraction folder is removed afterwards. A TitleID folder that is a link (or junction) is refused.
  - **Delete the extracted ISO after a successful convert** (Settings, off by default, asks you to confirm) saves disk space. It deletes only the ISO copy Unpack just extracted, and only after the GOD files are checked on disk. Your archives are never touched. While an ISO converts, you need room for both the ISO and the GOD copy.
- **DLC / title updates:**
  - After an archive extracts into its own folder, any Xbox 360 DLC inside is copied into its game automatically. Turn this off in Settings → Xbox 360 DLC.
  - Unpack finds the game by TitleID in the **Game library folders**: the chosen output folder, plus any folders you add in Settings. It understands both Unpack's `Game (GOD)/<TitleID>` layout and a console's `Content/0000000000000000/<TitleID>`.
  - **Scan drives for Content folders** only looks at `<drive>:\Content` and a few common top-level paths, and lists what it finds for you to **Add** one by one. It never crawls a whole drive.
  - If the game isn't found, the row says *Game not found (TitleID …)* and the file stays where it is. If a DLC file with the same name already exists, an identical copy is skipped and a different one is saved as `name-2`.
  - **Move DLC instead of copying** is off by default. When on, it deletes the original only after the copy is checked.
  - **Install DLC** (Ctrl+D) does the same for files you pick.
- **Library** (Ctrl+L): every game in your library folders, with DLC/TU counts. **Copy to USB…** asks for the drive's top folder.
- **Send to Xbox:** on the console, turn on FTP (Aurora: Settings → Network; XeXMenu and DashLaunch run it automatically) and note the IP on screen. Most consoles' FTP allows only one connection, so close other FTP apps (FileZilla etc.) while sending.
- **Shortcuts:** Ctrl+O open · Ctrl+, settings · Ctrl+L library · Ctrl+D install DLC · Ctrl+G ISO → GOD · Ctrl+Shift+X send to Xbox · Ctrl+N create archive · ? shortcuts · Esc close.
- **Settings** (gear icon): theme (System / Light / Dark), Auto-convert ISOs to GOD, delete-after-convert, DLC options, game library folders, max download size, Open zips inside zips, Jobs at once, Discord Rich Presence, Extract as soon as they open, Skip junk files.

## The right-click menu

After installing, right-click any `.zip`, `.7z`, `.rar`, `.tar`, `.gz`, `.tgz`, `.bz2` or `.xz` file:
- **Extract here (Unpack)** puts the files next to the archive.
- **Extract to folder (Unpack)** makes a folder named after the archive and extracts into it.
- **Open with Unpack** opens it so you can look inside.

On **Windows 11**, these entries are under **Show more options** (or press Shift+F10). If you select several archives, they all go into the same Unpack window's queue. Double-clicking a `.7z` or `.rar` opens it in Unpack. `.zip` files keep opening the way they do now, so use the right-click entries for those. Uninstalling Unpack removes the menu entries.

Command line: `Unpack.exe file.zip`, `Unpack.exe --extract-here file.zip`, `Unpack.exe --extract-folder file.zip`.

## Building it yourself

- **On Windows:** install Rust (rustup.rs) and Node.js, then double-click `build.bat`.
- **On Linux** (how these files were made): `npx tauri build --target x86_64-pc-windows-gnu` with MinGW and NSIS installed.
- **Tests:** CI (`.github/workflows/test.yml`) runs all of this on every push.
  - `npm test` (theme logic) and `python3 tests/gamelists.py` (TitleID lists and merge script).
  - `python3 tests/make_testdata.py /workspace/testdata` builds the small archive fixtures (needs 7-Zip), then `cd src-tauri && cargo test --lib`.
  - The UI suites in `tests/` (`e2e`, `autogod`, `dlc`, `download`, `mods`, `builtin`, `catalog`, `xbox`, `v160`) drive the real page in headless Chrome through `examples/bridge.rs`; see `HANDOFF.md` ("Tests") for the exact steps.
- **Release build in CI:** `.github/workflows/release.yml` is manual-only. It builds the installer and the portable zip and keeps them as workflow artifacts; it never creates a tag or a GitHub Release.

## Credits and licenses (see `licenses/`)

- 7-Zip `7za.exe` © Igor Pavlov, GNU LGPL (+ BSD-3 parts). Source: 7-zip.org
- iso2god-rs © Ilia Pozdnyakov, MIT. Its built-in list of Xbox 360 TitleIDs (extended to 5,908 with the CSV lists) and names (`src-tauri/vendor/iso2god/src/game_list`, generated from a public titles list by the iso2god-rs project, shipped under the same MIT license) is the offline game reference. The short alias table in `src-tauri/src/gamedb.rs` (official names + nicknames like BO2, MW2, GTA V) was written for Unpack and only uses TitleIDs that are in that list. No lookups happen online.
- Built-in fallback engines in `src/vendor/`: fflate (MIT), 7z-wasm (LGPL, includes unRAR license), node-unrar-js (MIT). Fonts: Fraunces and Outfit (SIL OFL)
- Built with Tauri 2.

## Uninstalling
- Installed version: Settings > Apps > Installed apps > Unpack > Uninstall. Tick "Delete the application data" to also remove settings/history (%LOCALAPPDATA%\com.qeezyxz.unpack; builds before 1.6.0 used com.justin.unpack, which Unpack-Cleanup also finds); leave it unticked to keep them.
- Portable version or leftovers: run Unpack-Cleanup.exe. It lists exactly what it will remove (only Unpack's own HKCU keys, its AppData folder and its temp files) and asks first. No admin needed.

## Status

- **1.6.0**: my later additions below, merged into the reviewed code base (details in `PORT-NOTES.md`).
- **1.5.0**: red error banner, full-Xbox-drive and lone Disc 2 messages, same-folder jobs wait their turn (the "unknown TitleID stops the convert" rule became a warning in 1.6.0).
- **1.4.0**: safer copies and Send to Xbox, nested archives, jobs-at-once setting.
- **1.3.0**: original dark UI and Unpack mark.
- **1.2.0**: Discord Rich Presence.
- **1.1.1**: full TitleID lists and Disc 1 / Disc 2 merge.

Per-release notes are in `.github/release-notes-*.md`.

## License

You may copy, modify and share the source under [LICENSE](LICENSE) (hobbyist / non-commercial: don't sell Unpack). See [DISCLAIMER.txt](DISCLAIMER.txt) and [THIRD-PARTY.md](THIRD-PARTY.md).
