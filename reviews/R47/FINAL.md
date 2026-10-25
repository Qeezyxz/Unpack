# R47 — Final security + behavior round (read-only)

**Repo:** Qeezyxz/Unpack  
**Reviewed commit:** `e735d90` (`v1.7.4-dev`)  
**Baseline:** `dedd323` (Overseer-passed; 10 files, +~220 / −~40)  
**Prior reports:** `reviews/R42/*`, `reviews/R43/FINAL.md`, `reviews/R45/FINAL.md` on `review/r42`  
**Role:** Read-only attack review of pending-native-drop grant model; no push to `v1.7.4-dev`, no merge/PR/release.

## Verdict

**PASS** — `session_drop_prepare` no longer widens grants; session grants for drops are issued only in the trusted native `DragDrop` handler (`native_drop::register_native_drop`). The webview cannot obtain filesystem access to paths that were not part of a native drop batch (prepare only intersects requested paths with the pending dropped **and** granted sets). Refused hostile drops no longer call `addPaths`, so the refusal toast survives. Release `unpack` / `unpack_lib` contain no `__test_*` symbols; test helpers live only in the `bridge` example (`download-test-loopback`). Full requested test matrix is green on the review VM. Residual issues are **availability/UX** (pending batch consumed on empty or wholly foreign prepare) and **harness-only** test hooks—not grant bypass.

### Finding counts

| Severity | Count |
|----------|------:|
| CRITICAL | 0 |
| HIGH | 0 |
| MEDIUM | 2 |
| LOW | 4 |

---

## Threat model: can the webview get a grant without a native drop?

| Attack | Result |
|--------|--------|
| `session_drop_prepare` with arbitrary paths, no drop | **Blocked** — empty `allowed`, no `grant_session`; `list_archives_in_folder` denied (`tests/drop-grant.mjs`, `native_drop.rs` unit tests) |
| Prepare after real drop with **superset** (dropped + extra paths) | **Blocked** for extras — only dropped∩granted returned; `foreign` notify emitted, no grant in prepare |
| Prepare with **subset** of dropped paths | **Allowed** subset only (intended) |
| Prepare **twice** | Second call: batch already `take()`n → “No file or folder was dropped.”, no new grants |
| Prepare after **8s TTL** | Empty allowed + expiry message; grants not added in prepare |
| **Concurrent** prepare (two tabs / double invoke) | Mutex + single `take()` — one winner; other sees no batch (no extra grants) |
| Path variants (`..`, case, trailing `/`, symlinks) | Matching uses `drop_path_key` (`canonicalize` + `simplify` when possible). Mismatch → path not in `allowed` (false reject, not widen). Symlink drops: in `dropped`, **not** in `granted` (`is_link` skip) → not returned in `allowed` |
| `__test_simulate_native_drop` / `__test_grant` in production | **Absent** from `src-tauri/src/*` invoke surface; only `examples/bridge.rs` (feature-gated example, not shipped in app bundle) |

**Grant boundary:** `consume_session_drop_prepare` never calls `Grants::grant_session`. Grants are applied only inside `register_native_drop`, which is reachable from `lib.rs` `on_webview_event` (production) and `__test_simulate_native_drop` (CI bridge).

---

## Design change summary (`dedd323` → `e735d90`)

| Area | Change |
|------|--------|
| Refused drop UX | `native.js` filters through `session_drop_prepare`; empty `allowed` → `addPaths` not run → refusal toast not replaced by “no archives” (`e2e.mjs` assertion) |
| Pending drop state | `PendingNativeDrop` + 8s TTL; single consume per batch |
| E2E batch history | History expectation derived from **cancelled** job name, not replayed events (`test-shim.js` no longer replays stale events) |
| CI | `tests/drop-grant.mjs`; bridge `__test_drop_grant` → `__test_simulate_native_drop` |

---

## Rated findings

