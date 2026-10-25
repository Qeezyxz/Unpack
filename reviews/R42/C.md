# R42 Review C — Behavior, UI, and tests

**Reviewer:** C (behavior / UI / tests)  
**Repo:** Qeezyxz/Unpack  
**Range:** `2f3a9db` → `d8d5054` (branch `v1.7.4-dev`)  
**Date:** 2026-10-25  

## Summary

I ran the full requested test matrix on `d8d5054`. Rust lib tests, `npm test`, every `tests/*.mjs` suite (bridge env from `.github/workflows/test.yml`, `UNPACK_BRIDGE_PORT` unset), and **three** extra passes each of `e2e.mjs` and `xbox.mjs` all passed. UI behavior for folder access, Scan drives → Add, Settings layout under production CSP, and `createArchive` status clearing were spot-checked manually (bridge + Puppeteer).

**`cargo clippy --lib --all-targets -- -D warnings` fails** on both native Linux and `x86_64-pc-windows-gnu` because of a new `drop(layout)` in `god/tests.rs` introduced in this range. That alone is a **FAIL** against the stated gate.

## Test matrix (reproduced on review VM)

| Command | Result |
|--------|--------|
| `UNPACK_TESTDATA=/workspace/testdata cargo test --lib` (after fixtures) | **380 passed**, 2 ignored |
| `cargo clippy --lib --all-targets -- -D warnings` | **FAIL** — `clippy::drop_non_drop` at `god/tests.rs:617` |
| `cargo clippy … --target x86_64-pc-windows-gnu -- -D warnings` (with `gcc-mingw-w64-x86-64`) | **FAIL** — same |
| `npm test` | **PASS** |
| `tests/*.mjs` (bridge: `UNPACK_TEST_NOTICE_DIR=/tmp/unpack-v160-notice`, `UNPACK_TEST_ALLOW_LOOPBACK=1`, `UNPACK_TEST_DRIVES=/tmp/unpack-dlc-e2e/usb`; built bridge + `unpack-catalog-sign`; `pyftpdlib==2.2.0`) | **PASS** |
| `e2e.mjs` × 3 | **43/43** each |
| `xbox.mjs` × 3 | **52/52** each |
| Fixtures per instructions | See finding **M1** (relative `UNPACK_7ZA`); succeeded with **absolute** path |

Bridge build: `cargo build --example bridge --features download-test-loopback`, `cargo build --bin unpack-catalog-sign`.

## Manual / UI verification

| Claim | How verified | Outcome |
|-------|----------------|--------|
| System drive root refused in dialog | `POST /invoke/record_user_folder` with `{"path":"/"}` | Error: *"Unpack can't use the whole system drive…"* |
| Normal folder grant | `record_user_folder` on `/tmp/fake-downloads-test` | `null` (OK); `__test_grant` returns `true` |
| Scan drives → Add (not auto-grant) | `dlc.mjs` (scan lists root; libraries stay null until Add) | **PASS** in suite |
| Downloads / profile paths allowed (logic) | `picked_folders` unit tests (`users_downloads_allowed`, `bulk_add_refuses…`) | **PASS** in `cargo test` |
| Settings 320px path wrap | Production CSP via bridge; viewport 320px; `getComputedStyle(.name).whiteSpace === 'normal'`, wrapped text | **Works** — screenshots below |
| `createArchive` clears status before `pickSave` | Puppeteer with delayed save; status `''` immediately after second submit while prior line was `Created …` | **Works** — not covered by `e2e.mjs` |
| Version **1.7.4** | `package.json`, `src-tauri/Cargo.toml`, `tauri.conf.json`, `README.md` | **1.7.4** |
| CHANGELOG **1.7.3** entry untouched | `diff` of `## 1.7.3` section vs `2f3a9db` | **Identical** |
| Docs first person | `CHANGELOG` 1.7.4, `release-notes-1.7.4.md`, `README` | **First person** ("I fixed…") |

**CSP / Settings screenshots (production CSP, bridge serves `tauri.conf.json`):**

- Normal width: `/opt/cursor/artifacts/settings-1180-csp.png`
- 320px with long library path: `/opt/cursor/artifacts/settings-320-csp.png`

## Findings

### F1 — CRITICAL — Clippy `-D warnings` broken on test targets

- **File:line:** `src-tauri/src/god/tests.rs:617`
- **Trigger:** `cargo clippy --lib --all-targets -- -D warnings` (Linux or `x86_64-pc-windows-gnu`)
- **Impact:** Required review/CI gate fails; introduced in this diff (`drop(layout)` after refactoring `verify_package_detects_damage`).
- **Fix:** Remove `drop(layout);` (and likely `drop(src);` if also non-`Drop`) or bind `layout` in a narrower scope so it goes out of scope naturally; alternatively `#[allow(clippy::drop_non_drop)]` only if you truly need the drop ordering for Windows file locking (prefer scope tightening).

