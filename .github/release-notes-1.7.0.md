# Unpack 1.7.0

In 1.7.0 I focused on library management, queue control, clearer errors, and safer console workflows. Discord Rich Presence stays **off** by default.

## Highlights

- **Library:** search, sort, and filter; cached index with stale badge; optional local/opt-in cover art.
- **Extract queue:** pause/resume, reorder, move to front; **Retry failed** on batch summary.
- **DLC/TU:** separate auto-install toggles for DLC vs title updates; picker when several games share a TitleID.
- **Send to Xbox:** LAN discovery (opt-in), FTP lock status, remote rename/new folder/delete (with guards), destination presets, save backup over FTP.
- **GOD:** read-only verify mode; Explorer verbs for `.iso`; companion-disc auto-queue setting.
- **Settings:** USB prep wizard (never formats), export/import settings JSON, opt-in GitHub release check.
- **Content folders:** pick a console `Content\0000000000000000` (or `Content`, or the drive) as output and I put GOD games straight in as `<TitleID>\<type>\<MediaID>` + `.data`, with no `(GOD)` wrapper; header and `.data` always move together, and archives with ISOs extract next to the Content folder, not inside it. **Replace files** writes a note of which copy is old and which is new before it renames anything, never merges onto an existing folder, and runs one job at a time per type folder, enforced by an OS lock that also holds across Unpack windows, user sessions and PCs sharing the drive (ISO-to-Content conversion takes it too); the lock can't be deleted or swapped while held, and every move into Content is no-overwrite. A failed Replace takes back only what provably came from it, and recovery acts only on notes that are exactly mine; if it's interrupted, I repair the package from that note the next time (the repair is safe to interrupt too).
- **Save restore:** uploads first, then swaps; if a restore is interrupted, the next start (or next restore) undoes all of it and tells you, so a save is never left half old, half new. Recovery undoes only files that still hold exactly what was uploaded: a save you've played since is kept (you're told which files and where the originals are), and a restore that finished is never undone.
- **Mods:** mod files on a network share or mapped network drive are refused on purpose; copy them to a local folder first.
- **Errors:** plain-English hints and **Copy details** on the error banner.

## Known issues

I know about two rare edge cases in **Replace files** into a Content folder, and I plan to fix both in the next update:

- If Unpack is killed at the very last step of a Replace and you then edit the new `.data` files before the next run repairs the package, the repair can leave the old header with your edited `.data` files. I warn you when it happens.
- If you rename or move the game's type folder (e.g. `<TitleID>\00007000`) while a Replace is running, the package can end up split across two folders.

This build is **not code-signed**, so Windows SmartScreen may warn you. Check the file against `SHA256SUMS` before running it.

See `HANDOFF.md` for architecture notes and `README.md` for user-facing usage.
