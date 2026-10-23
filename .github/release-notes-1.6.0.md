Unpack **1.6.0** merges the owner's later additions into the independently reviewed source (the 14-round review base), so the extra features keep every safety check of that base.

## Downloads

- `Unpack_1.6.0_x64-setup.exe` — per-user installer. It does not need administrator rights. Windows SmartScreen may warn because the file is unsigned: choose **More info**, then **Run anyway**.
- `Unpack-portable.zip` — unzip anywhere and run `Unpack.exe`. Keep `WebView2Loader.dll` and `7za.exe` in the same folder.

WebView2 is already part of Windows 10 and 11.

## What’s new in 1.6.0

- **Discord Rich Presence** (optional, on by default, switch in Settings → Discord). Shows the screen you are on and how many jobs run, never file names. Local IPC only.
- **Open zips inside zips** (Settings → Extraction, on by default) with a summary of how many were opened and how many were left closed.
- **Jobs at once** (Settings → Extraction, 1–4, default 2). Jobs that write the same folder wait for each other.
- **Multi-disc games.** Disc 1 and Disc 2 end up in one playable folder, in any order; Unpack offers other discs found next to an ISO.
- **Unknown Title IDs convert with a warning** (the hex ID is shown) instead of stopping. A lone Disc 2 with no `default.xex` gets a plain message.
- **Full Xbox drive vs. a Wi-Fi drop** are told apart when sending to the console; the `.part` file is kept so the next send resumes.
- **Red error banner** with Copy and Dismiss that stays above dialogs.
- **FAT32** hint when a big file cannot be written.
- Full Xbox 360 and original Xbox TitleID lists (5,908 titles).

Use files you own. Back up `launch.ini` before you convert or copy anything. See `DISCLAIMER.txt`.