```text
error: call to `std::mem::drop` with a value that does not implement `Drop`
   --> src/god/tests.rs:617:5
    |
617 |     drop(layout);
```

### F2 — MEDIUM — Fixture instructions: relative `UNPACK_7ZA` breaks `make_testdata.py`

- **File:line:** `tests/make_testdata.py:31-32`
- **Trigger:** `UNPACK_7ZA=src-tauri/binaries/7za-x86_64-unknown-linux-gnu python3 tests/make_testdata.py <tmp>` (as in review brief)
- **Impact:** `FileNotFoundError` on first archive because `sevenzip()` runs with `cwd=<dest>/src` while `sz` is a path relative to the repo root.
- **Reproduced:** Subprocess `7za` invoke from `cwd=.../src` with relative `UNPACK_7ZA` → `FileNotFoundError`. Absolute path works.
- **Fix:** Resolve `sz` with `os.path.abspath(sz)` (or `Path(sz).resolve()`) before calling `subprocess.run`, or document that `UNPACK_7ZA` must be absolute.

### F3 — MEDIUM — `csp-screens.mjs` 320px assertion is mostly non-failing

- **File:line:** `tests/csp-screens.mjs:67-71`
- **Trigger:** Run `node tests/csp-screens.mjs` with CSS that **fails** to apply `@media (max-width: 320px)` wrap rules.
- **Impact:** Check passes if `el.scrollWidth > 0` even when `whiteSpace` stays `nowrap` (any visible text has `scrollWidth > 0`), so the test does not reliably guard the 320px wrap fix in `src/app.css:213-215`.
- **Fix:** Assert `getComputedStyle(el).whiteSpace === 'normal'` (and optionally `scrollHeight` > single line) without the `|| scrollWidth > 0` escape hatch; or compare against viewport width / `clientWidth` with long path string.

### F4 — MEDIUM — Cover cache rename retry lacks targeted test

- **File:line:** `src-tauri/src/covers.rs:51-65` (new/changed retry loop)
- **Trigger:** Concurrent or AV-locked rename on Windows (`PermissionDenied` / OS error 5).
- **Impact:** Regression risk; existing `write_atomic_concurrent_valid_images` does not simulate denied renames.
- **Fix:** Unit test that mocks or injects failing `fs::rename` (or temp dir + chmod / platform hook) to assert retries and success/failure paths.

### F5 — LOW — `createArchive` status race fix not asserted in automation

- **File:line:** `src/app.js:2944-2947`; `tests/e2e.mjs` create block (~163-178)
- **Trigger:** Second archive create while status still shows `Created …` from first run.
- **Impact:** Fix works manually (status cleared to `''` before delayed `pickSave`); e2e only waits for final `Created|Failed`, not intermediate status.
- **Fix:** In `e2e.mjs`, after first successful create, delay `pick_save` in shim and assert `.status-line` is empty (or not matching previous `Created`) immediately after submit.

### F6 — LOW — `test-shim.js` still advertises app version `1.7.3`

- **File:line:** `tests/test-shim.js:10`
- **Trigger:** Any test using default `getVersion` without overriding `window.__TEST.version`.
- **Impact:** Cosmetic / drift from `1.7.4`; `v170.mjs` overrides for update-check tests.
- **Fix:** Set `version: "1.7.4"` in shim default.

### F7 — LOW — Docs: profile folders named in CHANGELOG but not in release notes bullet

- **File:line:** `.github/release-notes-1.7.4.md:14` vs `CHANGELOG.md:12`
- **Trigger:** User reads release notes only.
- **Impact:** Release notes say "Downloads" but omit Documents / Desktop / Public explicitly listed in CHANGELOG/README.
- **Fix:** Align release-notes folder-access bullet with CHANGELOG wording.

## Test quality notes (no separate severity)

- **`importcheck.test.mjs`** — strengthened `isSystemPath` cases (`D:\Windows\System32`, `AppData`) are real assertions; good.
- **`picked_folders` / archive replace tests** — K1/K2 and folder rules have meaningful `assert!`s; not vacuous.
- **`csp-screens.mjs` queue bar checks (86-112)** — verify JS can set `element.style.width` (sanity under CSP), not that production queue bars work; acceptable smoke, not regression tests for layout.

## Version and docs checklist

| Location | Version / rule |
|----------|----------------|
| `package.json` / lock | 1.7.4 |
| `src-tauri/Cargo.toml` | 1.7.4 |
| `src-tauri/tauri.conf.json` | 1.7.4 |
| `README.md` | 1.7.4 |
| CHANGELOG 1.7.3 section | Unchanged vs `2f3a9db` |
| CHANGELOG / release notes voice | First person developer |

## Overall

**FAIL** — All runtime/UI suites requested passed (including flake reruns), and the R41 folder-access / Scan drives / Settings / CSP behaviors look correct in manual checks, but **`cargo clippy` with `-D warnings` does not pass** on this commit (F1). That blocks the stated quality gate until fixed.
