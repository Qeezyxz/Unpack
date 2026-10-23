# Unpack — handoff for the next developer

Unpack is a Windows desktop archive tool (Tauri 2, Rust + plain JS) with Xbox 360 hobbyist
features: batch extraction with a bundled 7-Zip, ISO → GOD / extracted XEX folder, DLC install,
a game Library with Copy to USB, a local Mod Menus file manager, Homebrew Tools reference cards,
and user-initiated downloads from a pasted link, and Send to Xbox (FTP upload to a modded console on
the LAN). It is offline by design: the page has no network access, and the only network code is the
Rust download and ftp modules, run when the user clicks.

Hard product rules (keep them):
- No bundled mods, mod-menu names, catalogs or links: `resources/menus/menus.json` ships as `[]` (see "Adding built-in menus") and "Known mod names" starts empty and is user-built.
- Network only from Rust (`download.rs`, `ftp.rs`), only on click. The CSP allows no remote origins.
- Per-user only: HKCU registry, no admin. Never overwrite user files; never write through links.
- Don't push to any remote without the owner asking.

## Architecture

```
src/ (frontend, no bundler; ES modules served from disk)
  index.html  theme.js  native.js  app.js  vendor/ (fflate, 7z-wasm, unrar, fonts)
      │  window.__TAURI__.core.invoke(cmd, args)
      ▼
src-tauri/src/lib.rs  — Tauri commands, path grants check, job registry, events ("job-progress")
      │
      ├─ archive.rs    7za driver: list/read/extract/create, collision modes, atomic placement
      ├─ god.rs        ISO → GOD (vendored iso2god), verify, ISO token registry, delete-ISO rules
      ├─ iso_extract.rs ISO → extracted folder (default.xex), byte verify vs ISO before delete
      ├─ dlc.rs        STFS (CON/LIVE/PIRS) parse, install into <TitleID>/00000002, drive scan
      ├─ library.rs    scan game libraries, Copy to USB (GOD + DLC + TU, or extracted folder)
      ├─ mods.rs       identify mod files (XEX/STFS/folder) + copy to Plugins/Content/as-is
      ├─ modnames.rs   user-learned mod name → TitleID (normalize, validate, lookup)
      ├─ gamedb.rs     offline TitleID → name (iso2god list, merged with the full Xbox/360 CSVs in `src-tauri/vendor/iso2god/src/game_list/`: 5,908 titles) + alias table + fuzzy guess
      ├─ download.rs   SSRF-hardened http(s) download, pinned DNS, resume, SHA-256, sanitizer
      ├─ ftp.rs        Send to Xbox: LAN-only FTP client (Aurora/XeXMenu/DashLaunch), upload/resume/verify
      ├─ discs.rs      disc-number parsing ("Disc 2", "CD1"...), multi-disc grouping and notes
      ├─ destlock.rs   per-destination wait locks (jobs writing the same folder queue up)
      ├─ discord.rs    Discord Rich Presence worker thread (local IPC, off toggle, no network from the page)
      └─ safecopy.rs   grants registry, no-link writes, .partial + hash verify, FAT32 4 GiB check
src-tauri/vendor/iso2god   MIT iso2god-rs (patched: directory depth 64 + cycle detection)
src-tauri/examples/bridge.rs  TEST ONLY HTTP bridge exposing the same commands to headless Chrome
src-tauri/windows/hooks.nsh   NSIS hooks: Explorer verbs under HKCU\Software\Classes\SystemFileAssociations
cleanup/cleanup.c             Unpack-Cleanup.exe (per-user leftovers remover, MinGW)
tests/                        UI e2e suites (puppeteer) + fixture generators
```

Long jobs run in `spawn_blocking`, report progress through the `job-progress` event, and are
cancellable through the `Jobs` registry (`cancel_job`).

## Module map

