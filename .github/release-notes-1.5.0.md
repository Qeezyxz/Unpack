Unpack **1.5.0** makes errors easier to see and stops a few conversions that used to go wrong quietly.

## Downloads

- `Unpack_1.5.0_x64-setup.exe` — per-user installer. It does not need administrator rights. Windows SmartScreen may warn because the file is unsigned: choose **More info**, then **Run anyway**.
- `Unpack-portable.zip` — unzip anywhere and run `Unpack.exe`. Keep `WebView2Loader.dll` and `7za.exe` in the same folder.

WebView2 is already part of Windows 10 and 11.

## What’s new in 1.5.0

- **Errors are easy to spot.** A red **Error** banner sits above dialogs, with **Copy** and **Dismiss**. Errors from Send to Xbox and downloads no longer hide behind the dialog.
- **Unknown Title ID stops the convert.** It says `Title ID XXXXXXXX not recognized. Conversion stopped.` and writes no folder. The hex ID is still shown when you inspect an ISO.
- **Full Xbox drive.** Send to Xbox stops with “The Xbox drive is full” instead of retrying; if retries make no progress it stops too. The `.part` file is kept so the next send resumes.
- **Lone Disc 2.** “Convert Disc 1 first, or pick both discs together.”
- **Two jobs, one folder.** Jobs that write the same folder wait for each other; different folders still run side by side.
- **Inner zips.** With “Open zips inside zips” off, the status says how many inner archives were left closed.
- **FAT32.** A failed extract to a FAT32 folder explains the 4 GB limit.

## Also in 1.4.0 and earlier

- Safer copies, nested archives, jobs-at-once setting, Send to Xbox resume
- Original dark UI, Discord Rich Presence (optional), full Xbox 360 and original Xbox TitleID catalog

Use files you own. Back up `launch.ini` before you convert or copy anything. See `DISCLAIMER.txt`.
