# Reviewer B — Security & Trust Boundaries (R40)

**Target:** `f3bcc65` on `v1.7.4-dev`  
**Range:** `7ee077d` (v1.7.3-dev) → `f3bcc65`  
**Reviewer:** B (security / trust boundaries)  
**Date:** 2026-10-25

---

## Executive summary

The `picked-folders.json` redesign and removal of `grant_paths` materially tighten the webview→native write boundary: settings import cannot add grants, and startup rebuilds persisted grants only from the picked list (after `clear_persist`). Cover handling, download test hooks, and grant persistence hardening look solid in code and unit tests.

**Remaining issues** are mostly consistency gaps (volume-root grants via native picker vs migration rules), read-side IPC that bypasses grants (`library_scan` / `library_index`), CSP still allowing `'unsafe-inline'` styles, and dependency advisories from `cargo audit`. None of these are silent remote code execution by themselves, but volume-root grants and unauthenticated library reads are trust-boundary defects worth fixing before treating 1.7.4 as “hardened.”

**Verdict: FAIL** (fix volume-root grant inconsistency and either gate library read IPC on grants or document/accept the read oracle explicitly; tighten CSP if inline styles are truly gone).

---

## Scope reviewed

| Area | Result |
|------|--------|
| IPC surface (`generate_handler!` in `lib.rs`) | Catalogued below |
| `picked-folders.json` / `grant_paths` removal | Mostly PASS; gaps on drive roots & junction re-grant |
| `paths_are_granted` | LOW info disclosure |
| Cover cache / XboxUnity download | PASS (unit tests) |
| CSP / inline styles | Inline `style=` removed from `src/`; **`unsafe-inline` still in CSP** |
| Test flags / release binary | PASS (`UNPACK_TEST_*` absent from release `unpack`) |
| `cargo audit` / `npm audit` | 2 Rust vulns + warnings; npm clean |

---

## IPC command inventory (webview → native)

All commands are exposed via `tauri::generate_handler!` (`src-tauri/src/lib.rs` ~1958–2020). Capabilities (`capabilities/default.json`) allow dialog, notifications, opener, progress bar — no extra filesystem permissions beyond command logic.

| Command | Grant / trust gate | What it can trigger |
|---------|-------------------|---------------------|
| `native_info` | None | 7-Zip path, platform metadata |
| `file_info` | **None** | `metadata()` on arbitrary path string |
| `list_archive` | **None** | 7-Zip listing of arbitrary archive path |
| `read_entry` | **None** | Read one archive entry (≤4 MiB) from arbitrary archive |
| `extract_archive` | Dest: `grants.check` or “beside archive” parent match | Extract to disk, session `grant_session` on output |
| `extract_entry` | Same as extract | Partial extract |
| `read_file` | **None** (extension + size only) | Read whole archive ≤1.5 GiB if name looks like archive |
| `create_archive` | `check_or_parent` on `outFile` | Create zip/7z on disk |
| `iso_info` / `iso_companions` / `iso_disc_status` | Mixed: companions/disc use `grants.check` on paths | ISO/GOD introspection |
| `iso_to_god` / `iso_to_xex` | Dest granted; **ISO path ungranted** | Conversion jobs |
| `god_extracted` / `xex_extracted` | Token-based (prior job) | Finish extraction / delete ISO |
| `god_verify_only` | **None** | Read-only GOD folder verify on arbitrary path |
| `dlc_install` | Package path + library roots granted | Install DLC/TU |
| `dlc_match_games` | **None** (library strings from page) | Search library paths for title ID |
| `dlc_scan_drives` | **None** (does not grant) | Enumerate known `Content/0000000000000000` roots on drives |
| `library_scan` | **None** | Full library scan of arbitrary folder list |
| `library_index` | **None** | Cached index + metadata for arbitrary libraries |
| `library_cover` | `grants.check(game_path)` + `valid_tid` | Local/cache/network cover → data URL |
| `library_cover_cached` | `valid_tid` only (app-data cache) | data URL from cache |
| `library_cover_reset` | None | Delete cover-failure map in app data |
| `copy_to_usb` | Both paths granted | Copy game tree to USB |
| `mod_identify` / `mod_copy` | Path/target granted | Mod install |
| `builtin_menus` | None | Read built-in menu set from resources |
| `catalog` | Grant check on paths in catalog flow | Signed catalog fetch |
| `game_search` | None | Local gamedb search |
| `pick_path` | Native dialog → `picked_folders.record_dialog_pick` | **Persisted grant** |
| `pick_save` | Native save + grant pick | Save path |
| `download_url` | Dest `grants.check` or app-data Downloads (auto-grant) | SSRF-hardened HTTP(S) download |
| `download_folder` | Grants app-data Downloads | Returns downloads path |
| `list_archives_in_folder` | `grants.check` | List archives under root |
| `preflight_local` | **None** on `dest` | Space/FAT checks on dest + listed paths |
| `usb_prepare` | `grants.check(root)` | USB folder prep |
| `ftp_*` / `saves_*` / `screenshots_*` | FTP: console IP rules; disk: `grants.check` on dest/src | Xbox FTP operations |
| `lan_discover` | None | UDP LAN scan |
| `paths_are_granted` | None | Boolean vector for paths (see findings) |
| `migration_notices` | None | One-time migration text |
| `save_text_file` | `write_export_text` / grants | Export settings etc. |
| `check_release` | Opt-in only | GitHub release check |
| `discord_set_*` | None | Local Discord IPC |
| `cancel_job` / `notify_done` / `set_taskbar_progress` | None | UI |
| `take_launch_requests` | None | Second-instance argv queue |
| `error_help` | None | Static hints |

