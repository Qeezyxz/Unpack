# Reviewer A — Platform & crash safety (f3bcc65 on v1.7.4-dev)

**Scope:** diff `7ee077d..f3bcc65` vs released `64fc66d` / v1.7.0.  
**Focus:** Windows `path_id`, Replace manifest / `moved_live`, K1/K2, locks, grants / picked-folders, cover failure cache, FAT32 / cross-volume.

## Build & test execution

- `cargo test --lib --target x86_64-pc-windows-gnu --no-run` in `src-tauri`: **success** (after installing `gcc-mingw-w64-x86-64`).
- Wine: **works** (`wine` 9.0, fresh `WINEPREFIX=/home/ubuntu/.wine-unpack-r40-fresh`).
- Copied `WebView2Loader.dll` from `target/x86_64-pc-windows-gnu/debug/WebView2Loader.dll` and `7za.exe` from `binaries/7za-x86_64-pc-windows-gnu.exe` next to `unpack_lib-*.exe`.
- `UNPACK_7ZA=src-tauri/binaries/7za-x86_64-unknown-linux-gnu python3 tests/make_testdata.py <tmpdir>`: **success** (fixtures for archive tests).

### Tests run under Wine (all **passed**)

| Area | Tests |
|------|--------|
| Lock | `archive::tests::lock_file_removed_after_work_and_stale_one_cleaned`, `r29_lock_survives_delete_and_replace`, `r33_read_only_lock_file_is_an_error_not_a_wait` |
| Replace / crash | `r27_killed_replace_recovers_whole_pair`, `r27_double_crash_every_case`, `r27_concurrent_jobs_are_serialized_per_type_folder`, `r27_move_new_refuses_existing_destination`, `m1_legacy_replace_note_repairs_after_upgrade`, `h1_partial_live_data_restores_old_aside` |
| K1 / K2 | `k1_edited_data_after_kill_never_mixes_package`, `k2_folder_identity_mismatch_is_detected` |
| Covers | `covers::tests::failure_cache_expires`, `note_failure_keeps_all_concurrent_entries` |
| Picked folders | all five `picked_folders::tests::*` |
| Grants (atomic) | `safecopy::tests::grants_save_atomic_and_corrupt_load` (Wine) |

`archive::tests::k2_rename_during_replace_stops_and_rolls_back` is **`#[cfg(unix)]` only** — not compiled or run on the Windows-gnu target.

### Custom kill-mid-step scenarios (reviewer)

1. **`moved_live` + corrupted `.data` after kill at step 5** (manifest still `replacing`, both halves in `moved_live`): recovery **put the old package back** (Linux + Wine). Matches intent of H1/K1 for incomplete finish.
2. **Kill at step 6, then corrupt `abcd0001.data/Data0000` before recovery** (manifest `replacing`, `moved_live` lists header and `.data`): recovery treated the Replace as complete, **deleted `.unpack-old.*`**, left **NEW header + corrupted `.data`** (mixed package). Reproduced on **Linux and Wine** (see Finding 1).

---

## Findings

### 1. HIGH — `all_new_confirmed` trusts `moved_live` for `.data` without fingerprint

- **File:line:** `src-tauri/src/archive.rs:2323-2336` (`all_new_confirmed`), used from `recover_one` at `2399-2404` and `finish` at `2367-2371`.
- **Trigger:** Replace killed after both halves are moved in and `moved_live` updated (e.g. step 6 in `replace_unit`), but before asides are removed. Live `.data` is then wrong (truncation, bad cross-volume copy, media error, or post-crash edit) while the header still matches its fingerprint.
- **Impact:** On the next run, `recover_asides` can take the `all_new_confirmed` fast path, promote to `finishing`, and **remove the only good copy in `.unpack-old.*`**, leaving a **broken GOD pair** (header/data mismatch). Data loss of the previously working package.
- **Repro:** Kill at `REPLACE_KILL_AT` step 6 in `place_title_trees` Replace flow; overwrite `…/abcd0001.data/Data0000` with `CORRUPT`; call `recover_asides`. Warning: *"had finished moving the new copy in; I removed the old copy"*; live header `NEW`, data `CORRUPT`.
- **Suggested fix:** In `all_new_confirmed`, **never** return `true` for a `.data` half on `move_recorded` alone — always require `fingerprint(&p) == fp` (same as non-recorded path). Optionally re-verify both halves in `finish()` before `remove_any` on asides.

### 2. MEDIUM — `picked-folders.json` written non-atomically

