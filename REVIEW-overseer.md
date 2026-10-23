# Unpack: independent review (overseer)

Reviewed on Monday, Oct 19, 2026, 9:00–9:20 PM CDT, on Linux (Debian). I read REVIEW.md and the git history (8 upgrade commits plus a final "README, build.bat, dist" commit) and the source. I ran everything that can run on Linux. I did not change any project file. All scratch work was done in `/tmp`.

## What I actually ran

| What | Result |
|---|---|
| `cargo test` (all Rust unit tests) | **20/20 pass**. Note: with the stock test data, the 300 MB test **skips itself silently but still reports "ok"** because `big.7z` is missing. I made a 300 MiB `big.7z` and re-ran it: it passes. |
| Frontend UI suite `tests/e2e.mjs` (headless Chrome + real Rust code through `examples/bridge.rs`) | **38/38 pass** once I supplied the missing `big.7z` and `halo3.iso` test files in `/tmp`. With only the checked-in test data, the `big`, `queue` and `god` sections crash because files are missing. |
| My own harness (`/tmp`, calls `archive::extract`, `read_entry`, `god::convert`, `god::iso_info` directly) | tar with absolute/relative symlinks + files written "through" them, `../` and `/abs` names, hard links, empty/garbage/0-byte/missing archives, names that start with `-`, `*`, spaces or emoji, 300 runs of 2 parallel extractions, cutting the ISO short at 5 points, re-converting a GOD and then cancelling, and control characters/emoji in titles. |
| GOD header vs iso2god-rs | Downloaded upstream iso2god-rs at commit `76207d7`. The vendored `con_header.rs`, `god/mod.rs`, `hash_list.rs`, `file_layout.rs`, `iso/mod.rs` and `executable/*.rs` are **byte-identical** to upstream. In `god.rs`, the header calls (block counts, `parts_total_size = last + (n-1)*BLOCK_SIZE*0xa290`, content type, MHT chain, trim-from-end default) match upstream's CLI line for line. |
| Built installer / portable zip | Listed with 7z. The installer contains `unpack.exe`, `7za.exe`, `WebView2Loader.dll` and the licenses. The bundled `7za.exe` is identical (same sha256) in the installer, the portable folder and `binaries/`. I found no localhost or CDN strings in the exe. |
| **Not runnable here (Windows only)** | NSIS registry writes and uninstall, the Explorer menu, Windows-specific 7-Zip quoting (`raw_arg`), reparse-point/junction handling, and the GUI itself. I **reviewed these by reading only**. |

## Bugs, ranked

### Critical
None found. Zip-slip held up: `../`, absolute paths and symlinks pointing outside the folder never wrote outside the destination in any of my tests.

### High

**H1. Two archives with the same name can be silently mixed into one folder.** `src-tauri/src/archive.rs:687` (`free_name`) and `:691-692` (`rename` that falls back to `copy_recursive`)
- *What breaks:* Say you queue `ModA\game.zip` and `ModB\game.zip` with a chosen output folder. The queue runs 2 at a time. Both jobs can pick the free name `game` at the same moment. The second `rename` fails because the folder now exists, and the code then *copies into* that existing folder. The result is one `game` folder holding both mods, and both rows say "Saved … to game". Same-named files would overwrite each other, and you get no warning.
- *How I triggered it:* I ran 300 rounds of two parallel `extract()` calls on two different `game.zip` files into the same folder (Number mode). **4/300 merged**: one `game/` folder held `one.txt` + `two.txt`, and both results had `output: .../game`.
- *Fix:* Don't copy into an existing target. Reserve the name atomically: `fs::create_dir(final_target)` fails if it already exists, so retry with the next number. Or keep a process-wide mutex around "pick name + rename". Only fall back to copy after creating a brand-new empty folder yourself.

### Medium

**M1. "Replace files" follows links already in your destination folder.** `archive.rs:497-515` (`merge_replace`: `dst.is_dir()` / `create_dir_all(dst)` follow symlinks)
- *What breaks:* If the destination already has a symlink or junction (common in modding setups, e.g. a `data` link to another drive), "Replace files" pours the archive's files into the link's target, outside the folder you picked.
- *How:* I put a symlink `dest/data -> victim/` and extracted a zip containing `data/new.txt` with Extract here + Replace. `victim/new.txt` was created.
- *Fix:* In `merge_replace`, use `symlink_metadata`. If `dst` is a link or reparse point, remove the link itself (or refuse) instead of descending into it.

**M2. Cancelling (or failing) a GOD re-conversion deletes the earlier good conversion.** `src-tauri/src/god.rs:104-112`
- *What breaks:* If you convert a game that is already in the output folder, the old `.data` folder is wiped *before* the new run starts. On cancel or error, `cleanup` also deletes the old header file. You lose the working copy you already had.
- *How:* I converted a test ISO, then started the same conversion again and cancelled it. Afterwards only an empty `4D5307E6/00007000/` was left.
- *Fix:* Write into a temp folder (e.g. `<TitleID>/.unpack-tmp/`) and swap it in only on success. At minimum, ask before overwriting an existing conversion.

**M3. Preview / "Save file" of an entry that doesn't match gives an empty file instead of an error.** `archive.rs:725-764` (`read_entry`) and `:847` (`extract_entry_to`)
- *What breaks:* When 7-Zip finds no entry by that name, it exits 0 with no output. The preview shows a blank file, and Save writes a 0-byte file as "success". On Windows this is reachable for entry names containing `"` (see L1).
- *How:* `read_entry(names.zip, "nope.txt")` returned `Ok("")`.
- *Fix:* Treat "0 bytes out + stderr/stdout has 'No files to process' (or files=0)" as an error. Or check the name against the listing first.

**M4. `'unsafe-eval'` + global Tauri API + commands that accept any path.** `src-tauri/tauri.conf.json:10,24` and `src-tauri/src/lib.rs:177` (`read_file`), `:188` (`create_archive`), `extract_archive`
- *What breaks:* Nothing today. I found no injection: every archive name, title and error I checked goes through `escapeHtml`, and the e2e `<img onerror>` file name test passes. But the CSP is the only safety net. A single future HTML-injection bug in an archive file name would let a malicious archive read any file on your PC (`read_file` takes any path up to 1.5 GB), zip up your folders, or write anywhere. `unsafe-eval` makes such a bug easier to exploit.
- *How:* Read only. CDN loading is truly off (CSP has no remote origins, and grep found no remote URLs in app code).
- *Fix:* Only the RAR WebAssembly *fallback* needs `unsafe-eval`. Load `unrar.bundle.js` only when 7za is missing, or run it in a Worker or separate window with its own CSP. Limit `read_file` / `extract` / `create` to paths the user actually picked or dropped (keep an allow-list in Rust).

### Low

