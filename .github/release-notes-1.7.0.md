# Unpack 1.7.0

Quality-of-life release focused on library management, queue control, clearer errors, and safer console workflows. Discord Rich Presence stays **off** by default.

## Highlights

- **Library:** search, sort, and filter; cached index with stale badge; optional local/opt-in cover art.
- **Extract queue:** pause/resume, reorder, move to front; **Retry failed** on batch summary.
- **DLC/TU:** separate auto-install toggles for DLC vs title updates; picker when several games share a TitleID.
- **Send to Xbox:** LAN discovery (opt-in), FTP lock status, remote rename/new folder/delete (with guards), destination presets, save backup over FTP.
- **GOD:** read-only verify mode; Explorer verbs for `.iso`; companion-disc auto-queue setting.
- **Settings:** USB prep wizard (never formats), export/import settings JSON, opt-in GitHub release check.
- **Errors:** plain-English hints and **Copy details** on the error banner.

See `HANDOFF.md` for architecture notes and `README.md` for user-facing usage.
