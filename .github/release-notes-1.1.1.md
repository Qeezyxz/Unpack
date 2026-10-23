Unpack **1.1.1** is a Windows desktop tool for archives, Xbox 360 ISO conversion, a game library, DLC and title updates, USB copy, and LAN FTP to a modded console.

## Downloads

- `Unpack_1.1.1_x64-setup.exe` — per-user installer. It does not need administrator rights. Windows SmartScreen may warn because the file is unsigned: choose **More info**, then **Run anyway**.
- `Unpack-portable.zip` — unzip anywhere and run `Unpack.exe`. Keep `WebView2Loader.dll` and `7za.exe` in the same folder.

WebView2 is already part of Windows 10 and 11.

## What’s new in 1.1.1

- TitleID catalog includes every unique ID from the Xbox 360 and original Xbox game lists (**5,908** titles, up from 4,905). Conversion names games from that list, or uses the TitleID instead of a shared `(unknown)` folder.
- **Disc 1 and Disc 2** convert into one playable game: extracted XEX merges into one folder (disc 1 keeps `default.xex`); GOD keeps both discs under the same TitleID so Aurora can swap media.
- Convert recognizes Redump names like `Game (Disc 1).iso` and will look next to a selected ISO for the matching disc.

## Also in 1.1.0

- Send to Xbox skips `launch.ini` unless you opt in; replacements use rename, not delete-then-upload.
- Send to Xbox is local-network only.
- USB and Xbox copies keep the source folder name.

Use files you own. Back up `launch.ini` before you convert or copy anything. See `DISCLAIMER.txt`.
