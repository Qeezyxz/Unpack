Unpack **1.1.0** is the first source-complete release in this repository. It is a Windows desktop tool for archives, Xbox 360 ISO conversion, a game library, DLC and title updates, USB copy, and LAN FTP to a modded console.

## Downloads

- `Unpack_1.1.0_x64-setup.exe` — per-user installer. It does not need administrator rights. Windows SmartScreen may warn because the file is unsigned: choose **More info**, then **Run anyway**.
- `Unpack-portable.zip` — unzip anywhere and run `Unpack.exe`. The zip includes `WebView2Loader.dll` and `7za.exe` from the official 7-Zip 26.05 extra package (LGPL).

WebView2 is already part of Windows 10 and 11.

The older unpublished 1.0.0 installer is a different build. It is not this source tree.

## Fixes in 1.1.0

- Send to Xbox skips `launch.ini` unless you opt in, and a replacement is a rename rather than a delete-then-upload.
- Send to Xbox only accepts a local-network address.
- USB and Xbox copies keep the source folder name.
- Long jobs run off the UI thread.
- Downloads are limited to HTTP and HTTPS, with resume and an optional hash check.

Use files you own. Back up `launch.ini` before you convert or copy anything. See `DISCLAIMER.txt`.
