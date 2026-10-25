# Reviewer C — Behavior, UI & tests (4b9dca9 on v1.7.4-dev)

**Target:** `4b9dca9` on `v1.7.4-dev` (1.7.4 RC)  
**Compare:** `7ee077d` (v1.7.3-dev), released `64fc66d` (main / 1.7.0)  
**Focus:** Runtime behavior, UI under production CSP, automated suites, Wine vs Windows test signal  
**Date:** 2026-10-25

---

## Executive summary

Automated coverage on Linux is strong: **375/377** lib tests pass (2 ignored), **all** `tests/*.mjs` suites and `npm test` pass on the bridge harness, and headless Chrome under the **production CSP** (no `style-src unsafe-inline`) reports **zero** violations across major screens. The **1.7.4** frontend fixes (`paintJob` errors, migration notice queue, `downloadBusy`, early `onNativeDrop`, library cover patching) are present in `app.js` and align with passing UI suites.

**Blockers for release hygiene:** (1) **`CHANGELOG.md` edits the frozen 1.7.3 section** (explicit panel requirement violated). (2) **`cargo clippy --all-targets --target x86_64-pc-windows-gnu -D warnings` fails** on `testutil.rs` unreachable code when cross-compiling tests on Linux. (3) **Two lib tests fail under Wine only** — not reproduced on native Linux; same GOD test fails at `7ee077d` under Wine; treat as **Wine CI noise**, not proven Windows product bugs.

**Verdict: FAIL** (docs + Windows-target clippy; otherwise behavior/UI test bar is met).

---

## Test matrix (4b9dca9)

| Step | Command / action | Result |
|------|------------------|--------|
| Fixtures | `UNPACK_7ZA=/workspace/src-tauri/binaries/7za-x86_64-unknown-linux-gnu python3 tests/make_testdata.py /tmp/unpack-review-testdata` | **OK** (`test data ready`) |
| Rust lib (Linux) | `UNPACK_TESTDATA=… cargo test --lib` | **375 passed**, 0 failed, **2 ignored**, 377 filtered (90s) |
| Clippy (Linux host) | `cargo clippy --all-targets -D warnings` | **OK** |
| Clippy (Windows GNU) | `cargo clippy --all-targets --target x86_64-pc-windows-gnu -D warnings` | **FAIL** — see Finding 1 |
| Bridge | `cargo build --example bridge --features download-test-loopback` | **OK** |
| Catalog sign | `cargo build --bin unpack-catalog-sign` | **OK** |
| npm | `npm test` | **OK** (theme 7/7, no inline `style=`) |
| `tests/importcheck.test.mjs` | | **all passed** |
| UI suites (bridge env from `.github/workflows/test.yml`, `UNPACK_BRIDGE_PORT` unset, `pyftpdlib==2.2.0`) | `e2e`, `autogod`, `dlc`, `download`, `mods`, `builtin`, `catalog`, `xbox`, `saves`, `v160`, `v170`, `multidisc`, `content` | **ALL PASS** (counts below) |
| Flake check | `e2e.mjs` ×3, `xbox.mjs` ×3 | e2e: **43/43, 43/43, 42/43**; xbox: **52/52** ×3; see Finding 2 |
| CSP smoke | `node tests/csp-screens.mjs` | **ALL PASS** (0 violations) |
| Wine lib (fresh prefix) | Full `unpack_lib-*.exe` with `WebView2Loader.dll` + `7za.exe` beside exe | **355 passed, 2 failed**, 2 ignored, 359 total |

### UI suite counts (single run)

| Suite | Result |
|-------|--------|
| `e2e.mjs` | 43/43 |
| `autogod.mjs` | ALL PASS (C1 cases) |
| `dlc.mjs` | ALL PASS |
| `download.mjs` | 27/27 ALL PASS |
| `mods.mjs` | 39/39 ALL PASS |
| `builtin.mjs` | 9/9 |
| `catalog.mjs` | 37/37 |
| `xbox.mjs` | 52/52 |
| `saves.mjs` | all passed |
| `v160.mjs` | all passed |
| `v170.mjs` | all passed |
| `multidisc.mjs` | all passed |
| `content.mjs` | all passed |

