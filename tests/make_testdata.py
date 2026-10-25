#!/usr/bin/env python3
"""Build the small archive fixtures the Rust and UI suites read from UNPACK_TESTDATA / TESTDATA.

    python3 tests/make_testdata.py <dest-dir>

Everything is generated with the bundled 7-Zip (or a system 7za) and the standard library, so a fresh
checkout needs no external files. The big (300 MiB) and ISO fixtures are built on demand by the suites.
Existing files are left alone; delete the folder to rebuild.

If you omit the path, a fresh directory under the system temp folder is used (never the shared repo tree).
"""
import os, shutil, struct, subprocess, sys, tempfile, zipfile

if len(sys.argv) > 1:
    dest = os.path.abspath(sys.argv[1])
elif os.environ.get("UNPACK_TESTDATA"):
    dest = os.path.abspath(os.environ["UNPACK_TESTDATA"])
else:
    dest = tempfile.mkdtemp(prefix="unpack-testdata-")
    print(f"using temp testdata dir {dest}", file=sys.stderr)
sz = os.environ.get("UNPACK_7ZA") or next((p for p in ["/usr/lib/7zip/7za", "/usr/bin/7za", "/usr/bin/7zz", "/usr/bin/7z"] if os.path.exists(p)), None)
if not sz:
    sys.exit("7-Zip not found (set UNPACK_7ZA)")
sz = os.path.abspath(sz)
os.makedirs(dest, exist_ok=True)


def have(name):
    return os.path.exists(os.path.join(dest, name))


def sevenzip(*args, cwd):
    subprocess.run([sz, *args], cwd=cwd, check=True, stdout=subprocess.DEVNULL)


src = os.path.join(dest, "src")
if not os.path.isdir(src):
    os.makedirs(os.path.join(src, "nested/deep/er"))
    open(os.path.join(src, "a.txt"), "w").write("hello\n")
    open(os.path.join(src, "nested/n.json"), "w").write('{"k":1}\n')
    open(os.path.join(src, "nested/deep/er/d.txt"), "w").write("deep\n")
    open(os.path.join(src, "<img src=x onerror=alert(1)>.txt"), "w").write("html\n")

members = ["a.txt", "nested", "<img src=x onerror=alert(1)>.txt"]
# plain.zip / plain.7z / pw*.* hold the same four files; archives named X extract to a folder X.
for name, extra in [
    ("plain.zip", ["-tzip"]),
    ("plain.7z", ["-t7z"]),
    ("pw.7z", ["-t7z", "-psecret", "-mhe=on"]),
    ("pwnohe.7z", ["-t7z", "-psecret", "-mhe=off"]),
    ("pw.zip", ["-tzip", "-psecret", "-mem=AES256"]),
]:
    if not have(name):
        sevenzip("a", "-bso0", "-bsp0", *extra, os.path.join(dest, name), *members, cwd=src)

if not have("wrapped.zip"):
    with zipfile.ZipFile(os.path.join(dest, "wrapped.zip"), "w") as z:
        z.writestr("MyGame/readme.txt", "hi")

if not have("evil.zip"):
    with zipfile.ZipFile(os.path.join(dest, "evil.zip"), "w") as z:
        z.writestr("../../evil_escape.txt", "x")
        z.writestr("/abs_evil.txt", "x")
        z.writestr("..\\..\\win_evil.txt", "x")
        z.writestr("ok.txt", "ok")

if not have("notxbox.iso"):
    with open(os.path.join(dest, "notxbox.iso"), "wb") as f:
        f.write(b"\0" * (64 * 2048))

def rar4(path, files):
    """Tiny RAR 2.9 writer (stored files only): there is no RAR creator on Linux, and the in-page RAR engine needs a sample."""
    import zlib
    def block(btype, flags, body):
        hdr = struct.pack("<BHH", btype, flags, 7 + len(body)) + body
        return struct.pack("<H", zlib.crc32(hdr) & 0xFFFF) + hdr
    out = b"Rar!\x1a\x07\x00" + block(0x73, 0, b"\0" * 6)
    for name, data in files:
        n = name.encode()
        body = struct.pack("<IIBIIBBHI", len(data), len(data), 3, zlib.crc32(data), 0x5A000000, 29, 0x30, len(n), 0o100644 << 16) + n
        out += block(0x74, 0x8000, body) + data
    open(path, "wb").write(out + struct.pack("<H", 0x3DC4) + bytes([0x7B]) + struct.pack("<HH", 0x4000, 7))


if not have("sample.rar"):
    rar4(os.path.join(dest, "sample.rar"), [("a.txt", b"hello\n"), ("nested/n.json", b'{"k":1}\n')])

here = os.path.dirname(os.path.abspath(__file__))
if not have("unknown.iso"):
    subprocess.run([sys.executable, os.path.join(here, "make_iso.py"), os.path.join(dest, "unknown.iso"), "0ABCDEF1", "0"], check=True)

print("test data ready in", dest)
