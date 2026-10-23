#!/usr/bin/env python3
"""Build the small archive fixtures the Rust and UI suites read from UNPACK_TESTDATA / TESTDATA.

    python3 tests/make_testdata.py [/workspace/testdata]

Everything is generated with the bundled 7-Zip (or a system 7za) and the standard library, so a fresh
checkout needs no external files. The big (300 MiB) and ISO fixtures are built on demand by the suites.
Existing files are left alone; delete the folder to rebuild.
"""
import os, shutil, struct, subprocess, sys, zipfile

dest = os.path.abspath(sys.argv[1] if len(sys.argv) > 1 else os.environ.get("UNPACK_TESTDATA", "/workspace/testdata"))
sz = os.environ.get("UNPACK_7ZA") or next((p for p in ["/usr/lib/7zip/7za", "/usr/bin/7za", "/usr/bin/7zz", "/usr/bin/7z"] if os.path.exists(p)), None)
if not sz:
    sys.exit("7-Zip not found (set UNPACK_7ZA)")
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

here = os.path.dirname(os.path.abspath(__file__))
if not have("unknown.iso"):
    subprocess.run([sys.executable, os.path.join(here, "make_iso.py"), os.path.join(dest, "unknown.iso"), "FFFE0001", "0"], check=True)

print("test data ready in", dest)