| Severity | File:line | Trigger | Impact | Fix |
|----------|-----------|---------|--------|-----|
| **MEDIUM** | `native_drop.rs:83-116` | `session_drop_prepare` with `paths: []` or only non-dropped paths after a real native drop | `take()` clears pending batch; UI `onDrop` may not run; native grants may already exist for valid paths (grant ≠ prepare filter) | Only `take()` when `requested` is non-empty **and** at least one key ∈ `batch.dropped`, or use peek + explicit ack; reject empty prepare with batch retained |
| **MEDIUM** | `native.js:154-157`, `lib.rs:1976-1985` | JS `invoke(session_drop_prepare)` completes **before** `register_native_drop` (ordering not covered by bridge shim, which registers before firing the fake drop event) | Drop appears to do nothing (`allowed` empty); if native handler runs afterward, session grant may still exist without queue UI | Document/require Tauri ordering; or register pending batch before emitting to JS; add integration test on real webview |
| **LOW** | `native_drop.rs:70-74` | Two native drops &lt;8s apart | Second drop overwrites `DropBatch` before first prepare | Queue batches or reject overwrite while pending |
| **LOW** | `lib.rs:128-130` | Second prepare after successful first | Emits `path-grant-refused` (“No file…”) — confusing toast | Suppress notify when batch already consumed; distinguish expired vs consumed |
| **LOW** | `bridge.rs:378-397`, `408-423` | Loopback CI | `__test_grant` / `__test_simulate_native_drop` widen test surface | Keep feature + loopback only (current); never bind bridge on LAN |
| **LOW** | `tests/*.mjs` (gap) | TTL expiry, symlink-only drop, `prepare([])` race | Regressions possible without CI signal | Unit tests for TTL + empty prepare; optional Tauri e2e |

---

## Release / test-helper surface

| Artifact | `__test_*` present? |
|----------|---------------------|
| `cargo build --release` → `unpack`, `unpack_lib` (strings scan) | **No** |
| `src-tauri/src/lib.rs` `invoke_handler` | **No** test commands |
| `cargo build --example bridge --features download-test-loopback` | `__test_grant`, `__test_simulate_native_drop` |
| `tests/test-shim.js` | TEST ONLY; not loaded by app |

`[[example]] bridge` requires `download-test-loopback`; default release app build does not compile the bridge.

---

## Behavior checks (drop-to-extract)

| Scenario | Result |
|----------|--------|
| Archive / folder extract via drop (bridge e2e) | **PASS** — open 7z, extract, batch queue, composer, ISO→GOD |
| Hostile folder drop (`/etc`) | **PASS** — refusal toast; not overwritten after refused drop (`e2e.mjs` ×3) |
| Grant widening without native drop | **PASS** — `drop-grant.mjs` |

---

## Test matrix (executed at `e735d90` on review VM)

| Command | Result |
|---------|--------|
| `UNPACK_7ZA=src-tauri/binaries/7za-x86_64-unknown-linux-gnu` + `python3 tests/make_testdata.py <tmp>` + `UNPACK_TESTDATA=<tmp>` + `cargo test --lib` | **391 passed**, 2 ignored |
| `cargo clippy --all-targets -- -D warnings` (Linux) | **PASS** |
| Same `--target x86_64-pc-windows-gnu` | **PASS** |
| Wine lib tests: fresh `WINEPREFIX`, `WebView2Loader.dll` + `7za.exe` beside `unpack_lib-*.exe`, `UNPACK_7ZA` as `Z:\…` | **373 passed**, 2 ignored (~278 s) |
| `npm test` | **PASS** |
| Bridge: `UNPACK_BRIDGE_PORT` unset; `cargo build --example bridge --features download-test-loopback`; `unpack-catalog-sign`; `pyftpdlib==2.2.0`; `UNPACK_7ZA` for bridge | **PASS** — `drop-grant`, `autogod`, `dlc`, `download`, `mods`, `builtin`, `catalog`, `saves`, `v160`, `v170`, `multidisc`, `content`, `importcheck.test.mjs` |
| `node tests/e2e.mjs` × 3 | **46/46** each |
| `node tests/xbox.mjs` × 2 | **52/52** each |

**Environment notes:** UI needed `p7zip-full` for `big.7z` generation and `UNPACK_7ZA` when starting the bridge. Linux `cargo test --lib` without `UNPACK_TESTDATA` / `UNPACK_7ZA` fails archive tests (expected).

---

## What was NOT tested

- **Native Windows / macOS** Tauri app drag-and-drop (ordering race, case-folding, 8.3 names, junctions)—only Linux bridge + Wine unit coverage.
- **`session_drop_prepare` TTL** and **`prepare([])`** clearing a live batch (code review only).
- **Real GUI** refusal for `C:\` / `C:\Windows` (e2e uses `/etc` on Linux bridge).
- **`cargo build --release --target x86_64-pc-windows-gnu`** full app bundle (only release Linux `unpack` scanned for `__test_*`).
- **`python3 tests/gamelists.py`** (CI `rust-and-theme` job).
- **GitHub-hosted** workflow runners (local matrix matched `.github/workflows/test.yml` UI job).

---

*R47 review complete. Product commit reviewed: `e735d90`; this report committed on `review/r42` only.*
