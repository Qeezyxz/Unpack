# R40 consolidator — FINAL (v1.7.4-dev @ `4b9dca9`)

**Release candidate:** `4b9dca9` on `v1.7.4-dev`  
**Reviews merged:** A (`f3bcc65`), B (`f3bcc65`), C (`4b9dca9`)  
**Re-verification:** Checked out `4b9dca9`, read code, ran Linux lib tests (`375 passed`, `2 ignored`, with `UNPACK_TESTDATA`), reproduced A-1 with a temporary `repro_a_high_corrupt_data_after_step6_kill` test (not committed), built `x86_64-pc-windows-gnu` tests and ran `cargo clippy --all-targets --target x86_64-pc-windows-gnu -D warnings` (fails on `testutil.rs`). Wine not available on this VM; Wine conclusions lean on C’s matrix plus code review.

---

## Verdict: **FAIL**

Ship blockers remain: **Replace recovery can delete the last good GOD copy** after a post-crash corrupt/truncated live `.data` when both halves are already in `moved_live` (A-1, still open at `4b9dca9`). **CHANGELOG 1.7.3** was edited in violation of panel rules (C-3). **Windows-gnu clippy with `-D warnings`** still fails when cross-compiling tests from Linux (C-1). Security fixes in `ff28d51` (`library_scan` / `library_index` grants, atomic `picked-folders.json`, CSP, system-drive policy) address most of B’s FAIL items; they do not fix A-1.

---

## Finding ledger (merged)

