#!/usr/bin/env python3
"""Merge Xbox 360 / original Xbox TitleID CSVs into titles.jsonl and mod.rs.

Existing names win. New TitleIDs from the CSVs are appended so ISO conversion
and the in-app library can label every listed disc.
"""

from __future__ import annotations

import json
from pathlib import Path

ROOT = Path(__file__).resolve().parent
JSONL = ROOT / "titles.jsonl"
MOD_RS = ROOT / "mod.rs"


def parse_csv(path: Path) -> dict[str, str]:
    rows = path.read_text(encoding="utf-8-sig").splitlines()
    header = rows[0].split("\t")
    i_id = header.index("title_id")
    i_name = header.index("title_name")
    out: dict[str, str] = {}
    for row in rows[1:]:
        if not row.strip():
            continue
        parts = row.split("\t")
        if len(parts) <= max(i_id, i_name):
            continue
        tid = parts[i_id].strip().upper()
        name = parts[i_name].strip()
        if len(tid) != 8 or not name:
            continue
        try:
            int(tid, 16)
        except ValueError:
            continue
        out.setdefault(tid, name)
    return out


def rust_string(name: str) -> str:
    return json.dumps(name, ensure_ascii=False)


def main() -> None:
    records: list[dict] = []
    by_id: dict[str, dict] = {}
    for line in JSONL.read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        rec = json.loads(line)
        tid = str(rec.get("TitleID", "")).upper()
        rec["TitleID"] = tid
        records.append(rec)
        by_id[tid] = rec

    added: list[tuple[str, str, str]] = []
    for csv_name, title_type in (
        ("gamelist_xbox360.csv", "360"),
        ("gamelist_xbox.csv", "Xbox1"),
    ):
        for tid, name in parse_csv(ROOT / csv_name).items():
            if tid in by_id:
                continue
            rec = {
                "TitleID": tid,
                "HBTitleID": "00000000",
                "Name": name,
                "LinkEnabled": "0",
                "TitleType": title_type,
                "Covers": "0",
                "Updates": "0",
                "MediaIDCount": "0",
                "UserCount": "0",
                "NewestContent": "2026-10-23",
            }
            records.append(rec)
            by_id[tid] = rec
            added.append((tid, title_type, name))

    records.sort(key=lambda rec: int(rec["TitleID"], 16))
    with JSONL.open("w", encoding="utf-8", newline="\n") as fh:
        for rec in records:
            fh.write(json.dumps(rec, ensure_ascii=False, separators=(",", ":")) + "\n")

    lines = [
        "// DO NOT EDIT; this file is auto-generated.",
        "// Run `python3 merge_gamelists.py` (or `./generate.bash titles.jsonl mod.rs`) to re-generate.",
        "",
        "",
        "pub fn find_title_by_id(title_id: u32) -> Option<String> {",
        "    GAMES_BY_TITLE_ID",
        "        .binary_search_by_key(&title_id, |x| x.0)",
        "        .ok()",
        "        .map(|i| GAMES_BY_TITLE_ID[i].1.to_owned())",
        "}",
        "",
        "#[rustfmt::skip]",
        "const GAMES_BY_TITLE_ID: &[(u32, &'static str)] = &[",
    ]
    for rec in records:
        lines.append(f'    (0x{rec["TitleID"]}, {rust_string(rec["Name"])}),')
    lines.append("];")
    lines.append("")
    MOD_RS.write_text("\n".join(lines), encoding="utf-8")

    print(f"titles: {len(records)}")
    print(f"added: {len(added)}")
    for tid, title_type, name in added[:12]:
        print(f"  {tid} [{title_type}] {name}")
    if len(added) > 12:
        print(f"  … {len(added) - 12} more")


if __name__ == "__main__":
    main()
