# R43 — Final re-check (panel round)

**Repo:** Qeezyxz/Unpack  
**Reviewed commit:** `c4d5740` (`v1.7.4-dev`)  
**Baseline for fixes:** `d8d5054` (R42 panel target)  
**Prior reports:** `reviews/R42/A.md`, `B.md`, `C.md` on `review/r42`  
**Role:** Read-only re-check; reproduce R42 triggers and run the full test matrix.

## Verdict

**PASS** — R42 **HIGH** and **CRITICAL** gate items are addressed at `c4d5740`. `record_user_folder` is bound to canonical roots from the latest `dlc_scan_drives`, Rust `Users\…\AppData` rules match `importcheck.js` on any drive letter, Clippy `-D warnings` is clean on Linux and `x86_64-pc-windows-gnu`, and the requested automated suites (including Wine lib tests and double e2e/xbox) are green. Residual **LOW** items (bridge `__test_grant`, optional symlink UX hardening, Windows-only path edge cases) do not block ship.

---

## R42 finding status

### Reviewer A (platform & crash safety)

| ID | R42 severity | Status at `c4d5740` | R43 rating | Location | Trigger (reproduced) | Impact | Fix / notes |
|----|--------------|---------------------|------------|----------|----------------------|--------|-------------|
| A-1 | MEDIUM | **Fixed** | — | `picked_folders.rs:380-394` | `is_system_path(r"D:\Users\me\AppData\Local")` → `true`; `users_downloads_allowed_appdata_refused` passes | Non-`C:` AppData/Default could be granted | Any-drive `users\` rules; tests at `569-576` |
| A-2 | LOW | **Fixed** | — | `archive.rs:2514-2524` | Code path `entry_count <= 1` quarantine branch | Silent relocate of foreign edits | Warning pushed with live + quarantine paths |
| A-3 | LOW | **Fixed** | — | `safecopy.rs:22-32` | `simplify` on `\\?\UNC\server\share\games` | Inconsistent stored/grant paths | UNC extended-prefix strip added |
| A-4 | LOW (observation) | **Unchanged OK** | LOW | `covers.rs:40-93` | `rename_with_retry_retries_permission_denied` | Retry on Win32 error 5 | Acceptable; now has targeted test |

Replace/crash items from R41 (inverted `from_this_replace`, foreign `.data`) remain covered; `b41_recovery_keeps_foreign_live_data` and Wine lib suite pass.

### Reviewer B (security & trust)

| ID | R42 severity | Status at `c4d5740` | R43 rating | Location | Trigger (reproduced) | Impact | Fix / notes |
|----|--------------|---------------------|------------|----------|----------------------|--------|-------------|
| B-1 | HIGH | **Fixed** (scan-bound) | LOW residual | `lib.rs:1109-1116`, `picked_folders.rs:16-50` | Before scan: `require_scanned_root` → err; path not in scan → err (`record_user_folder_requires_scanned_root`, `dlc.mjs` bogus-root invoke); subpath/stale scan rejected (ephemeral `r43_scan_bypass` matrix, not committed) | Arbitrary persistent grants from webview | `LastScannedRoots` + `require_scanned_root` on IPC and bridge `397-404`. **Residual:** any root from the *last* scan can still be granted via `invoke` without clicking **Add** (same paths the UI offers). |
| B-2 | HIGH | **Fixed** | — | `picked_folders.rs:380-394` vs `importcheck.js:30-31` | `cargo test users_downloads_allowed_appdata_refused`; `node tests/importcheck.test.mjs` | Rust/JS disagree on `D:\Users\…\AppData` | Rules aligned on any drive letter |
| B-3 | LOW | **Mitigated** | LOW | `picked_folders.rs:114-129` | `restore_skips_symlink_to_root`; symlink `link → /etc` + `record_dialog_pick` → `Err` (canonicalize resolves) | Symlink hides intent pre-canonicalize | Post-canonicalize block; optional pre-link refusal unchanged |
| B-4 | LOW | **OK** | — | `picked_folders.rs:173-196` | `corrupt_picked_folders_backed_up`, `restore_skips_symlink_to_root` | Tampered JSON grants | Still adequate |
| B-5 | LOW | **Unchanged** | LOW | `bridge.rs:376-395` | `__test_grant` grants dirs via `record_dialog_pick` without scan | CI harness widens grants | Document loopback-only; not in shipping `lib.rs` |
| B-6 | LOW | **OK** (Linux) | LOW | `picked_folders.rs:355-366`, `importcheck.js` | `importcheck.test.mjs` M2 cases | Path trick bypass | Canonicalize + normalize; **not** re-run on real Windows for 8.3/junction (see NOT tested) |
| B-7 | N/A | **Improvement retained** | — | `lib.rs:1066-1073` | `dlc.mjs` scan lists root, libraries null until Add | Scan auto-grant removed | Still true at `c4d5740` |

#### `record_user_folder` bypass attempts (security focus)

| Attack | Result at `c4d5740` |
|--------|------------------------|
| Call before any scan | **Blocked** — `"That folder wasn't in the last Scan drives result…"` |
| Path not in last scan | **Blocked** — unit test + `dlc.mjs` |
| Subpath of scanned root | **Blocked** |
| Stale root after rescan | **Blocked** |
| Trailing separator on allowed root | **Allowed** (canonical match) — intended |
| Case change (Linux) | **Blocked** when path differs by case |
| Trailing dot segment (`other.`) | **Allowed** only if that exact path was scanned and exists |
| Symlink to scanned root | **Allowed** — canonicalizes to scanned root |
| Symlink to `/etc` via pick | **Blocked** after canonicalize |
| `\\?\` / 8.3 / junction | **Not exercised** on Linux/Wine for `require_scanned_root` |

### Reviewer C (behavior, UI, tests)

| ID | R42 severity | Status at `c4d5740` | R43 rating | Location | Trigger | Impact | Fix / notes |
|----|--------------|---------------------|------------|----------|---------|--------|-------------|
| C-F1 | CRITICAL | **Fixed** | — | `god/tests.rs:554-617` | `cargo clippy --lib --all-targets -- -D warnings` (Linux + `x86_64-pc-windows-gnu`) | CI gate fail | `drop(layout)` removed; layout scoped in block |
| C-F2 | MEDIUM | **Fixed** | — | `make_testdata.py:24` | `UNPACK_7ZA=src-tauri/binaries/7za-x86_64-unknown-linux-gnu python3 tests/make_testdata.py <tmp>` | Fixture build fails | `os.path.abspath(sz)` |
| C-F3 | MEDIUM | **Fixed** | — | `csp-screens.mjs:67-75` | `node tests/csp-screens.mjs` | Weak 320px wrap assertion | Asserts `whiteSpace === 'normal'` + content/title/height |
| C-F4 | MEDIUM | **Fixed** | — | `covers.rs:588-606` | `rename_with_retry_retries_permission_denied` | Retry loop untested | Injectable `rename_with_retry_using` seam + test |
| C-F5 | LOW | **Fixed** | — | `e2e.mjs:178-181` | Second archive create | Status race | E2e asserts stale status cleared |
| C-F6 | LOW | **Fixed** | — | `test-shim.js` | Default version | Drift from 1.7.4 | `1.7.4` in shim |
| C-F7 | LOW | **Fixed** | — | `release-notes-1.7.4.md:14` | Docs compare | Release notes omit profile folders | Wording aligned with CHANGELOG |

---

## Regressions checked (`d8d5054..c4d5740`)

| Area | Change | R43 assessment |
|------|--------|----------------|
| God tests | `verify_package_detects_damage` — inner block scope instead of `drop(layout)` | **No behavior regression**; same assertions; Clippy clean |
| Covers | `rename_with_retry_using` + unit test | **Improvement**; no production regression |
| Archive undo | Quarantine warning for `entry_count <= 1` | **Improvement** (A-2) |
| `record_user_folder` | Scan binding | **Security improvement** (B-1) |

---

## Test matrix (executed on review VM at `c4d5740`)

| Command | Result |
|---------|--------|
| `UNPACK_TESTDATA=/workspace/testdata UNPACK_7ZA=…/7za-x86_64-unknown-linux-gnu cargo test --lib` | **384 passed**, 2 ignored, 0 failed (~80 s parallel) |
| Same with `-- --test-threads=1` | **384 passed**, 2 ignored (~148 s) |
| `cargo clippy --lib --all-targets -- -D warnings` (Linux) | **PASS** |
| Same `--target x86_64-pc-windows-gnu` | **PASS** (after `rustup target add` + mingw) |
| Wine lib tests (fresh `WINEPREFIX=/home/ubuntu/r43-wine-fresh`, `WebView2Loader.dll` + `7za.exe` beside `unpack_lib-*.exe`, `UNPACK_TESTDATA=Z:\workspace\testdata`) | **366 passed**, 2 ignored, 0 failed (~278 s). Log: `/opt/cursor/artifacts/wine-r43-lib-tests.log` |
| `npm test` | **PASS** (`theme.test.mjs` 7/7, `no-inline-style.mjs`) |
| `node tests/importcheck.test.mjs` | **all passed** |
| All `tests/*.mjs` (bridge env: `UNPACK_TEST_NOTICE_DIR`, `UNPACK_TEST_ALLOW_LOOPBACK`, `UNPACK_TEST_DRIVES`; `UNPACK_BRIDGE_PORT` unset; bridge with `UNPACK_7ZA=…`) | **18/18 PASS** (after `p7zip-full` for system `7za` in e2e/v160 generators) |
| `node tests/e2e.mjs` × 2 | **44/44** each |
| `node tests/xbox.mjs` × 2 | **52/52** each |
| `cargo test --lib picked_folders` | **13 passed** |
| Fixtures | `UNPACK_7ZA=src-tauri/binaries/7za-x86_64-unknown-linux-gnu python3 tests/make_testdata.py <tmp>` — **OK** |

---

## What was NOT tested

- **Native Windows** desktop app (only **Wine** for cross-compiled lib tests; no MSVC host).
- **`record_user_folder` on Windows** with `\\?\` prefixes, 8.3 short names, junctions/reparse points, and case-folding on a case-insensitive volume.
- **Shipping release binary** / code signing / SmartScreen.
- **macOS** builds or UI.
- **Manual** DevTools CSP walkthrough (automated via `csp-screens.mjs` only).
- **Quarantine** branch (A-2) under a live interrupted Replace with ≤1 foreign entry (logic + warning text only).
- **Full CI workflow file** verbatim (local bridge + env matched `.github/workflows/test.yml` UI job intent).

---

## New / residual R43 notes (not R42 regressions)

| Severity | Location | Trigger | Impact | Fix |
|----------|----------|---------|--------|-----|
| LOW | `lib.rs:1109-1116` | `invoke("record_user_folder", { path: scannedRoot })` without clicking **Add** | Any compromised webview can persist grants for **all** roots from the last scan, not only one | Optional: server-side “Add” nonce per root index, or drop IPC and use `add_scanned_root(index)` only |
| LOW | `bridge.rs:376-395` | `__test_grant` with a directory path | Test harness bypasses scan binding | Accept for CI; keep loopback-only |

---

*R43 re-check complete. Product commit reviewed: `c4d5740`; this report committed on `review/r42` only.*
