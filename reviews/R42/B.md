# R42 Reviewer B — Security & trust boundaries

**Repo:** Qeezyxz/Unpack  
**Reviewed commit:** `d8d5054398e75c7b71df15c971a9e2dfad1b8c6b` (`v1.7.4-dev`)  
**Baseline:** `2f3a9db`  
**Role:** Security / trust boundaries (read-only product review)  
**Reviewer:** B  

## Scope summary

This round tightens picked-folder persistence (`picked_folders.rs`), adds the `record_user_folder` Tauri IPC for “Scan drives → Add”, stops auto-granting on `dlc_scan_drives` in the test bridge, and aligns import-side path rules in `importcheck.js`. Canonicalization at pick/restore and removal of silent `grant_session` fallback for “bad” dialog picks are clear improvements.

The remaining issues are an **unbound IPC grant primitive** and **Rust `Users\…` deny rules that only apply on `C:`**, which diverge from `importcheck.js` and leave AppData / Default profiles reachable on other drive letters.

---

## Findings

### B-1 — `record_user_folder` is webview-controlled persistent grant (no scan/dialog binding)

| Field | Detail |
|--------|--------|
| **Severity** | **HIGH** |
| **Location** | `src-tauri/src/lib.rs:1104-1112`, `src/app.js:3544-3553` |
| **Trigger** | From the desktop webview (DevTools, XSS, or any code that can call Tauri `invoke`), run: `await window.__TAURI__.core.invoke("record_user_folder", { path: "D:\\Games" })` (any existing non-system directory the app would accept). No prior `dlc_scan_drives` result or native dialog is required. The Settings “Scan drives → Add” button uses the same IPC with `path: r` where `r` is only validated client-side as `foundRoots[i]` — nothing in Rust checks membership in the last scan. |
| **Impact** | **Persistent** filesystem grants (`grants.grant` + `picked-folders.json`) for arbitrary user-chosen paths that pass `record_dialog_pick`. This bypasses the trust boundary that `pick_path` enforces via a native dialog. Any webview compromise (or a user with DevTools) can widen access to entire library trees without the native consent flow Overseer expects for grant expansion. System folders are still blocked by canonical checks (see B-2), but the IPC is not tied to scan output or dialog attestation. |
| **Fix** | Remove `record_user_folder` from the Tauri handler surface, or replace it with a Rust-only flow: e.g. `add_scanned_root(index: usize)` that reads paths from `State<LastDriveScan>` populated only inside `dlc_scan_drives`, or a single command `dlc_scan_and_add_root` that scans and records in one native call. If a string path must remain, require an HMAC/nonce issued when returning scan results. Mirror the same restriction in `src-tauri/examples/bridge.rs:395-401` only if tests need it. |
| **Reproduction** | **Code path:** `record_user_folder` → `picked_store.record_dialog_pick` (`lib.rs:1111`). **Automated (same backend, no dialog):** `cd src-tauri && cargo test --lib dialog_picked_folder_is_restored -- --nocapture` — grants and restores a folder without any native picker. **Manual (shipping app):** DevTools invoke as above; then `paths_are_granted` / restart shows persisted access. |

---

### B-2 — `windows_users_blocked` only inspects `c:\users\` (Rust vs JS mismatch; AppData on other drives)

