#!/usr/bin/env python3
"""Pin the REA analysis targets for the Toram companion project.

Every native or metadata analysis has to name the exact binary it was taken
from. This manifest records the on-disk path, size, SHA-256 and the role each
target plays, and fails loudly when a target has moved or changed.

Usage
-----
    python scripts/rea_targets.py            # print the manifest
    python scripts/rea_targets.py --check    # verify digests against the pin file
    python scripts/rea_targets.py --pin      # (re)write the pin file
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import sys
from pathlib import Path

PROJECT = Path(__file__).resolve().parent.parent
PIN_FILE = PROJECT / "specs" / "verification" / "rea-targets.json"

GAME = Path(r"C:\Asobimo\AsobimoGames\Toram")

TARGETS = [
    {
        "id": "gameassembly",
        "path": GAME / "GameAssembly.dll",
        "role": "primary native target: every gameplay method lives here",
        "lane": "ghidra",
        "notes": ".text is encrypted in the shipped build; decompile a live dump instead",
    },
    {
        "id": "gameassembly-live",
        "path": Path(os.environ.get("TEMP", r"C:\Temp")) / "combat-decompile-2260" / "GameAssembly.live.dll",
        "role": "decrypted live dump of the same module",
        "lane": "ghidra",
        "notes": "preferred analysis input when the shipped .text is still encrypted",
    },
    {
        "id": "toramonline-exe",
        "path": GAME / "ToramOnline.exe",
        "role": "Unity player: process/module inventory surface",
        "lane": "pe",
        "notes": "use the PE provider for headers, resources and imports",
    },
    {
        "id": "unityplayer",
        "path": GAME / "UnityPlayer.dll",
        "role": "engine runtime: memory checks and the asset loader live here",
        "lane": "ghidra",
        "notes": "candidate host for the memory-response check",
    },
    {
        "id": "baselib",
        "path": GAME / "baselib.dll",
        "role": "CRT/utility module shipped with the player",
        "lane": "pe",
        "notes": "reference module for the module-list comparison",
    },
    {
        "id": "global-metadata",
        "path": GAME / "ToramOnline_Data" / "il2cpp_data" / "Metadata" / "global-metadata.dat",
        "role": "IL2CPP metadata for the CURRENT build: string literals, method table, RVAs",
        "lane": "metadata",
        "notes": "the shipped dump/dump.cs predates this build; prefer this file",
    },
    {
        "id": "firebase-plugin",
        "path": GAME / "ToramOnline_Data" / "Plugins" / "x86_64" / "FirebaseCppApp-13_8_0.dll",
        "role": "third-party plugin: reference point for what a normal plugin looks like",
        "lane": "pe",
        "notes": "control sample for module-shape comparison",
    },
    {
        "id": "apk-dex",
        "path": PROJECT / "base" / "classes.dex",
        "role": "Asobimo Android shell: JADX lane for the SDK-side code",
        "lane": "android",
        "notes": "classes2.dex and classes3.dex sit beside it",
    },
    {
        "id": "our-core",
        "path": PROJECT / "src" / "wcore.dll",
        "role": "our in-process core: every symbol a scanner could see",
        "lane": "pe",
        "notes": "footprint audit: exports, section names, embedded strings",
    },
    {
        "id": "our-loader",
        "path": PROJECT / "src" / "wboot.exe",
        "role": "our loader: process name, window, command line",
        "lane": "pe",
        "notes": "the process list entry a game-side app scan would see",
    },
    {
        "id": "our-companion",
        "path": PROJECT / "companion.exe",
        "role": "our companion UI: window title and class are visible to any app scan",
        "lane": "pe",
        "notes": "first candidate for an app-visibility disconnect",
    },
    {
        "id": "our-observer",
        "path": PROJECT / "observer.exe",
        "role": "our observer: a second visible process for the same purpose",
        "lane": "pe",
        "notes": "same exposure as the companion",
    },
]


def digest(path: Path) -> dict:
    if not path.is_file():
        return {"path": str(path), "present": False}
    h = hashlib.sha256()
    size = 0
    with path.open("rb") as fh:
        while True:
            chunk = fh.read(1 << 20)
            if not chunk:
                break
            size += len(chunk)
            h.update(chunk)
    return {
        "path": str(path),
        "present": True,
        "bytes": size,
        "sha256": h.hexdigest(),
    }


def build() -> dict:
    targets = []
    for t in TARGETS:
        entry = {
            "id": t["id"],
            "role": t["role"],
            "lane": t["lane"],
            "notes": t["notes"],
        }
        entry.update(digest(t["path"]))
        targets.append(entry)
    return {
        "schema": "rea-targets/1",
        "project": "toram_work",
        "targets": targets,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    group = parser.add_mutually_exclusive_group()
    group.add_argument("--check", action="store_true", help="compare against the pin file")
    group.add_argument("--pin", action="store_true", help="rewrite the pin file")
    args = parser.parse_args()

    manifest = build()

    if args.pin:
        PIN_FILE.parent.mkdir(parents=True, exist_ok=True)
        PIN_FILE.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
        print(f"pinned {len(manifest['targets'])} targets -> {PIN_FILE}")
        return 0

    if args.check:
        if not PIN_FILE.is_file():
            print(f"pin file missing: {PIN_FILE}", file=sys.stderr)
            return 1
        pinned = json.loads(PIN_FILE.read_text(encoding="utf-8"))
        by_id = {t["id"]: t for t in pinned["targets"]}
        failures = 0
        for t in manifest["targets"]:
            old = by_id.get(t["id"])
            if old is None:
                print(f"  NEW     {t['id']}")
                continue
            if old.get("sha256") != t.get("sha256"):
                print(f"  CHANGED {t['id']}: {old.get('sha256')} -> {t.get('sha256')}")
                failures += 1
            elif not t.get("present"):
                print(f"  MISSING {t['id']}: {t['path']}")
                failures += 1
        for t in manifest["targets"]:
            state = "ok" if t.get("present") else "MISSING"
            print(f"  {state:8} {t['id']:20} {t.get('sha256', '-')[:16]}  {t['path']}")
        return 1 if failures else 0

    print(json.dumps(manifest, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
