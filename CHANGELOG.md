# Changelog

## 1.7.3

- I fixed the Settings window on Windows. Some rows (Game library folders, Max download size, Auto-convert ISOs) squeezed their text to about one word per line. Settings is wider now, and long rows put the control under the text.
- I fixed extracting to FAT32 drives. 1.7.0 refused any archive over 4 GB, even though FAT32's limit is per file. Now I only refuse a single file of 4 GB or more, and I name that file. A game ISO over 4 GB with Auto ISO → GOD (or XEX) on is extracted to a temporary folder on your PC's drive, converted straight onto the FAT32 drive, and the temporary copy is deleted afterwards, even if the convert fails or you cancel. Your PC's drive needs room for everything in the archive (not just the ISO) plus 256 MiB while that happens. Other game files in the archive land next to the `Content` folder, the same as a normal extract.
- When you extract to a `Content` folder, I now skip extras that aren't needed on the console (readme `.txt`, `.nfo`, `.url`, pictures and the like) that sit beside the ISO, instead of leaving them in a folder at the drive root, and the summary lists what I skipped. I never touch anything inside a game or app folder (one with a `.xex` or any other real file), so its pictures, text files and web UI stay.
- I fixed `.unpack-replace.lock` being left in the game's type folder (for example `5454081A\00007000`) after a GOD convert or Replace on Windows. I now delete it when the job is done, and a leftover one from an older version or a crash is cleaned up the next time I write to that folder.
- I fixed the Library. In 1.7.0, pointing it at a profile folder like `D:\Content\0000000000000000` could show "No games found yet" (an old empty index was reused for up to an hour, and Refresh didn't rescan), and pointing it at `D:\Content` or a whole drive could spin forever. The scan is now bounded (it no longer walks every file inside each game's `.data` folders or every folder on a drive), Refresh always rescans, and the spinner shows progress with a Cancel button and gives up after 10 minutes. The drive root (`D:\`), `D:\Content`, any profile folder and a single TitleID folder all work as a library folder.
- The two Replace files edge cases from 1.7.0 are still open (see Known issues in the release notes).

## 1.7.0

See `.github/release-notes-1.7.0.md`.