| ID | Source | Severity | Status @ `4b9dca9` | Evidence |
|----|--------|----------|-------------------|----------|
| **A-1** | A #1 | **HIGH** | **STILL OPEN** | `all_new_confirmed` (`archive.rs:2323-2336`) returns `true` for `.data` when `move_recorded` without checking `fingerprint`. Repro at `4b9dca9`: kill at Replace step 6, overwrite `Data0000` with `CORRUPT`, `recover_asides_locked` → warning *"had finished moving the new copy in; I removed the old copy"*, live `NEW` + `CORRUPT`, no `.unpack-old.*`. `k1_edited_data_after_kill_never_mixes_package` passes (K1 path); `h1_partial_live_data_restores_old_aside` passes only when `.data` is **not** in `moved_live`. |
| **A-2** | A #2 | MEDIUM | **FIXED** (`ff28d51`) | `picked_folders.rs` `save()` uses temp + `sync_all` + `rename`; corrupt load calls `backup_corrupt` (`.json.bak`). `picked_folders::tests::picked_folders_save_is_atomic` + `corrupt_picked_folders_backed_up` pass. |
| **A-3** | A #3 | LOW | **STILL OPEN** (accepted) | `FAILURE_LOCK` (`covers.rs:32`) in-process only; `write_atomic` still atomic on disk. |
| **A-4** | A #4 | LOW | **STILL OPEN** | No `\\?\` extended-path helper in Replace/lock paths (`archive.rs`). |
| **A-5** | A #5 | LOW | **STILL OPEN** (test gap) | `k2_rename_during_replace_stops_and_rolls_back` remains `#[cfg(unix)]` (`archive/tests.rs:2710`). |
| **B-F1** | B F1 | HIGH | **RESOLVED BY DESIGN** (`ff28d51`) | Volume roots are **not** universally blocked: `is_system_drive_root` blocks only `/` and the **Windows system volume** (e.g. `C:\`); `E:\` USB roots may persist (`allow_auto_restore`, tests `windows_usb_root_may_restore_when_present`). `record_dialog_pick` persists only when `allow_auto_restore`; otherwise `grant_session` only. Matches CHANGELOG (“drive root `D:\` … work as a library folder”). **Not** the same as B’s “reject all bare roots”; **C:\** picker path is blocked. |
| **B-F2** | B F2 | MEDIUM | **STILL OPEN** | `restore_grants` / `record_dialog_pick` still `canonicalize` without refusing junctions/reparse points (`picked_folders.rs:78-138`, `safecopy.rs` grant). No new `is_link` gate in `picked_folders`. |
| **B-F3** | B F3 | MEDIUM | **FIXED** (`ff28d51`) | `library_scan` / `library_index` take `grants` and filter via `granted_library_roots` (`lib.rs:654-674`, `1505-1519`). |
| **B-F4** | B F4 | LOW | **STILL OPEN** | `paths_are_granted` unchanged (`lib.rs`). |
| **B-F5** | B F5 | MEDIUM | **FIXED** (`ff28d51`) | `tauri.conf.json` `style-src` is `'self'` only (no `'unsafe-inline'`). |
| **B-F6** | B F6 | LOW | **STILL OPEN** (pre-existing) | `read_file` / `file_info` / ungranted archive IPC unchanged. |
| **B-F7** | B F7 | LOW | **NOT RE-RUN** | `cargo audit` not installed on consolidator VM; B’s advisories not re-verified. |
| **C-1** | C #1 | HIGH | **STILL OPEN** | `cargo clippy --all-targets --target x86_64-pc-windows-gnu -- -D warnings` → `testutil.rs:22` **unreachable** `std::env::temp_dir()` when `cfg(windows)` is true for the Windows target (cross-compile from Linux). `4b9dca9` improved Wine temp paths but did not split `#[cfg(not(windows))]` fallback. |
| **C-2** | C #2 | MEDIUM | **STILL OPEN** (flake) | Not re-run ×3; C saw one `42/43` on `e2e.mjs`. |
| **C-3** | C #3 | HIGH | **STILL OPEN** | `CHANGELOG.md` line 26: 1.7.3 tail changed from *"still open"* to *"fixed in 1.7.4"*; 1.7.3 bullets reordered vs `7ee077d` (cover vs game-count order). |
| **C-4** | C #4 | LOW | **STILL OPEN** (Wine) | Wine not run here. Linux: `write_atomic_concurrent_valid_images` + `verify_package_detects_damage` **pass** with fixtures. |

---

## A-1 deep dive: K1 vs data loss

**What happens:** After both halves are live and recorded in `moved_live`, `all_new_confirmed` treats the `.data` half as confirmed without re-fingerprinting. `finish()` then deletes `.unpack-old.*` when the aside still matches its **old** fingerprint, even if live `.data` no longer matches the **new** fingerprint in the manifest.

**Cross-volume hardening (H1):** Truncated `.data` **before** `.data` is added to `moved_live` is handled (`h1_partial_live_data_restores_old_aside`). The gap is **after** `moved_live` records both halves—e.g. media corruption, manual damage, or a bad edit **before** recovery runs.

**K1 intent (CHANGELOG / `k1_edited_data_after_kill_never_mixes_package`):** If the user edits live `.data` after the new header is in place and `moved_live` says the half finished moving, keep header + edits; do not roll back to the old header beside edited data.

**Conflict:** Post-crash corruption is indistinguishable from K1 user edits using fingerprints alone—both yield `move_recorded` + live `.data` fingerprint ≠ manifest new fingerprint.

**Safest rule (recommended):** Keep K1’s `all_new_confirmed` shortcut for `.data` (do **not** require fingerprint there—that would undo K1 on mismatch). **Instead, in `finish()` before `remove_any` on old asides:** if any new `.data` half has `move_recorded(m, live)` and `fingerprint(live_path) != manifest_fp`, **do not delete** any old aside; warn that live `.data` changed after the move completed and the set-aside copy was left for manual check; still clear the manifest if the live header is in place (or keep manifest—prefer clearing with a strong warning to avoid endless re-finish attempts).

**Exact code change:**

- **File:** `src-tauri/src/archive.rs`
- **Location:** Inside the `finish` closure (~`2372-2393`), before the `for (_, a, _, fp) in &m.old` loop that calls `remove_any`.

```rust
let live_data_changed_after_move = m.new.iter().any(|(live, _, is_data, fp)| {
    *is_data
        && move_recorded(m, live)
        && fingerprint(&ct_dst.join(live)).as_deref() != Some(fp.as_str())
});
if live_data_changed_after_move {
    warnings.push(format!(
        "An earlier Replace of {unit} had both halves in place, but the live .data folder no longer matches what finished moving; I left the set-aside copy for you to check."
    ));
    st.next()?;
    fs::remove_file(mpath)?;
    return Ok(());
}
```

- **Add test:** `archive/tests.rs` — kill step 6, corrupt `Data0000`, recover → assert `.unpack-old.*` remains and `r27_check` can still see `OLD` in aside (or assert aside not removed).
- **Regression:** Re-run `k1_edited_data_after_kill_never_mixes_package` (may leave aside when user edits; acceptable vs data loss).

**Not recommended:** Requiring fingerprint in `all_new_confirmed` for `.data` when `move_recorded`—breaks K1.

---

## Wine-only failures vs real Windows

| Test | Linux @ `4b9dca9` | Wine (per C) | Hides Windows bug? |
|------|-------------------|--------------|-------------------|
| `covers::tests::write_atomic_concurrent_valid_images` | Pass | `PermissionDenied` on `write_atomic` | **Unlikely.** Production does not run 4×50 concurrent cover writes. Real Windows can fail `rename` over open files (`ERROR_SHARING_VIOLATION`); failures surface as errors, not silent corruption. Wine stress test is a poor Windows signal. |
| `god::tests::verify_package_detects_damage` | Pass | `DirectoryNotEmpty` on teardown `remove_dir_all` after `set_len` | **Unlikely** for product logic; **possible** on real Windows if handles stay open on truncated STFS files (teardown flake). Fails on Wine at `7ee077d` too—not a 1.7.4 regression. |

**Consolidator:** Treat as **non-gating Wine CI noise**; optional `#[cfg(not(target_os = "windows"))]` on the concurrent cover test or Wine-only `#[ignore]`. No product change required from Wine alone.

---

## Gaps not covered by A/B/C

1. **Non-system whole-volume grants (`D:\`, `E:\`):** Intentional for USB/console libraries after `ff28d51`, but wide blast radius if user picks a whole data drive—worth release-note emphasis (not a code bug).
2. **`dlc_match_games` / `god_verify_only`:** Still ungranted read surfaces (B catalogued; not retested).
3. **`finish()` never validates GOD header/data pairing** before deleting asides (A-1 is the critical instance).
4. **`r28_two_processes_are_serialized`:** Not run under Wine (A noted).
5. **Real hardware FAT32 / long paths / native WebView2 installer:** Out of scope for all reviewers.

---

## Required fixes (prioritized)

| P | File:line | Exact change |
|---|-----------|--------------|
| **P0** | `src-tauri/src/archive.rs` ~`2372` | Add `live_data_changed_after_move` guard in `finish()` as above; skip `remove_any` on old asides when live `.data` fingerprint ≠ manifest new fp after `move_recorded`; add regression test for step-6 kill + corrupt `.data`. |
| **P1** | `CHANGELOG.md` ~`23-26` | Restore **exact** 1.7.3 section from `7ee077d` (including *"still open"* line and original bullet order). Keep K1/K2 closure only under `## 1.7.4`. |
| **P1** | `src-tauri/src/testutil.rs` ~`7-22` | Fix cross-target clippy: e.g. `#[cfg(windows)] fn test_temp_base() -> PathBuf { ... }` **and** `#[cfg(not(windows))] fn test_temp_base() -> PathBuf { std::env::temp_dir() }` (no unreachable fallback after a Windows-only block that always returns when `target_os = "windows"`). |

---

## Optional (low) follow-ups

- **A-3:** Document single-instance expectation for `cover-failures.json` or add file lock.
- **A-4:** Audit Windows long-path (`\\?\`) for Replace and folder locks.
- **A-5:** Windows-gnu or in-process K2 rename integration test.
- **B-F2:** Refuse links/reparse points on `record_dialog_pick` / `restore_grants`; re-prompt if canonical target drifts.
- **B-F4 / B-F6:** Tighten `paths_are_granted` and optional grants on archive read IPC.
- **B-F7:** `cargo update` for `rustls-webpki` / `tokio` advisories.
- **C-2:** Harden `e2e.mjs` logging on failure.
- **C-4:** Wine CI policy / ignore list for the two lib tests.

---

## What was NOT tested (consolidator)

- Native **Windows 10/11** (WebView2, folder picker, SmartScreen, release NSIS/portable).
- **Wine** lib suite on this VM (`wine` not installed).
- **Real FAT32 USB** hardware; cross-volume Replace beyond unit tests / `fat_check`.
- **Long paths (>260)** on NTFS/FAT32.
- **cargo audit** (tool missing).
- Full **E2E flake** reproduction (`e2e.mjs` 42/43).
- **SIGKILL** of production `unpack.exe` (only `REPLACE_KILL_AT` injection).
- **Multi-process** lock / SMB / delete-pending timing.
- **Signed release** binary layout from `release.yml`.

---

## Post-`ff28d51` / `4b9dca9` delta (reviewer context)

| Landed | Effect on findings |
|--------|-------------------|
| `ff28d51` | Atomic `picked-folders.json` + `.bak`; `library_scan`/`library_index` grant checks; CSP without `unsafe-inline`; system-drive vs USB-root policy; `record_dialog_pick` session vs persist split. |
| `4b9dca9` | `paintJob` uses `job.error` (`app.js:650`); `xbox.mjs` flow; `testutil` Wine temp tweak (clippy cross-target still broken). |

---

*Consolidator run: 2026-10-25, branch `review/r40`, RC `4b9dca9`.*
