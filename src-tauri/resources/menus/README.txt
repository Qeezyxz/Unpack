Built-in menus folder (ships EMPTY).

Unpack reads menus.json from this folder at startup. It is a JSON array of entries:
  { "name": "Display name", "file": "relative/path/in/this/folder.xex",
    "titleId": "8 hex digits of a game in Unpack's offline list",
    "version": "1.0", "sha256": "optional 64 hex digits" }

Rules (entries that break any of them are skipped and logged):
- "file" must be a relative path inside this folder (no "..", no drive letters,
  no links/junctions) and must exist as a regular file.
- "titleId" must be a known Xbox 360 TitleID.
- If "sha256" is given, the file's SHA-256 must match.
- At most 500 entries; menus.json at most 1 MB.

Valid entries show in Mod Menus as read-only "Built-in" items and their names seed the
auto-detect list (your own learned names always win).
See HANDOFF.md / README.md, section "Adding built-in menus".
