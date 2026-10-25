# R45 — Final panel round (read-only)

**Repo:** Qeezyxz/Unpack  
**Reviewed commit:** `88f0fcf` (`v1.7.4-dev`)  
**Baseline:** `c4d5740` (8 files, +159 / −43)  
**Prior reports:** `reviews/R42/*`, `reviews/R43/FINAL.md` on `review/r42`  
**Role:** Read-only review of Overseer R44 **LOW** fixes; no push to `v1.7.4-dev`, no merge/PR/release.

## Verdict

**PASS** — R44 LOW items are implemented correctly at `88f0fcf`: segment-bound Windows system-folder checks, profile-scoped `Users\…\AppData` blocking, drag-and-drop `grant_session` gated by `validate_session_grant`, JS/Rust trim alignment on `isSystemPath`, archive quarantine / `.unpack-part` tests, Wine-friendly `c1_iso_list` teardown, and Settings narrow-layout CSS (320–360px). Full requested test matrix is green on the review VM. Residual **MEDIUM** Rust/JS disagreement on mid-path `System32` suffixes (pre-existing import-check rule) and **LOW** items (trailing-dot segment bypass, silent drag-drop refusal, no UI test for hostile drops) do not block ship.

### Finding counts

| Severity | Count |
|----------|------:|
| CRITICAL | 0 |
| HIGH | 0 |
| MEDIUM | 1 |
| LOW | 4 |

---

## R44 LOW fix verification

| Fix | Status | Evidence |
|-----|--------|----------|
| Segment-bound system folders (`E:\Windows Games`, `D:\ProgramData2` allowed) | **Correct** | `path_has_prefix_segment` + `system_folder_checks_use_path_segments` |
| Only `\<drive>:\Users\<profile>\AppData` blocked (nested `…\Games\AppData` allowed) | **Correct** | `windows_users_blocked` second segment + tests / `importcheck.test.mjs` |
| Drag-and-drop calls `validate_session_grant` (refuse `/`, system drive root, system paths; USB roots OK) | **Correct** | `lib.rs` webview handler + `bridge.rs` drop path; `session_grant_refuses_system_drive_root` (Wine) |
| Rust / JS trim alignment | **Correct** | `normalize_path_for_check` trims; `importcheck.js` `String(…).trim()`; test `normalize_path_for_check_trims_leading_space` + importcheck leading-space case |
| Archive quarantine + `.unpack-part` cleanup tests | **Correct** | `undo_quarantines_foreign_single_entry_data`, `recover_asides_removes_stale_unpack_part_files` (Linux + Wine) |
| Wine cleanup (`c1_iso_list` drops manual `remove_dir_all`) | **Correct** | `TmpDir` owns fixture dir; test still asserts ISO list behaviour |
| Settings CSS 320–360px | **Correct** | `app.css` `@media (max-width: 360px)`; `csp-screens.mjs` checks 320px and 340px |

### Drag-and-drop grant refusal (`C:\`, `C:\Windows`)

