Unpack **1.4.0** is a safer Windows build of the original desktop UI.

## Downloads

- `Unpack_1.4.0_x64-setup.exe` — per-user installer. It does not need administrator rights. Windows SmartScreen may warn because the file is unsigned: choose **More info**, then **Run anyway**.
- `Unpack-portable.zip` — unzip anywhere and run `Unpack.exe`. Keep `WebView2Loader.dll` and `7za.exe` in the same folder.

WebView2 is already part of Windows 10 and 11.

## What’s new in 1.4.0

- Safer copies: USB, settings, DLC, downloads, and ISO converts finish the new file first, then swap it in. A crash should not wipe `launch.ini` or a good GOD package.
- Send to Xbox streams to a `.part` file, then renames it when complete. If Wi-Fi drops, Send again — Unpack reconnects and continues the half-sent file when the console allows it.
- Open zips inside zips (on by default, up to three layers). If an ISO is inside and Auto ISO is on, it can convert next.
- Run two jobs at once by default (extract + convert, or convert + Send). Change **How many jobs at once** in Settings (click the Unpack logo). 1–4.
- Disc 1 + Disc 2 still merge into one playable folder. Disc 1’s `default.xex` stays the one that launches.

## Also in 1.3.0 / 1.2.0 / 1.1.1

- Original dark UI and Unpack mark
- Discord Rich Presence (optional)
- Full Xbox 360 and original Xbox TitleID catalog (5,908 titles)

Use files you own. Back up `launch.ini` before you convert or copy anything. See `DISCLAIMER.txt`.