Screenshots (production CSP + bridge): `/opt/cursor/artifacts/review-settings-csp.png`, `review-library-loaded.png`, `review-migration-single.png`.

---

## Wine vs Windows: two failing lib tests

Environment: `WINEPREFIX=/home/ubuntu/.wine-unpack-review-4b9d` (fresh), `WINEDEBUG=-all`, `UNPACK_7ZA` / `UNPACK_TESTDATA` as `Z:\…` paths, DLLs beside test exe per `HANDOFF.md`.

| Test | Linux native (4b9dca9) | Wine (4b9dca9) | Wine (7ee077d) | Wine (6a6b9b8) |
|------|------------------------|----------------|----------------|----------------|
| `covers::tests::write_atomic_concurrent_valid_images` | **pass** | **fail** (`PermissionDenied` at `covers.rs:516`) | *test absent* | **fail** |
| `god::tests::verify_package_detects_damage` | **pass** | **fail** (`DirectoryNotEmpty` at `god/tests.rs:613` `remove_dir_all`) | **fail** | **fail** |

### Conclusion (with evidence)

1. **`verify_package_detects_damage`** — Fails on **Wine at 7ee077d, 6a6b9b8, and 4b9dca9** with `Os { code: 145, … DirectoryNotEmpty }` during teardown after `set_len` truncation (`god/tests.rs:608-613`). **Passes on Linux** at 4b9dca9. **Not a new 1.7.4 regression**; consistent with Wine not releasing directory handles like Windows. **Classification: Wine-only artifact** (not proven Windows bug).

2. **`write_atomic_concurrent_valid_images`** — **New in 1.7.4** (not in `7ee077d`). Stresses concurrent `write_atomic` (`covers.rs:511-517`). **Passes on Linux**; under Wine, worker threads hit **`PermissionDenied`** on `write_atomic` (`covers.rs:516`). **Classification: Wine-only artifact** for concurrent rename/write; production cover writes are not 4×50 concurrent writers.

No file:line product fix required for Windows based on this evidence. Optional test-hardening: `#[cfg(not(target_os = "windows"))]` gate or `#[ignore]` on Wine CI only.

---

## Claimed 1.7.4 fixes — verification

| Claim | How verified | Result |
|-------|----------------|--------|
| Library types (GOD, disc, XBLA, Xbox, apps) + skip DLC/TU-only | `src-tauri/src/library.rs` unit tests (`justin_listing_tests`, `content_folders_in_line`); `content.mjs` TitleID panel | **OK** (logic + UI) |
| TU vs DLC badges (`000B0000` / `00000002`) | `xbox.mjs` install paths; `v170.mjs` settings copy | **OK** |
| Covers after toggle with library open; initials hide | `app.js` `patchLibraryCoverArt`, `gameTile` `hidden` on SVG; code review | **OK** in code; no dedicated e2e for toggle-after-open |
| Picked folders restore; one-time confirm; Grant access | `picked_folders::tests` (375 lib tests); `v160.mjs` migration notice; `app.js` `data-lib-grant` | **OK** (Rust + migration UI) |
| `paintJob` shows `job.error` | `app.js:650`; was `job.status` only at `7ee077d` | **OK** (fix present) |
| Settings under strict CSP | `csp-screens.mjs`, `no-inline-style.mjs`, screenshot | **OK** |
| Migration notices one at a time | `app.js` queue; `v160.mjs`; `csp-screens.mjs` (single `.migration-notice`) | **OK** |

---

## `app.js` regression scan (`7ee077d..4b9dca9`)

- **`downloadBusy` guard** (`app.js:2441-2479`): **new**; prevents overlapping mod downloads. **Not a regression** — closes a race where a second download could clobber `modsView.dl.job`.
- **`onNativeDrop` ordering** (`app.js:4617-4621`): moved **before** `refreshLibraryGrants`, migration notices, and `take_launch_requests`. **Intentional fix** so bridge/E2E `__TEST.drop` works immediately; **not a regression**.
- **`paintAllJobBars()`** on each `render()` (`app.js:4279`): fixes stale progress/status (e.g. Xbox send) after re-render — matches commit message on `4b9dca9`.

