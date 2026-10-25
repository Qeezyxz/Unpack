# IPC path grants

Every Tauri `invoke` that reads or writes user filesystem paths must go through `safecopy::Grants`. The page cannot widen grants except via native pickers, native drag-and-drop (`native-drop-accepted`), extraction outputs, and other Rust-only grant paths.

| Command | Path-like args | Gate | Test |
|---------|----------------|------|------|
| `file_info` | `path` | `grants.check` | `ipc-gates.mjs`, Rust `archive_list_and_read_require_grants` |
| `list_archive` | `path` | `grants.check` (use canonical) | `archive-grant.mjs`, `ipc-gates.mjs` |
| `read_entry` | `path` | `grants.check` | `archive-grant.mjs`, `ipc-gates.mjs` |
| `read_file` | `path` | `grants.check` + archive name | `ipc-gates.mjs`, Rust `read_file_and_extract_entry_require_grants` |
| `extract_entry` | `path`, `out_file` | `check` + `check_or_parent` | `ipc-gates.mjs`, Rust `read_file_and_extract_entry_require_grants` |
| `extract_archive` | `path`, `dest` | `check` on archive; `dest` check or beside-parent rule | e2e extract suite |
| `create_archive` | `sources[]`, `out_file` | each source `check`; `check_or_parent` on output | e2e composer |
| `iso_info` | `path` | `grants.check` | `ipc-gates.mjs` |
| `iso_companions` | `path` | `grants.check` | e2e GOD |
| `iso_disc_status` | `path`, optional `dest`, lists | `check` / filter | e2e GOD |
| `iso_to_god` | `path`, `dest` | `check` both | e2e GOD |
| `iso_to_xex` | `path`, `dest` | `check` both | e2e |
| `god_verify_only` | `path` | `grants.check` | `ipc-gates.mjs` |
| `preflight_local` | `dest`, `paths[]` | `check` all | `ipc-gates.mjs` |
| `list_archives_in_folder` | `path` | `grants.check` | `drop-grant.mjs` |
| `dlc_install` | `path`, `libraries[]`, `game_path` | `check` / filter | `dlc.mjs` |
| `dlc_match_games` | `libraries[]` | filter `check` | `ipc-gates.mjs` |
| `library_scan` / `library_index` | `libraries[]` | `granted_library_roots` | `catalog.mjs` |
| `mod_copy` | `path`, `target` | `check` both | `mods.mjs` |
| `copy_to_usb` | `src`, `dest` | `check` | mods/USB tests |
| `download_url` | `dest_dir` | `check` or app Downloads grant | `download.mjs` |
| `download_folder` | (returns app dir) | grants app Downloads | `download.mjs` |
| `pick_path` / `pick_save` | (native) | `record_dialog_pick` / grant in Rust | e2e picks |
| `save_text_file` | `path` | `write_export_text` | export tests |
| `usb_prepare` | `root` | `grants.check` | manual |
| `saves_backup` / `saves_restore` | `dest` / `src` | `check` | `saves.mjs` |
| `screenshots_pull` | `dest` | `check` | `saves.mjs` |
| `record_user_folder` | `path` | scan-bound + dialog pick | `catalog.mjs` |
| `ftp_send` | `src` | `check` | `xbox.mjs` |
| `paths_are_granted` | `paths[]` | read-only probe | internal |
| `native-drop` | (native only) | `process_native_drop` → `native-drop-accepted` | `drop-grant.mjs` |
| `drop_listener_ready` | — | flushes queued drops | `drop-grant.mjs` |

No path args: `native_info`, `cancel_job`, `take_launch_requests`, `game_search`, `builtin_menus`, `migration_notices`, `error_help`, `discord_*`, `lan_discover`, `check_release`, `notify_done`, `set_taskbar_progress`, FTP remote paths, `saves_recover`, `screenshots_list`, token-based `god_extracted` / `xex_extracted`, `catalog` (URL), etc.

Bridge (`examples/bridge.rs`) mirrors the same gates for CI; `__test_grant` and `__test_simulate_native_drop` are loopback-only.
