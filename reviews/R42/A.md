# R42 — Reviewer A (platform & crash safety)

**Repo:** Qeezyxz/Unpack  
**Range:** `2f3a9db` → `d8d5054` (branch `v1.7.4-dev`, read-only)  
**Focus:** Windows folder grants, Replace kill/crash recovery, cover atomic writes, canonical paths.

## Method

- Read diff and crash paths in `picked_folders.rs`, `archive.rs` (`recover_asides` / `recover_one`), `covers.rs`, `bridge.rs`, `lib.rs`.
- Linux: targeted `cargo test --lib` for `picked_folders::`, `archive::tests::b41_recovery_keeps_foreign_live_data`, `covers::`, `k2_rename_during_replace_stops_and_rolls_back`.
- **Required Windows cross-build:** `cargo test --lib --target x86_64-pc-windows-gnu --features download-test-loopback --no-run` in `src-tauri/`.
- **Required Wine run:** fresh `WINEPREFIX`, `WebView2Loader.dll` and `7za.exe` copied beside `unpack_lib-*.exe`, fixtures via `UNPACK_7ZA=/workspace/src-tauri/binaries/7za-x86_64-unknown-linux-gnu python3 tests/make_testdata.py /workspace/testdata`, then full lib tests with `UNPACK_TESTDATA=Z:\workspace\testdata`.

## Build / test evidence

| Step | Result |
|------|--------|
| `x86_64-pc-windows-gnu` lib test binary | Built OK (`unpack_lib-3f963a162335b0a6.exe`) |
| Wine lib tests (fresh prefix) | **362 passed, 0 failed, 2 ignored** (~276 s). Log: `/opt/cursor/artifacts/wine-r42-lib-tests.log` |
| `b41_recovery_keeps_foreign_live_data` (Linux) | Pass |
| `picked_folders::` (12 tests, Linux) | Pass |
| `covers::` (9 tests, Linux) | Pass |

Wine ran successfully (Wine 9.0). Without `UNPACK_TESTDATA`, 13 archive tests fail on missing `Z:\workspace\testdata`; with fixtures, full suite is green.

## Replace / crash safety (summary)

The R41 **inverted `from_this_replace` bug** is fixed: live halves that match the manifest are **renamed aside** (`.unpack-new.*`) instead of being deleted or mis-reported as foreign (`archive.rs:2496–2511`). Foreign multi-entry `.data` folders are no longer removed (`b41_recovery_keeps_foreign_live_data`; previously `remove_any` on the live path).

`finish` still deletes only **fingerprint-matched** old asides (`archive.rs:2437–2450`). Undo paths avoid putting old halves over occupied slots (`archive.rs:2520–2525`). `.unpack-part.*` cleanup under the type-folder lock is limited to dot-prefixed leftovers (`archive.rs:2317–2335`).

**User file loss:** No reproducible path found in this range where recovery deletes an unrelated multi-file `.data` tree; the prior B41-H1 failure mode is covered by the new test and Wine run.

## Findings

### 1. `Users\…\AppData` blocked only on `C:` (Rust ≠ JS, ≠ stated R41 scope)