- **Code:** `validate_session_grant` canonicalizes, rejects `is_system_drive_root` and `is_system_path` before `grant_session` (`lib.rs:1967–1976`, `picked_folders.rs:410–427`). Errors are swallowed (no grant), which matches “cannot grant” but gives no user-visible error (**LOW** below).
- **Tests:** `session_grant_refuses_system_drive_root` passes under Wine (`C:\` on Windows target). No dedicated drop simulation for `C:\Windows` in `tests/*.mjs`; reliance on `is_system_path` + canonicalize is sound on Wine/Linux string checks.

---

## Rust / JS `is_system_path` parity (33 inputs)

Compared via ephemeral `parity_check` binary (`unpack_lib::picked_folders::is_system_path`) vs `importcheck.js` `isSystemPath` on the review VM.

| Input | Rust | JS | Match |
|-------|:----:|:--:|:-----:|
| `C:\Windows` | 1 | 1 | ✓ |
| `C:\Windows\System32` | 1 | 1 | ✓ |
| `C:\WINDOWS.` | 0 | 0 | ✓ |
| `C:\Windows Games` | 0 | 0 | ✓ |
| `D:\Windows` | 1 | 1 | ✓ |
| `C:\Program Files` | 1 | 1 | ✓ |
| `C:\ProgramData2` | 0 | 0 | ✓ |
| `C:\Users\a\AppData` | 1 | 1 | ✓ |
| `C:\Users\a\Games\AppData` | 0 | 0 | ✓ |
| `D:\Users\a\AppData\Local` | 1 | 1 | ✓ |
| `\\?\C:\Windows` | 1 | 1 | ✓ |
| `\\server\share\foo` | 1 | 1 | ✓ |
| `/` | 0 | 0 | ✓ |
| `/etc` | 1 | 1 | ✓ |
| `E:\` | 0 | 0 | ✓ |
| `E:\games` | 0 | 0 | ✓ |
| **`D:\x\System32`** | **0** | **1** | **✗** |
| `C:\Windows\System32\` | 1 | 1 | ✓ |
| `C:\Users\Default` | 1 | 1 | ✓ |
| `C:\Users\All Users` | 1 | 1 | ✓ |
| `C:\Users\me\Downloads` | 0 | 0 | ✓ |
| `D:\ProgramData2\cache` | 0 | 0 | ✓ |
| `E:\Windows Games\save` | 0 | 0 | ✓ |
| `E:\Windows\System32` | 1 | 1 | ✓ |
| `C:\Program Files (x86)\x` | 1 | 1 | ✓ |
| `C:\$Recycle.Bin\x` | 1 | 1 | ✓ |
| `  C:\Windows\System32` | 1 | 1 | ✓ |
| `C:/Windows/System32` | 1 | 1 | ✓ |
| `//server/share` | 1 | 1 | ✓ |
| `\\.\C:\foo` | 1 | 1 | ✓ |
| `/usr/bin` | 1 | 1 | ✓ |
| `C:\Mods\subdir\system32\evil` | 1 | 1 | ✓ |
| `C:\notsystem32` | 0 | 0 | ✓ |

**Note:** `/` and bare `E:\` are not “system paths” in either layer; session grants still refuse `/` and the **system** volume root via `validate_session_grant` / `is_system_drive_root`, while USB roots like `E:\` remain allowed for restore/grant policy.

---

## Rated findings

| Severity | File:line | Trigger | Impact | Fix |
|----------|-----------|---------|--------|-----|
| **MEDIUM** | `picked_folders.rs:437-438` vs `importcheck.js:32` | `is_system_path("D:\\x\\System32")` → Rust **false**, JS **true** | Settings import (JS) drops mod paths Rust would still treat as user paths; inconsistent trust boundary | Align rules: e.g. Rust `ends_with("\\system32")` / `\\syswow64` with segment boundary, or narrow JS regex to require a path separator before `system32` only when not under `Windows\` |
| **LOW** | `picked_folders.rs:373-375`, `importcheck.js:29` | `C:\WINDOWS.` (trailing dot on segment) → **not** blocked | On real Windows, dot-suffixed segments may normalize to `Windows` and bypass string checks | After canonicalize at pick time, reject if normalized display matches blocked segments with optional trailing `.` / spaces |
| **LOW** | `lib.rs:1967-1970`, `bridge.rs:423-425` | Drop `C:\Windows` on hostile path | Grant correctly withheld but **no** toast / log | Surface `validate_session_grant` error via existing notification path (once per drop) |
| **LOW** | `tests/*.mjs` (gap) | No puppeteer drag-drop of `C:\` / `C:\Windows` | Regression could reintroduce silent grants without CI signal | Bridge hook or unit test calling `validate_session_grant` on fixture paths under Wine |
| **LOW** | `bridge.rs:376-395` (unchanged) | `__test_grant` loopback | CI widens grants; not in shipping `lib.rs` | Keep loopback-only; document in HANDOFF |

---

## Regressions (`c4d5740..88f0fcf`)

| Area | Assessment |
|------|------------|
| Scan-bound `record_user_folder` | **Unchanged** — still required |
| R43 Clippy / god layout | **No regression** |
| Removing `\\.\` strip in `normalize_path_for_check` | **Safe** — `raw_path_is_device_or_unc` still blocks `\\.\` / `\\?\` / UNC before normalize |
| Archive ISO list test teardown | **Improvement** — avoids Wine double-delete races |

---

## New tests — failure sensitivity (revert check)

| Test | Would fail if fix reverted? |
|------|----------------------------|
| `system_folder_checks_use_path_segments` | **Yes** — prefix `starts_with("windows")` would block `E:\Windows Games` |
| `users_downloads_allowed_appdata_refused` (`Games\AppData`) | **Yes** — old `contains("\\appdata\\")` rule |
| `session_grant_refuses_system_drive_root` | **Yes** — without `validate_session_grant` wrapper |
| `undo_quarantines_foreign_single_entry_data` | **Yes** — without quarantine branch / warning |
| `recover_asides_removes_stale_unpack_part_files` | **Yes** — without `.unpack-part` recovery |
| `importcheck.test.mjs` new `isSystemPath` cases | **Yes** |
| `csp-screens.mjs` 340px pass | **Yes** — without CSS `@media (max-width: 360px)` |

---

## Test matrix (executed at `88f0fcf` on review VM)

| Command | Result |
|---------|--------|
| `UNPACK_7ZA=src-tauri/binaries/7za-x86_64-unknown-linux-gnu` + `UNPACK_TESTDATA=<tmp>` + `python3 tests/make_testdata.py <tmp>` + `cargo test --lib` | **389 passed**, 2 ignored |
| `cargo clippy --lib --all-targets -- -D warnings` (Linux) | **PASS** |
| Same `--target x86_64-pc-windows-gnu` (after `mingw-w64`) | **PASS** |
| Wine lib tests: fresh `WINEPREFIX=/home/ubuntu/.wine-r45-fresh`, `WebView2Loader.dll` + `7za.exe` beside `unpack_lib-*.exe`, `UNPACK_TESTDATA` via `Z:\…` | **371 passed**, 2 ignored (~150 s) |
| `npm test` | **PASS** |
| `node tests/importcheck.test.mjs` + all other `tests/*.mjs` with bridge (`UNPACK_BRIDGE_PORT` unset; `cargo build --example bridge --features download-test-loopback`; `unpack-catalog-sign`; `pyftpdlib==2.2.0`) | **PASS** |
| `node tests/e2e.mjs` × 2 | **44/44** each |
| `node tests/xbox.mjs` × 2 | **52/52** each |

**Environment notes:** First `cargo test --lib` without `UNPACK_7ZA` / `UNPACK_TESTDATA` failed 36 archive tests (missing `7za`). UI suites needed `p7zip-full` and `/usr/bin/google-chrome` symlink for fixture zip generation and Puppeteer.

---

## What was NOT tested

- **Native Windows** desktop (drag-and-drop UX, Explorer paths, 8.3 / junction / trailing-dot segments on a case-insensitive volume).
- **Real** `C:\` / `C:\Windows` drag-and-drop in the GUI (only `validate_session_grant` + `is_system_path` logic and Wine unit test for system drive root).
- **macOS** build or UI.
- **Shipping** installer / portable zip / code signing.
- **Full** `.github/workflows/test.yml` job graph on GitHub-hosted runners (local env matched the UI job’s bridge + env vars).
- **`python3 tests/gamelists.py`** (in CI `rust-and-theme` job but not in the R45 run list).

---

*R45 review complete. Product commit reviewed: `88f0fcf`; this report committed on `review/r42` only.*