No additional behavioral regressions identified in `app.js` beyond documentation issues below.

---

## Documentation & version manifests

**Five manifests at 1.7.4:** `package.json`, `package-lock.json`, `src-tauri/Cargo.toml`, `src-tauri/Cargo.lock` (`unpack` package), `src-tauri/tauri.conf.json` — **all 1.7.4**.

**README** and **`.github/release-notes-1.7.4.md`** match implemented behavior (HTTPS XboxUnity, atomic covers, picked-folders, K1/K2, CSP) to spot-check against `covers.rs`, `picked_folders.rs`, `archive.rs`.

**CHANGELOG violation (panel rule):** At `7ee077d`, the 1.7.3 section ended with:

> The two Replace files edge cases from 1.7.0 are still open (see Known issues in the release notes).

At `4b9dca9`, that line was **replaced** with “fixed in 1.7.4”, and bullets were **reordered** (cover vs game-count order). The **1.7.3 section must remain unchanged** — see Finding 3.

---

## Findings

### 1. HIGH — Windows-gnu `clippy -D warnings` fails (CI parity)

- **File:line:** `src-tauri/src/testutil.rs:7-22`
- **Trigger:** `cargo clippy --all-targets --target x86_64-pc-windows-gnu -- -D warnings` on Linux (same as panel checklist).
- **Impact:** Cross-target clippy **does not pass**; `-D unreachable-code` on `std::env::temp_dir()` after `#[cfg(windows)]` block that always returns on Windows targets.
- **Suggested fix:** Split non-Windows temp path, e.g. `#[cfg(windows)] { … } #[cfg(not(windows))] { std::env::temp_dir() }`, or `#[allow(unreachable_code)]` on the fallback with a comment.

### 2. MEDIUM — Intermittent `e2e.mjs` flake

- **File:line:** `tests/e2e.mjs` (suite-level; failing case not captured in log)
- **Trigger:** Third consecutive `e2e.mjs` run in the panel batch reported **42/43**; **10** immediate reruns afterward were **43/43**.
- **Impact:** Rare CI noise; unknown which assertion flickered.
- **Suggested fix:** Capture failing check name on non-zero exit; consider `subscribe` retry or quarantine if it recurs in CI.

### 3. HIGH — `CHANGELOG.md` 1.7.3 section edited (must be frozen)

- **File:line:** `CHANGELOG.md:23-26` (vs `7ee077d:CHANGELOG.md` 1.7.3 tail)
- **Trigger:** Diff `7ee077d..4b9dca9`
- **Impact:** Violates release panel rule; misstates what 1.7.3 shipped vs what 1.7.4 fixed.
- **Suggested fix:** Restore the **exact** 1.7.3 block from `7ee077d`; keep K1/K2 closure only under `## 1.7.4`.

### 4. LOW — Wine lib suite: 2 failures (informational)

- **File:line:** `covers.rs:516`, `god/tests.rs:613`
- **Trigger:** Full lib tests under Wine (see table above).
- **Impact:** **None on Linux CI**; misleads if Wine is treated as Windows truth.
- **Suggested fix:** Document Wine as non-gating, or ignore those tests on Wine runners.

---

## What I could NOT test

- **Native Windows 10/11** desktop app (installer, WebView2, real folder picker, SmartScreen).
- **Signed release binaries** / portable zip layout from `release.yml`.
- **Real Xbox hardware** FTP/send paths (only bridge + `xbox.mjs`).
- **Library grid with a live multi-type fixture** in one headless pass (ad-hoc Puppeteer fixture did not grant/scan correctly; reliance on Rust `library` tests + green `content`/`xbox`/`v170` suites instead).
- **Second migration notice in queue** with two distinct notice files (bridge `__plant_migration_notice` overwrites one file; queue logic verified for single notice + code review only).

---

## Overall

**FAIL** — restore immutable **1.7.3** changelog text and fix **Windows-gnu clippy** on test helpers before treating the RC as fully green on the panel’s command list. Behavior/UI automation and Linux lib tests otherwise support **1.7.4** feature claims.