| Field | Detail |
|--------|--------|
| **Severity** | **HIGH** |
| **Location** | `src-tauri/src/picked_folders.rs:343-353` vs `src/importcheck.js:30-31` |
| **Trigger** | Call `is_system_path(r"D:\Users\me\AppData\Local")` (or `record_user_folder` / `record_dialog_pick` / `remember_approved` / `restore_grants` with that path after canonicalization on a machine where it exists). |
| **Impact** | **Persistent grant** to another user’s or relocated **AppData** tree on non-`C:` volumes, plus `Users\Default` and `Users\All Users` on those drives. JS import checks and `isUserPath` **do** block these (`importcheck.js:30-31`); Rust authoritative checks in the app **do not**, so settings import may drop a path that the native layer would still grant via scan/add, migration, or tampered `picked-folders.json` + later existence. |
| **Fix** | Change `windows_users_blocked` to match JS: after `normalize_path_for_check`, require `^[a-z]:\\users\\` (any drive), then apply the same Default / All Users / `...\AppData` rules. Add tests: `D:\Users\me\AppData\Local`, `D:\Users\Default`, `D:\Users\All Users` must be `is_system_path` and must fail `record_dialog_pick`. |
| **Reproduction** | ```bash
cd /workspace/src-tauri && cargo test --lib users_downloads_allowed_appdata_refused -- --nocapture
python3 - <<'PY'
# Mirrors picked_folders.rs:343-353 vs importcheck.js:30-31
def rust_users_blocked(s):
    s = s.replace("/","\\").lower()
    if not s.startswith("c:\\users\\"): return False
    rest = s[len("c:\\users\\"):]
    first = rest.split("\\")[0] if rest else ""
    if first in ("default", "all users"): return True
    return "\\appdata\\" in rest or rest.endswith("\\appdata")
import re
def js_blocks(s):
    s = s.replace("/","\\").lower()
    if re.match(r'^[a-z]:\\users\\(default|all users)(\\|$)', s): return True
    return bool(re.match(r'^[a-z]:\\users\\[^\\]+\\appdata(\\|$)', s))
p = r"D:\Users\me\AppData\Local"
print("rust blocks:", rust_users_blocked(p), "js blocks:", js_blocks(p))
assert js_blocks(p) and not rust_users_blocked(p), "mismatch reproduced"
PY
```
Temporary failing unit test (add to `picked_folders::tests`, run once): `assert!(is_system_path(r"D:\Users\me\AppData\Local"));` — **fails** on `d8d5054`. |

---

### B-3 — `record_dialog_pick` does not reject symlink/junction *before* canonicalize (mitigated post-canonicalize)

| Field | Detail |
|--------|--------|
| **Severity** | **LOW** (mitigated for system targets) |
| **Location** | `src-tauri/src/picked_folders.rs:78-106` |
| **Trigger** | Pick or `record_user_folder` a directory symlink/junction whose target is a forbidden system path (e.g. Unix symlink `link → /etc`). |
| **Impact** | Without `fs::canonicalize`, a symlink could hide intent; **after** canonicalize, `is_system_path` on the resolved path blocks `/etc` and similar. Restore from tampered JSON is also safe: `restore_skips_symlink_to_root` (`picked_folders.rs:537-550`). |
| **Fix** | Optional hardening: `symlink_metadata` + `safecopy::is_link` on the user-supplied path before canonicalize; refuse links explicitly (clearer UX, matches drag-drop handling in `bridge.rs:413-417`). |
| **Reproduction** | `cd src-tauri && cargo test --lib restore_skips_symlink_to_root -- --nocapture` (pass). Ephemeral test `record_dialog_pick(&link)` where `link → /etc` returns `Err` and does not grant (verified locally during review). |

---

### B-4 — Tampered `picked-folders.json` with forbidden paths

| Field | Detail |
|--------|--------|
| **Severity** | **LOW** (adequately mitigated on `d8d5054`) |
| **Location** | `src-tauri/src/picked_folders.rs:137-158` |
| **Trigger** | Edit `%AppData%/…/picked-folders.json` to include `"/etc"` or a symlink to `/`, restart app / call `restore_grants`. |
| **Impact** | **No grant** to `/etc/passwd`; list is rewritten to canonical safe entries only. Corrupt JSON is backed up and ignored (`corrupt_picked_folders_backed_up` test). |
| **Fix** | None required for canonical system paths; fix B-2 so non-`C:` AppData entries cannot persist. |
| **Reproduction** | `cd src-tauri && cargo test --lib restore_skips_symlink_to_root corrupt_picked_folders_backed_up -- --nocapture`. Manual: seed JSON with `["/etc"]`, run `restore_grants` — entry dropped (review-run ephemeral test on Linux). |

---

### B-5 — `__test_grant` / bridge-only hooks in release builds

| Field | Detail |
|--------|--------|
| **Severity** | **LOW** (release OK; test harness caveat) |
| **Location** | `src-tauri/examples/bridge.rs:374-393`, `src-tauri/Cargo.toml:60-62`, `src-tauri/src/picked_folders.rs:501-511` |
| **Trigger** | `grep __test_grant src-tauri/src/lib.rs` (empty). Run `cargo test --lib grant_paths_ipc_command_removed -- --nocapture`. Build release binary: no `bridge` example without `download-test-loopback`. |
| **Impact** | **Shipping Tauri app** does not expose `__test_grant`. The **bridge** HTTP harness (feature `download-test-loopback`) still exposes `__test_grant` and `record_user_folder` to any local client on the bridge port — acceptable for CI only. |
| **Fix** | Document that bridge must not be exposed beyond loopback CI; optionally gate `record_user_folder` in bridge behind the same scan-state check as production. |
| **Reproduction** | `cd src-tauri && cargo test --lib grant_paths_ipc_command_removed -- --nocapture && ! grep -q __test_grant src-tauri/src/lib.rs` |