### Rust (`src-tauri/src`)
| File | What it owns |
|---|---|
| `lib.rs` | Every `#[tauri::command]` (see `generate_handler!` list), CLI/Explorer arg parsing (`--extract-here`, `--`), single instance, launch queue, toast/taskbar progress. Commands check paths with `grants.check(..)`. |
| `archive.rs` (+`archive/tests.rs`) | Finds 7za (`UNPACK_7ZA`, next to exe, dev fallbacks). Windows-safe argument quoting, list files for awkward names, zip-slip checks, collision modes Number/Skip/Replace, `PLACE_LOCK` + atomic `create_dir` claim, `safe_segment` name cleaner (shares `download::is_windows_reserved_stem`). Returns the exact ISO/DLC lists it placed. |
| `god.rs` (+`god/tests.rs`) | `convert`/`convert_named` into a temp folder then swap in. Truncation check, free-space check, deep verify (every data block and hash list, header rebuilt byte for byte). Extracted-ISO registry: random single-use tokens, identity (file id/ctime/size) + sealed full hash. |
| `iso_extract.rs` | XDVDFS extract via the vendored reader. `verify_tree` (counts/sizes) and `verify_content_vs_iso` (byte compare) before any delete. `folder_title_id` reads default.xex/xbe. |
| `dlc.rs` (+tests) | STFS header parse (TitleID, content type, display name), install with no overwrite (`name-2`), title updates → `000B0000`, drive scan for `Content` roots (user approves each). |
| `library.rs` | Game scan (GOD `<TID>/00007000`, extracted XEX folders), DLC/TU counts, Copy to USB (plan, identical skip, conflict stops before writing). |
| `mods.rs` | `identify_learned`: header TitleID > learned name (unless the name confidently points elsewhere) > alias/fuzzy guess. `copy_mod` modes plugins/content/as_is. |
| `modnames.rs` | `normalize` (strips versions, menu/site/generic words incl. plural/leet, the chosen game's names), `key_problem` (refuses short keys, generic words, any game name/alias incl. space-stripped forms), `learn_key`, `lookup` (longest key wins; stored keys re-validated). |
| `gamedb.rs` | `name(tid)`, `guess(texts)`, `confident`, `search` for the game picker. |
| `download.rs` | See security model. Also hoster rewrites (Google Drive, Dropbox, MediaFire by exact host), `list_archives` for folder drop. |
| `ftp.rs` (+`ftp/tests.rs`) | `check_target` (IPv4 LAN only), `Ftp` (control conn, multi-line replies, PASV to the console's own IP, LIST Unix/DOS parse, SIZE w/ LIST fallback, MKD, REST+STOR, RNFR/RNTO, 421 retry), `destination` (god/dlc/tu/xex/plugins/custom), `custom_dest` (user-edited folder: FATX segments, first segment must be a drive name, existence checked by `mkdirs` before writing; `ftp_dest` command pre-fills/validates, `ftp_send` re-validates), `send` (plan: one LIST per remote folder, conflicts stop before sending; workers over 1–2 connections; `.part` → rename → SIZE verify; MB/s + ETA in the progress label). lib.rs: `ftp_list`, `ftp_send` (grant-checked `src`), app-wide `FTP_BUSY` lock (one session at a time). |
| `mods.rs` built-in menus | `load_builtin(dir)` reads `<resource_dir>/menus/menus.json` at startup (lib.rs `setup`, managed as `BuiltinSet`, files granted session-only); `merge_learned` adds built-in name keys after the user's (user keys win). Commands: `builtin_menus`; `mod_identify` merges the seed. |
| `catalog.rs` (+`catalog/{ini,target,install,tests}.rs`) | Mod catalog. `catalog.rs`: schema types (`deny_unknown_fields`), `verify` (Ed25519 `verify_strict` over the exact bytes, before any parsing), `check_entry` validation, `dest_segments` (per-handler allowed folders), `Store` (app data `catalog/`: `config.json`, `seen.json` rollback map, `journal.json`, `backups/`, `lkg/`, `stage/`), `refresh` (remote via `download_with`, or grant-checked local file + `.sig`), `activate` (aliases → `gamedb::set_extra_aliases`, mod names → learned keys), `call` dispatcher (one `catalog` Tauri command; same in the bridge). `ini.rs`: lossless launch.ini parser, `choose_slot`, `validate_change`. `target.rs`: `Target::Local` (granted root, no links) / `Target::Console` (FTP: `send`, `retr`, `dele`, `.new` + rename for launch.ini). `install.rs`: `preflight`, `download_all`, `stage`, `install`, `uninstall`, `states`, journal types. |
| `bin/catalog_sign.rs` | `unpack-catalog-sign` CLI: keygen / pubkey / sign / verify / check. Not in the installer; shipped as a separate exe in `dist/`. |
| `discs.rs` | `stem_without_disc`, disc number from header or file name, `companions()`, session registry so discs of one game share one output folder. |
| `destlock.rs` | `acquire(path, progress)` returns a guard; a second job for the same canonical destination waits (emits `WAIT_MESSAGE`) inside its `spawn_blocking` job. Used by `copy_to_usb`, `mod_copy`, `create_archive`. |
| `discord.rs` | Presence worker (`discord-rich-presence`, MIT). Commands `discord_set_enabled` / `discord_set_activity`: the page sends only a view name plus a count label, Rust builds the text via `activity_for`. Setting `unpack.discord` (default on) switches it off. |
| `safecopy.rs` | `Grants` (session-only path allow-list), `is_link`, `plain` (path display), partial + verify copy. |

### Frontend (`src`)
| File | What it owns |
|---|---|
| `index.html` | Shell; loads fonts, `theme.js`, `app.js`. No remote URLs. |
| `theme.js` | Light/dark/system theme before first paint. |
| `native.js` | Tauri bridge, `settings` (incl. `maxJobs` 1-4 default 2, `nested` default on, `discordPresence` default off (R15); all `localStorage` keys `unpack.*`, validated on read), pickers (Rust `pick_path`/`pick_save` so picks are granted), event helpers. |
| `app.js` | The whole UI (one render function + bind functions): desk/queue/history, preview, create archive, ISO → GOD/XEX dialog, Auto ISO mode (Off/GOD/XEX), DLC rows, Library, Mod Menus (Your files / Download from link / Known names / Catalog: `catalogView`, `renderCatalog`, `bindCatalog`), Homebrew Tools, Settings. Send to Xbox (`xboxView`: per-item editable destination pre-filled by `ftp_dest`, Browse / Reset to default / Last-folder chip (`unpack.xboxLastDest` per type) / Send all to this folder; console form + saved consoles `unpack.consoles`, remote browser, send queue; Library/Mods buttons call `sendGameToXbox`/`sendModToXbox`). All dynamic text goes through `escapeHtml`. Browser-only fallbacks: fflate, 7z-wasm, unrar wasm (only when 7za is missing). |

## Security model
- **CSP** (`tauri.conf.json`): `script-src 'self' 'wasm-unsafe-eval'`, no remote origins, no `unsafe-eval` (the unrar bundle is patched by `scripts/patch-unrar.py`). Capabilities: dialog, opener (reveal + https URLs only), notification, progress bar.
- **Path grants** (`safecopy::Grants`): commands that write or read user paths only accept paths the user picked in a Rust dialog, dropped (session only, links skipped), or that came from Unpack's own scan/extraction output. `grant_dir` was removed; the page can't add grants. GOD/XEX `dest`, mod/USB copy targets, DLC install, extract dest and create outFile are all gated. The test bridge enforces the same rules (`__test_drop_grant` exists only there).
- **No writes through links**: targets and every folder walked under them must be real directories (no symlink/junction/reparse point); Replace never descends into a link. Copies go to `.partial`, are hash-verified, then renamed; never over an existing file.
- **Downloads (SSRF)**: http/https only. The host is resolved once, every address is checked against `is_blocked_ip` (loopback, private, link-local, CGNAT, multicast, test nets, ULA, site-local, 6to4, NAT64, v4-mapped/compatible), and ureq is given a pinned resolver that returns only those checked addresses, rebuilt for every redirect hop (DNS-rebinding defense). Redirects: max 7, no https→http, re-checked each hop. Size cap (settings, default 20 GB), idle 120 s, overall 1 h, HTML-page refusal, optional expected SHA-256 (must be 64 hex, else error), mismatch → quarantine as a unique `.bad-hash` name. Resume: only with a strong ETag or Last-Modified, Content-Range start must match, validators re-compared on 206, `.partial`/`.meta` opened no-follow, a hard-linked partial (nlink / `GetFileInformationByHandle` link count > 1) restarts instead of resuming. Filenames sanitized (Windows reserved names incl. CONIN$/CONOUT$ and superscript COM/LPT, trailing dots/spaces, bidi controls). `UNPACK_TEST_ALLOW_LOOPBACK` only exists under `cfg(test)` or feature `download-test-loopback` (bridge); the release exe has no copy of it.
- **Send to Xbox (FTP)**: target must be an IPv4 literal in 10/8, 172.16/12, 192.168/16 or 169.254/16 (the inverse of the downloader rule; no host names, no DNS, no IPv6); loopback only under the same test flag as downloads. Passive only; the 227 host is ignored and data goes to the console's own IP (no FTP bounce). CR/LF refused in user/pass/paths; every remote segment FATX-checked (≤42 chars, printable ASCII, no `"*+,/:;<=>?\|`, no `.`/`..`). Source must pass `grants.check`; the walk uses `symlink_metadata`, refuses a link root, skips inner links/junctions (counted), opens files with O_NOFOLLOW / FILE_FLAG_OPEN_REPARSE_POINT and re-checks the handle. Never overwrites: same-size remote = skip, other size = whole job refused before any STOR; uploads go to `<name>.part` (or a 42-char-safe `…~.part`), resume with REST from the `.part` size, rename, then SIZE (LIST fallback) must equal the local size. No hashing over FTP (size check only). Passwords are kept in localStorage for saved consoles (Xbox FTP is plaintext anyway; default xboxftp).
- **Delete-ISO rules** (setting off by default, confirm dialog): only an ISO that Unpack itself just extracted (Rust registry token, not a page path); never when the extraction merged into an existing folder (Replace); identity + sealed full hash must still match; GOD: output deep-verified (all blocks, hash lists, header byte for byte); XEX: file count/sizes and every file byte-compared with the ISO; cancel at any stage keeps the ISO; tokens are random, single-use, expiring.
- **Crafted ISOs**: truncation refusal; vendored directory reader caps depth at 64 and detects cycles (error, not a crash).
- **Import/export** (saved links and known names): CSV formula escaping (`'` before `= + - @ tab CR`), http(s)-only links, length caps, de-dupe within a file and against the list, TitleIDs exactly 8 hex and known to the DB, file size caps (2 MB links / 1 MB names).

## Adding built-in menus
The mechanism ships EMPTY (Justin adds entries himself): `src-tauri/resources/menus/` holds only `README.txt` and `menus.json` = `[]`,
bundled via `bundle.resources` (`"resources/menus/": "menus/"`). To add one, put the file in that folder
(subfolders allowed) and an entry in `menus.json`:
```json
[{ "name": "Display name", "file": "sub/file.xex", "titleId": "4D5307E6", "version": "1.0", "sha256": "<optional 64 hex>" }]
```
At startup `mods::load_builtin` validates each entry and skips (logs) bad ones: unknown fields refused;
`file` relative, only normal components, no `..`/drive letters/absolute, no link or junction anywhere on
the path, must be a regular file whose canonical path stays inside the folder; `titleId` exactly 8 hex
and known to `gamedb`; `sha256` (if given) 64 hex and must match; max 500 entries, `menus.json` ≤ 1 MB,
duplicates skipped. Valid entries show in Mod Menus as read-only "Built-in" items (no Remove/notes/game
change, never saved into `unpack.mods`), can be copied/sent like any mod (their paths are session-granted),
and their names become learned-name keys appended after the user's (a user key always wins).
Tests use dummy fixtures only (`mods/builtin_tests.rs`, `tests/builtin.mjs`); `shipped_folder_entries_are_all_valid`
loads the real `resources/menus/` and fails if any entry has a problem (empty is fine; Justin fills it).

## Mod catalog
Ships EMPTY: no catalog URL, key or entries are bundled; `config.json` starts with `source: ""` (off) and
`refresh` refuses to run without a pinned key. Flow and rules:
- **Trust:** the user pins the owner's Ed25519 public key (64 hex) in the Catalog tab (Rust-side `config.json`;
  the page can only set it through `config_set`, which validates). Remote: `url` + `sig_url` (default `url.sig`),
  both https (plain http only with the test loopback switch), fetched through `download_with` (SSRF/DNS-pin
  rules, 4 MB / 1 KB caps). Local: a picked (granted) `catalog.json` + `catalog.json.sig`, no links.
  The signature (128 hex) is verified with `verify_strict` over the exact bytes *before* `serde_json` sees them.
  `catalog_version` lower than the highest seen for that key is refused (rollback).
- **Validation** (`check_entry`): ids `[a-z0-9][a-z0-9._-]{0,63}` unique; TitleID valid; https URLs, ≤5 mirrors;
  `size` 1 B–20 GB; `sha256` mandatory 64 hex; handler in xex/plugin/tu/dlc/trainer/patch; `dest` hdd/usb/any;
  `dest_path` FATX segments inside the handler's allowed roots (xex: Games/Apps, plugin: Plugins, tu/dlc: exactly
  `Content/0000000000000000/<TID>/000B0000|00000002`, trainer: Trainers, patch: Patches/Games; never the drive
  root, so launch.ini can't be a target); `dashlaunch` only for plugins, slot 2–5; no two files on one name;
  paid or non-redistributable entries need `source_page`; paid entries' files are dropped. Bad entries are skipped
  and listed (UI "N entries skipped").
- **Preflight** (`preflight`, also run inside `install`): paid / not redistributable → blocked; same version
  already installed → blocked; `requires` must be installed (complete) on that target; `conflicts` checked both
  ways against the journal; free space (local only; console shows an info line); FAT32/FATX 4 GB per file;
  TU: reads the STFS headers in `<TID>/000B0000` (local or FTP `retr` of 0x491 bytes), TU number from the display
  name ("Title Update 18", "TU18"); none installed counts as TU 0; mismatch → blocked, unreadable number →
  warning; `dash_min` is informational (not readable); dest hint mismatch and a missing launch.ini → warnings.
  Warnings need `acceptWarnings` (UI "Install anyway").
- **Install:** DashLaunch slot is decided before downloading (`SLOT_OCCUPIED:<n>:` error → UI asks → retry with
  `confirmSlot`). `download_all`: url then mirrors; retries with backoff (1 s, 4 s) only for transient errors,
  hash/size/blocked/404 move to the next mirror; ≤2 files at once; cancel interrupts the backoff. Files land in
  `stage/<id>-<ns>/`, are re-hashed, archives unpacked with 7-Zip (`Layout::Here`), then `walk_stage` refuses
  links, special files, non-FATX names, depth > 16 and anything canonicalizing outside the stage. Handler checks:
  xex needs an XEX2 file; plugin file must be XEX2; tu/dlc packages must be the right kind for the entry's TitleID.
  Update: the old version is uninstalled after the new one is staged. The journal record is written
  (`complete: false`) before copying, so a failure part-way can still be uninstalled. Local copy: all destinations
  checked first (identical = skip, different = stop before writing), then `copy_file_safe`. Console: `ftp::send`
  (same plan/no-overwrite/.part/SIZE rules as Send to Xbox) under the app-wide FTP lock.
- **launch.ini:** read whole (≤64 KB, UTF-8, no NUL); value `Usb:\…` for local targets, `Hdd:\…`/`Usb:\…` for
  console drives; `plugin1` never written; lowest free of plugin2–5 or the catalog's slot; a used slot only with
  `confirmSlot`. Before writing: `validate_change` (re-parse; every other key identical), timestamped backup on the
  target (`launch.ini.unpack-YYYYMMDD-HHMMSSZ[-n].bak`, ≤ 42 chars, never overwritten) and in `backups/`
  (`<target hash>-<stamp>-<ms>-<counter>-launch.ini`, created with `create_new`, never replaced; R12 LOW-3).
  `Target::replace` never leaves the drive without launch.ini (R12 HIGH-1): the new file is written beside it
  (local `.unpack-new`; console `.new.part`, size-checked), the original is moved aside to `launch.ini.old`
  (`.old-2`… if taken; an existing `.old` is never touched), the new one is renamed in and read back / size-checked,
  and only then is the old one deleted. On any error after the move the original is put back (rename; on the console
  a re-upload of its bytes if that rename fails too) and the error says whether that worked. Console recovery runs
  on a FRESH connection (the failed one is closed first, since Aurora allows one connection), up to 3 tries with
  backoff (R13 MEDIUM-1); if the new file turns out to be fully in place, the install counts as done.
  `.old` names: `.old`, `.old-2`…`.old-99`, then a time-based `.old-<n>`; never refuses (R13 LOW-2).
- **`repair_ini`** runs in preflight (so before every install) and before every uninstall/launch.ini edit: if
  launch.ini is missing and an Unpack `launch.ini.old`/`.old-N` exists, the newest (preferring one byte-identical to
  the latest app-side backup for this target) is renamed back and the user gets a note; stray `launch.ini.new.part`
  / `.unpack-new` are removed; with launch.ini present and parseable, `.old*` files byte-identical to one of
  Unpack's backups for this target (or to the current launch.ini) are deleted. A user's own `.old` is left alone.
  If the rename back fails, the action is blocked with a message telling the user to rename it themselves.
  Names match case-insensitively everywhere (`LAUNCH.INI`, `Launch.ini.OLD-2`, `.New.part`; R14 MEDIUM-1):
  `ini_name(t)` resolves the real name and every read/replace uses it. An `.old` is only restored if it is
  non-empty, ≤ 64 KB, parses as an ini (UTF-8, no NUL) and — when Unpack has backups for this target — is
  byte-identical to one of them; otherwise nothing is touched and the blocker names the file and the reason
  (R14 LOW-1). Time-based `.old-<unix seconds>` names sort after `.old-99` by recency. `repair_ini` and
  `Target::replace` share a process-wide lock (`target::ini_lock`), so a repair can't see a half-done local swap.
  Swap failure messages say only what is known (R14 LOW-2/3): drop during the aside move → "most likely not
  moved"; new file renamed in but unverified → "renamed into place but couldn't be verified… checks on next
  connect"; only a refused rename-in says "could NOT be put back yet". `write_ini` then reads
  back and compares (restore on mismatch) and keeps the new file as last-known-good in `lkg/`. Uninstall
  removes/restores only a line that still has Unpack's value and that no other installed mod on the target uses.
- **Journal** (`journal.json`): per target key (`local:<path>` / `console:<ip>/<drive>`) and entry id: version,
  files (path, size, sha256), backups, ini changes (key, value, previous). Drives the Installed / Update available /
  Partly installed badges. Uninstall: local files removed only if the SHA-256 still matches, console files only if
  the size matches; changed files are kept and listed; empty folders Unpack made are removed (local). A file (or
  launch.ini line) that another journal record on the same target also lists is kept and reported as
  "kept (used by X)" — reference counting by journal (R12 MEDIUM-1). A launch.ini change kept for another mod is
  handed to that mod's journal record, so the last user's uninstall removes the line (R13 LOW-1).
  Note: the same USB stick used as a local drive and as console `Usb0` gets two target keys (`local:<path>` vs
  `console:<ip>/Usb0`), so the two journals don't see each other's shared files; uninstall from the same kind of
  target you installed with. `journal.json` / `seen.json` that exist but
  don't parse are errors, never "empty" (R12 LOW-2): a copy is saved as `<name>.bad-<stamp>` (once per content),
  the original is left in place, and refresh/states/preflight/install/uninstall refuse with a message naming the
  file until the user fixes or deletes it (deleting seen.json drops the rollback floor; deleting journal.json
  forgets installs).
- **Unpack limits** (R12 LOW-1): an archive's listed total (7-Zip listing) must be ≤ `unpack_cap(size)` =
  4× the declared download size, at least 256 MB, at most 20 GB, and fit the staging drive's free space + 64 MB;
  the real total after unpacking is checked again. Before downloading, staging free space must cover the sum
  of declared sizes + 64 MB.
- **Console drives:** every target picker uses the drives the console lists at its root (`ftp_list "/"`,
  names matching `ftp::is_drive`: Hdd1, HddX, Usb0, Usb1, UsbMu0…). Default: the drive remembered for that
  console (`unpack.xboxDrives`, key `host:port`, IPv4 octets ≤ 255 and port 1–65535, else dropped; leading zeros normalized, so `10.0.0.07:021` → `10.0.0.7:21`), else Hdd1, else the first USB, else the first drive. Send to Xbox
  has a "Send to drive" picker above the browser (applies to all unedited queue items) plus the per-item select;
  "Send all to this folder" also sets each item's drive to the browsed path's drive. The catalog's console target
  lists drives when you pick the console (or **List drives**). Picks are remembered per console.
- **Detection:** `activate` adds catalog `game.aliases` to `gamedb::guess` (same scoring as built-in aliases) and
  mod names as learned keys (via `learn_key`, so generic/game-name keys are refused); `mod_identify` merges user →
  built-in → catalog keys (user wins).
- Tests use dummy fixtures only (`catalog/tests.rs`, `tests/catalog.mjs`): generated keys, invented ids, fake
  XEX2/STFS headers, local HTTP + fake FTP servers.

## Build
Done on Linux (Debian 13) and cross-compiled:
1. Toolchain: Rust stable + `rustup target add x86_64-pc-windows-gnu`, `mingw-w64`, `nsis` (makensis), Node 20 (`npm install` for `@tauri-apps/cli`).
2. Bundled 7-Zip: `src-tauri/binaries/7za-x86_64-pc-windows-gnu.exe` (and a Linux 7za for dev). `tauri.conf.json` `externalBin` ships it next to `unpack.exe`.
3. `npx tauri build --target x86_64-pc-windows-gnu` → `src-tauri/target/x86_64-pc-windows-gnu/release/unpack.exe` and `bundle/nsis/Unpack_1.0.0_x64-setup.exe` (WebView2 `embedBootstrapper`, NSIS hooks in `windows/hooks.nsh`).
4. `dist/`: copy the installer; `cargo build --release --bin unpack-catalog-sign --target x86_64-pc-windows-gnu` → `dist/unpack-catalog-sign.exe`; portable zip = `Unpack.exe` + `7za.exe` + `WebView2Loader.dll` from the release folder + the `menus/` folder (`7za a -tzip`). `Unpack-Cleanup.exe` is built from `cleanup/cleanup.c` with MinGW.
5. On Windows, `build.bat` does the native MSVC build instead.

Disk: a full `target/` is ~10–15 GB. Reuse one target dir; delete `src-tauri/target` when done.

## Tests
Rust (from `src-tauri`):
- `cargo test --lib` — or `--release`; must pass in default parallel mode (env-var tests share a poison-tolerant `env_lock()`). Currently 222 (+1 ignored benchmark). Archive fixtures are generated first with `python3 tests/make_testdata.py [dir]` (`/workspace/testdata` or `UNPACK_TESTDATA`). Retry classification (`install::is_permanent`) ignores URLs: a random test port like 40407 once made a 503 look like a permanent 404 (the R14 LOW-4 flake in `cancel_stops_downloads`); 24 runs × 4 in parallel passed after the fix. Temp folders use `testutil::TmpDir` (removed on drop); archive fixtures live in `target/test-fixtures`, so nothing is left in `/tmp`.
- Wine: `cargo test --lib --target x86_64-pc-windows-gnu --features download-test-loopback --no-run`, then
  `WINEDEBUG=-all UNPACK_7ZA='Z:\…\src-tauri\binaries\7za-x86_64-pc-windows-gnu.exe' wine target/x86_64-pc-windows-gnu/debug/deps/unpack_lib-*.exe` (173; unix-only symlink cases are skipped there; copy `WebView2Loader.dll` from `target/x86_64-pc-windows-gnu/debug/build/webview2-com-sys-*/out/x64/` and `7za.exe` next to the test exe first).

UI e2e (headless Chrome + `/workspace/testenv` puppeteer-core, real Rust via the bridge):
1. `cargo build --example bridge --features download-test-loopback`
2. From `src-tauri`: `UNPACK_TEST_ALLOW_LOOPBACK=1 UNPACK_TEST_DRIVES=/tmp/unpack-dlc-e2e/usb ./target/debug/examples/bridge ../src &` (port 18790, `UNPACK_BRIDGE_PORT` to change)
3. From the repo root: `node tests/e2e.mjs` (42), `tests/autogod.mjs` (15), `tests/dlc.mjs` (16), `tests/download.mjs` (27), `tests/mods.mjs` (39), `tests/builtin.mjs` (9; own bridge on a random port with `UNPACK_TEST_MENUS` dummy fixtures), `tests/catalog.mjs` (36; own bridge + local HTTP + fake_aurora.py; keys from `target/debug/unpack-catalog-sign`, so `cargo build --bin unpack-catalog-sign` first), `tests/v160.mjs` (37: jobs-at-once, nested archives, Discord toggle/presence via the bridge's `__discord_calls` recorder, error banner, unknown-TitleID warning, multi-disc notes, companion prompt), `tests/xbox.mjs` (52, incl. the pyftpdlib LIST-425 overwrite repro and a Usb0+Usb1 console (`fake_aurora.py --drives`); needs `pip install pyftpdlib`, starts `tests/fake_aurora.py`). Fixtures are generated (`make_iso.py`, `make_stfs.py`, big.7z). Screenshots go to /tmp unless `UPDATE_SHOTS=1`. Download tests use local servers only.

Other checks: `npm test` (`tests/theme.test.mjs`, node:test), `python3 tests/gamelists.py` (game-list merge script consistency). CI: `.github/workflows/test.yml` runs all of the above; `.github/workflows/release.yml` is a manual Windows build (installer + portable zip) that never tags or publishes.

FTP benchmark: `tests/fake_aurora.py ROOT PORT [--limit MBPS] [--cons N] [--no-size]` (pyftpdlib that mimics Aurora: no MLSD/MLST/EPSV, one connection; note pyftpdlib counts its listen socket and data sockets in `--cons`), then `BENCH_PORT=PORT BENCH_SRC=/dir [BENCH_PAR=1] cargo test --release --lib ftp::tests::bench -- --ignored --nocapture`.

## Known limits
- Mod catalog: not tested against a real catalog, real DashLaunch or a real console. TU detection relies on the TU package's display name; `dash_min` isn't checked; console free space isn't readable over FTP. On Windows, keygen restricts the key file with `icacls` (warns if that fails); not tested on real Windows. The console launch.ini swap relies on the server supporting RNFR/RNTO (tested on pyftpdlib, not on real Aurora/DashLaunch). A catalog signed with a leaked owner key is trusted (rotate the key in the Catalog tab). Two different mods writing the same file name are refused at copy time, not in preflight.
- Send to Xbox rename window: right before RNTO the final name is checked again, and after RNTO its size is verified (must equal the PC file). FTP has no "rename without replacing", so a file another client creates in the few milliseconds between that check and RNTO can be replaced by ours on servers that let RNTO overwrite; the after-rename size check then reports a mismatch only if the sizes differ. Aurora/XeXMenu are single-user, so this needs a second FTP client writing the same name at the same moment.
- Send to Xbox resume trusts the local record, not the console's bytes: it resumes when the source's path, size, mtime and the SHA-256 of the sent prefix match the record and the `.part` size equals the recorded offset. If something on the console rewrote the `.part` content but kept its size, that isn't detected before the final size check (which compares sizes only).
- Send to Xbox is tested against pyftpdlib and an in-test fake server, not a real Aurora/XeXMenu/DashLaunch. If a console's server lacks RNFR/RNTO the upload is left as `.part` with a clear error. Small files cost ~6 round trips each (PASV, STOR, SIZE re-check of the final name before RNTO, RNFR, RNTO, SIZE); not measured with real WiFi latency (no netem in the build box).
- Windows-only behaviour reviewed by reading, not run on real Windows: NSIS registry/uninstall, Explorer verbs, reparse points/junctions, superscript COM/LPT device names, real WebView2 GUI. Wine covers the Rust side.
- No proof a real console boots the GOD output (header/parts match upstream iso2god-rs exactly).
- A corrupted-but-full-length ISO converts faithfully; Unpack can't know an ISO's "real" content.
- Two ISO entries sharing one directory are refused as a cycle (real Xbox ISOs don't do that).
- Not in the IP block list: 2001:db8::/32 and Teredo 2001::/32 (nothing routes there).
- Hoster rewrites only handle simple public links; no logins, captchas or tokens.
- Known names matching is word-based; a mod named exactly like a game can't be learned (by design).
- Mod game detection by name is a guess unless the header has a TitleID.

## Review history (REVIEW.md = developer notes, REVIEW-overseer.md = independent reviews)
- Upgrades 1–8: Tauri wrapper, offline vendoring, native 7-Zip, extract to folder, Explorer/CLI/single instance, create archives, batch queue, ISO → GOD.
- R1: same-name race (atomic claim), Replace through links, GOD re-convert loss, empty preview, CSP `unsafe-eval` → fixed.
- R2/R3/R4: delete-ISO safety (exact extracted list, truncation, deep verify, identity + sealed hash, tokens, late cancel, header byte check) → fixed; DLC, Library, Copy to USB added.
- R5: no writes through links on targets, path grants, `.partial` + hash-verified copies, FAT32 check; session-only drop grants.
- R6: download hardening; Homebrew Tools, link import/export, ISO → XEX.
- R7: DNS rebinding pin, ISO stack overflow, import/export safety, many download lows.
- R8: known-names collisions (generic/game-name keys refused, conflict asks), parallel test flakiness, hard-link partial, validators, exact TitleIDs, superscript devices, shared reserved-name rule, bridge dest grant.
- R9: PASS on 6bf5f8b. Final pass adds space-stripped/plural/leet key checks and the Windows link-count check.
- Send to Xbox: FTP upload (ftp.rs, UI, fake Aurora e2e, benchmark).
- Built-in menus loader (ships empty) and editable Send to Xbox destinations.
- R10 (FAIL on ff440ff) fixes: a failed LIST aborts the send instead of counting as an empty folder (only a failed CWD means missing); listings over 16 MB abort; the final name is re-checked right before RNTO so a file that appeared is kept; a `.part` is resumed only if its first and last 64 KB match the source (RETR range), otherwise restarted from 0; reply lines capped at 8 KB; REST refused counts as not resumed and MB/s uses the real offset; cargo tests clean up their temp folders.
- R11 (PASS) lows fixed: `.part` head/tail sampling replaced by a local resume record (`<app data>/ftp-resume/<sha256(console|remote .part)>.json`: source path, size, mtime, offset = `.part` size the console reported after the cancel/failure, SHA-256 of those local bytes). Resume only if all match and the remote `.part` size equals the offset, else restart from 0; record removed after success. Cancel now reads the STOR reply so that SIZE can be asked. Size verified after RNTO (window documented in Known limits). Built-in menus test checks every shipped entry is valid instead of requiring an empty folder.
- Round 12 (FAIL on 8291ebf, mod catalog) fixes: launch.ini swap-aside with restore (local + console), shared files/lines reference-counted by journal, unpack-size cap + staging free-space check, corrupt journal/seen refused (copy kept), unique app-side backup names, catalog size messages in MB, signing tool reads the secret from a file or stdin only, Windows ACL on keygen, catalog_version < 2^53. Plus: console drive pickers list every drive (Usb0/Usb1/UsbMu0…), Hdd1 default else first USB, remembered per console.
- Round 13 (FAIL on 87ff978) fixes: console swap recovery on a fresh connection with retries; `repair_ini` restores a missing launch.ini from Unpack's `.old` at the start of every ini operation and in preflight, and tidies `.new.part` / stale `.old`; ini lines handed to the remaining mod on uninstall; `.old` naming never locks out; seen.json / rename / size-cap messages fixed; stored console keys need port 1–65535.
- Round 14 (FAIL on 7e80df3) fixes: case-insensitive launch.ini / .old / .new.part names, `.old` validated before restore (size, ini parse, backup match), swap messages say what is known, flaky retry classification (URL digits) fixed, repair/replace lock, leading-zero port keys normalized. Self-tests: handoff in every order for 3 mods (36 runs) and 4 mods (48), pruning under refused deletes, repair racing a write. Awaiting review.

## Round 16 fixes
- Nested archives get a live byte cap while 7-Zip runs (`run_capped`, archive.rs): output over min(2x listed + 1 MB, budget left, free − 256 MB) kills 7-Zip, removes the partial folder, and leaves the archive closed with a warning. Forged-size gz regression test.
- migrate.rs: staged copy into `<new>.migrating`, size-verified, renamed in one step; free-space check; Downloads moved, not copied; any copy failure means no marker and a retry next start; `.migration-pending` covers a kill after the rename.
- Catalog lock wait shows "Waiting for another catalog install to finish…" and is cancellable (downloads still run under the lock).
- Discord: Rust test with a mock IPC socket (Unix) proves connect + handshake + SET_ACTIVITY only after enabling.