- **File:line:** `src-tauri/src/picked_folders.rs:36-44` (`PickedFolders::save`).
- **Trigger:** Process kill or power loss during `fs::write` of `picked-folders.json` (dialog pick, migration `remember_approved`, etc.).
- **Impact:** `load` ignores parse errors and starts with an **empty** list (`picked_folders.rs:24-28`). User loses persisted folder picks; **`restore_grants` clears and rebuilds from that list** (`restore_grants` + `grants.clear_persist`), so **auto-restored library access can disappear** until the user re-picks folders. `grants.json` may still hold old entries until the next `clear_persist`, but 1.7.4 flow prefers picked-folders as source of truth.
- **Repro:** Write a truncated JSON file to `picked-folders.json`; `serde_json::from_str` fails → empty `paths` (same behavior as `load`).
- **Suggested fix:** Mirror `Grants::save` / `write_atomic` in `covers.rs`: temp file in the same directory + `rename`, or write-then-rename with fsync.

### 3. LOW — `FAILURE_LOCK` only serializes in-process cover failure map updates

- **File:line:** `src-tauri/src/covers.rs:32, 179-195`.
- **Trigger:** Two Unpack processes updating `cover-failures.json` concurrently.
- **Impact:** Last writer wins; possible lost TTL entries (cosmetic / extra network fetch), not game data. Writes themselves use `write_atomic` (`covers.rs:39-55`).
- **Suggested fix:** Optional file lock or accept documented best-effort (single instance app).

### 4. LOW — Long paths and `\\?\` prefix not used in Replace / lock paths

- **File:line:** `src-tauri/src/archive.rs` (Replace, `open_lock`, `path_id`, `move_new`); contrast `safecopy::simplify` / `canonicalize` stripping `\\?\` for display only.
- **Trigger:** Content on FAT32 USB with deep paths or components near `MAX_PATH` (260) without extended-path prefix on every Win32 call.
- **Impact:** Intermittent open/rename/copy failures on real consoles’ USB layouts; not covered by unit tests (temp dirs are short).
- **Suggested fix:** Central helper to open/rename with `\\?\` canonical paths where `std` does not already (audit `MoveFileExW`, `OpenOptions`).

### 5. LOW (informational) — K2 rename rollback integration test is Unix-only

- **File:line:** `src-tauri/src/archive/tests.rs:2710-2755` (`#[cfg(unix)]`).
- **Trigger:** N/A (test gap).
- **Impact:** Windows-specific `ensure_same_folder` + `path_id(FILE_FLAG_BACKUP_SEMANTICS)` rollback under real folder rename is only exercised via `k2_folder_identity_mismatch_is_detected` (logic), not full `replace_unit` pause/rename.
- **Suggested fix:** Windows-gnu or in-process rename hook test mirroring the Unix pause test.

---

## Positive notes (7ee077d → f3bcc65)

- **`path_id` on directories:** `FILE_FLAG_BACKUP_SEMANTICS` when `symlink_metadata` is a directory (`archive.rs:1873-1890`) fixes folder identity for K2 / lock revalidation on NTFS (and Wine’s FS). Prior `FILE_FLAG_OPEN_REPARSE_POINT`-only open was wrong for directory paths.
- **`FolderLock` stores `folder_id` + `ensure_same_folder`:** Checked between Replace steps (`replace_unit` 2207-2238); aligns with K2 rollback story.
- **Cross-volume `move_new`:** Copy to `.unpack-part.*`, `sync_tree` (incl. Windows `FlushFileBuffers` on dirs), then `rename_noreplace` — avoids publishing a half-copied live name.
- **Manifest:** Old-half fingerprints + `moved_live` + legacy 1.7.3 repair (`trust_manifest` / `m1_legacy_replace_note_repairs_after_upgrade`) — exercised under Wine.
- **Grants persistence:** Atomic tmp+rename (`safecopy.rs:360-379`); corrupt load backs up file.
- **Type-folder lock:** Windows delete-sharing + disposition-on-drop documented; `r27`/`r29`/`r33` tests pass under Wine.

---

## Overall verdict: **FAIL**

One **HIGH** recovery bug (Finding 1) can destroy the last good GOD copy after a crash window that the new manifest fields were meant to harden. MEDIUM picked-folders durability should be fixed before treating 1.7.4 as crash-safe for permissions migration.

---

## What could NOT be tested

- Real **FAT32 USB** volume for cross-volume Replace / `sync_tree` / `fs_type` (only logic and `fat_check` unit tests).
- **Long-path** (>260) Replace on physical Windows NTFS/FAT32.
- **`k2_rename_during_replace_stops_and_rolls_back`** (Unix-only test).
- **`r28_two_processes_are_serialized`** (spawns a second test executable; not run under Wine in this review).
- Real **multi-user SMB / NAS** lock sharing, **delete-pending** timing beyond unit mocks.
- Full **Tauri UI** migration dialog and WebView2 runtime (lib tests only).
- **SIGKILL** of production `unpack.exe` mid-job (only `REPLACE_KILL_AT` / `RECOVER_KILL_AT` injection).