**Test-only (not in release app):** `examples/bridge` adds `__test_grant`, `__test_drop_grant`, `__plant_migration_notice`, etc. (`bridge.rs`); requires `download-test-loopback` feature.

---

## Findings

### F1 — Native folder picker can persist a bare volume root (inconsistent with migration rules)

| Field | Value |
|-------|--------|
| **Severity** | **HIGH** |
| **Location** | `src-tauri/src/picked_folders.rs:47-61` (`record_dialog_pick`), `225-234` (`allow_auto_restore`) |
| **Trigger** | On Windows, user chooses **drive root** in `pick_path` (e.g. Settings → “Add library” or `pick_path` kind `folder`). `record_dialog_pick` canonicalizes and appends to `picked-folders.json` without calling `reject_for_bulk_add` / `is_bare_drive_root`. On next startup, `restore_grants` uses `allow_auto_restore`, which also **does not** reject bare drive roots (only `is_system_path` + `is_hostile_root`). |
| **Impact** | Persisted grant covers entire volume (`E:\`, `D:\`, …). Any IPC that uses `grants.check` can read/write anywhere on that drive (extract, copy to USB, DLC install, downloads, etc.). Migration path correctly skips `E:\` (`remember_approved_skips_root` test) but dialog path does not. |
| **Reproduction** | **Automated (partial):** `cd src-tauri && cargo test --lib remember_approved_skips_root bulk_add_refuses_root_and_system` — proves bulk/migration blocks `E:\` but does not cover dialog. **Manual (Windows):** Pick `E:\` as library folder → confirm `picked-folders.json` contains canonical `E:\` and `paths_are_granted` returns true for `E:\Games\...`. |
| **Suggested fix** | Call `reject_for_bulk_add` (or `is_bare_drive_root`) in `record_dialog_pick` before save/grant; add `is_bare_drive_root` to `allow_auto_restore`; reject `\\?\` / UNC roots consistently; add unit test `dialog_pick_refuses_volume_root`. |

---

### F2 — Startup re-grant follows junctions/reparse points (TOCTOU)

| Field | Value |
|-------|--------|
| **Severity** | **MEDIUM** |
| **Location** | `src-tauri/src/picked_folders.rs:92-103`, `safecopy.rs:397-402` (`grant` uses `canonicalize`) |
| **Trigger** | Legitimate folder listed in `picked-folders.json` is replaced (malware or second process) with a junction to e.g. `C:\Windows\System32` before Unpack starts. `allow_auto_restore` checks `is_system_path` on the **stored** path string, not the canonical target. `grants.grant` re-`canonicalize`s and may grant the symlink target. |
| **Impact** | Widened effective grant to sensitive locations on restart without user dialog. Writes still use `ensure_dir_under` / `is_link` in safecopy, but read IPC and listing can expose data. |
| **Reproduction** | **Manual (Windows):** (1) Pick `D:\Games` in dialog. (2) Replace `D:\Games` with junction to `C:\Users\<user>\Documents`. (3) Restart app → invoke `paths_are_granted` on path under Documents → `true`. **Linux:** not reproduced (no junctions); see `safecopy::tests::r5_refuses_symlinked_components` for write-path refusal only. |
| **Suggested fix** | On restore and `record_dialog_pick`, use `symlink_metadata` + `is_link` and refuse; canonicalize only after verifying each path component is not a reparse point; optionally re-prompt if canonical path differs from stored path. |

---

### F3 — `library_scan` / `library_index` read arbitrary directories without grants

| Field | Value |
|-------|--------|
| **Severity** | **MEDIUM** |
| **Location** | `src-tauri/src/lib.rs:654-667`, `1497-1548` |
| **Trigger** | Webview (or compromised page logic) calls `invoke('library_scan', { libraries: ['C:\\Users\\Public'] })` or `library_index` with the same — paths come from `localStorage` settings but are **not** validated with `grants.check`. |
| **Impact** | Information disclosure: game titles, paths, sizes, DLC counts from any readable directory on the machine. No write, but bypasses the grant model for filesystem reads. |
| **Reproduction** | DevTools in desktop app: `await window.__TAURI__.core.invoke('library_scan', { libraries: ['<absolute-path>'] })` → returns game list if folder looks like a library. E2E uses granted fixtures only; no negative test. |
| **Suggested fix** | Filter `libraries` through `grants.check` (or require each root to appear in `picked-folders.json`); return error or empty for ungranted roots. Align with `dlc_install` / `library_cover`. |

---

### F4 — `paths_are_granted` leaks grant coverage for existing paths

| Field | Value |
|-------|--------|
| **Severity** | **LOW** |
| **Location** | `src-tauri/src/lib.rs:1415-1417`, `safecopy.rs:382-389` |
| **Trigger** | `invoke('paths_are_granted', { paths: ['D:\\Games', 'D:\\SteamLibrary', ...] })` for paths that **exist** on disk. |
| **Impact** | Reveals which candidate directories fall under persisted or session grants (including drag-drop session grants). Does not distinguish “not granted” vs “does not exist” (both `false` when `canonicalize` fails), but **does** distinguish granted vs ungranted among existing paths. Aids targeting follow-up social engineering (“pick this folder again”). |
| **Reproduction** | Grant `D:\lib` via picker; invoke with `['D:\\lib', 'D:\\other']` where both exist → `[true, false]`. |
| **Suggested fix** | Rate-limit or restrict to settings library paths only; return coarse status (“needs_pick”) without probing arbitrary strings; or move grant hints to Rust-only after `pick_path`. |

---

### F5 — CSP still includes `'unsafe-inline'` for styles

| Field | Value |
|-------|--------|
| **Severity** | **MEDIUM** (defense-in-depth / XSS) |
| **Location** | `src-tauri/tauri.conf.json:25` |
| **Trigger** | Diff removes `dangerousDisableAssetCspModification` but leaves `style-src: 'self' 'unsafe-inline'`. |
| **Impact** | Any future HTML/script injection (or third-party script bug) can inject inline styles; combined with `img-src data:` this weakens CSP compared to stated 1.7.4 goal in CHANGELOG/release notes. |
| **Reproduction** | `node tests/no-inline-style.mjs` → **PASS** (no `style=` in `src/`). Inspect `tauri.conf.json` → still `'unsafe-inline'`. |
| **Suggested fix** | Remove `'unsafe-inline'` from `style-src` now that UI uses CSS classes; run full E2E after change. |

---

### F6 — `read_file` / `file_info` / archive IPC without grants (pre-existing, still in scope)

| Field | Value |
|-------|--------|
| **Severity** | **LOW** (bounded) |
| **Location** | `lib.rs:127-170`, `507-523` |
| **Trigger** | Invoke `read_file` with `C:\secret.zip` (≤1.5 GiB, archive extension); `file_info` on any path. |
| **Impact** | Read metadata or full archive bytes without user pick. Extension gate limits `read_file` to archive names. |
| **Reproduction** | Bridge test harness: `POST /invoke/read_file` with path to test zip (see `tests/e2e.mjs`). |
| **Suggested fix** | Optional: require grant if path is outside app data and not from launch queue / drag-drop session. |

---

### F7 — Dependency advisories (`cargo audit`)

| Field | Value |
|-------|--------|
| **Severity** | **LOW** (desktop app, TLS via ureq/rustls) |
| **Location** | `src-tauri/Cargo.lock` (transitive) |
| **Trigger** | `cd src-tauri && cargo audit` |
| **Impact** | `rustls-webpki` 0.103.15 — RUSTSEC-2026-0382 (distrusted intermediates); `tokio` 1.54.0 — RUSTSEC-2026-0384 (Interval race). Unmaintained gtk/zbus crates on Linux build only. |
| **Reproduction** | `cargo audit` (run on review VM) → 2 vulnerabilities, 6 warnings. |
| **Suggested fix** | `cargo update` / bump Tauri stack to pull patched `rustls-webpki` and `tokio` ≥1.54.1. |

**npm:** `npm audit` → **0 vulnerabilities**.

---

## Areas explicitly checked — PASS notes

### `picked-folders.json` / settings import

- `grant_paths` IPC removed (`picked_folders::tests::grant_paths_ipc_command_removed`).
- Settings import drops `libraries` / `destPath` (`importcheck.js` `LOCAL_PATHS`; `planImport` sets `ok = null`).
- `restore_grants` clears old `grants.json` persist set then rebuilds from picked list only (`lib.rs:1913-1914`, `safecopy::clear_persist`).
- **Reproduction:** `cargo test --lib imported_path_not_granted_without_picked_list dialog_picked_folder_is_restored`.

### `dlc_scan_drives`

- Returns scan results only; does not call `grants.grant` (`lib.rs:1059-1063`). UI adds paths to settings but grant requires `pick_path` / `grantLibraryAccess` (`app.js:3530-3532`).

### Cover cache & XboxUnity

- HTTPS only to fixed `UNITY_BASE`; `redirects(0)`; caps 256 KiB JSON / 8 MiB image; magic bytes + dimension cap before decode; `valid_tid` blocks traversal; atomic cache writes; data URL capped at 512 KiB file (`covers.rs`).
- **Reproduction:** `cargo test --lib covers::` (9 tests, all pass).

### Test hooks vs release

- `allow_test_loopback()` compiled to `false` without `test` or `download-test-loopback` (`download.rs:127-135`).
- `strings target/release/unpack | rg UNPACK_TEST` → **no matches**.
- Bridge requires feature `download-test-loopback` (`Cargo.toml:60-62`); contains `UNPACK_TEST_*` and `__test_grant` (expected).

### Drag-and-drop grants

- Session-only `grant_session`; skips links (`lib.rs:1938-1956`). Not persisted to `picked-folders.json`.

---

## Tests & commands run (review VM)

```bash
node tests/no-inline-style.mjs
cd src-tauri && cargo test --lib picked_folders:: covers:: ssrf_blocks
cd src-tauri && cargo audit
npm audit
cd src-tauri && cargo build --release && strings target/release/unpack | rg UNPACK_TEST || true
cd src-tauri && cargo build --example bridge --features download-test-loopback
```

---

## What could NOT be tested

- **Windows-only behavior:** volume-root picker (F1), junction/reparse re-grant (F2), `E:` vs `E:\` picker return values, WebView2 drag-drop, NSIS release binary (built Linux GNU target only).
- **Full E2E / Chrome bridge suite** against this commit (time; not required for this report).
- **Real XboxUnity network** fetch (unit tests mock logic; no live HTTP to xboxunity.net).
- **Malicious `picked-folders.json` tampering** as separate local attacker (out of webview model; noted for completeness).
- **cargo audit fixes** not applied (report-only).

---

## Overall verdict

**FAIL** — Ship blockers for trust-boundary sign-off: **F1** (volume-root grant via native picker + auto-restore), **F3** (ungranted library read IPC). Address **F5** (CSP `unsafe-inline`) for alignment with 1.7.4 security messaging. **F2**, **F4**, **F6**, **F7** tracked as follow-ups.
