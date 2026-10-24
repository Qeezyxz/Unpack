# Unpack desktop — review log

Each upgrade was built on its own, then reviewed as a strict code reviewer (re-read the diff, hunted for bugs, fixed, rebuilt, tested) before the next one started. Builds were done on Linux (Debian 13) and cross-compiled for Windows with the `x86_64-pc-windows-gnu` Rust target + MinGW + NSIS.

## Upgrade 1 — Basic Tauri wrapper

**What changed**
- New Tauri 2 project: `src/index.html` (Justin's page, unchanged), `src-tauri/` (Rust shell, config, icons generated from the page's folder logo).
- Window 1180×820, dark background colour matching the page so there's no white flash on start.

**Review findings**
1. *Drag-and-drop dead on Windows.* Tauri intercepts OS file drops by default, so the page's own HTML5 `drop` handler would never fire. Fixed: `dragDropEnabled: false` (upgrade 4 later switches to Tauri's native drop events, which give real file paths).
2. *GNU build needs `WebView2Loader.dll`.* `objdump` shows the exe imports it. The NSIS installer already bundles it (verified with `7z l`), and the portable folder ships it next to `Unpack.exe`.
3. Page still loads code/fonts from CDNs — expected, fixed in upgrade 2.

**Tests**
- `tauri build --target x86_64-pc-windows-gnu` → `unpack.exe` (4.1 MB) + `Unpack_1.0.0_x64-setup.exe` (1.5 MB). ✅
- Installer listing shows `unpack.exe` + `WebView2Loader.dll`. ✅

## Upgrade 2 — Fully offline