| Field | Detail |
|-------|--------|
| **Severity** | **MEDIUM** |
| **Location** | `src-tauri/src/picked_folders.rs:343–353` (`windows_users_blocked`) |
| **Trigger** | User picks or “Scan drives → Add” records `D:\Users\<name>\AppData\…` (or any non-`C:` profile path). `record_dialog_pick` / `remember_approved` / `restore_grants` call `is_system_path` → `windows_users_blocked`, which only inspects `c:\users\`. |
| **Impact** | Persistent grant + `picked-folders.json` entry to a sensitive profile store on a secondary volume. Frontend/import (`src/importcheck.js:31`) **does** block any drive letter; Rust is weaker, so UI/import and backend disagree. |
| **Reproduce** | Node: `isSystemPath('D:\\Users\\x\\AppData\\Local')` → `true`. Mirror of `windows_users_blocked` on `d:\users\x\appdata\roaming` → `false` (reproduced with small standalone program in review VM). `bulk_add_refuses_system_paths_and_system_drive` only asserts `C:\Users\Justin\AppData`. |
| **Fix** | Match `importcheck.js`: e.g. `^[a-z]:\\users\\(default\|all users)(\\|$)` and `^[a-z]:\\users\\[^\\]+\\appdata(\\|$)` on normalized paths; add tests for `D:\Users\…\AppData` and `D:\Users\Default`. |

### 2. Silent quarantine of single-entry foreign `.data` during undo

| Field | Detail |
|-------|--------|
| **Severity** | **LOW** |
| **Location** | `src-tauri/src/archive.rs:2513–2518` |
| **Trigger** | Interrupted Replace in `undoing`; old aside exists; live `.data` is **not** from this Replace but contains **≤1** directory entry; recovery tries to restore old aside onto `orig`. |
| **Impact** | Data is moved to `.unpack-new.conflict.<titleid>.data*` without a warning (unlike the `continue` branch at `2520–2525`). Nothing is deleted, but the user may not notice relocated edits. |
| **Reproduce** | No dedicated test (B41 uses two files → warning + `continue`). Logic review + `entry_count <= 1` branch. |
| **Fix** | Push a warning naming the quarantine path, or require `entry_count == 0` before quarantine. |

### 3. Extended-path `simplify` only strips `\\?\` for drive-letter paths

| Field | Detail |
|-------|--------|
| **Severity** | **LOW** |
| **Location** | `src-tauri/src/safecopy.rs:23–30`, used from `picked_folders.rs:82,148` |
| **Trigger** | `canonicalize` returns `\\?\UNC\server\share\games` (or other non–`X:` extended form). |
| **Impact** | Stored path may keep the `\\?\` prefix; grants/compare behavior may differ from plain UNC. Uncommon for typical library picks. |
| **Reproduce** | Code inspection; no Windows UNC VM case run. |
| **Fix** | Extend `simplify` / `normalize_path_for_check` to strip known `\\?\` UNC prefixes consistently. |

### 4. Cover rename retry — behavior acceptable

| Field | Detail |
|-------|--------|
| **Severity** | **LOW** (observation, no defect filed) |
| **Location** | `src-tauri/src/covers.rs:40–93` |
| **Trigger** | `ACCESS_DENIED` / Win32 error 5 during `fs::rename` of temp cover. |
| **Impact** | Up to 5 attempts with backoff; if dest already looks like a valid image, temp is dropped and write succeeds (avoids tearing). `write_atomic_concurrent_valid_images` passes on Linux; full suite passes under Wine. |
| **Reproduce** | `cargo test --lib covers::write_atomic_concurrent_valid_images` |
| **Fix** | N/A |

## Positive notes (R41 fix verification)

- **Replace `from_this_replace`:** Ours moved aside; foreign kept (`archive.rs:2501–2511`, test `b41_recovery_keeps_foreign_live_data`).
- **Warning text** for mismatched live `.data` after move now reflects old aside vs new aside vs live path (`archive.rs:2397–2431`).
- **Picked folders:** `canonicalize` + `simplify` on pick/restore; system volume root refused; no silent `grant_session` for blocked paths (`picked_folders.rs:78–105`); `record_user_folder` wired in app + bridge (`lib.rs:1106–1111`, `bridge.rs:395–401`).
- **Bridge:** `scan_roots` no longer blanket-grants every hit (`bridge.rs` diff ~836).
- **God tests:** file handles dropped before `remove_dir_all` (`god/tests.rs`).

## Verdict

**FAIL** — Replace/crash-safety regressions from R41 are addressed and proven under Wine (362/362 runnable tests), but **folder grant policy is still incomplete** on non-`C:` volumes for `Users\…\AppData` (and related profile paths), contradicting `importcheck.js` and the stated “block Users AppData” fix. Ship after finding **#1** is fixed and covered by tests.