---

### B-6 — Path tricks (`\\?\`, `..`, case, 8.3, trailing dot/space)

| Field | Detail |
|--------|--------|
| **Severity** | **LOW** (largely handled) |
| **Location** | `picked_folders.rs:300-316`, `78-81`; `importcheck.js:40-48` |
| **Trigger** | `node tests/importcheck.test.mjs` (M2 cases). Rust: `canonicalize` + `simplify` before `is_system_path`; `normalize_path_for_check` strips `\\?\` / `\\.\` for string rules. |
| **Impact** | Import layer refuses `..`, 8.3, trailing space/dot, UNC, device paths. Native pick uses canonical path for denial; `C:\Windows\..\Users\foo` resolves to a non-system path (intended). 8.3 on pick likely expands at canonicalize on Windows. |
| **Fix** | Keep JS/Rust rules in sync (B-2). Optional: segment-level checks on canonical path for parity with `isUserPath`. |
| **Reproduction** | `node tests/importcheck.test.mjs` (all M2 checks pass). |

---

### B-7 — `dlc_scan_drives` no longer auto-grants (positive)

| Field | Detail |
|--------|--------|
| **Severity** | N/A (improvement) |
| **Location** | `src-tauri/src/lib.rs:1066-1070`, `src-tauri/examples/bridge.rs:835-839` |
| **Trigger** | Compare `2f3a9db` bridge: scan called `GRANTS.grant` per root; at `d8d5054` grants only after `record_user_folder` / dialog pick. |
| **Impact** | Removes drive-scan grant widening in tests; production scan was already non-granting in Rust. |

---

## Rust vs JavaScript rule agreement

| Rule | `importcheck.js` | `picked_folders.rs` (`is_system_path`) |
|------|------------------|----------------------------------------|
| `Windows`, `Program Files`, etc. on any drive | Yes (`:25-29`) | Yes (`windows_system_folder_on_drive`) |
| `Users\Default`, `All Users` | Any drive (`:30`) | **Only `c:\users\`** (B-2) |
| `Users\*\AppData` | Any drive (`:31`) | **Only `c:\users\`** (B-2) |
| `\system32\`, `\syswow64\` | Yes (`:32`) | Yes (`:362-363`) |
| UNC / `\\?\` / `\\.\` | Yes (`:27-28`, `isUserPath`) | Yes (`raw_path_is_device_or_unc`, `normalize`) |
| `/etc`, Unix system roots | Yes (`:33-34`) | Yes (`regex_hostile`) |
| `..`, 8.3, trailing dot/space | `isUserPath` only (`:44-47`) | Not pre-canonicalize; canonicalize at pick/restore |

**Automated:** `node tests/importcheck.test.mjs` — pass on `d8d5054`.

---

## Tests run during review

| Command | Result |
|---------|--------|
| `cd src-tauri && cargo test --lib picked_folders -- --nocapture` | 12 passed |
| `cd src-tauri && cargo test --lib grant_paths_ipc_command_removed -- --nocapture` | passed |
| `cd src-tauri && cargo test --lib restore_skips_symlink_to_root -- --nocapture` | passed |
| `node tests/importcheck.test.mjs` | all passed |
| Ad-hoc `is_system_path("D:\\Users\\me\\AppData\\Local")` | **not blocked in Rust** (B-2) |

---

## Verdict

**FAIL**

`d8d5054` materially improves canonicalization, restore scrubbing, and scan/grant separation, but **ships a new unauthenticated IPC (`record_user_folder`) that can persist grants without native dialog or scan attestation**, and **Rust `Users\` deny rules are narrower than JS and only cover the `C:` profile volume**, leaving non-`C:` AppData/Default paths grantable through pick, scan-add, migration, and restore.

Address **B-1** and **B-2** before treating this boundary as closed for R42.
