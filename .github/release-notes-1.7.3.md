# Unpack 1.7.3

This is a fix-up release for 1.7.0.

## What I fixed

- **Settings layout:** on Windows, some Settings rows (Game library folders, Max download size, Auto-convert ISOs) squeezed their text down to about one word per line. I made Settings wider and put long rows' controls under their text, so everything reads normally.
- **FAT32 and big games:** 1.7.0 refused archives over 4 GB when extracting to a FAT32 drive, even when every file inside would fit. FAT32's limit is 4 GB *per file*, so now I only refuse a single file of 4 GB or more, and the message names that file instead of the archive.
- **Game ISOs over 4 GB onto FAT32:** with Auto ISO → GOD (or → Extracted folder) on, I extract the ISO to a temporary folder on your PC's drive, convert it straight onto the FAT32 drive (GOD games go into `Content\0000000000000000` as before, multi-disc games included), and delete the temporary copy afterwards, even if the convert fails or you cancel. Your PC's drive needs room for everything in the archive (not just the ISO) plus 256 MiB while that happens; if it doesn't have it, I tell you how much is needed. With Auto ISO off, I tell you which ISO is too big and to turn Auto ISO → GOD on.
- **No more leftovers on the drive:** when you extract to a `Content` folder, I skip extras the console doesn't need (readme `.txt`, `.nfo`, `.url`, pictures and the like) that sit beside the ISO, instead of leaving them in a folder at the drive root, and the summary lists what I skipped. I never touch anything inside a game or app folder (one with a `.xex` or any other real file); those are kept whole next to `Content`. I also delete the `.unpack-replace.lock` file from the game's type folder (e.g. `5454081A\00007000`) when the job finishes; a leftover one from 1.7.0 or a crash is cleaned up the next time I write there.
- **Library:** in 1.7.0, setting the library to a profile folder like `D:\Content\0000000000000000` could show "No games found yet", and `D:\Content` or a whole drive could spin forever on "Looking in your library folders…". I fixed both: the scan is bounded, an old empty index is no longer reused, Refresh always rescans, and the spinner shows progress, has a Cancel button and gives up after 10 minutes. The drive root, `Content`, a profile folder or a single TitleID folder all work.

## Known issues

I know about two rare edge cases in **Replace files** into a Content folder, and these are still open in 1.7.3 and I plan to fix both soon:

- If Unpack is killed at the very last step of a Replace and you then edit the new `.data` files before the next run repairs the package, the repair can leave the old header with your edited `.data` files. I warn you when it happens.
- If you rename or move the game's type folder (e.g. `<TitleID>\00007000`) while a Replace is running, the package can end up split across two folders.

This build is **not code-signed**, so Windows SmartScreen may warn you. Check the file against `SHA256SUMS` before running it.

See `HANDOFF.md` for architecture notes and `README.md` for user-facing usage.