**What changed**
- Vendored into `src/vendor/`: fflate 0.8.2, 7z-wasm 1.2.0 (`7zz.es6.js` + `7zz.wasm`), node-unrar-js 2.0.2 (its ESM build uses extension-less imports browsers can't resolve, so it was bundled into one file `unrar.bundle.js` with esbuild) + `unrar.wasm`, and the Fraunces/Outfit fonts (5 woff2 files + local `fonts.css`). Each library's license file sits next to it.
- The inline `<script>` moved to `src/app.js` so a strict Content-Security-Policy can be used.
- CSP turned on: only the app's own files may load; no network origins at all.
- Wording that mentioned downloading engines / "Opera" was updated.

**Review findings & fixes**
1. *Header-encrypted 7z crashed the page.* With no `-p` switch, 7-Zip (wasm) asks for a password on stdin, which Emscripten implements as `window.prompt()` — in a headless test this killed the page; in the app it would pop an ugly prompt. Fixed: a dummy `-p` is always passed and `stdin` returns null; a "Wrong password" result now routes to the app's own password dialog.
2. *Password ZIPs gave scrambled files silently* (pre-existing bug). fflate can't decrypt, and returned garbage marked as success. Fixed: the ZIP central directory is checked for the "encrypted" flag; encrypted ZIPs go to 7-Zip with the password dialog, and a wrong password now fails clearly instead of writing empty files.
3. *ZIP folder entries showed up as 0-byte "files"* (pre-existing, "7 files" for a 4-file zip). Fixed in the fast, streaming and bulky-listing paths.
4. *Deferred 7z member streaming ignored the password.* Fixed.
5. *CSP vs. the RAR engine.* node-unrar-js (Emscripten embind) builds functions with `new Function`, which a strict CSP blocks. Patching embind deeply was judged too risky, so `'unsafe-eval'` is allowed for scripts; this is acceptable because the CSP still forbids loading any script that isn't shipped inside the app.

**Tests** (headless Chrome against `src/`, with every non-local request aborted and the same CSP header as the app)
- Zero external requests; Fraunces + Outfit load from local files. ✅
- `plain.zip` 4 files ✅ · `plain.7z` 4 files ✅ · `pw.7z` (encrypted headers) → password dialog → 4 files ✅ · `pwnohe.7z` ✅ · `pw.zip` + right password → correct 28 B ✅, wrong password → clear error ✅
- `sample.rar`, `FolderTest.rar` (public node-unrar-js test files; no RAR creator on Linux) → listed correctly incl. Chinese names ✅
- File named `<img src=x onerror=alert(1)>.txt` shows as plain text, no dialog fired ✅
- `evil.zip` (`../../`, `/abs`, `..\..\` entries) — names are flattened to safe paths in the in-memory view ✅
- `big.zip` (300 MB) opens ✅

## Upgrade 3 — Native 7-Zip backend

**What changed**
- Bundled **7-Zip 25.01 `7za.exe`** (standalone console build from the official "extra" package) as a Tauri sidecar; it installs next to `unpack.exe`. Its `License.txt` (LGPL-2.1 + BSD-3) ships in `licenses/`.
- New Rust module `src-tauri/src/archive.rs` (plain Rust, no Tauri types, so it's unit-testable):
  - `list` — `7za l -slt -ba`, parsed into entries (path, size, folder, encrypted).
  - `extract` — `7za x -bsp1` streams progress percentages from stdout; Rust forwards them to the page as `job-progress` events (throttled to ~8/s). Files go straight to disk, so archive size is no longer limited by memory.
  - `read_entry` — previews one file (`7za e -so -spd`, capped at 4 MB).
  - `extract_entry_to` — "Save file" for one entry, streamed to disk.
  - Cancel: each running job's 7-Zip process is tracked by job id and killed on request.
- New `src/native.js` + desktop branches in `app.js`: native drag-and-drop (real file paths), native Open dialog (Ctrl+O), previews via Rust.
- **Fallback kept:** if `7za` can't be found, the archive is read by Rust and handed to the original in-page WebAssembly engines (fflate / 7z-wasm / unrar-wasm).
- Test harness (not shipped): `src-tauri/examples/bridge.rs` exposes the same commands over local HTTP backed by the real `archive` module; `tests/test-shim.js` fakes `window.__TAURI__`; `tests/e2e.mjs` drives the real page in headless Chrome.

**Review findings & fixes**
1. *7-Zip could hang waiting for a password on stdin* → stdin is `null` and a dummy `-p` is always passed; "Wrong password"/"Cannot open encrypted archive" map to `PASSWORD_REQUIRED` / `WRONG_PASSWORD` codes.
2. *Zip-slip / link escapes.* 7-Zip already strips `../` and absolute paths, but as defence in depth: extraction runs with `-snl-` (never create symlinks), and afterwards `sanitize_tree` deletes any symlink/junction (Windows reparse point) and anything whose real path resolves outside the destination.
3. *Half-finished output on failure/cancel.* 7-Zip writes into a hidden `.unpack-tmp-<job>` folder; a drop-guard deletes it on every exit path (error, cancel, panic), and files are moved into place only after success.
4. *Cancel race:* a cancel arriving between "spawn" and "register process" would have been lost → re-checked right after registering.
5. *Wildcards in names:* an entry named `*.txt` would match many files when previewing → `-spd` makes names literal.
6. *Console window flash on Windows* for every 7-Zip call → `CREATE_NO_WINDOW` flag.
7. *Non-ASCII names* → `-sccUTF-8` so listing output is decoded correctly.
8. *Progress repaint* re-rendered the whole page many times a second (focus loss in the search box) → progress now only updates the bar and status text, once per animation frame.
9. *Job registry leak:* jobs are now unregistered on every exit path of the command.

**Tests** — `cargo test` (12 tests, all ✅) against real archives made on the box, and the headless UI run (below, all ✅):
- list/extract `plain.zip`, `plain.7z`; ZIP folder entries not counted as files ✅
- `pw.7z` (hidden names) / `pwnohe.7z` / `pw.zip`: no password → `PASSWORD_REQUIRED`, wrong → `WRONG_PASSWORD`, right → correct contents; no staging folder left after failures ✅
- nested folders `nested/deep/er/d.txt` ✅
- `evil.zip` (`../../evil_escape.txt`, `/abs_evil.txt`, `..\..\win_evil.txt`): nothing written outside the destination ✅
- `symlink.zip` (a link to `/tmp` followed by a file "through" the link): no symlink survives, nothing written to `/tmp` ✅
- `big.7z` (300 MB): extracted byte-exact size, progress events seen, ends at 100 ✅; cancelled after 300 ms → `CANCELLED`, destination left empty ✅
- preview: literal name read, >limit → `TOO_LARGE` ✅
- Windows cross-build: installer now contains `7za.exe` + licenses ✅

## Upgrade 4 — Extract straight to a folder

**What changed**
- Destination setting (saved between runs): **Next to archive** (default), **A chosen folder**, or **Ask every time**; "Choose folder" uses the native folder dialog.
- Per-archive buttons: **Extract** (default destination), **Extract to…** (always asks), **Extract here** (files go straight into the archive's folder, no wrapper folder).
- Name conflicts (existing setting, now real): **Add a number** (`Game`, `Game-2`, …; files `a-2.txt`), **Replace files** (merges, overwriting same-named files, keeps others), **Skip archive**.
- If an archive already contains one top folder (e.g. `MyGame/…`), it isn't nested again inside `MyGame/MyGame`.
- "Skip junk files" now removes `__MACOSX`, `.DS_Store`, `Thumbs.db`, `desktop.ini` after extraction.
- Password-protected archives reuse the existing password dialog; a wrong password re-opens it with a "That password didn't work" hint instead of failing. ZIPs (whose names aren't encrypted) are detected as encrypted on open, so the password is asked before extracting rather than half-way.
- Note: the frontend for upgrades 3 and 4 was written in the same pass (they share the same buttons); they were reviewed and tested separately.

**Review findings & fixes**
1. *Auto-extract surprise:* with "Next to archive" as default, "extract as soon as they open" would have unpacked every archive the user merely opened to look inside → auto-extract now only runs when the user has chosen a destination folder (same rule as the web version).
2. *"Skip" did the whole extraction first* → the final folder name is now worked out from the listing before 7-Zip runs, so Skip returns immediately.
3. *Save-file dialog path* mixed `/` and `\` on Windows → separator now follows the archive's path.
4. *Replace into an existing file with a folder's name* (or vice versa) would error → handled in `merge_replace`.

**Tests** (headless UI run against real Rust code; 17/17 ✅)
- drop `plain.7z` → 4 files listed; HTML-looking file name rendered as text, no `<img>` injected ✅
- Extract → `plain/`; again → `plain-2/`; Extract here → files beside archive ✅
- chosen-folder mode → `chosen/plain/` ✅
- text preview loads through `read_entry` ✅
- `pw.7z`: dialog → wrong password → dialog with hint → right password → extracted ✅; `pw.zip`: password asked on open → extracted ✅
- `evil.zip` with Extract here: nothing escapes the folder ✅
- `big.7z` 300 MB with "Ask every time": progress shown, extracted ✅; second run cancelled mid-way → "Cancelled", no temp folder left ✅
- no page errors ✅
- cargo tests for Number/Skip/Replace and single-root handling ✅

## Upgrade 5 — Explorer right-click menu, double-click, command-line

**What changed**
- Installer hook `src-tauri/windows/hooks.nsh` adds three right-click entries for `.zip .7z .rar .tar .gz .tgz .bz2 .xz`: **Extract here (Unpack)**, **Extract to folder (Unpack)**, **Open with Unpack**. They're written under `HKCU\Software\Classes\SystemFileAssociations\<ext>\shell\…` (per-user install, no admin needed) and removed again by the uninstaller.
- `.7z` and `.rar` are registered as file types so double-clicking opens them in Unpack. `.zip` is deliberately *not* taken over (Windows' own zip handling stays default; "Open with Unpack" is in the menu).
- Command line: `unpack.exe <archive…>` opens, `--extract-here <archive>` and `--extract-folder <archive>` extract next to the archive with no clicks.
- Single-instance (tauri-plugin-single-instance): a second launch (e.g. right-clicking 5 archives, or double-clicking while Unpack is open) hands its request to the running window and exits; the window comes to the front.

**Review findings & fixes**
1. *Lost requests at startup:* an event fired before the page attached its listener would be dropped → requests are queued in Rust and the page drains the queue (`take_launch_requests`) on start and on every `launch-request` signal; draining is serialised so two signals can't process the same request twice.
2. *Relative paths from a second instance* would be resolved against the first instance's folder → resolved against the forwarding instance's working directory.
3. *Unknown switches* (WebView/OS flags) were treated as file names → ignored; non-existent paths are dropped.
4. *Hijacking `.zip`* would change how Windows opens every zip → not associated (see above).
5. *Menu didn't appear until Explorer restart* → installer calls `SHChangeNotify(SHCNE_ASSOCCHANGED)`.
6. *Flaky cancel test* (the 300 MB extract sometimes finished before a timer-based cancel) → test now cancels on the first progress report; test temp folders are cleaned up.

**Tests**
- Unit test for argument parsing (actions, relative paths, missing files, unknown flags) ✅ (cargo: 13/13)
- UI run (21/21 ✅): simulated forwarded launches — `--extract-here plain.zip` → files beside archive; `--extract-folder wrapped.zip` → `MyGame/` (not double-nested); `--extract-folder plain.zip` → `plain/`; plain path → opens without extracting.
- Windows build: generated `installer.nsi` includes the hook macros and the `.7z`/`.rar` associations; makensis compiled it ✅
- **Windows-only, not testable here:** the actual registry writes, the menu appearing in Explorer (Windows 11: under "Show more options"), and the uninstaller removing them.

## Upgrade 6 — Create archives

**What changed**
- "Create archive" button in the header, and dropping files/folders that aren't archives opens the same window. While it's open, anything else dropped is added to the list.
- Options: name, **ZIP** or **7Z**, compression (Store / Fast / Normal / Maximum), optional **password** (AES-256; for 7Z the file names are hidden too, `-mhe=on`). Native Save dialog picks where it goes; the extension is added if missing. Progress bar + Cancel.
- Rust `archive::create`: builds into a hidden temp file then renames, so cancel/failure never leaves a half archive and an existing archive with the same name is replaced, not "updated" with extra files.

**Review findings & fixes**
1. *Password quoting on Windows (real bug, found by testing under Wine).* 7-Zip splits its own command line (a `"` only toggles quoting; backslashes are literal), but Rust quotes arguments using MSVC rules, so a password such as `a b\` would silently become a different password. Fixed: every 7-Zip argument is now quoted 7-Zip-style on Windows (`raw_arg`). A `"` can't be expressed to 7-Zip at all, so passwords containing it are refused with a clear message instead of producing an archive nobody can open.
2. *Archive saved inside a folder being archived* would make 7-Zip read its own output → refused.
3. *Stale progress:* each create run gets a fresh job id, so late events from a cancelled run can't move the next run's bar.
4. *AES ZIP compatibility* — Windows Explorer's built-in zip can't open AES-encrypted ZIPs; the window says so and suggests 7Z.

**Tests**
- cargo: ZIP/7Z × with/without password round-trip (names, nesting, progress hits 100, 7Z hides names, ZIP entries unreadable without password); overwrite replaces; "inside itself" refused; no temp files left ✅; awkward passwords (`a b\`, `x\\ y`, unicode, `%PATH%&|^`) in a folder with spaces ✅
- UI run: dropping a folder + a file opens Create archive with 2 items → `made.zip` created (extension added) ✅; 7Z with password `pa ss"word` → created, re-opened → password asked → 2 files ✅ (on Linux; on Windows that password is now refused, see fix 1)
- **Whole Rust test suite cross-compiled for Windows and run under Wine with the real bundled `7za.exe`: 15/15 ✅** (this covers upgrades 3–6 on a Windows build: paths, quoting, zip-slip, cancel, create).

## Upgrade 7 — Batch queue, per-job progress, cancel, history

**What changed**
- Drop or open many archives at once: they're all opened (listed) in parallel; extractions go through a queue that runs **2 at a time** (more would just fight over the disk).
- Queue panel in the top area: one row per archive with its own progress bar, live status (percent + current file) and a **Cancel** button (cancels a running job, or removes a waiting one). "Clear finished" tidies the list; "Cancel all" stops everything.
- Password dialogs for several archives are shown one after another instead of overwriting each other.
- **Recent** panel (last 40 jobs, kept between runs): name, what happened (Extracted / Extracted here / Created / Skipped / Cancelled / Failed), when, and a **Show** button that opens the output in Explorer (tauri-plugin-opener).

**Review findings & fixes**
1. *Overlapping password dialogs:* two archives needing a password at the same time replaced each other's dialog, leaving one promise unresolved forever → prompts are chained.
2. *"Ask every time" asked when the job's turn came* (possibly minutes later) → the folder is asked when you click, then the job queues.
3. *Double-clicking Extract while the folder dialog was open* could queue the same job twice → re-checked after the dialog.
4. *Closing a tab / "Remove all"* left 7-Zip running in the background → running and queued jobs are cancelled first.
5. *History "Show" used the row index*, which shifts when a job finishes between render and click → keyed by timestamp.
6. Test bug: the queue test was cancelling a fixed name that wasn't always the queued one → now cancels whichever job is waiting.

**Tests** (UI run, all ✅; cargo check for Windows + Linux ✅)
- 3 × 300 MB archives + a zip dropped with auto-extract on: at most 2 "Extracting" at any moment ✅, others show "Waiting in queue" ✅; the waiting one cancelled → never extracted, the other 3 finish ✅
- Recent panel lists the finished jobs ✅
- full earlier suite still passes (31/31)
- **Windows-only:** "Show" opening Explorer.

## Upgrade 7 — completion check (after the interruption)

Re-verified before starting upgrade 8: the upgrade 7 commit is in git, its REVIEW section above is complete, the full UI suite passed again (31/31 at the time, 38/38 after upgrade 8), and Windows + Linux `cargo check` were clean. No unfinished work was found.

## Upgrade 8 — Xbox 360 ISO → Games on Demand (GOD)

**Source & license**
- Based on **iso2god-rs** by Ilia Pozdnyakov (github.com/iliazeus/iso2god-rs, v1.8.0, commit `76207d7`). **License: MIT** (checked the repo's `LICENSE` file); a copy ships in `licenses/iso2god-rs-LICENSE.txt`.
- The decompiled Chilano Iso2God code was **not** used. iso2god-rs itself is an independent Rust re-implementation; one source comment links to the old C# project for reference.
- Integrated as a **Rust library** (vendored at `src-tauri/vendor/iso2god`, CLI-only bits and dev-dependencies removed). No extra exe is bundled.

**What changed**
- `src-tauri/src/god.rs`: `iso_info` (Title ID, name from iso2god's built-in title list, Xbox 360 vs original Xbox, trimmed size, part count) and `convert`. The conversion follows iso2god's own CLI step by step (trim unused space from the end, write the `DataNNNN` parts, chain the MHT hashes, write the CON header). Parts are written one at a time, so progress is reported per part and Cancel works between parts.
- UI: an **ISO → GOD** button, and dropping `.iso` files opens it too. It has Add ISOs (batch), Choose output folder (remembered), a game-title field per ISO (filled in from the ISO, editable), a progress bar and status per ISO, Cancel / Cancel all, and history entries. Conversions run one at a time because they use the disk heavily.

**Review findings & fixes**
1. *Title overflow (bug in how iso2god's API can be used):* `with_game_title` writes the string into a 0x80-byte header slot without a length check, so a long title would overwrite neighbouring fields → titles are cut to 63 UTF-16 units without splitting emoji, control characters are dropped, and the input has `maxlength=63`.
2. *Half-written output on error/cancel* → that title's `.data` parts and header are deleted, and the Title-ID folder too if this run created it.
3. *Re-converting the same game* → the `.data` folder is cleared first (same as iso2god), so no stale extra parts are left.
4. *Non-Xbox ISOs* gave a raw parser error → "Not an Xbox ISO (…)" shown on that row; the rest of the batch continues.
5. *Stale progress between runs* → each run gets a fresh job id.
6. *HTML in titles* (`My <b>Homebrew</b>`) is escaped in the UI and written literally into the header.

**Tests**
- **No real game ISO was available**, so the tests build small but structurally valid XDVDFS images (volume descriptor, root directory, `default.xex` with a real XEX2 execution-info block, plus a 400 MB sparse padding file so the output spans 3 parts). This checks parsing, part layout, hash chaining (runs without error), the header (`LIVE` magic, title written, terminated), progress, cancel and cleanup. **It does not prove a console will boot the result.** That needs a real ISO on a real Xbox 360.
- cargo (Linux): 5 GOD tests ✅ (info + name lookup "Halo 3", rejects non-ISO, 3-part conversion with custom then built-in title, cancel cleans up, long-title clamp)
- Same tests cross-compiled for Windows and run under Wine: whole suite **20/20 ✅**
- UI run: 3 ISOs dropped → titles "Halo 3" / file-name fallback / "Not an Xbox ISO" ✅; batch converts both valid ones (3 parts + 1 part) ✅; custom title in header ✅; cancel mid-part → "Cancelled", no output left ✅. Full suite **38/38 ✅**

---

## Fixes from Overseer review (2026-10-19)

The Overseer review (REVIEW-overseer.md) failed upgrade 7. Each finding is listed below with its fix and the regression test that covers it.

| # | Finding | Fix | Regression test |
|---|---|---|---|
| **H1** | Two jobs extracting archives with the same name could merge into one folder: the free name was picked and then filled later, and `rename` fell back to copying into a folder that already existed. | `claim_dir` reserves the folder with `fs::create_dir`, which fails if anything already has that name, retrying `name-2`, `-3`, and so on. Files are then moved into the folder this job owns, and nothing is copied into a folder that already existed. A process-wide `PLACE_LOCK` also makes the name-and-move step run one job at a time. "Extract here" with numbering claims new files with `create_new`. | `h1_parallel_same_name_extractions_never_merge`: 12 threads extract two different `same.zip` archives into one folder. The test asserts 12 separate folders, each holding exactly one archive's files, plus 16 racing `claim_dir` calls. |
| **M1** | "Replace files" followed symlinks and junctions in the destination, so it could write outside the chosen folder. | `merge_replace` checks with `symlink_metadata` and never follows a link. A link (or Windows reparse point) is removed as a link and replaced with a real folder or file. | `m1_replace_does_not_follow_symlinks` (Unix): the destination folder is a link, then a subfolder and a file are links, then an "Extract here" target is a link. The victim folder stays byte-for-byte unchanged. |
| **M2** | A GOD re-convert that was cancelled or failed deleted the previous output. | The package is built in `dest/.unpack-god-tmp-<job>/`, which is removed whatever happens. Only after a full success does `install()` move the old `.data` aside, rename the new one into place and swap the header. If the move fails, the old `.data` is put back. | `m2_failed_reconvert_keeps_previous_output`: three cancels (after part 1, after part 2, at hashing) and one write failure midway each leave the earlier package identical and no temp folders behind. A later successful re-convert replaces it. Confirmed failing on the old `god.rs`. |
| **M3** | Previewing or saving an entry name that isn't in the archive produced an empty file, because 7-Zip exits 0 when nothing matches. | `ensure_entry()` checks the listing first. A missing entry gives "… was not found in the archive." and a folder gives "… is a folder". No temp file is created. | `m3_missing_entry_is_an_error` |
| **M4** | The CSP allowed `'unsafe-eval'` for the whole page only because of the RAR fallback. | Feasible, so done. `scripts/patch-unrar.py` replaces node-unrar-js's embind code generation (`new Function` in `createNamedFunction` and `craftInvokerFunction`) with closures, and it fails if any `new Function` remains. The CSP is now `script-src 'self' 'wasm-unsafe-eval'`. `read_file`, which feeds the in-page fallback, now refuses anything that isn't an archive name. The bridge serves the page with the exact CSP from tauri.conf.json. | e2e `csp` section: the header has no `'unsafe-eval'`. sample.rar (unrar wasm) and plain.7z (7z wasm) open in the page with zero CSP violations. Rust: `read_file_allow_list`. |

**Low issues fixed**
- **L1, entry-name quoting:** names are no longer put on the command line. They are passed in a temp list file (`-i@list -scsUTF-8 -spd`), so `"`, a leading `-`, `*`, spaces and Unicode are all matched exactly on Linux and on Windows. Checked by hand with the real 7za.exe under Wine. Test: `l1_awkward_entry_names_are_literal`.
- **L2:** `extract_entry_to` checks the password before creating its temp file. A cancel that arrives before 7-Zip is attached now kills it straight away. Test: `l2_cancel_before_attach`.
- **L3:** `read_entry` reads stderr on a separate thread, so a full pipe can't deadlock it.
- **L4:** `-snh-` does not stop 7-Zip from failing on a hard link that points outside the archive (tested). Instead, exit code 2 is accepted when every error is "Cannot create hard link". Those entries are skipped and reported as warnings. Test: `l4_bad_hard_link_is_a_warning`.
- **L5:** the wasm single-entry extraction now passes `-spd` and `--` before the entry name.
- **L6:** `parse_args` treats everything after `--` as a path, and the Explorer menu verbs now pass `-- "%1"`. Test added to `parses_explorer_args`.
- **L7, self-contained tests:**
  - `symlink.zip`, `wrapped.zip` and `big.7z` (300 MiB) are generated during the test run (a small ZIP/TAR writer, plus the bundled 7-Zip for big.7z). If a fixture can't be built, the test fails. Nothing skips any more.
  - `tests/e2e.mjs` generates `big.7z` and `halo3.iso` (via `tests/make_iso.py`) when they're missing, and fails if it can't.
- **L9:** the installer embeds the WebView2 bootstrapper (`embedBootstrapper`), which is why it grew to 4.8 MiB.
- **L10:** the unused `mut` warning is gone. The build and tests now produce no warnings.

**Left open**
- **L8:** left as is (cosmetic; see the Overseer report).
- **M1 on Windows:** the junction/reparse-point branch is covered by `is_reparse_point`, but the regression test runs only on Unix. Wine can't create junctions for the test.
- **Remaining CSP risk:** `extract_archive`, `create_archive` and `iso_to_god` still accept any path the page sends. The pickers run in JS, so a full allow-list would need the dialogs moved into Rust. With no remote content and no `unsafe-eval`, the page can't be scripted from outside.
- The plain test archives (plain.zip, pw.7z, evil.zip, sample.rar, …) are still read from `UNPACK_TESTDATA`. Only the large and special fixtures are generated.

**Test results after the fixes**
- `cargo test --lib` on Linux: **28/28 passed**, 0 warnings.
- The same suite cross-compiled for Windows (x86_64-pc-windows-gnu) and run under Wine with the real 7za.exe: **27/27 passed** (M1 is Unix-only).
- e2e (headless Chrome + the real Rust backend through the bridge): **42/42 passed**, including the new CSP checks.
- Rebuilt `dist/Unpack_1.0.0_x64-setup.exe` (4.8 MiB) and `dist/Unpack-portable.zip`.

---

## Round: batch extract + auto ISO → GOD, and the UI redesign (2026-10-20)

Coder Overseer could not be messaged from this session (no agent-messaging tool available), so this is a self-review, done adversarially. Overseer should still do an independent pass.

**What changed**
- Rust (`god.rs`): `convert_named` (atomically claims a new `<Game> (GOD)` folder via `claim_dir`, checks the ISO *before* creating anything, and removes the folder again if the conversion fails or is cancelled. `remove_dir` only works on empty folders, so real output is never deleted), `find_isos` (recursive, doesn't follow symlinks or junctions, ignores `.unpack-*` temp folders), `verify_output` (header present plus every `DataNNNN` part present and non-empty), and `convert_extracted` (optional ISO delete). New commands are `find_isos` and `god_extracted`. The existing `iso_to_god` is unchanged.
- Frontend: after a native extract is **saved** with Auto on, the ISOs in that output are converted one at a time across the whole app (`godChain`). Each ISO gets a sub-row with progress, Cancel, and done, skipped or failed. Cancel all also cancels them.
- New settings `unpack.autoGod` and `unpack.delIso`, both off by default. Turning on delete needs an in-app confirmation (not `window.confirm`, which can't be relied on in WebView2).
- UI redesign:
  - A new theme with light, dark and system modes. `theme.js` applies it before first paint, which is allowed by CSP `script-src 'self'`.
  - Hand-made SVG icons: logo, gear, pack and disc, plus ZIP/RAR/7Z/ISO/GOD/TAR file badges and an animated drop-zone illustration.
  - Lightweight CSS animations that respect `prefers-reduced-motion`.
  - A Settings dialog that closes with Esc or a backdrop click and returns focus.
  - New app icon (`app-icon.svg`, with `src-tauri/icons/*` and `icon.ico` regenerated).
  - Fonts: only the already-bundled OFL Outfit. CSP is unchanged.

**Findings**
- **H1 (fixed):** with *Replace files*, an archive can be merged into a folder that already holds the user's own ISOs. A plain folder scan would have converted them and, with delete on, **deleted** them. The fix only considers ISOs whose relative path matches an entry listed in *this* archive. On the Rust side, delete also requires the ISO to be inside the extraction root (canonicalised) and the output to verify.
- **M1 (fixed):** the queue repaint skipped GOD sub-rows, so progress only updated on full renders. `schedulePaint` now paints them.
- **M2 (by design):** *Extract here* has no per-archive folder, so auto-convert is skipped there, with a warning on the job.
- **L1:** a single-root archive (`MyGame/…` flattened) is matched by also trying the path without its first segment.
- **L2 (open):** free disk space isn't checked before converting. Converting needs about the ISO's size again. A failure there is handled cleanly: the temp output is removed and the ISO is kept.
- **L3 (open):** the earlier "auto-convert when opening an ISO directly" approach was dropped, per Justin's correction. Opening an ISO directly still opens the manual ISO → GOD panel, as before.

**Tests**
- `cargo test --lib`, Linux: **29/29**. New: `auto_named_claims_new_folder_and_rejects_non_xbox` (new folder, `-2` never reuses one, a non-Xbox ISO creates nothing and is kept, cancel cleans up and keeps the ISO, delete only happens inside the extraction root and after success).
- Same suite cross-compiled for Windows and run under Wine with the real 7za.exe: **28/28** (M1 symlink test is Unix-only).
- `tests/e2e.mjs` (headless Chrome + real Rust bridge): **42/42**.
- `tests/autogod.mjs`: **12/12**. It covers off by default, the delete confirmation, persistence, a batch of zip+7z into one folder, `Halo 3 (GOD)` created, the non-Xbox ISO skipped with a message and kept, a second batch giving `(GOD)-2` with the extracted ISO deleted only after success, the source archive untouched, and no page errors. It also writes the screenshots.
- Not tested: real Windows/WebView2 (the GUI doesn't run under Wine) and real retail ISOs (only synthetic Xbox images).

---

## Round: fixes for Coder Overseer Round 2 (2026-10-20, ~1:40 AM CDT)

- **C1 (critical, fixed): which ISOs count as extracted.**
  - `archive::extract` now returns `isos`, the exact final paths of the `.iso` files *this* extraction placed. They're computed from the staging folder before the move, with links never followed.
  - It also returns `merged` (true when Replace merged into an existing folder).
  - The page converts only those paths. The folder scan (`find_isos`) and the suffix/"strip first folder" matching are removed.
  - Delete-ISO is forced off when `merged` is true, and a warning shows on the job.
  - Regression tests:
    - Rust `c1_iso_list_is_exact_and_replace_is_flagged`.
    - UI `autogod.mjs` C1 checks: Replace with delete on and a pre-existing `keep/game.iso` → my ISO is untouched, the extracted ISO is kept, and nothing extra is converted.
- **H1 (high, fixed): truncated ISOs.**
  - `iso_info` and `convert` refuse an ISO shorter than `root_offset + get_max_used_prefix_size()` ("incomplete or truncated"). The ISO is kept and nothing is created.
  - The ISO's length is re-checked at the end ("changed while converting").
  - Regression test: `h1_truncated_iso_is_refused_and_kept`.
- **H1/M1 (fixed): the success check.** The old `verify_output` is replaced by `verify_package`, which runs on the temp package *before* it is swapped in and again on the final location. It checks:
  - every `DataNNNN` is exactly the expected size (master hash block + one hash block per sub-part + data, computed from the ISO length)
  - each part's master hash list is chained into the previous one
  - the header starts with `LIVE` and contains part 0's master-hash digest

  Any failure is an error, so the job isn't marked done and the ISO isn't deleted. Test: `verify_package_detects_damage` (bad magic, broken chain, short part).
- **M2 (fixed):** `tests/autogod.mjs` writes screenshots to `/tmp/unpack-shots` and deletes them afterwards. The tracked `screenshots/` folder is only rewritten with `UPDATE_SHOTS=1`.
- **L1 (fixed):** when Replace replaces a destination symlink or junction with real files, a warning now says so (`merge_replace`).
- **L2 (fixed):** free-space check before converting (`GetDiskFreeSpaceExW` on Windows, `df` elsewhere). It needs the estimated output (`estimated_output`, test `free_space_estimate_matches_output`) plus 64 MB, or it fails with "Not enough free space…" before writing anything.
- **L3, documented:** `dist/Unpack-Cleanup.exe` (source `cleanup/cleanup.c`, built with MinGW) is the cleanup tool for the portable copy and for leftovers. It's per-user only and needs no admin. It lists exactly what it will remove (Unpack's own HKCU keys, its AppData folder and its temp files), asks before deleting, and removes the Uninstall key only if `uninstall.exe` is gone. Not run on real Windows (reviewed by reading).

**Tests this round**
- `cargo test --lib`, Linux: **33/33**.
- Windows build of the same suite under Wine with the real 7za.exe: **32/32** (one test is Unix-only).
- `tests/e2e.mjs`: **42/42**.
- `tests/autogod.mjs`: **15/15**.
- Not tested: the real WebView2 GUI on Windows, and real retail ISOs.

---

## Round: fixes for Coder Overseer Round 3 (2026-10-20, ~2:00 AM CDT)

- **H1 (fixed): same path from two archives in one batch with Replace.**
  - `archive::extract` registers each ISO it placed in a Rust-side registry (`god::register_extracted`). The registry records the path, the extraction folder, whether delete is allowed, and an **identity**: size, mtime, and a SHA-1 over 1 MiB samples from the start, middle and end.
  - The page gets back only opaque `iso_tokens`.
  - `convert_registered` re-checks the identity before converting, after converting, and again right before `remove_file`. A swapped file is refused ("replaced or changed after it was extracted"): it isn't converted and it isn't deleted.
  - Simplest safe rule, also applied: **delete is never allowed when Collision = Replace**, even into a fresh folder.
  - Tests: `r3h1_swapped_iso_is_not_converted_or_deleted` and `r3_replace_mode_never_allows_delete`.
- **M1 (fixed): verify doesn't re-hash the data.** New `deep_verify` runs only when a delete is about to happen. It re-reads every part, checks each sub-hash list against the master list, and SHA-1s every data block against its sub-hash list. Any mismatch means an error and the ISO is kept. Test: `verify_package_detects_damage` now includes a 4 KB data-block corruption inside Data0001. The cheap check passes it; `deep_verify` reports "damaged".
- **M2 (fixed): delete decision trusted from the page.** `god_extracted` now takes only `{id, token, delete_iso}`, where `delete_iso` is just the user's preference. Rust decides whether delete is allowed, which extraction folder applies and where output goes. Unknown or forged tokens are rejected (tested in `auto_named_claims_new_folder_and_rejects_non_xbox`). The path-taking `convert_extracted` is removed.
- **L1 (fixed): cancel after the package is placed.**
  - `convert` checks for a cancel after install and returns CANCELLED, keeping the package.
  - `convert_registered` checks for a cancel again after the deep check, right before deleting, and keeps the ISO with "ISO kept: cancelled".
  - Test: `r3l1_late_cancel_keeps_iso`.
- **L2 (done):** the delete confirmation now says that Unpack can't tell whether a game ISO itself was already damaged (for example a bad download of the right size), and that deletion never happens with "Replace files".
- **UI:** each ISO row shows why its ISO was kept (`kept_reason`). An ISO that couldn't be fingerprinted is skipped and kept.

**Tests this round**
- `cargo test --lib`, Linux: **36/36**.
- Windows build of the same suite under Wine with the real 7za.exe: **35/35**. One test is Unix-only. `r3_replace_mode_never_allows_delete` needs python to build its fixture, so under Wine it may return early; it runs fully on Linux.
- `tests/e2e.mjs`: **42/42**.
- `tests/autogod.mjs`: **15/15**.
- Not tested: the real WebView2 GUI on Windows, and real retail ISOs.

---

## Round: DLC/TU placement, Library, Copy to USB, QOL, and Overseer Round 4 fixes (2026-10-20, ~2:10–2:35 AM CDT)

Commits: `c1669f6` (DLC), `29d5c2d` (Overseer R4 fixes + TU + library backend), plus this UI/QOL commit.

**Overseer R4 fixes**
- **M (header):** before deleting, the GOD header on disk is compared **byte for byte** with the header rebuilt from the same inputs (`build_header`, also used to write it). This covers title ID, title text, block/part counts, content type, media ID and the header SHA-1. Test: `r4m2_header_checked_byte_for_byte` flips each of 0x32c, 0x344, 0x354, 0x360, 0x392, 0x3a0 and 0x411, and every flip is caught.
- **M (fingerprint):** the identity now also includes the file ID: (dev, inode, ctime) on Unix and (volume serial, file index, creation time) on Windows.
  - When delete is on, the page asks Rust to **seal** the ISOs right after extraction (`seal_isos`), which takes a full-content SHA-1.
  - The delete requires that seal, and re-hashes the ISO right before `remove_file`. An unsealed ISO is never deleted.
  - Test: `r4m1_restored_mtime_swap_is_caught` writes 12 bytes at 100 MiB mid-convert, then restores the mtime. The ISO is kept.
- **L (tokens):** 128-bit random tokens (`RandomState`-keyed), **single use** (consumed on the first call), expiring after 12 h. Test: `r4l1_tokens_random_single_use`.
- **L (progress):** the delete path shows "Verifying GOD data, part i/n" and "Verifying the ISO is unchanged".

**New features**
- **DLC:**
  - `dlc.rs` parses STFS headers: magic CON/LIVE/PIRS, content type at 0x344, TitleID at 0x360, UTF-16BE name at 0x411.
  - `archive::extract` returns `dlcs`, the exact placed files with an STFS magic (Folder layout only).
  - `find_game` checks `L/<TID>`, `L/0000000000000000/<TID>`, `L/Content/0000000000000000/<TID>` and `L/*/<TID>`. A folder counts only if it contains `00007000` or `00004000`.
  - DLC goes to `00000002`, title updates to `000B0000`. Other package types are ignored (hidden).
  - Never overwrites: the file is created with `create_new`, then copied, fsynced and SHA-1 verified, and removed if anything fails. Identical files are skipped; a different file with the same name becomes `-2`. A symlinked `00000002` is refused.
  - Move is optional and off; it deletes the source only after a verified copy.
- **Drive scan:** only `<drive>:\{Content, Xbox360\Content, XBOX\Content, Games\Content, GOD\Content}\0000000000000000`. Results are shown for the user to approve one by one; nothing is added automatically.
- **Library (`library.rs`):** games with name (from the game's own STFS header, then iso2god's built-in list), TitleID, size, and DLC/TU counts. Original generated SVG tiles (colours from the TitleID, initials), plus an empty-state illustration.
- **Copy to USB:** plans every file first. Any file already on the target with different content stops the copy **before anything is written**; identical files are skipped, so a re-run resumes. Free-space check, `create_new` for every file, cancellable.
- **QOL:**
  - Batch summary card (animated check), with extracted/failed/skipped, GOD, DLC and space-freed counts, plus Open folder.
  - First-run tips; keyboard shortcuts with a `?` overlay; tooltips on header buttons.
  - DLC "game not found" rows are retried when a GOD conversion finishes or a library folder is added.
  - The test bridge port is configurable (`UNPACK_BRIDGE_PORT`), so test runs don't collide with Overseer's.
- **Not done:** Windows taskbar/toast progress (needs extra Windows APIs and plugins); dropping whole folders as a batch source (folders still go to Create archive).

**Own adversarial review**
- **DLC source path from the page (low, open):** `dlc_install` takes a path. Copy mode can't harm the source. With **Move** on, a compromised page could make Unpack copy-then-delete any readable STFS-magic file into a library folder. The data is preserved (verified copy first), but it's moved. Same class as the old R2 M2; with the tight CSP it needs a page compromise. A future fix is tokens like the ISO registry.
- **Copy to USB partial (low, by design):** if a copy fails halfway, the files already copied stay on the USB (each one complete and verified by `sync_all`; the half-written file is removed). A re-run skips the identical ones.
- **Batch stats (cosmetic):** a "game not found" row that later succeeds after a retry can be counted in two summaries.
- **find_game ambiguity (low):** if the same TitleID exists in several libraries, the first library and the first matching layout wins (deterministic order). The row shows the destination.
- **Free space for DLC copies (low):** not checked. DLC is small, and a full disk fails cleanly (the partial file is removed).
- Checked and OK: TitleID validated as 8 hex characters (no `..` path injection); names rendered with `escapeHtml`; library tile SVG text is escaped; no network access; no HKLM; CSP unchanged.

**Tests this round**
- `cargo test --lib`, Linux: **45/45**. New: `parses_headers_and_kinds`, `installs_into_god_game_and_never_overwrites`, `not_found_title_update_and_layouts`, `never_writes_through_linked_dlc_folder`, `scans_games_with_counts_and_names`, `copy_to_usb_never_overwrites_and_skips_identical`, and the three r4 tests.
- Windows build under Wine: **43/43** (two tests are Unix-only).
- `tests/e2e.mjs`: **42/42**.
- `tests/autogod.mjs`: **15/15**.
- New `tests/dlc.mjs`: **16/16**. It covers:
  - DLC auto-install, conflict renaming, TU → 000B0000, and game not found
  - drive-scan approval, manual Install DLC, identical skip
  - the batch summary, the Library with counts, Copy to USB, Esc, `?`, and tips
- Screenshots refreshed with `UPDATE_SHOTS=1` (01–11).
- Not tested: the real WebView2 GUI on Windows, a real console, real DLC files (only synthetic STFS headers).

## Mod Menus section (2026-10-20)

**What it is:** a local manager/browser for mod files the user already has. Nothing is bundled, downloaded or linked; there is no catalog of mod names or hashes and no network code (the e2e test fails on any non-local request). The dialog says: for offline / modded-console use.

**Pieces**
- `src-tauri/src/mods.rs`: identifies a file or folder. XEX2 header → execution-info TitleID (key 0x00040006, +0x0C), "XEX plugin" when absent. STFS via `dlc::parse_stfs` (name, TitleID, content type). Folder → size, file count, and the game if all its packages share one TitleID. `copy_mod` plans first: any existing file with different content stops the copy before anything is written, identical files are skipped, writes use `create_new`, there's a free-space check (+8 MB), and cancel is supported. Targets: `<drive>\Plugins\<name>`, `<drive>\Content\0000000000000000\<TitleID>\<ContentType>\<file>` (STFS only, from the header, never from a guess), or as is. Refuses a target inside the source or the same path.
- `src-tauri/src/gamedb.rs`: offline game reference. The ~4,900-entry iso2god-rs TitleID list (online/multiplayer titles included: every CoD, GTA IV/V, Halo 3/ODST/Reach/4/CEA, Gears, Battlefield, Borderlands, Destiny, Minecraft…) plus an alias table of ~40 popular titles. `guess()` scores alias hits, exact names (sequel-aware: "black ops" doesn't match inside "black ops 2"), and word coverage. `confident()` needs ≥85 and an 8-point lead. `search()` powers the picker (name, alias or TitleID).
- UI (`app.js` Mod Menus): header button + Ctrl+M. Search box, game/type filters, card grid with generated tiles ("?" when unknown), details panel (game + where it came from: header / your choice / guess N%, best-match chips, game search, notes, destination, Copy to USB, Show, Remove). The list, notes, destination and game choices persist in `localStorage` (`unpack.mods`). Remove only edits the list. New original SVG icon (`ICONS.mod`) and empty-state art.

**Own adversarial review**
- Wrong guess risk (medium → mitigated): a name guess only labels the card. It never decides where files go. The Content destination uses the header TitleID only, and the option isn't offered for non-packages. Two-letter aliases (`aw`, `h3`) score below the confidence bar, so they show as suggestions only. Longer generic words that are aliases (`reach`, `ghosts`) can give a confident wrong label. The user can change it, and the label is harmless.
- Untrusted headers: XEX header counts are capped (64). The execution-info offset is read with `read_exact` (cut-off → treated as a plugin). STFS goes through the existing parser. TitleID/content type are hex-formatted, so no path injection.
- Overwrite: planned check + `create_new` on every write, so a file that appears between plan and copy is still not overwritten.
- Partial copy (low, open): if a folder copy fails midway (disk pulled), the files already copied stay; the error is shown. No rollback, by design, to avoid deleting anything on the target.
- Symlinks inside a mod folder are skipped (not followed); a symlink added directly is refused. Low: skipped links are not listed.
- Free space: unknown free space (`free_space` → None) skips the check; a full disk fails cleanly and removes the partial file.
- XSS: every name, path, note and stored game name is rendered with `escapeHtml`. CSP unchanged. No HKLM, no admin.

**Tests this round**
- `cargo test --lib`, Linux: **50/50**. New: `mods::identifies_xex_stfs_folder_and_other` (also covers name guesses and header-wins), `mods::copies_to_plugins_and_content_without_overwrite`, `gamedb::aliases_and_names`, `gamedb::unknown_or_vague_is_not_confident`, `gamedb::names_and_search`.
- Windows build under Wine: **48/48** (two tests are Unix-only).
- New `tests/mods.mjs`: **22/22**. It covers the note with no links, the five kinds identified, header vs alias vs folder-hint guesses, "Which game?" + offline search by alias + a remembered choice, type/game filters, search, copy to Plugins / Content / as is, no overwrite, identical skip, reload persistence + Ctrl+M, remove-only-list, and no network requests.
- `tests/e2e.mjs` 42/42, `tests/autogod.mjs` 15/15 (it now builds its own fixtures when `/tmp/gt` is missing), `tests/dlc.mjs` 16/16 (the bridge needs `UNPACK_TEST_DRIVES=/tmp/unpack-dlc-e2e/usb`).
- Screenshots refreshed with `UPDATE_SHOTS=1`; new `screenshots/12-mod-menus.png`.
- Not tested: the real WebView2 GUI, a console, real mod files (synthetic XEX/STFS only).


## Overseer Round 5 fixes (2026-10-20)

- **M: symlinked/junctioned folders on the target (Copy to USB, mod copy, DLC install).** New `src-tauri/src/safecopy.rs`. `ensure_dir_under(root, rel)` creates the path one folder at a time and refuses any existing symlink or Windows reparse point (junction/mount point, `FILE_ATTRIBUTE_REPARSE_POINT`) or non-folder. It then checks that the canonical result is still under the canonical root. `library::copy_to_usb`, `mods::copy_mod` and `dlc::install_dlc` use it in place of `create_dir_all`. Final files that are links are refused too.
- **L: raw paths from the page.** Native pickers now run in Rust (`pick_path`). Picked paths, extraction outputs and drive-scan roots are recorded in `safecopy::Grants`, which persists in the app data folder so saved library/mod entries keep working. `dlc_install`, `copy_to_usb` and `mod_copy` canonicalize every path (so `..` is resolved) and accept it only if it is a granted path or inside one. Libraries that aren't granted are ignored. Raw invokes get "wasn't picked in Unpack".
- **L: partial files after an unplug.** Every copy is written to `<name>.partial`, synced, re-hashed (SHA-1) against the source, and only then renamed. It never renames over a file that appeared meanwhile. A stale `.partial` (plain file only) is replaced on the next copy, so it no longer blocks. A failed copy removes its `.partial`.
- **L: FAT32.** The target's filesystem is read up front (Windows `GetVolumeInformationW`, Unix `stat -f`). On FAT/FAT32/vfat, any file over 4 GiB−1 refuses the whole copy before writing, with a clear message. exFAT/NTFS are unaffected. Post-copy hash verification is above.
- **mods.mjs count:** the suite has 21 checks; I misreported it as 22. Every UI suite I added now prints `N/N passed` from a counter, so the reported number is the measured one.
- Windows fix found by Wine: `canonicalize` returns `\\?\C:\…`, which is now simplified to `C:\…` for display and comparisons.

**Remaining (low):** rename-without-overwrite is a check-then-rename (a file created in the microseconds between them could be replaced on Windows). The grant file is user-writable (the same trust level as the user). Partial folder copies are not rolled back.

**Tests:** Rust 55/55 (new: `r5_refuses_symlinked_components`, `r5_partial_then_rename_and_stale_partial_cleared`, `r5_fat_limit`, `r5_grants`, `r5_copy_to_usb_refuses_symlinked_content`). Wine 51/51 (four tests are Unix-only). `mods.mjs` 26/26 (new: symlinked Plugins refused, stale .partial replaced, unpicked target refused, `..` escape refused, raw `copy_to_usb` refused). e2e 42/42, autogod 15/15, dlc 16/16. Screenshots refreshed (UPDATE_SHOTS=1).

## Download from link + folder drop + toasts + R5 addendum (2026-10-20)

**Download (`src-tauri/src/download.rs`, ureq + sha2):** only runs from the `download_url` command after an explicit click. Follows redirects (cap 8), resolves simple public Google Drive / Dropbox / MediaFire links, reads `Content-Disposition`, writes `name.partial` with optional Range resume, refuses HTML bodies, SHA-256 of the finished file. Default folder: app-data `Downloads` (granted). CSP unchanged — `connect-src` still ipc-only; no frontend `fetch` to the internet.

**Saved links:** localStorage only (`unpack.links`). No bundled URLs or mod catalog.

**Folder drop:** `list_archives_in_folder` + confirm dialog. Create archive remains the path for packing folders mixed with files.

**Notifications:** `tauri-plugin-notification` + `set_progress_bar`. Setting `unpack.notify` (on by default).

**R5 addendum fixes:**
- `ExtractResult.created` only true for `claim_dir` folders; Skip / Replace-merge / Extract-here are false.
- `grants.grant_session` for created folders — not persisted. User picks / drive scan still persist.
- `extract_archive` requires a granted dest (or the archive's own parent for “beside”).
- `create_archive` / `pick_save` gate the out file via `check_or_parent`.
- Refuse TitleID `00000000` / `FFFFFFFF` for Content placement.
- `safecopy::plain` strips `\\?\` in user-facing paths; DLC multi-library matches are deterministic (first library) with a note in the message.

**Own review:** compromised page could still call `grant_dir` on any real directory (same class as the original raw-path issue, narrower). Hoster page parsers are best-effort and fail closed. Download size is not capped (disk can fill; cancel works). Toast/plugin needs OS permission on first use.

**Tests:** Rust 61/61 (Linux); Wine release 57/57 (unix-only tests omitted). e2e 42/42, autogod 15, dlc 16, mods 26/26, download 11/11 (local HTTP server). Screenshot `13-mod-download.png`.