- **L1. Windows quoting for names containing `"`.** `archive.rs:71-85` `add_args` wraps args with spaces in `"…"`, but a `"` inside an *entry name* (legal in zip/7z) toggles 7-Zip's quoting, so a different name is requested. Combined with M3, that means a silent empty preview/save. Passwords are guarded (`check_password`), entry names are not. *Reviewed by reading. I could not run it, because Linux uses `cmd.arg`.* Fix: for entry names containing `"`, extract via a list file (`@listfile` with `-scsUTF-8`) or refuse with a message.
- **L2. `extract_entry_to` leaves a `.unpack-part-*` temp file** if `check_password` fails (the file is created at `archive.rs:851`, before the check), and a cancel that arrives before `attach` is ignored, because there is no re-check like the one in `run_with_progress`. Fix: run `check_password` first and re-check `is_cancelled` after `attach`.
- **L3. `read_entry` doesn't drain stderr while reading stdout** (`archive.rs:725+`). Huge warnings could block 7-Zip. This is unlikely with a 4 MB cap. Fix: read stderr in a thread, as `run_with_progress` already does.
- **L4. A tar with a hard link pointing outside the archive fails the whole extraction** (I triggered it with `pkg/hl -> /etc/hostname`: "Cannot create hard link", no files at all). It is safe, but unfriendly. Fix: pass `-snh-` or treat code 2 with partial output as "extracted with warnings".
- **L5. WebAssembly fallback passes entry names to 7z-wasm without `--` or `-spd`** (`src/app.js:1369`). An entry named `-x.txt` or `*.txt` is read as a switch or wildcard. This only matters when 7za is missing. Fix: add `-spd` and `"--"` before the archive path.
- **L6. `parse_args` drops any path starting with `-`** (`lib.rs:28`). A relative file name like `-mod.zip` from a command line is ignored with no message. Explorer passes full paths, so the menu is fine. Fix: honour `--` as "end of options" and only treat known flags as flags.
- **L7. Tests that silently pass when data is missing** (`src-tauri/src/archive/tests.rs:118,138`). The suite shows green even though the 300 MB / cancel tests never ran. The e2e needs `big.7z` and `halo3.iso`, which aren't in `/workspace/testdata`. Fix: fail or `#[ignore]` with a message, and add a script that generates the test data (as `make_iso.py` already does for ISOs).
- **L8. No "ISO → GOD" Explorer entry** (`windows/hooks.nsh:23-31`, `.iso` not registered). Only a missing feature. Also, uninstall leaves empty `SystemFileAssociations\.ext\shell` keys behind (harmless).
- **L9. Installer may go online:** the NSIS bundle includes `NSISdl.dll` (Tauri's default WebView2 bootstrapper download) when WebView2 is missing. That's fine on Win10/11, but it isn't "fully offline". Fix if needed: `webviewInstallMode: embedBootstrapper` or `offlineInstaller`.
- **L10. Leftovers:** one compiler warning (`unused mut`, `src-tauri/src/god/tests.rs:23`). No `console.log` or debugger statements and no hardcoded secrets were found. `examples/bridge.rs` binds only to 127.0.0.1 and isn't shipped (confirmed by the exe strings).

## NSIS / Explorer entries (read only, can't run on Linux)
- All keys are written under `HKCU\Software\Classes\SystemFileAssociations\<ext>\shell\Unpack.*`, so no admin is needed and they don't take over other apps' default handlers. Good.
- The command is `"$INSTDIR\unpack.exe" --extract-here "%1"`. Both the exe path and `%1` are quoted, so paths with spaces, `&` or unicode are safe. Good.
- Uninstall (`NSIS_HOOK_PREUNINSTALL`) deletes exactly the 3 verbs per extension for the same 8 extensions and refreshes Explorer. Good.
- The installer exe name `unpack.exe` matches the registry entries (case-insensitive on Windows).

## Verdicts per upgrade

| # | Upgrade | Verdict | Basis |
|---|---|---|---|
| 1 | Tauri wrapper | **PASS** | The crate compiles and its tests run. I inspected the built installer and its contents. The GUI window itself was not launched (Windows build). |
| 2 | Fully offline | **PASS** (fix M4 soon) | e2e passes. No remote URLs in app code or exe, and the CSP has no network origins. `unsafe-eval` is a standing risk (M4). L5 is minor. |
| 3 | Native 7-Zip backend | **PASS** | Ran zip-slip, symlink, abs/`../`, `-`/`*`/unicode names, password, cancel and big-file tests. All held up. M3 and L2–L4 should be fixed. |
| 4 | Extract to folder | **PASS** with a required fix | Collision modes and single-root work in tests. M1 (Replace follows existing links) was reproduced. |
| 5 | Explorer + CLI + single instance | **PASS** (registry part reviewed by reading only) | `parse_args` tests and e2e "launch" pass. I could not run the NSIS/registry part on Linux. L6 and L8 are minor. |
| 6 | Create archives + Windows quoting | **PASS** (Windows quoting reviewed by reading only) | Create round-trips (zip/7z, passwords with spaces/backslashes/unicode) pass on Linux. I didn't re-run Wine. L1 applies to entry names, not passwords. |
| 7 | Batch queue | **FAIL** | H1 reproduced: two parallel jobs with the same archive name merge into one folder and both report success. The 2-at-a-time limit, waiting/cancel and history do work (e2e). |
| 8 | ISO → GOD | **PASS** with a required fix | The header and part logic match upstream iso2god-rs exactly, bad/truncated ISOs give clean errors with no panics, and title clamping works. M2 (re-convert + cancel deletes the old output) was reproduced. As REVIEW.md says, I could not prove that a real console boots the result. |

---

# Round 2: batch into one folder, auto ISO → GOD, delete-ISO, UI redesign (Tuesday, Oct 20, 2026, ~1:25–1:30 AM CDT)

**Heads-up on git state:** only the round-1 fixes are committed (`db00da1`). The whole batch / auto-GOD / redesign update is **uncommitted** in the working tree (`git status` shows `src/app.js`, `god.rs`, `lib.rs`, `hooks.nsh`, the new `tests/autogod.mjs`, `src/theme.js`, `cleanup/`, `dist/Unpack-Cleanup.exe` and others). I reviewed the working tree as it is. Please commit it so the reviewed code is pinned.

## What I ran
| What | Result |
|---|---|
| `cargo test --lib` | **29/29 pass** |
| `tests/e2e.mjs` (headless Chrome + real Rust bridge, with the app's real CSP) | **42/42 pass** |
| `tests/autogod.mjs` | **12/12 pass**. I ran a copy from `/tmp` with screenshots redirected to `/tmp`, because the original **writes into the project's `screenshots/` folder**. |
| My own UI attack scripts (bridge + Chrome, auto-GOD on, delete on) | Replace-merge into existing folders, nested ISOs, a pre-existing ISO in the output folder, an ISO named `<img src=x onerror=alert(1)>.iso`, and Cancel mid-convert |
| My own Rust harness calling `god::convert_extracted`, `find_isos` and `archive::extract` | Truncated ISO, corrupt ISO, file and folder symlinks, ISO outside the root, root-prefix trick (`ex` vs `ex2`), read-only output, cancel mid-part, 200× parallel same-name race (Number and Replace), Replace through existing symlinks, missing preview entry, evil.zip |
| Not runnable here | NSIS hooks (registry), `Unpack-Cleanup.exe`, the real WebView2 GUI. **Reviewed by reading only.** |

## Bugs, ranked

### Critical

**C1. With "Replace files" + auto-GOD + delete on, your *own* ISOs that were already in the folder get converted and permanently deleted.** `src/app.js:748-749`
- *What breaks:* the "only ISOs this archive contained" filter matches by **file name ending**, not exact path. A top-level entry `game.iso` matches any `…/game.iso` anywhere in the output folder. The "strip first folder" fallback for single-root archives (`disc/game.iso` → `game.iso`) widens it further. With Replace, the output is an existing folder full of your files. The Rust guard (`god.rs:287`, "inside extract root") doesn't help here, because the extract root *is* that whole folder.
- *How I triggered it:* I pre-created `out/merge/keep/game.iso` and `out/disc/mine/game.iso` (my "own" ISOs), set Collision = Replace, turned auto-GOD + delete on, and dropped `merge.zip` (top-level `game.iso`) and `nested.zip` (`disc/game.iso`). Result: both of my pre-existing ISOs were converted to `game (GOD)` / `game (GOD)-2`, the status said "extracted ISO deleted", and **both were gone**. A pre-existing ISO with a different name (`disc/game2.iso`) was left alone.
- *Fix:* decide the ISO list in Rust, not the page. Have `extract()` return the exact relative paths it moved into place (or record each file's inode/size+mtime right after the move), and only convert or delete those exact paths. Never delete when the extraction merged into an existing folder (Replace), or at least turn delete off for Replace. Drop the "strip first segment" fallback and use the single-root info `extract()` already has.

### High

**H1. A truncated or incomplete Xbox ISO is "converted" into a broken GOD, then the ISO is deleted.** `src-tauri/src/god.rs:227-245` (`verify_output`), `:287`, `:130`
- *What breaks:* "success" only checks that the header file and each `DataNNNN` file exist and are non-empty. The converter reads the ISO until it runs out and never checks that it got as much data as the ISO's own directory says it should (`data_size`, line 91). So a half-downloaded ISO produces a GOD that won't boot, shows "GOD done", and with delete on the ISO is removed.
- *How:* I made a valid 400 MB test ISO and cut it to 200 MB. `convert_extracted(…, delete=true)` returned `Ok`, 3 parts (the last one is just a 4 KB hash table), and **`iso deleted = true`**.
- *Fix:* before converting, refuse if `file length < root_offset + get_max_used_prefix_size()`. In `verify_output`, check each part's size against what's expected (every part except the last is full-sized, and the total matches `data_size`). Even better, re-read the MHT and check the header's SHA-1 before deleting.

### Medium

**M1. `verify_output` checks much less than its name suggests.** `god.rs:227`. It returns true for the first content-type folder with *any* non-empty file whose `.data` sibling has the parts. It doesn't check the header magic (`LIVE`), the title ID, part sizes or hashes. Together with H1, this is the whole "is it safe to delete" gate. *Fix:* as in H1, plus confirm the header starts with `LIVE` and that its part count and size fields match.

**M2. `autogod.mjs` overwrites tracked project files** (`tests/autogod.mjs:4`, SHOTS = `../screenshots/`). Running the test suite rewrites `screenshots/*.png` in the repo. *Fix:* write to `/tmp`, or behind an `UPDATE_SHOTS=1` flag.

### Low
- **L1.** Replace into a folder whose name is an existing **symlink** (`dest/data -> elsewhere`) now silently *replaces the link with a real folder*. Nothing is written through it (that's the round-1 fix, confirmed), but your link is gone without a warning. `archive.rs` `merge_replace`. *Fix:* add a warning ("data was a link; replaced") or skip it.
- **L2.** No free-space check before a GOD convert (already listed in REVIEW.md L2). It fails cleanly: I tested a read-only output, and the ISO was kept with no leftovers.
- **L3.** `dist/Unpack-Cleanup.exe` / `cleanup/cleanup.c` is new and isn't mentioned in this round's REVIEW.md section. I read the source: it's per-user only, asks first, only touches Unpack's own keys and folders, and only removes the "Uninstall" key if `uninstall.exe` is gone. I didn't run it (Windows).

## Delete-ISO safety, what "success" actually checks
- Deleted **only** if `convert_named` returned Ok **and** the canonical ISO path is strictly inside the canonical extract root **and** `verify_output` passes (`god.rs:283-289`).
- ✅ Never on failure: a non-Xbox ISO, a corrupt ISO (cut inside the directory table) and a read-only output all kept the ISO. A non-Xbox ISO creates no folder.
- ✅ Never on cancel: when I cancelled mid-part (Rust and UI), the ISO was kept, the temp folder was removed, and no empty `(GOD)` folder was left.
- ✅ Never through symlinks: `find_isos` skips file and folder symlinks. Passing a symlinked ISO directly resolves outside the root, so it isn't deleted. The prefix trick (`ex` vs `ex2`) and an ISO outside the root are not deleted.
- ✅ An ISO already in the shared output folder (not inside the new extract folder) was never touched (Number mode).
- ❌ **Wrong file:** C1. Pre-existing ISOs inside a *Replace-merged* folder are treated as extracted and deleted.
- ❌ **Partial output counted as complete:** H1. A truncated ISO passes verification.

## Batch into one folder
- Old H1 race: **fixed**. 200 parallel same-name runs in Number mode gave 0 merges and left no temp folders (the repo's own `h1_` test passes too). In Replace mode, merging is by design.
- Same-named files from different archives: in Number mode each archive gets its own folder, so no overwrites. Replace merges, as documented.
- Path traversal: evil.zip (`../`, `/abs`, `..\`) stayed inside. Symlinks in the shared destination are no longer followed on Replace (I tested both the Here and Folder layouts).

## Round-1 findings
| Round-1 item | Status |
|---|---|
| M1 Replace follows symlinks | **Fixed** (I tested it; see new L1 for the side effect) |
| M2 cancel deletes the previous GOD | **Fixed**. Builds in `.unpack-god-tmp-*` and swaps in on success. The repo test passes, and my cancel test left nothing behind. |
| M3 0-byte preview of a missing entry | **Fixed**. Returns "nope.txt was not found in the archive." |
| M4 `unsafe-eval` | **Fixed**. The CSP is now `script-src 'self' 'wasm-unsafe-eval'`. The unrar bundle is patched to remove `new Function`, there are no `eval`/`new Function` calls left in `src/`, the e2e suite runs under the real CSP, and `read_file` is limited to archive names. |
| H1 same-name race | **Fixed** (above) |

## UI redesign
- XSS: every new template I checked (GOD sub-rows, settings, theme switch, file badges) uses `escapeHtml`, and the badge labels come from a fixed table. **Live test:** an ISO named `<img src=x onerror=alert(1)>.iso` shown in the queue and GOD rows injected no `<img>`, and no dialog fired.
- No CDN or remote links: `index.html` loads only `./vendor/fonts/fonts.css`, `./theme.js` and `./app.js`, and grep found no http(s) URLs other than SVG namespaces. The CSP got tighter, not looser.
- Regressions: none found. The full e2e suite (42) and autogod (12) pass with no page errors.
- Installer: `webviewInstallMode: embedBootstrapper` (fixes the round-1 note about the installer downloading). The NSIS hooks now use `-- "%1"`, clean up empty keys, and fix Tauri's .7z/.rar backup handling. **Reviewed by reading only.**

## Verdicts
| Feature | Verdict |
|---|---|
| Batch extract into one folder | **PASS** |
| Auto ISO → GOD (convert) | **PASS**. Non-Xbox, corrupt, nested and cancel cases all behave. |
| Delete ISO after success | **FAIL**: C1 (deletes your own pre-existing ISOs with Replace) and H1 (truncated ISO passes "verified"). |
| UI redesign (XSS / CDN / CSP / regressions) | **PASS** |
| Round-1 fixes | **PASS**: all fixed, none regressed. |
| **Overall** | **FAIL** until C1 and H1 are fixed. Until then, keep "Delete ISO after convert" off, or disable it whenever Collision = Replace. |

---

# Round 3: re-check of commit 215c77f, focused on delete-ISO (Tuesday, Oct 20, 2026, ~1:40–2:05 AM CDT)

Git state before I wrote this section: **clean**. All of the update is committed in `215c77f`.

## Reported counts, re-run by me
| Suite | Code Smith | Mine |
|---|---|---|
| `cargo test --lib` (Linux) | 33/33 | **33/33** ✅ |
| Same suite cross-compiled for Windows, run under Wine with the bundled `7za.exe` | 32/32 | **32/32** ✅ |
| `tests/e2e.mjs` | 42/42 | **42/42** ✅ |
| `tests/autogod.mjs` | 15/15 | **15/15** ✅ (screenshots now go to `/tmp/unpack-shots`, and `git status` stayed clean) |

## Delete-ISO attack results (my harness + UI scripts, delete on)
| Case | Result |
|---|---|
| **Round-2 C1**: my own `merge/keep/game.iso` and `disc/mine/game.iso`, Replace | ✅ **Fixed.** Both kept and not converted. The extracted ISOs are converted but kept, because the merge turns delete off. |
| **Round-2 H1**: 400 MB → 200 MB truncation | ✅ **Fixed.** Refused as "incomplete or truncated", and the ISO is kept. |
| Truncated by 1 byte | ✅ Refused, ISO kept |
| Truncated just past the directory | ✅ Refused ("couldn't read executable"), ISO kept |
| Only the last part missing (cut at part 3) | ✅ Refused, ISO kept |
| Exact-length valid ISO | ✅ Converted, verified, deleted |
| Directory claims a smaller size than the file / extra bytes at the end | ✅ Converted (trimmed like iso2god), verified, deleted. Correct behaviour. |
| Corrupted middle, right length | Converted and deleted. **Expected**: GOD is a faithful copy of the ISO's bytes, so this isn't a converter bug. Nothing can check an ISO's "real" contents without a known-good hash. |
| Case-variant `GAME.ISO`, unicode `ゲーム 😀 Ünï.iso` | ✅ Both found, converted and deleted correctly |
| Symlink named `.iso` in a tar (pointing outside) | ✅ Dropped during extraction, not listed, and the outside file is untouched |
| Hard link `hard.iso` → `a.iso` inside the same tar | Both listed and both deleted. They're the same data inside the batch's own folder, so nothing outside is lost. OK. |
| Disk full mid-write (simulated with a 100 MB file-size limit, so writes fail with EFBIG) | ✅ "Error writing part 0", ISO kept, temp folder removed, no `(GOD)` folder left |
| Cancel at each stage (Starting, part 1, 2, 3, Hashing) | ✅ All CANCELLED, ISO kept, no leftovers |
| Cancel at "Done" (after the package is installed) | Still deletes the ISO and reports success (low, see L1) |
| Replace into a **fresh** folder vs a **merged** one | ✅ fresh → `merged:false` (delete allowed). Merged → `merged:true` (delete off). |
| **Same path from two archives in one batch** (Replace) | ❌ **Bypass, see H1 below** |
| Does verify read the real output? | ✅ Yes, it re-reads the files on disk (sizes, master hash lists, header), both in the temp folder and again after the move. **But** it doesn't re-hash the data blocks (M1). |

## Bugs, ranked

### Critical
None. The round-2 critical (C1) is fixed.

### High

**H1. The merged flag can be bypassed when two archives in one batch extract to the same path.** `src/app.js:747,760` (delete decided per job, from that job's `merged` flag) and `src-tauri/src/god.rs:331-350` (deletes whatever file is at the path when the conversion finishes)
- *What breaks:* With Replace, archive A extracts into a fresh `Game/` (not merged, so delete is allowed). Archive B then merges into the same `Game/` and overwrites `Game/game.iso` before A's queued GOD conversion runs. A's job then converts **B's** ISO and deletes it, even though B's job was marked merged, which is supposed to mean "don't delete". B's own job then fails with "Cannot open ISO: No such file". A's game never gets converted. Nothing outside the batch is lost, and both source archives are untouched. But you end up with the wrong GOD, one missing GOD, and a delete that the merged rule should have blocked. If B lands *while* A is converting, the length-only "ISO changed" check (`god.rs:281`) won't catch a same-size swap.
- *How:* `a.zip` (Game/game.iso, Halo 3 4D5307E6) and `b/a.zip` (Game/game.iso, title 0ABCDEF1), dropped together with Replace, auto-GOD + delete on. UI result: "GOD failed · Cannot open ISO", then "GOD done → game (GOD)/0ABCDEF1 · extracted ISO deleted". I reproduced it the same way in the Rust harness.
- *Fix:* when extracting, record each ISO's identity in Rust (device + inode/file ID, size, mtime) and return it with `isos`. In `convert_extracted`, refuse to convert or delete unless the file still has that identity, both before converting and right before `remove_file`. Simpler still: turn delete off for the whole batch when Collision = Replace, or whenever two jobs in the batch share an output folder.

### Medium

**M1. "Strict verify" doesn't check the data itself.** `src-tauri/src/god.rs:98-127`
- *What breaks:* it checks part sizes, the chain of master hash lists, and the `LIVE` header plus Data0000's hash. It never re-hashes the actual 4 KB data blocks against their sub-hash lists. A bad write in the middle of a part (bit rot, flaky USB or SD card, an antivirus touching the file) passes, and then the ISO is deleted.
- *How:* I converted a 3-part package, overwrote 4 KB of data inside `Data0001`, and called `verify_package`: **Ok(())**. Zeroing a master hash or breaking the header *is* caught.
- *Fix:* re-read each part and SHA-1 every block against its sub-hash list, and every sub-hash list against the master. That costs one extra read of the output, about the same time as the conversion. Only do it when delete is on.

**M2. The delete decision is trusted from the page.** `src-tauri/src/lib.rs` `god_extracted(…, delete_iso: bool)`. Rust doesn't know whether that extraction merged. It deletes any `.iso` inside whatever `extract_root` the page sends. With the current tight CSP this needs a page compromise, so it's medium-low. *Fix:* keep a Rust-side registry of what each extract job produced (path + identity + merged), and have `god_extracted` take only a job ID + index.

### Low
- **L1.** Cancel at the very end (after the package is moved into place) is ignored. The ISO is still deleted and the job says done (`god.rs:286-350`: no cancel check after `install`). *Fix:* check `is_cancelled` before `remove_file` and keep the ISO if it's set.
- **L2.** Corrupted-but-full-length ISOs convert and delete (expected, see the table). Consider a note in the delete confirmation: "Unpack can't tell whether a game ISO itself is damaged."

## Round-2 low fixes
- Screenshots go to `/tmp` unless `UPDATE_SHOTS=1`: ✅ (git stayed clean after running autogod).
- Symlink-replaced warning: ✅ present (`archive.rs:600`).
- Free-space check: ✅ by reading (`god.rs:185`, uses `df` on Linux and `GetDiskFreeSpaceExW` on Windows). The repo test `free_space_estimate_matches_output` passes on Linux and Wine. A failure mid-write is handled cleanly (EFBIG test above).
- Cleanup tool documented: ✅ README.md:62, REVIEW.md:294.
- Regressions: none found. All 4 suites pass, and path traversal, symlink and race checks still hold (the repo's own tests for them pass).

## Verdicts
| Feature | Verdict |
|---|---|
| Batch extract into one folder | **PASS** |
| Auto ISO → GOD | **PASS** |
| Truncated / incomplete ISO refusal | **PASS** |
| Strict GOD verify | **PASS with a caveat**: real on-disk checks, but no data-block re-hash (M1) |
| Delete ISO after success | **FAIL**: H1 (the merged rule is bypassed by a same-path collision in one batch). Every other way in I tried was blocked. |
| UI / CSP / round-2 low fixes | **PASS** |
| **Overall** | **FAIL**, narrowly. Fix H1 (identity check, or no delete with Replace) and ideally M1. Until then, the safe setting is: delete on only with Collision = "Add a number". |

---

# Round 4: re-check of commit f5c10ec (Tuesday, Oct 20, 2026, ~1:58–2:20 AM CDT)

Git state before I wrote this section: **clean**.

## Reported counts, re-run by me
| Suite | Code Smith | Mine |
|---|---|---|
| `cargo test --lib` (Linux) | 36/36 | **36/36** ✅ |
| Windows cross-build under Wine (real `7za.exe`) | 35/35 | **35/35** ✅ |
| `tests/e2e.mjs` | 42/42 | **42/42** ✅ (one earlier run timed out while my 4 GB performance test was hogging the disk. The clean re-run passed.) |
| `tests/autogod.mjs` | 15/15 | **15/15** ✅ |

## Re-runs of the round-3 findings
- **R3 H1 (two archives, same `Game/game.iso`)**, run through the real `extract()` + `convert_registered()`, the same code the UI command calls:
  - Replace: A is refused ("replaced or changed after it was extracted… not converted"). B converts, and its ISO is kept ("deleting is off when … Replace files"). ✅ **Fixed.**
  - Add a number: separate folders. Both convert and both delete correctly. ✅
- **R3 M1 (4 KB corruption in the middle of Data0001):** the deep check reports "Data0001 block 253 is damaged" and the ISO is kept. ✅ **Fixed.**

## Attacks on the new design
| Attack | Result |
|---|---|
| Sampled identity: same size, mtime restored, a byte changed outside the three 1 MiB samples | ❌ **Not detected.** Converted *and deleted* (see M1) |
| Same, without restoring the mtime | ✅ Refused ("replaced or changed") |
| Token: forged unknown token | ✅ "Unknown ISO" |
| Token: guessed (format is `iso-<pid>-<counter>`, sequential) | Accepted, but it only names an ISO that was already registered. It gives no access to any other path. (L1) |
| Token: reused without delete | Allowed. Each reuse makes another `(GOD)-N` folder. (L1) |
| Token: reused with delete after a non-delete run | Allowed, and it still runs every check. OK |
| Token: replayed after a delete | ✅ "Unknown ISO" (removed from the registry) |
| Token expiry | None. It lives until delete or app restart. (L1) |
| A Replace-registered token sent with want-delete | ✅ Kept: "deleting is off… Replace" |
| Raw-path delete gone? | ✅ The `generate_handler!` list (`lib.rs:294`) has `god_extracted(id, token, delete_iso)` only. There is no `find_isos` and no path-based delete. Capabilities are unchanged (dialog + opener only). `iso_to_god` still takes a path, but it never deletes. |
| Deep re-hash: last byte of the last block | ✅ "Data0002 block 14852 is damaged" |
| Deep re-hash: a byte in a sub-hash list | ✅ "sub-hash list 0 doesn't match the master hash" |
| Deep re-hash: header area (title ID 0x360, title text 0x411, block count 0x392, part count 0x3a0, content type 0x344, media ID 0x354, and even the header's own SHA-1 at 0x32c) | ❌ **All pass as OK** (see M2) |
| Cancel at Starting, part 1, part 2, part 3, Hashing | ✅ CANCELLED, ISO kept |
| Cancel at "Done" and during "Re-checking GOD data" | ✅ The package is kept, and so is the ISO ("ISO kept: cancelled.") |
| Disk full / truncation / symlink cases from round 3 | Unchanged code paths. The repo tests pass. |
| **Performance**, 4.2 GB ISO (25 parts), *debug* build, on the box's SSD | Total **75 s**, of which the deep re-hash took **36 s**, roughly doubling the time when delete is on. A release build will be faster; this is fine for a hobby tool. Consider showing "Re-checking… N%" progress, because right now the bar sits at 100% for that whole time. |

## Bugs, ranked

### Critical
None.

### High
None. Round-3 H1 is fixed.

### Medium

**M1. The sampled identity can be fooled by a same-size swap that keeps the mtime.** `src-tauri/src/god.rs:342-370`
- *What breaks:* only the size, the mtime and 3 MiB of samples (start, middle, end) are checked. A file changed elsewhere with its mtime put back passes, then converts and gets deleted. Inside the app this is now hard to reach. Delete only happens for fresh, non-Replace folders, so no other archive in the batch writes there. And 7-Zip *does* set mtimes from the archive, so two different same-size ISOs with the same stored timestamp would look identical. It mainly matters for an outside program, or a later feature that writes into that folder.
- *How:* I registered a 400 MB ISO, wrote 12 bytes at offset 100 MB, restored the mtime with `set_modified`, then called `convert_registered(token, delete=true)`: **Ok, iso_deleted = true**.
- *Fix:* also record the file ID (inode / Windows file index + volume serial) and ctime/change time (which can't be set from user space), or keep the ISO open with a share-deny-write handle from registration to delete. Cheapest of all: hash the full ISO while converting (it's read in full anyway) and compare it with a full hash taken at registration, or with a second pass right before delete.

**M2. The deep check ignores the GOD header, except its magic and Data0000's hash.** `god.rs:120` / `:479`
- *What breaks:* a damaged header (wrong title ID, block or part counts, content type, even a broken header SHA-1) passes. The console won't show or launch the game, yet the ISO has already been deleted.
- *How:* I flipped single bytes at 0x360, 0x411, 0x392, 0x3a0, 0x344, 0x354 and 0x32c in a finished header. `deep_verify` returned **Ok** every time.
- *Fix:* rebuild the expected header from the same inputs (`ConHeaderBuilder` is deterministic) and compare byte for byte, or at minimum recompute SHA-1(0x344..0x344+0xacbc) and compare it with the bytes at 0x32c, plus check the title-ID, part-count and block-count fields.

### Low
- **L1.** Tokens are sequential and guessable, reusable, and never expire (`god.rs:386`). They don't give access to anything that isn't registered, so the risk is low. *Fix:* use a random 128-bit token, mark it used after one conversion, and drop it when its batch is cleared.
- **L2.** There's no progress during the deep re-hash: the bar shows 100% for about as long again as the conversion took. *Fix:* report "Re-checking N%" per part.

## Regressions
None found. All four suites pass, and the earlier traversal, race and symlink checks are covered by the passing repo tests. The new confirmation text mentions both "never with Replace" and "can't detect an already-damaged ISO". ✅

## Verdicts
| Feature | Verdict |
|---|---|
| Same-path collision (round-3 H1) | **PASS** |
| Deep data re-hash | **PASS** for data blocks and hash lists, **FAIL** for the header area (M2) |
| ISO identity re-check | **PASS** against in-app swaps, but bypassable with a same-size, mtime-preserving change (M1) |
| Token design / raw-path delete removed | **PASS** (L1 hardening only) |
| Late cancel at every stage | **PASS** |
| Performance | **PASS** (about 2× time with delete on; L2) |
| Batch / auto-GOD / UI / earlier fixes (regressions) | **PASS** |
| **Overall** | **PASS with 2 medium fixes recommended.** There are no critical or high issues left, and none of my in-app attack paths could make it delete the wrong ISO or a pre-existing one. M2 (header check) is the one I'd fix before trusting delete on a real collection, since a bad header means an unbootable GOD after the ISO is gone. |

**Note on concurrent changes:** while this review was running, a new commit `c1669f6` ("Xbox 360 DLC…") landed at 2:08 AM CDT, and `src-tauri/src/dlc.rs` + `dlc/tests.rs` now have uncommitted edits. Neither was made by me. My Rust, Wine and harness results above were built from `f5c10ec`, before that commit. The final e2e re-run (42/42) probably ran against `c1669f6`'s frontend. The DLC feature has **not** been reviewed.

---

## Round 7 — 2026-10-20 (CDT), commit 2151b3c

Built from a clean `git archive 2151b3c` copy in /tmp (not the live tree). Network tests used only local servers on 127.0.0.1 and the box IP. One disclosure: a nip.io hostname test triggered one external **DNS lookup** through the box resolver. No HTTP request went to any outside host. "By reading" means I checked the code but didn't run it.

### Counts (all actually run)
Rust 73/73 · Wine 69/69 · e2e 42/42 · autogod 15/15 · dlc 16/16 · download 16/16 · mods 26/26 — **PASS**

### Bugs, ranked by severity

**Medium**
1. **DNS rebinding beats the SSRF check.** At download.rs:105 `assert_safe_url` resolves the host and checks it, then ureq resolves it again to connect. I used a shim where rebind.test returned a public IP for the first 4 lookups and 127.0.0.1 after that. The download passed every check, then connected to 127.0.0.1 and downloaded from the local server. **Fix:** give ureq a custom Resolver that returns only the addresses you already checked (pin the IP), on every redirect hop too.
2. **A crafted ISO crashes the whole app (stack overflow).** vendor iso2god `iso/directory_table.rs:120` recurses into subdirectories with no depth limit and no loop check. A directory that points at itself, a two-directory cycle, or 60k-deep nesting each abort the process. I reproduced this on Linux and under Wine. It is reachable through iso_info, ISO→GOD and ISO→XEX, including auto-processing of an ISO pulled out of an archive. **Fix:** cap the depth (for example 64), keep a set of visited sectors, or make the parser iterative.

**Low-Medium**
3. **`.partial.meta` follows symlinks.** download.rs:363 writes it with `fs::write`. If `<name>.partial.meta` is a symlink, the file it points to gets overwritten with JSON (/tmp/ov7-victim was clobbered). **Fix:** remove the file first, then open it with create_new, or refuse symlinks (symlink_metadata).

**Low**
4. **Resume can produce a corrupted file with some servers.** download.rs:501–530 sends If-Range, but never compares the ETag on the 206 reply with the saved one. A server that ignores If-Range but honors Range gave a file that was old prefix + new tail. **Fix:** if the ETag or Last-Modified differs, throw away the partial and restart.
5. **A bad expected hash is silently ignored.** download.rs:657 only checks the hash when it is exactly 64 hex characters. A 63-character or garbage value means no check at all, and the download "succeeds". The UI doesn't validate it either. **Fix:** reject a malformed expected hash up front.
6. **Quarantine can overwrite an older file.** download.rs:658 renames to `.bad-hash`, which can replace an existing `.bad-hash`. **Fix:** use a unique name.
7. **Some Windows reserved names get through.** In sanitize_name (download.rs:317), `Lpt9.tar.gz` (more than one dot), `nul .txt` (space before the dot) and `CONIN$`/`CONOUT$` aren't caught. Under Wine those downloads fail with "File not found" or "Invalid handle". A U+202E right-to-left override is kept, so `piz.exe` can display as a fake `.zip`. A non-ASCII quoted filename* falls back to the URL name. **Fix:** compare the part before the first dot, trimmed, against the reserved list, add CONIN$/CONOUT$, and strip bidi control characters.
8. **IPv6 literals fail by accident.** `host_str` keeps the brackets, so `[::1]`, `[::ffff:127.0.0.1]`, `[fc00::]` and `[fe80::]` end up at "Cannot resolve". They're blocked, but not for the right reason, and a public IPv6 literal never works. 6to4 and NAT64 ranges aren't in is_blocked_ip (:75). **Fix:** use `url.host()` and match on Host::Ipv6.
9. **The test-only loopback switch ships in the release build.** `UNPACK_TEST_ALLOW_LOOPBACK` is read in production code (download.rs:108). **Fix:** put it behind `#[cfg(test)]` or a feature flag.
10. **Saved-links import/export has gaps (app.js:1150 and :1171).**
    - CSV export has formula injection: a cell like `=1+1` stays live. Fix: prefix cells starting with `= + - @` (or tab/CR) with `'`.
    - Import keeps `javascript:` and `file:` URLs and "[object Object]" (Rust refuses them later at download time). Fix: keep only http/https on import.
    - The CSV path doesn't cap lengths (a 10,000-character note went in).
    - Duplicates inside one file aren't removed.
    - No XSS, and malformed or huge files are handled.
11. **ISO→XEX deletes the ISO after checking sizes only.** verify_tree (iso_extract.rs:53) doesn't re-hash content the way GOD does (by reading).
12. **`dest` isn't gated.** `iso_to_god` and `iso_to_xex` (lib.rs:469 and :488) accept any destination folder from the page (by reading).
13. **Library edge case.** A library root that itself contains default.xex shows up as one game covering the whole root.

### Per feature
| Feature | Result |
|---|---|
| grant_dir removed; DragDrop grants only for the session, links skipped, page can't fake them | PASS (by reading + run) |
| SSRF (loopback, private, link-local, decimal/hex/octal, 0.0.0.0, schemes, private redirects) | PASS, except DNS rebinding **FAIL** (#1) and IPv6 (#8, blocked by accident) |
| Relative, absolute-path and protocol-relative redirects; https→http refused | PASS (the downgrade only by reading plus the unit test) |
| Hoster page fetch with an exact host match | PASS |
| Resume with If-Range and a bad Content-Range | PASS for a compliant server; Low FAIL #4 |
| Size cap (50 GB declared, endless stream at a 5 MB cap) | PASS |
| Stall timeout (61 s) | PASS; trickle server limited by the 1 h overall timeout (by reading) |
| SHA mismatch: quarantined, never extracted or queued | PASS; Low #5 and #6 |
| http warning, `;` in names, CON/con/AUX. | PASS; Low #7 |
| iso_extract: traversal, odd names, long names, control characters, duplicates, sizes, truncated ISO, bomb, output stays in the folder | PASS (also on Wine; case-only duplicates error cleanly) |
| iso_extract: directory loops and deep nesting | **FAIL** (#2) |
| Auto mode Off/GOD/XEX with delete-ISO (sealed deletes, unsealed keeps, single-use tokens) | PASS |
| Saved-links import/export | **FAIL** (#10) |
| Homebrew Tools: 12 https cards; file:, javascript: and exe blocked by the opener scope | PASS (scope checked by reading plugin 2.7.2) |
| Library and Copy to USB Games: symlinks, containment, FAT32 (moot) | PASS |
| Regressions | none found |
| git status | clean, nothing modified |

**Overall: FAIL (conditional).** Fix the two Mediums (#1 DNS rebinding and #2 ISO stack overflow) and the import/export issue (#10). Everything else is Low.

---

## Round 8 — final — 2026-10-20 (CDT), commits a445b35 + 899b04a (HEAD)

I built everything from clean `git archive` copies in /tmp, never from the live folder. I tested the download and ISO attacks on a445b35. `git diff a445b35 899b04a` shows no change to download.rs, iso_extract.rs or the vendored ISO reader, so those results also cover HEAD. All counts, the UI tests and the Known mod names tests ran on 899b04a.

Network: everything used local servers on 127.0.0.1, ::1 and the box IP only. The download harness ran under an LD_PRELOAD shim that answers test hostnames itself and **refuses every connection to anything that isn't local**. So there were **no outside DNS lookups and no outside connections** (Chrome tests: 0 outside requests). "By reading" means checked in the code, not run.

### Counts (all run on 899b04a)
| Suite | Claimed | Result |
|---|---|---|
| Rust `cargo test --release` (normal, parallel) | 83 | **79/83 — FAIL**, every time (5 runs) |
| Rust with `--test-threads=1` | 83 | 83/83 |
| Wine (Windows test exe) | 79 | 79/79 |
| e2e / autogod / dlc / download / mods | 42 / 15 / 16 / 26 / 36 | 42 / 15 / 16 / 26 / 36 — all pass |

Why Rust fails in parallel: the test `ipv6_literals_and_blocked_ranges` (download.rs:1190) removes `UNPACK_TEST_ALLOW_LOOPBACK` without taking the `TEST_ENV` lock. That makes `downloads_with_sha` fail ("That address isn't allowed"). The poisoned lock then fails `hoster_host_exact_not_substring`, `refuses_non_http_and_html` and `ssrf_blocks_loopback_and_private`. The app code is fine; the test suite is broken. **Fix:** take `TEST_ENV.lock()` in that test, and lock with `unwrap_or_else(|p| p.into_inner())`.

### R7 fixes re-tested, plus attempts to get around each one
| Area | Result |
|---|---|
| **DNS rebinding** (my test resolver) | **PASS.** The connection now always goes to the address that was checked, and ureq never looks the name up again. rebind.test said "public" then "127.0.0.1": the app connected to the public address only (my shim refused it) and the local server got 0 requests. Same for a redirect hop and the MediaFire page fetch. A name that returns both public and local addresses is refused. v6map.test (resolves to ::ffff:127.0.0.1) is refused. |
| SSRF on the release-style build | **PASS.** All 35 forms blocked: 127.0.0.1, localhost, `LOCALHOST.`, 127.1, decimal/hex/octal forms, 0.0.0.0, 172.30.0.2, 100.64/10, 169.254.169.254, `[::1]`, `[::ffff:127.0.0.1]`, `[::ffff:7f00:1]`, `[::127.0.0.1]`, `[::]`, fc00/fd00/fe80/fec0, 2002:7f00:1:: (6to4), 64:ff9b:: and 64:ff9b:1:: (NAT64), mapped 10/8 and metadata addresses, ff02::1, user@host and host@host tricks, file/ftp/javascript. A redirect to 172.30.0.2 or to mapped/v6map local addresses is refused. A pinned `[::1]` literal downloads only in the test build, so IPv6 literals now work properly. Info: 2001:db8::/32 (documentation range) and Teredo 2001::/32 aren't in the block list; nothing routes there, and my shim refused the connection. |
| Loopback test switch removed from release | **PASS.** I built `cargo build --release` and grepped the binary: 0 copies of `UNPACK_TEST_ALLOW_LOOPBACK`, against 1 in the bridge/test builds. A harness built without the feature, run with the env var set to 1, still refused 127.0.0.1. |
| ISO self, mutual and deep folder loops on Linux **and Wine** | **PASS.** I tried self, mutual, a loop back to root, the same folder at a different size, 60k-deep, and depth 63/64/65, through iso_info, ISO→XEX and ISO→GOD. Every case gives a clean "cycle detected" or "too deep (max 64)" error, with no crash on either platform; depth 64 still works. Side effect: an ISO where two entries share one folder is now refused as a "cycle". Real Xbox ISOs don't do that. |
| `.partial.meta` / `.partial` symlinks | **PASS.** A symlinked, dangling-symlink or directory meta file is refused and the victim file isn't touched. A symlinked `.partial` is refused. |
| ↳ way around: **hard link** | **FAIL (Low).** If `a.bin.partial` is a hard link to another file and a valid meta file sits next to it, resume appends into that other file (victim became `KEEP`+new data). The no-follow check only catches symlinks (download.rs:645). **Fix:** refuse to resume unless the file has exactly one link (or the same file id on Windows), or copy the partial to a new file before appending. You need write access to the download folder to set this up, so it's Low. |
| ETag / Last-Modified compare | **PASS.** Server ignores If-Range and sends a new ETag: "ETag changed", partial cleared. The 206 reply has no ETag: refused. Changed Last-Modified: refused. Same ETag or Last-Modified: resumes correctly. |
| ↳ way around: no ETag or Last-Modified saved | **Low.** If the first reply had neither, resume sends a plain Range with nothing to compare, so a changed file still gets old start + new end (download.rs:668). A weak `W/` ETag is also accepted for resume. **Fix:** start over when there's no strong validator. |
| Malformed hash | **PASS.** 63 or 65 characters, `zz`, `sha256:` prefix, a space inside, and a full-width character are all refused before any download. Upper case and surrounding spaces are accepted and normalized. The UI checks too. |
| Quarantine name collision | **PASS.** With existing `.bad-hash`, `-2`, a symlinked `-3` and a dangling `-4` already there, the new quarantine files went to `-4` and `-5`. None of the old files changed and the victim is untouched. The dangling link was replaced, not followed. |
| Reserved and bidi names (download) | **PASS** on Linux and Wine: Lpt9.tar.gz, `nul .txt`, CONIN$, CONOUT$.log, `con.`, `AUX.`, `con  .txt`, ` CON`, COM0. Bidi characters U+202E, U+2066–2069, U+200F and U+061C become `_`, both in raw `filename=` and in `filename*`. **Low, Windows-only, by reading:** Windows 10/11 also reserve `COM¹ COM² COM³ LPT¹ LPT² LPT³` (superscript digits); these pass through unchanged (download.rs:384). Wine doesn't treat them as devices, so I couldn't run this. Zero-width space and BOM are kept (cosmetic). |
| ISO extract names | Traversal (`..`, `/`, `\`, `C:`, control characters, 255 characters) stays inside the folder, **PASS** on Linux and Wine. **Low:** the ISO name cleaner (`archive::safe_segment`, archive.rs:393) misses `CONIN$` and `nul .txt`. On Linux they're written as-is; on Wine extraction fails with "Invalid handle". Contained, no escape, just a crafted ISO that won't extract on Windows. Fix: reuse the download sanitizer's reserved-name check. |
| XEX delete after byte check | **PASS (run).** I hooked the "Re-checking" step. Changing one byte of a same-size file (middle or end) gives "Content mismatch… ISO was kept". An extra file gives "Extra or missing files… kept". Untouched: the ISO is deleted. |
| GOD/XEX `dest` grant check | **By reading only** (lib.rs:482, :502 `grants.check(&dest)`). The test bridge doesn't use the app's grant check for these two commands: an ungranted dest was accepted there. So this is a test gap, not an app bug. |
| Saved-links CSV / import | **PASS.** 19 hostile JSON rows plus a CSV: only http(s) kept (javascript:, JAVASCRIPT:, tab-split, file:, data: and `https:javascript:` all dropped). Duplicates that differ in case or spaces collapse to one. Notes capped at 500 (a 20,000-character CSV note too). Files over 2 MB refused; malformed JSON gives a toast; no XSS. Export CSV puts `'` in front of cells starting with `= + - @`, tab and CR. Info: leading-space ` =1+1`, full-width `＝`, and a cell that starts with a newline aren't prefixed. Excel and Sheets treat those as text. |

### New in 899b04a: Known mod names
| Test | Result |
|---|---|
| Starts empty, nothing bundled | PASS |
| Bad TitleIDs: 00000000, FFFFFFFF, non-hex, 7 characters | PASS (refused; Rust also needs the ID to be a known game) |
| Wrong length, too long | **FAIL (Low).** `4D5307E6FF` is silently cut to `4D5307E6` and accepted (app.js:1203 cuts to 8 characters before checking). **Fix:** check the length before cutting. |
| XSS in names | PASS (keys are normalized to plain words; the panel escapes them) |
| Formula injection on export | PASS (no exported cell starts with `= + - @`) |
| Malformed or huge import | PASS: malformed gives a toast, over 1 MB is refused, 700 rows are capped at 500, and Jiggy/jiggy collapse to one |
| Where it's stored, corrupt data | PASS. Stored in webview localStorage `unpack.modNames`. `{bad`, a plain string, an object, and an array with junk all load as an empty or filtered list without crashing, and the corrupt data isn't overwritten on load. A lowercase TitleID in storage is silently dropped. |
| **Name collisions → WRONG game** | **FAIL (Medium).** A learned or imported key beats strong evidence in the file name, at 99% and "confident" (mods.rs:101). That confident guess becomes the game the mod is filed under (app.js:1040). Tested with Halo 3 keys: <br>• "Black Ops II" gets the key `black ops`, so `Black Ops II Menu v3.xex` changes from **Black Ops II 95% → Halo 3 99%**. <br>• `jiggy`: `BO2 Jiggy Menu v5.xex` changes from Black Ops II 93% → Halo 3 99%. <br>• `aimbot`: `MW2 Aimbot.xex` changes from MW2 93% → Halo 3 99%. <br>The normalizer only removes the *chosen* game's own name, so another game's name or a generic word is accepted as a key, and an imported list from someone else can quietly mis-file mods. A file's header TitleID still wins. **Fix:** refuse keys that match any known game name or alias (or are generic words such as aimbot or godmode). When the built-in name guess is confident about a *different* game, mark it as a conflict and ask instead of auto-confirming. |

### Regression sweep (earlier rounds)
- Delete-ISO safety: autogod 15/15, the R4/R5 unit tests in the Rust/Wine runs, and the byte-tamper test above. PASS.
- Path traversal (zip-slip): e2e "nothing escapes"; ISO names. PASS.
- XSS: no page errors, no XSS from import names or notes. PASS.
- CSP: unchanged, strict, and e2e shows no CSP violations. PASS.
- Grants: the bridge refuses an ungranted download folder, and `grant_dir` is "unknown command". App-side checks on GOD/XEX dest are by reading.

### Read-only (Windows-native, not run on real Windows)
- The GOD/XEX `dest` grant check inside the real Tauri app (lib.rs:482 and :502).
- Windows superscript COM/LPT names.
- File-id/hard-link behaviour on NTFS.
- The tauri-plugin-opener scope (checked in R7).

### Overall verdict for HEAD 899b04a: **FAIL — not ready to ship yet.** Every R7 security fix holds and every UI/e2e count matches. These block a PASS:
1. **Medium:** Known mod names can send a mod to the wrong game at 99% "confident" (above).
2. **Test suite:** `cargo test --release` fails 4/83 in its default parallel mode, so the reported "Rust 83" only holds single-threaded.

Small leftovers to fix too: hard-linked `.partial` append, resume with no validators or a weak ETag, TitleID cut to 8 characters, superscript COM/LPT names, and CONIN$ / `nul .txt` in ISO names. Once #1 and #2 are fixed and checked, the rest is Low.

---

## Round 9 — final — 2026-10-20 (CDT), HEAD 6bf5f8b

I built everything from a clean `git archive 6bf5f8b` copy in /tmp. Network: local servers only (127.0.0.1 and ::1). The download harness ran under a shim that answers test hostnames itself and **refuses any connection to anything that isn't local**, so there were **no outside DNS lookups or connections**; the Chrome tests made 0 outside requests. Disk before: 31 GB free. I kept scratch small and deleted the Wine build folder as soon as I was done with it.

### Counts (all run)
| Suite | Claimed | Result |
|---|---|---|
| `cargo test --release`, parallel, 5 runs | 89 | **89/89 ×5** — PASS |
| Wine (Windows test exe) | 83 | 83/83 — PASS |
| e2e / autogod / dlc / download / mods | 42 / 15 / 16 / 27 / 39 | 42 / 15 / 16 / 27 / 39 — PASS |
| Release binary vs test loopback switch | — | 0 copies of `UNPACK_TEST_ALLOW_LOOPBACK` in the release binary (1 in the bridge). A non-test harness with the env var set to 1 still refuses 127.0.0.1 — PASS |

### R8 items re-tested, plus attempts to get around each fix
| Area | Result |
|---|---|
| **Known mod names, wrong game** | **PASS for the R8 cases.** <br>• "Black Ops II…" and "black ops" are refused, including `BLACK   OPS` and `Black-Ops`. <br>• So are "ops", "duty", "bo2", "cod", "cod4", "mw2", "modern warfare", "halo", "reach", "gta", "grand theft", "minecraft" and "skyrim". <br>• Aimbot, AIMBOT, "aim bot", "godmode trainer", "zombies" and "ab" are refused. <br>• Bad keys already stored or imported never match. <br>• Conflicts now ask: `BO2 Jiggy Menu v5` gives Black Ops II 93% then Halo 3 80%, not confident, so "Which game?" appears. The same happens with an archive hint "BO2 pack.zip", and for `MW2 Aimbot` and `Black Ops II Menu v3` (correct game). The longest key wins. |
| ↳ ways around it | **Low.** <br>• Joined or slang spellings of a game name are accepted as keys: `blackops`, `blops`. <br>• So are plural or leet generic words: `aimbots`, `aimb0t`. <br>• The built-in name guess doesn't recognise those spellings, so there's no conflict to catch. With a Halo 3 list, `Blackops 2 Menu.xex`, `Blops Zombies Menu.xex`, `Aimbots v3.xex` and `Aimb0t.xex` file as **Halo 3 99%, confident**. <br>• Needs a wrong or hostile list (for example an imported one), and the header TitleID still wins. <br>• Fix: also compare keys with spaces removed against game names and aliases (`blackops`). Stem the generic list (plural `s`, 0→o). Consider starting imported keys as "suggest, not confirm" until the user confirms one. <br>• Info: `jig` matches only the whole word (`jigsaw` doesn't match). |
| Long and odd TitleIDs | **PASS.** `4D5307E6FF`, 7 hex characters, `0x…`, full-width digit, NUL, 00000000 and FFFFFFFF are all refused, in JSON import, CSV import and the Rust `mod_name_key`. Lower case and surrounding spaces are accepted and normalized. |
| Hard-linked `.partial` | **PASS on Linux:** the link is dropped, the download restarts, and the victim stays `KEEP`. Same for a hard-linked meta file. Symlinked partial or meta: refused. |
| ↳ on Wine (Windows code path) | **Low.** The victim is kept, because the link is broken by copying first. But the hard-linked file is still treated as a valid partial, so the finished download starts with the victim's bytes (`KEEP\n` + server tail). It's corrupted, and it leaks the victim's content into the download. Fix: on Windows read the link count (GetFileInformationByHandle `nNumberOfLinks`) and restart like on Unix. Needs a hard link and meta planted in the download folder. |
| No, weak or missing validators | **PASS.** No ETag/Last-Modified, or a weak `W/` ETag only: restart with no Range sent. Changed ETag, a missing ETag on the 206, or a changed Last-Modified: refused and partial cleared. Same ETag or Last-Modified: resumes correctly. A weak ETag from the server is never saved. Info: a *hand-planted* meta with a weak ETag plus Last-Modified still resumes on the ETag compare; it can't occur naturally. |
| COM¹ / LPT² / CONIN$ / `nul .txt` | **PASS on Linux and Wine**, for both downloads and ISO names: `_COM¹.txt`, `_LPT²`, `_lpt³.tar.gz`, `_CONIN$`, `_CONOUT$.log`, `_nul .txt`, `_AUX .bin`. `COM¹0` is left alone (correct). The ISO with these names now extracts on Wine (it failed in R8). |
| Bridge dest grant for GOD/XEX | **PASS.** An ungranted dest, a `granted/../ungranted` dest, or an empty dest are all refused and nothing is written. A granted dest works. |
| ISO loops (self, mutual, 5000-deep) | PASS, Linux and Wine. |

### Regression sweep
| Area | Result |
|---|---|
| SSRF | PASS: loopback, localhost, decimal/hex, `[::1]`, mapped, NAT64, 6to4, metadata, 10/8, mixed answers, file: |
| DNS rebinding | PASS. Direct, through the MediaFire page and on a redirect hop, the connection goes only to the checked address and the local server got 0 requests. |
| Delete-ISO | PASS: autogod 15/15 plus unit tests. The ISO→XEX delete code hasn't changed since R8, where the byte-tamper test passed. |
| Grants | PASS (see above; download folder grant in the suites) |
| Zip-slip | PASS (e2e) |
| XSS | PASS: no XSS from name import, and no page errors |
| CSP | PASS: e2e shows no CSP violations, and the CSP config is unchanged |
| Saved/names export formula escaping | PASS |

### Read-only (Windows-native, not run on real Windows)
- The real Tauri app's `iso_to_god` / `iso_to_xex` grant check (lib.rs). I tested the bridge's matching check instead.
- Real Windows (not Wine) treatment of `COM¹`/`LPT²` as devices.
- NTFS hard-link behaviour beyond what Wine emulates.
- The opener plugin scope (R7 reading).

### Overall verdict for HEAD 6bf5f8b: **PASS (with Low follow-ups).**
Every R7 and R8 Medium is fixed and holds against the variants I tried. All counts match and the parallel tests are stable over 5 runs. What's left is Low and none of it is a security hole in normal use:
1. Misspelled game names and plural/leet generic words are still accepted as learned keys.
2. A hard-linked partial on Windows produces a corrupted download, but the other file stays safe.

---

## Round 10 — 2026-10-20 (CDT)

Commits reviewed: `ff440ff` (Send to Xbox FTP), `731ce15` (built-in menus loader, editable per-item destinations), base `a932981`. I built in-tree (target kept) and ran the suites. My hostile tests went through the debug bridge (`--features download-test-loopback`) against `fake_aurora.py`, a pyftpdlib variant, and a raw-socket hostile server I wrote, all on 127.0.0.1. No outside connections. No source files were changed.

### Test results
| Suite | Result |
|---|---|
| `cargo test --release --lib`, 5 runs, default parallel mode | 110/110 each run (1 ignored benchmark) |
| Wine (Windows test exe) | 104/104 (1 ignored) |
| e2e / autogod / dlc / download / mods | 42/42 · 15/15 · 16/16 · 27/27 · 39/39 |
| builtin.mjs / **xbox.mjs** (+ fake_aurora.py) | 9/9 · 41/41 |
| Release `unpack` binary | 0 copies of `UNPACK_TEST_ALLOW_LOOPBACK` and 0 of `UNPACK_TEST_MENUS` |

### Bugs

**HIGH-1: "Never overwrites" fails when the folder listing fails. A file already on the console gets replaced.** `src-tauri/src/ftp.rs:586` (`ftp.list(&parent).ok()`)
- **What breaks:** `send()` treats any LIST error as "this folder has nothing in it". Possible errors include a 425 data-connection refusal, a timeout, or a dropped data socket. Xbox FTP servers often fail data connections, which is why the code already has 425/421 retry logic. When LIST fails, the conflict check is skipped, the upload goes to `.part`, and RNTO then replaces the existing file. Many servers (pyftpdlib among them) let RNTO overwrite. After that, the SIZE check passes against the new file. The user loses whatever was on the console, for example a save or an existing game file, and Unpack reports "sizes verified".
- **How I triggered it:** I ran pyftpdlib with LIST answering `425 Can't open data connection` and put an 18-byte `/Hdd1/Games/game.bin` on it. I then sent a 5000-byte `game.bin` with kind custom `/Hdd1/Games`. Result: `sent:1`, and the console file was overwritten with 5000 bytes.
- **Related:** `ftp.rs:308` silently cuts a listing off at 16 MB, so a very large folder can hide an existing file in the same way. There is also a time-of-check gap: a file created between the plan and RNTO gets overwritten too (by reading).
- **Fix:**
  - Only treat the folder as missing when CWD to it fails (`Ok(false)`).
  - For any other LIST error, or a listing that hit the 16 MB cap, stop the job with an error.
  - Optionally, just before RNFR, do `SIZE <final>` (or a LIST fallback) and refuse if the file exists.

**MEDIUM-1: Resuming from an old or wrong `.part` produces a corrupted file that is reported as verified.** `ftp.rs:605`
- **What breaks:** any remote `<name>.part` smaller than the local file is resumed from its size, and its bytes are never compared. Examples: a `.part` left by an earlier send of a different version of the game or TU with the same name, or one left by another tool. The final size matches, so the UI says "sizes verified". The game is corrupted and the user isn't told.
- **How I triggered it:** I put `game.bin.part` with 3000 `Z` bytes on the console, then sent a different 5000-byte `game.bin`. Result: `resumed:1`, and the console file started with `ZZZZ…` and differed from the source at byte 1.
- **Fix:** only resume a `.part` this app wrote for this exact source. Write a small local resume record (remote path, source size, mtime, file id), keyed per console. Restart from 0 when the record is missing or doesn't match. A cheaper option: re-download the last ~64 KB of the `.part` with REST/RETR and compare it to the source before resuming.

**LOW-1: A hostile server can make Unpack use a lot of memory.** `ftp.rs:226` and `:237` (`read_line` has no length cap)
- **What breaks:** a device at the entered IP that streams a reply with no newline keeps growing the app's memory for as long as data keeps arriving. The read timeout applies per read, so it never fires.
- **How I triggered it:** my socket server sent a 300 MB `220 AAAA…` greeting with no newline. The bridge's memory went from 19 MB to 327 MB, then the request timed out. A bigger stream would keep growing it.
- **Note:** it requires the user to enter that device's LAN IP.
- **Fix:** read with `take(8192)` per line, and fail with "FTP reply too long."

**LOW-2: The transfer count is wrong when the server refuses REST.** `ftp.rs` `send_one` / `Done { sent: len - j.offset }`
- **What breaks:** the transfer still restarts from 0, which is correct. But "resumed" is counted and the MB/s figure is under-reported. Cosmetic.
- **How I found it:** by reading.
- **Fix:** return the real offset from `stor`.

**LOW-3: The Rust unit tests leave folders in /tmp.**
- **What breaks:** each `cargo test` run leaves about 80 MB of `/tmp/unpack-ftp-*`, `unpack-builtin-*` and `unpack-test-*` folders. Five runs left about 400 MB. That matters on Justin's tight disk if he runs the tests.
- **Fix:** remove the folders at the end of each test, or use a drop guard.

### Attacks that were handled correctly (all run)
- **LAN-only:** these were all refused:
  - 8.8.8.8, 100.64.0.1, 172.15/172.32, 224.0.0.1, 255.255.255.255, 0.0.0.0, 169.254.0.x and 169.254.255.x
  - `localhost`, `::1`, `[::1]`, `::ffff:192.168.1.5`
  - `127.1`, `0x7f000001`, `0177.0.0.1`, `192.168.001.5`
  - `xbox.local`, `192.168.1.5.nip.io`, and an IP with CRLF appended

  No DNS lookup is ever done, so DNS rebinding isn't possible. Hosts with surrounding spaces are trimmed and accepted, which is fine.
- **FTP bounce:** a PASV reply of `227 (8,8,8,8,p1,p2)` led to a data connection to the console's own IP (127.0.0.1) only.
- **CRLF injection:** CRLF in the user name, password or path was refused.
- **Path checks:**
  - `..`, `C:`, names over 42 characters and non-ASCII names were refused.
  - `\\Hdd1\\G` was normalised to `/Hdd1/G`.
  - A blank edited destination falls back to the default, and `/` is refused.
  - FATX has no reserved names, so `CON/nul` passing through is correct for the Xbox.
- **Hostile LIST content:**
  - Names like `../../evil`, `..`, the symlink-arrow form and a 23-digit size were dropped.
  - `<img onerror>`, `a\b` and ANSI/BEL control characters were kept as names but rendered through `escapeHtml` (I checked every new `${…}` in app.js).
  - A 40 MB listing was capped at 16 MB with no crash (but see HIGH-1).
  - A 2000-line multi-line reply gave "FTP reply too long."
- **Overwrite policy:**
  - A remote file with a different size makes the whole job stop (xbox.mjs and unit tests).
  - A `.part` larger than the source restarts from 0 and the result is byte-identical.
  - A remote file with the same size but different content is skipped. That is the documented behaviour, but worth knowing: it is size-only.
- **FTP_BUSY:** a second `ftp_list` sent while one was hanging got the "already talking to the console" error right away.
- **Source links:** the inner symlink was skipped and counted (xbox.mjs). A link as the source root is refused (unit test). Files are opened with O_NOFOLLOW.
- **menus.json:** all of these were rejected with a reason, and nothing escaped the menus folder:
  - `../`, absolute paths, `C:x`, `..\`, `sub/../a`, `./a`
  - a file symlink, a symlinked folder in the path, and `menus.json` as a symlink
  - a folder entry, a case mismatch, a wrong sha256, an unknown TitleID, an unknown JSON field
  - a JSON object instead of an array, a 2 MB file, 100k levels of nesting

  5000 entries were accepted with a cap of 500. HTML in a name is escaped in the UI, and control characters are stripped.
- **Leftover debug code:** none. `println!` appears only in the ignored benchmark, and `eprintln!` only logs skipped built-in menus. The `.ok()` calls I found are socket-option calls only. The one real swallowed error is HIGH-1.

### Checked by reading only
- A real Aurora, XeXMenu or DashLaunch server: how RNTO overwrites behave, missing RNFR, real WiFi latency.
- The real Tauri `ftp_send` grant check (lib.rs:398). I tested the bridge's matching check.
- Windows FILE_FLAG_OPEN_REPARSE_POINT and junctions in the source. Wine ran the unit tests, not a junction case.
- The `ftp_send` 2-connection 425 hand-back path. It is covered by the unit tests, and I didn't attack it separately.
- The NSIS bundling of `resources/menus` into the installer (tauri.conf.json).

### Verdict
- `ff440ff` (Send to Xbox): **FAIL**, because of HIGH-1 (it can overwrite a file on the console) and MEDIUM-1 (a corrupted resume is reported as verified).
- `731ce15` (built-in menus + editable destinations): **PASS**. Destination validation and the menus loader held up against everything above.
- **Round 10: FAIL.** HIGH-1 and MEDIUM-1 need fixes. The LOW issues aren't blocking.

/tmp: all my scratch folders, plus the folders the test suites left, were deleted.

---

## Round 11 — 2026-10-20 (CDT)

Commit reviewed: `03521b2` (fixes for the Round 10 findings). I built it in-tree and ran the hostile tests through the debug bridge against pyftpdlib variants and my own raw-socket servers, all on 127.0.0.1. I did not change any source.

### Test results
| Suite | Result |
|---|---|
| `cargo test --release --lib`, 5 runs, parallel | 119/119 every run (1 ignored) |
| Wine | 113/113 (1 ignored) |
| e2e / autogod / dlc / download / mods / builtin / xbox.mjs | 42 / 15 / 16 / 27 / 39 / 9 / 42 — all pass, 0 fail |
| LOW-3 /tmp leftovers | 5 cargo runs left **no** new `/tmp/unpack-*` folders (only a `unpack-e2e` from the node e2e suite) — **fixed** |

### Round 10 fixes: re-tested
| Finding | Result |
|---|---|
| HIGH-1: failing LIST | LIST answered with 425, 450, 550 or 451 after a successful CWD: the job stops with "Couldn't list … nothing was sent" and the existing file is untouched. Data connection closed partway (control connection also closed): stops. Data connection reset (RST) followed by 426: stops. **Fixed.** |
| HIGH-1: 16 MB cap | A listing of exactly 16,777,216 bytes is accepted. At 16,777,217 bytes the list and send both refuse with "larger than 16 MB". **Fixed.** |
| HIGH-1: re-check before RNTO | I created the target file during STOR, both with and without SIZE support (LIST fallback). Unpack refused with "appeared on the console during the send … kept as game.bin.part" and the other file was untouched. **Fixed.** |
| MEDIUM-1: stale `.part` | The Round 10 repro (3000 bytes of Z) now restarts from 0 and the result matches the source byte for byte. A genuine prefix `.part` resumes (`resumed:1`) and matches byte for byte. A bad `.part` under 64 KB restarts. **Fixed.** |
| `.part` larger than the source / 0 bytes / equal size | Each one restarts from 0 and matches byte for byte. |
| REST refused (502) with a good `.part` | Restarts from 0, matches byte for byte, and the result reports `resumed:0` with the full byte count. LOW-2 is **fixed.** |
| LOW-1: reply line cap | Lines of 8191 and 8192 bytes (including CRLF) are accepted. Lines of 8193 and 9000 bytes give "FTP reply too long." **Fixed.** |

### Remaining bugs (both low, neither blocks)
**LOW-1: a `.part` whose first and last 64 KB match but whose middle differs is still resumed, and the result is corrupt.** `src-tauri/src/ftp.rs:590` (`part_matches`)
- **What breaks:** if an interrupted `.part` came from a different build of the same-named file that differs only in the middle (for example a patched mod or data file), the finished file is silently wrong. The final message still says "sizes verified".
- **Repro:** a 200 KB prefix `.part` with one byte flipped at offset 100000 → `resumed:1` and `cmp` reports a difference at byte 100001.
- **Does it matter:** rarely. It needs a leftover `.part` from a different version with the same name that shares the first and last 64 KB of that prefix. Round 10's realistic case (unrelated or stale content) is now caught.
- **Fix (optional):** also sample a few blocks spread through the `.part`, or keep a small local resume record (console, remote path, source size, mtime, file ID) and resume only when it matches.

**LOW-2: there is still a one-round-trip window between the final check and RNTO.** `ftp.rs:623`
- **What breaks:** a file created on the console between the SIZE check and RNFR/RNTO can still be overwritten on servers that let RNTO replace files.
- **Repro:** the server creates the target inside its RNFR handler → overwritten, `sent:1`.
- **Why it's low:** it needs another writer on the console at that exact moment. Plain FTP has no way to close the window completely.
- **Fix (optional):** after RNTO, if SIZE differs, report it clearly. Otherwise accept the risk and document it.

### Send to Xbox regressions
None found. xbox.mjs passes 42/42 (up from 41) and all other suites are unchanged. Resume now costs 1–2 extra RETR round trips, and each file costs one extra SIZE before the rename. That is expected.

### Checked by reading only
- `retr_range` early close (linger 0 and the 426/451 reply) on a real Xbox server.
- The Windows socket behaviour behind the linger change. Wine ran the unit tests only.
- Real Aurora, XeXMenu and DashLaunch servers. Not available here.

### Verdict
**Round 11 (`03521b2`): PASS.** Every Round 10 finding is fixed and confirmed by running it. The two remaining items are low and don't block.

/tmp: my scratch, every `/tmp/unpack-*` folder, and all bridges and fake servers have been removed or stopped.

---

## Round 12 — 2026-10-20 (CDT)

Commit reviewed: `8291ebf`, the mod catalog (base `e6b48e2`). I built it in-tree: the release app, the release and debug `unpack-catalog-sign`, the bridge, and the Wine test exe. Scratch attacks ran through the debug bridge's `catalog` command with local HTTP and pyftpdlib servers only (127.0.0.1); there were no outside connections. Keys were generated in /tmp and deleted afterwards. I did not change any source.

### Test results
| Suite | Result |
|---|---|
| `cargo test --release --lib`, 5 runs, parallel | 155/155 every run (1 ignored) |
| Wine | 148/148 (1 ignored) |
| e2e / autogod / dlc / download / mods / builtin / xbox / **catalog.mjs** | 42 / 15 / 16 / 27 / 39 / 9 / 42 / **29**, all pass, 0 fail |
| Release `unpack` and `unpack-catalog-sign` | 0 copies of `UNPACK_TEST_ALLOW_LOOPBACK`, `UNPACK_TEST_CATALOG_DIR` and `UNPACK_TEST_MENUS`. No hard-coded key: no 64-hex constant outside the tests. |

### Bugs

**HIGH-1: If renaming launch.ini over FTP fails, the console is left with no launch.ini, and Unpack doesn't restore it.** `src-tauri/src/catalog/target.rs:315` (DELE before RNTO) together with `catalog/install.rs:451` (`t.replace(...)?` returns before restoring)
- **What breaks:** to replace launch.ini on the console, Unpack uploads `launch.ini.new.part`, deletes the real `launch.ini`, then renames the new file into place. If that rename fails (server refusal, or WiFi dropping between DELE and RNTO), the error returns straight away. The original is only written back when a read-back comparison fails, which never happens here. DashLaunch then boots with no launch.ini: plugin1, the Aurora default path and every setting are gone until the user restores a `.bak` by hand over FTP. This is the "last-known-good restore" the design promises, and it doesn't happen on this path.
- **Repro:** pyftpdlib with RNTO to `launch.ini` answering `550`. Installing a plugin to console Hdd1 gave `Rename to /Hdd1/launch.ini failed: 550`. Afterwards `launch.ini` was gone, `launch.ini.new.part` and the `.bak` files remained, and the journal recorded `complete:false`.
- **Fix:**
  - On any error after the DELE, try to put the original back (RNTO the `.new.part`, or upload `before_bytes` directly) and report what was done.
  - Better: rename the old file to `launch.ini.unpack-old` first, rename the new one into place, and only then delete the old one. That avoids the window where no launch.ini exists.
  - Also wrap the local `replace` call the same way.

**MEDIUM-1: Uninstalling one mod deletes a file that another installed mod still uses.** `catalog/target.rs:200` (an identical file counts as "skip") and `:230` (`delete_if` checks only the hash)
- **What breaks:** if two catalog entries ship the same file (same name and content, such as a shared plugin), the second install skips copying it but still records it in its journal. Uninstalling the second mod deletes the file, so the first mod's plugin disappears. Its launch.ini line then points at a missing file.
- **Repro:** I installed `moda` and then `modb`, both shipping `Plugins/shared.xex`. Uninstalling `modb` gave `removed:1`; the shared file was gone and `plugin3 = Usb:\Plugins\shared.xex` was left pointing at nothing.
- **Fix:** before deleting, skip any file that another journal record on the same target also lists. Alternatively, don't record skipped-identical files as owned, or keep a reference count. Report such files as "kept (used by X)".

**LOW-1: No size cap when unpacking archives (zip bombs).** `catalog/install.rs:363`
- **What breaks:** a 2 MB zip unpacked to 2 GB in the app-data staging folder and was then copied to the target. Nothing limits the unpacked total, and free space is checked on the target, not on the staging drive (C:). The archive's hash is pinned by the signed catalog, so only a malicious or careless catalog owner can trigger this.
- **Repro:** a 2,087,538-byte zip containing a 2 GiB zero file installed fine (`bytes: 2147485696`).
- **Fix:** cap the unpacked total (for example the sum of 7za's listed sizes ≤ 20 GB and ≤ 200× the archive size), and check free space where staging happens before unpacking.

**LOW-2: A corrupt `journal.json` or `seen.json` is silently treated as empty.** `catalog.rs:474` (`read_json` → default)
- **What breaks:**
  - A corrupt journal makes every install show as "new". Uninstall says "no record", and the next install overwrites the file, so all records are lost.
  - A corrupt `seen.json` resets rollback protection: after I wrote `{corrupt`, a v1 catalog loaded once v7 had been seen.

  Both need local damage or tampering.
- **Fix:** if the file exists but doesn't parse, refuse catalog actions with a clear error, and keep the bad file aside (`journal.json.bad-<ts>`) rather than overwriting it.

**LOW-3: The app-side launch.ini backup can be overwritten within the same second.** `catalog/install.rs:450`
- **What breaks:** `backups/{kh}-{ts}-launch.ini` uses whole-second timestamps and `write_atomic` replaces an existing file. The update flow (uninstall old, then write new) or a quick install and uninstall writes twice in one second, and the copy of the original is replaced by the intermediate one. The target-side `.bak` names are made unique, so the original survives there.
- **Repro:** 12 target `.bak` files but only 2 app-side backups after my ini runs.
- **Fix:** use the same `-2`/`-3` suffixing as on the target, or `create_new`.

**LOW-4: Confusing size-limit message.** Refreshing a catalog of 4 MB + 1 byte reports "File is 0.0 GB; the size limit is 0 GB. Raise it in Settings…". It's refused correctly, but the message is wrong, and it points to a setting that doesn't apply. Fix: report the cap in MB from `fetch_small` / `download_with`.

**LOW-5 (info), by reading:**
- `unpack-catalog-sign sign <64-hex>` accepts the secret key as a literal argument, so it can end up in shell history.
- On Windows the key file gets no ACL restriction; the 0600 mode is Unix-only.
- A signed catalog with `catalog_version` = u64::MAX permanently blocks later versions for that key. It's the owner's mistake, but maybe worth a cap (I ran this one).

### Attacks that were handled correctly (all run)
- **Signatures:**
  - Refused: a tampered byte, a trailing newline, a wrong key, a truncated signature, an empty one, 129 hex characters, all zeros, and a malleable S+L signature (`verify_strict`).
  - Garbage signed with the wrong key gives a signature error, not a JSON error, so verification happens before any parsing.
  - Garbage signed with the right key gives a parse error.
  - Uppercase or whitespace-padded signatures are accepted; they're the same bytes.
  - Schema 2, unknown top-level fields, a float version, 5000-deep nesting and anything over 4 MB are all refused.
- **Rollback:** v6 or v0 after v7 is refused. Loading v7 again (replay or equal version) is accepted, which is fine.
- **Entry validation:** each of these was skipped with a reason:
  - bad, duplicate or long ids
  - dest_path containing `..`, `C:`, `Plugins/../..`, é or RLO
  - xex directly in `Games` or in `Aurora/Data`; TU for another TitleID; trainer root
  - plugin slots 1 and 6; `dashlaunch.file` with `..` or CRLF
  - `file:`, `ftp:`, URLs with a login, `javascript:` source_page, a %0d%0a filename, filenames over 42 characters
  - size 0 or over 20 GB, missing or bad sha256
  - an entry requiring itself, paid without a source_page, TitleID 00000000, more than 20 aliases
  - `/Plugins` and `Plugins\sub` are normalised. `Plugins/CON` is accepted; FATX has no reserved names, but it fails on a Windows USB target (by reading).
- **Downloads:**
  - 10.0.0.1, a redirect to 169.254.169.254 and a redirect to 192.168.1.1 are all blocked by the downloader.
  - A wrong hash is quarantined as `.bad-hash`.
  - Note: through the bridge the redirect error names the loopback hop. The downloader's own redirect and SSRF tests are covered by download.mjs and the unit tests.
- **Staging:**
  - zip-slip `../../..` and absolute names stay inside the game folder.
  - Symlinks in a zip are dropped or turned into plain folders, never followed (nothing was written outside).
  - A tar hard link to /etc/passwd is refused.
  - A `:` in a name and 20-deep nesting are refused.
  - The stage folder is empty after every install.
- **launch.ini:**
  - A used slot gives `SLOT_OCCUPIED` and is replaced only with `confirmSlot`.
  - plugin1 is refused even when confirmed.
  - All slots full asks before replacing plugin5.
  - NUL bytes, binary content and over 64 KB are refused, and the file is left untouched.
  - An empty file, duplicate sections or keys, LF-only lines and lowercase keys are handled (a new line or the next free slot).
  - Uninstall removes only Unpack's line.
  - A target `.bak` is written every time, and the last-known-good copy is kept.
  - A broken `[Plugins` header makes Unpack append a second `[Plugins]` section. That's harmless.
- **Uninstall:**
  - A changed file is kept and listed.
  - Journal paths `../victim`, `/tmp/…/victim`, `Plugins/../../victim` and backslash forms are all refused, and the victim file survived.
- **Pre-install checks:**
  - TU: none present with TU18 required, TU18 present, TU17+18 present, "no TU" required but TU18 present, an unreadable name, and a junk file all give the right block or warning.
  - requires cycle: blocked, no hang.
  - conflicts: blocked.
- **Console (FTP):** install to Hdd1 sets plugin2, uninstall removes the file and the line, under the FTP lock. HIGH-1 above is the failure case.
- **CLI:**
  - `keygen` writes 0600 and refuses an existing file or a dangling symlink.
  - The secret key never appears in the output of keygen, pubkey, sign or verify.
  - A bad key path gives "must be 64 hex", without echoing anything.
  - `sign` runs the loader as a check, and `verify` round-trips.
- **UI:** every new catalog `${…}` either goes through `escapeHtml`, is a number or boolean, or ends up in `textContent` (`paintJob`). I fed `<script>` names, notes and author fields in through refresh; they're stored control-stripped, at most 2000 characters.

### Checked by reading only
- The FAT32 4 GB check on a real FAT target (`t.is_fat`). My target was ext4; the preflight for a 5 GB file correctly had no FAT blocker there.
- Free space on real drives.
- Windows junctions in staging.
- NSIS packaging (catalog-sign isn't in the installer).
- A real Aurora server's RNFR/RNTO and DELE behaviour.
- Windows key-file ACLs.
- The UI rendering of hostile catalog text in Chrome. Escaping was checked in code, and catalog.mjs passes.

### Verdict
**Round 12 (`8291ebf`): FAIL.**
- HIGH-1: a failed rename can leave the console without launch.ini, and the promised restore doesn't happen.
- MEDIUM-1: uninstall deletes a file another installed mod shares.

LOW-1 to LOW-5 don't block.

/tmp: all of my scratch, all `/tmp/unpack-*` folders (including the catalog e2e keys and the bridge catalog store), and every scratch key have been deleted. All bridges and HTTP/FTP servers are stopped.

---

## Round 13 — 2026-10-20 (CDT)

Commit reviewed: `87ff978` (Round 12 fixes + multi-drive picker). I built it in-tree and ran the scratch attacks through the debug bridge against local HTTP and pyftpdlib servers only (127.0.0.1). Scratch keys were made in /tmp and deleted afterwards. No source was changed.

### Test results
| Suite | Result |
|---|---|
| `cargo test --release --lib`, 5 runs, parallel | 166/166 every run (1 ignored) |
| Wine | 159/159 (1 ignored) |
| e2e / autogod / dlc / download / mods / builtin / xbox / catalog | 42 / 15 / 16 / 27 / 39 / 9 / **51** / **36** — all pass, 0 fail |
| Release `unpack`, `unpack-catalog-sign` | 0 copies of the test switches, `LOCAL_SWAP` or "injected failure" (the failure hook is test-only) |

### Round 12 repros
| R12 finding | Now |
|---|---|
| HIGH-1: RNTO of launch.ini refused | **Fixed** for a refused rename. The original is put back by rename. If renaming it back is also refused, it is uploaded again from its bytes. Remaining gap: the cases where the console's FTP **connection drops** or upload is refused, see MEDIUM-1 below. |
| MEDIUM-1: shared file | **Fixed.** With 3 mods sharing `Plugins/shared.xex`, each uninstall order keeps the file while any other mod uses it ("used by …"); it is deleted when the last one goes. A case-variant path still counts as shared. A second grant of the same folder (`usb/`) gets the same target key, so it's counted too. One leftover: LOW-1 below. |
| LOW-1: zip bomb | **Fixed.** Exactly 256 MB unpacks; 256 MB + 1 byte is refused before unpacking. A 9 MB → 2.1 GB bomb is refused in 0.4 s. 30,000 small files install fine. The stage folder is empty every time. |
| LOW-2: corrupt journal/seen | **Fixed.** A corrupt, empty or wrong-type `journal.json`, and a corrupt `seen.json`, are refused with a clear message, and a `.bad-<stamp>` copy is kept (only once per distinct content). Rollback can't be bypassed by corrupting the file. It can still be bypassed by deleting it, which needs local access. |
| LOW-3: same-second backups | **Fixed.** 8 launch.ini writes in under a second produced 8 app-side backups and 8 target `.bak` files. |
| LOW-4: size message | **Fixed.** "…is larger than the 4.0 MB limit for catalog files." |
| LOW-5: signer | **Fixed.** A literal key on the command line is refused ("give it as a file or -"). stdin (`-`) works. Over 4 KB on stdin gives a clean error. The secret never appears in any output. A version of 2^53−1 signs with a warning and loads; 2^53 is refused by both the signing tool and the app. The Windows icacls step was checked by reading only. |

### Bugs

**MEDIUM-1: If the connection drops during the launch.ini swap, the console is left without launch.ini, and later runs don't repair it.** `src-tauri/src/catalog/target.rs:400` (recovery runs on the same `Ftp` connection) and `catalog/install.rs:569`
- **What breaks:** a WiFi drop between "move original aside" and "move new in" now leaves the original safe as `launch.ini.old`, and the error message says so. Recovery is attempted on the connection that just died, so it can never succeed, and nothing puts the original back. On the next install Unpack sees no launch.ini and says "No launch.ini on this drive: add the plugin to DashLaunch yourself", which invites the user to carry on. DashLaunch has no config until the user renames `.old` by hand over FTP.
- **Repro:**
  - **Drop:** the server closes the control connection on the new-file RNTO. launch.ini is MISSING; `launch.ini.old` and `launch.ini.new.part` are left. A second install on a healthy server reports the "No launch.ini" note, and launch.ini is still missing.
  - **Refused re-upload:** RNTO back to launch.ini refused, then STOR refused. Same end state.
- **Fix:**
  - When the swap fails, reconnect (`Ftp::connect`) before trying to restore.
  - At the start of every console and local ini operation (and in preflight): if `launch.ini` is missing and `launch.ini.old` (or `.old-N`) exists, restore it, or block with "Unpack found your previous launch.ini as … — restore it?".
  - Tidy the stray `launch.ini.new.part`.

**LOW-1: When shared mods are uninstalled, the last one can leave a launch.ini line pointing at a file that's gone.** `catalog/install.rs:631`
- **What breaks:** only the first mod that set the line owns it in the journal. If that mod is uninstalled while others still use the file, the line is correctly kept. The last remaining mod has no ini record, so its uninstall deletes the file but leaves `pluginN = …shared.xex` pointing at nothing.
- **Repro:** install m1, m2, m3 (same file); uninstall in the order m2, m1, m3. The file is gone and `plugin2 = Usb:\Plugins\shared.xex` remains.
- **Fix:** when a kept line's owner leaves, move the ini change to the remaining user in the journal. Or, on the last file removal, also remove a line whose value equals that file's path.

**LOW-2: A failed delete of `launch.ini.old` leaves `.old` files that pile up.** `target.rs:395` (`let _ = f.dele(&old)`)
- **What breaks:** with DELE of `.old` refused, every successful swap leaves another `launch.ini.old` / `.old-N`. Unpack doesn't tidy them on later runs. After 100 it refuses ("tidy them first").
- **Repro:** the `nodele` server gave two successful installs and `launch.ini.old` stayed.
- **Fix:** report it as a note, and tidy `.old` files that match a stored backup on later runs.

**LOW-3: Some messages are wrong or misleading.**
- The damaged-`seen.json` message says "(removing it forgets what Unpack installed)". That's wrong for the rollback record: removing it resets rollback protection. That's the one remaining way around rollback (local access needed).
- A refused rename-aside reports "The upload is kept as /Hdd1/launch.ini.." (double dot, and the `.new.part` was actually deleted).
- The cap message reads "would unpack to 256.0 MB, more than the 256.0 MB allowed" for cap + 1 byte. Show bytes near the limit.

**LOW-4 (info), by reading:**
- A USB stick installed to as a local drive and then as console `Usb0` gets two different target keys, so shared-file counting and uninstall don't see each other. That's rare.
- Saved per-console drives accept a port above 65535 in the key (`1.1.1.1:99999`). It's cosmetic; the drive value is validated.

### Attacks that were handled correctly (all run)
- **Swap:** a refused aside rename leaves the original untouched. A refused new-file rename puts the original back. A refused new-file rename plus a refused rename back puts it back by re-upload. A normal swap leaves no `.old`.
- **Drive names:**
  - The catalog console target refuses `../Hdd1`, CRLF+`DELE`, `Hdd1/Plugins`, `Hdd1\x`, `HddÜ`, an empty name, 17 characters and a leading space. It accepts `Hdd1x`, `HddX` and `Usb2`; the last is a file on my server, which fails later at CWD and is harmless.
  - `ftp_dest` refuses `../x`, `Hdd1/x`, CRLF and `Ü`.
- **Odd drives from the server:** the root listing included `1Usb`, `a b`, `Ünï`, a 20-character name, `..` and a file. The page's `rootDrives` kept only `Game`, `Hdd1`, `Hdd1x`, `HddX` and `Usb1`.
- **Remembered drive:** `pickDrive` with a remembered drive the console no longer has falls back to Hdd1, with no drives listed it falls back to the remembered one, and with only USB drives it picks Usb0.
- **Stored drives:** `unpack.xboxDrives` drops `../x`, a CRLF value, non-IP keys and `__proto__`, and garbage becomes `{}`.

### Checked by reading only
- The local `replace` failure paths. They are covered by the unit test hook (`LOCAL_SWAP`), not by my own run.
- Windows icacls on the key file.
- A real Aurora's rename/delete behaviour.
- The drive picker UI clicks. I ran the functions and storage validation in node, and xbox.mjs and catalog.mjs passed.
- Free-space checks on real drives.

### Verdict
**Round 13 (`87ff978`): FAIL**, because of MEDIUM-1: a dropped connection still ends with no launch.ini on the console, and Unpack's later runs don't notice. Everything else from Round 12 is fixed and confirmed by running it. LOW-1 to LOW-4 don't block.

/tmp: all of my scratch, every `/tmp/unpack-*` folder, and every scratch key have been deleted. All bridges and HTTP/FTP servers are stopped.

## Round 14 — 2026-10-20 (CDT)

Commit reviewed: 7e80df3 (on top of 87ff978). Source was not modified.

**Suite results (all run this round):**
- `cargo test --release`, 5 runs in parallel: 172/172 (1 ignored) on most runs. In the first batch, 2 of the 5 runs got 171/172. I couldn't reproduce this: more than 32 later runs passed 172/172, both one at a time and 3 in parallel.
- Wine: 165/165 (1 ignored).
- Node suites, all pass: e2e 42, autogod 15, dlc 16, download 27, mods 39, builtin 9, xbox 52, catalog 36.
- The release binary contains no test hooks (checked with strings).

**Auto-repair (I ran these):** I dropped the connection at each swap step, then ran a later preflight and install on a healthy server.
- Steps tested: aside RNFR, aside done, RNTO, RNTO done, DELE of .old. Each was also tested with reconnects refused (`_dead`).
- Every case ended with the original or the new launch.ini in place.
- In dropnew_dead, launch.ini was missing; the next preflight restored it from .old and showed a note.
- Leftover .new.part and .old files were cleaned up.
- The later install worked every time.
- When launch.ini and a .old were both present, the good launch.ini was kept.
- With several .old files, the highest-numbered one (.old-7) was restored.

**Verdict: FAIL**

### MEDIUM-1: repair is case-sensitive and blocks every install
- **Where:** catalog/install.rs:495-526
- **Repro:** the drive has `LAUNCH.INI` (a good file) plus `launch.ini.old`.
- **Impact:** repair thinks launch.ini is missing and tries to restore it. The rename then fails with "already exists..", and preflight reports a blocker on every run. Typical Xbox FTP servers and FAT drives treat file names as case-insensitive, so a user with an upper-case file can't install anything until they delete files by hand. The message also ends in a double period (".." after "already exists").
- **Fix:** compare names case-insensitively. If any case variant of launch.ini exists, treat it as present and only prune the .old files.

### LOW-1: restore puts back a .old without checking it
- **Where:** catalog/install.rs:526
- **Repro:** launch.ini is missing and launch.ini.old is one of: empty, 3000 random bytes, or 20 MB. Each one became launch.ini.
- **Impact:** the random and 20 MB cases then block all installs ("isn't plain text" / "larger than 65536"). The 20 MB file was written onto the console. A broken or planted .old silently becomes the dashboard config.
- **Fix:** before restoring, check the .old's size against the 64 KiB limit, require plain text, and require a match with the app's backup when one exists. Otherwise, leave the files alone and show a blocker that names the file.

### LOW-2: a failure is reported after the swap actually worked
- **Where:** catalog/target.rs:428
- **Repro:** RNTO succeeds, then the connection drops and reconnects fail (afternew_dead).
- **Impact:** the install reports "failed… could NOT be put back", but the new launch.ini (with the mod line) is live and the .old is the stale copy. On the next run, the leftover .old is pruned and the mod's line stays.
- **Fix:** after reconnecting (or on the next run), check which file is in place before saying the install failed. Or word the message as "state unknown, checking on next connect".

### LOW-3: the message says launch.ini was moved when it wasn't
- **Where:** catalog/target.rs:428
- **Repro:** dropaside_dead. The connection dropped on the RNFR, so launch.ini was never moved.
- **Impact:** the message still says the original "is safe on the console as /Hdd1/launch.ini.old", but no .old exists. This is confusing for the user.
- **Fix:** only mention the .old when the aside rename is known to have happened.

### LOW-4: test flake
- 2 of the first 5 parallel cargo runs had one failure each. I didn't capture which test failed, and I couldn't reproduce it.

### INFO: leading-zero ports
- **Checked by reading only:** native.js:57 accepts stored keys like `1.2.3.4:021`, but app.js:992 builds `1.2.3.4:21`. The two never match, so a saved drive choice is ignored (cosmetic).
- Ports 0 and 65536 fall back to 21, and 65535 is accepted (app.js:992, native.js:57). ftp.rs:50 rejects ports outside 1-65535. I checked all of this by reading only.

### Not run this round
These were not exercised, so I make no PASS claim for them:
- the ownership handoff with 3 or 4 mods, all orders, and different slots
- pruning when deletes are refused (nodele)
- a repair racing with another write
- the Round 13 repro for the m2,m1,m3 order

**Checked by reading only:** the handoff code (`ini_user`), `free_old_name` (never refuses; time-based names don't sort by recency), the pruning rule (only .old files identical to a backup or to the current launch.ini), and that a local repair has no lock.
