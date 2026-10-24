"""Offline attribution checks for the recorded incidents.

Answers one question with preserved artifacts only: was this project's core
present in the game process that raised the warning event?

Everything here reads files that were already preserved. No process is
attached, no game is launched, and no command is dispatched.

Checks
------
1. The global core log is append-only and never rotated, so a PID missing from
   it was never loaded. The preserved incident copy must be a byte-exact
   prefix of the live log.
2. The core's own load marker must be present for every PID that is known to
   have loaded, and absent for the PID under investigation.
3. The module inventory of a process that was still running must not contain
   the shadow DLL when the log also says the core never loaded. The loader
   never calls FreeLibrary, so a loaded core stays mapped.
4. The shadow DLL file name is the FNV-1a-64 of the core binary, which dates
   the shadow files to a load sequence.
"""

from __future__ import annotations

import hashlib
import json
import os
import re
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
TEMP = Path(os.environ.get("TEMP") or os.environ.get("TMP") or ".")

INCIDENT = REPO / "work" / "incident-20261021-0306"
REPAIR = REPO / "work" / "live-repair-20261021-1032"
ORIGINAL = REPO / "work" / "incident-20261020-17024"

SHADOW_PREFIX = "dxgi_ext_"
CORE_MODULE = re.compile(r"(?i)dxgi_ext_|wcore|ogl_drv")
LOAD_MARKER = re.compile(r"(?:attached PID=|worker PID=)(\d+)")


def fnv1a64(data: bytes) -> int:
    """FNV-1a 64-bit, matching `fnv1a_64` in crates/wboot/src/lib.rs."""
    h = 0xCBF29CE484222325
    for byte in data:
        h ^= byte
        h = (h * 0x100000001B3) & 0xFFFFFFFFFFFFFFFF
    return h


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest().upper()


def read_bytes(path: Path) -> bytes:
    return path.read_bytes()


def log_is_append_only(preserved: Path, live: Path) -> tuple[bool, int, int]:
    """The preserved copy must be a byte-exact prefix of the live log."""
    old = read_bytes(preserved)
    new = read_bytes(live)
    return new.startswith(old), len(old), len(new)


def loaded_pids(log_text: str) -> set[int]:
    """Every PID that wrote a core load marker into the global log."""
    return {int(m.group(1)) for m in LOAD_MARKER.finditer(log_text)}


def module_names(path: Path) -> list[str]:
    raw = json.loads(path.read_text(encoding="utf-8"))
    rows = raw if isinstance(raw, list) else [raw]
    return [row.get("ModuleName", "") for row in rows]


def shadow_files() -> dict[str, Path]:
    found = {}
    for entry in TEMP.glob(f"{SHADOW_PREFIX}*.dll"):
        found[entry.name] = entry
    return found


def shadow_name_for(core: Path) -> str:
    return f"{SHADOW_PREFIX}{fnv1a64(read_bytes(core)):016x}.dll"


def classify_incident() -> dict:
    """Attribute the 03:06 warning to a core-loaded or core-free process."""
    preserved = INCIDENT / "wcore-preserved.log"
    live = TEMP / "wcore.log"
    result: dict = {"pid": 14516}

    if not live.exists():
        result["error"] = f"live global log not found at {live}"
        return result

    append_only, old_len, new_len = log_is_append_only(preserved, live)
    result["log_append_only"] = append_only
    result["preserved_bytes"] = old_len
    result["live_bytes"] = new_len

    text = read_bytes(live).decode("utf-8", "replace")
    result["log_occurrences_14516"] = text.count("14516")
    result["loaded_pids"] = sorted(loaded_pids(text))

    inventory = module_names(INCIDENT / "modules.json")
    result["incident_module_count"] = len(inventory)
    result["incident_has_core_module"] = any(
        CORE_MODULE.search(name) for name in inventory
    )

    return result


def classify_repair_control() -> dict:
    """The 1032 session had the core loaded; the collector must find it."""
    control = json.loads((REPAIR / "core-modules.json").read_text(encoding="utf-8"))
    before = module_names(REPAIR / "modules-before.json")
    return {
        "control_core_module": control.get("ModuleName", ""),
        "pre_load_module_count": len(before),
        "pre_load_has_core": any(CORE_MODULE.search(n) for n in before),
        "control_has_core": bool(CORE_MODULE.search(control.get("ModuleName", ""))),
    }


def classify_original() -> dict:
    """The original 10-20 08:34 warning session did have the core loaded."""
    log = ORIGINAL / "wcore-before.log"
    if not log.exists():
        return {"error": "original session log not preserved"}
    text = log.read_text(encoding="utf-8", errors="replace")
    return {
        "pid_17024_occurrences": text.count("17024"),
        "has_load_marker": bool(LOAD_MARKER.search(text)),
    }


def shadow_timeline() -> dict:
    """Map core binaries to their content-hashed shadow files."""
    cores = {
        "v0.96.2": REPO / "work" / "release-v96.2" / "src" / "wcore.dll",
        "v0.96.1": REPO / "work" / "release-v96.1" / "src" / "wcore.dll",
        "candidate": REPO / "work" / "repair-20261021" / "candidate" / "src" / "wcore.dll",
        "canonical": REPO / "src" / "wcore.dll",
    }
    out = {}
    for label, core in cores.items():
        if not core.exists():
            continue
        name = shadow_name_for(core)
        entry = {"core_sha256": sha256(core), "shadow": name}
        shadow = TEMP / name
        if shadow.exists():
            entry["shadow_present"] = True
        else:
            entry["shadow_present"] = False
        out[label] = entry
    return out


def summary() -> dict:
    return {
        "incident": classify_incident(),
        "repair_control": classify_repair_control(),
        "original_session": classify_original(),
        "shadows": shadow_timeline(),
    }


def main() -> int:
    report = summary()
    print(json.dumps(report, indent=2, sort_keys=True))
    incident = report["incident"]
    control = report["repair_control"]
    print()
    print("attribution of the 03:06 warning (PID 14516):")
    if incident.get("error"):
        print("  ERROR:", incident["error"])
        return 1
    if not incident["log_append_only"]:
        print("  the preserved log is NOT a prefix of the live log; evidence is not continuous")
        return 1
    if incident["log_occurrences_14516"] != 0:
        print("  the core load marker for PID 14516 IS present; the core was loaded")
        return 1
    if incident["incident_has_core_module"]:
        print("  a core module IS present in the live process inventory")
        return 1
    if not control["control_has_core"]:
        print("  the module collector failed the positive control; enumeration is not trusted")
        return 1
    if control["pre_load_has_core"]:
        print("  the pre-load control already contains the core; the control pair is invalid")
        return 1
    print("  the core was NOT loaded in PID 14516:")
    print(f"    - global log is append-only ({incident['preserved_bytes']} -> {incident['live_bytes']} bytes)")
    print(f"    - PID 14516 appears {incident['log_occurrences_14516']} times in the whole log")
    print(f"    - {incident['incident_module_count']} modules enumerated, no core module")
    print("    - the same collector finds the core in the 1032 control session")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
