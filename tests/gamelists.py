#!/usr/bin/env python3
"""The TitleID lists and their merge script: regenerating titles.jsonl / mod.rs from the two CSVs
must reproduce the committed files exactly (nothing missing, nothing stale), and a TitleID that is
only in a CSV must be appended while an existing name always wins."""
import json, pathlib, shutil, subprocess, sys, tempfile

SRC = pathlib.Path(__file__).resolve().parent.parent / "src-tauri/vendor/iso2god/src/game_list"
fails = 0


def check(name, ok, info=""):
    global fails
    fails += 0 if ok else 1
    print(("PASS " if ok else "FAIL ") + name + (" - " + info if info else ""))


def run(d):
    return subprocess.run([sys.executable, "merge_gamelists.py"], cwd=d, capture_output=True, text=True)


with tempfile.TemporaryDirectory() as t:
    t = pathlib.Path(t)
    for f in SRC.iterdir():
        if f.is_file():
            shutil.copy(f, t / f.name)
    r = run(t)
    check("merge script runs", r.returncode == 0, r.stderr.strip())
    for f in ("titles.jsonl", "mod.rs"):
        check(f"regenerated {f} equals the committed file", (t / f).read_bytes() == (SRC / f).read_bytes())
    check("script reports nothing new to add", "added: 0" in r.stdout, r.stdout.strip())

    # a new TitleID in the CSV is appended; an existing one keeps its name
    csv = t / "gamelist_xbox360.csv"
    text = csv.read_text(encoding="utf-8-sig")
    existing = next(json.loads(l) for l in (t / "titles.jsonl").read_text().splitlines() if l.strip())
    tid_key, name_key = "TitleID", "Name"
    old_tid, old_name = existing[tid_key], existing[name_key]
    csv.write_text(text.rstrip("\r\n") + f"\r\nFFFE0001\t00000001\tZZ Test Game\r\n{old_tid}\t00000002\tRenamed In Csv\r\n", encoding="utf-8")
    r = run(t)
    jl = [json.loads(l) for l in (t / "titles.jsonl").read_text().splitlines() if l.strip()]
    by = {str(x[tid_key]).upper(): x[name_key] for x in jl}
    check("a new CSV-only TitleID is added", any("ZZ Test Game" == v for v in by.values()), r.stdout.strip() + r.stderr.strip())
    check("an existing name wins over the CSV", by.get(str(old_tid).upper()) == old_name, f"{old_tid}: {by.get(str(old_tid).upper())!r}")
    check("the generated mod.rs lists the new title", "ZZ Test Game" in (t / "mod.rs").read_text())

sys.exit(1 if fails else 0)
