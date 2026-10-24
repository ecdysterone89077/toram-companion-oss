"""Audit the Faunas port's hook targets against the installed IL2CPP metadata.

Faunas' port resolves every mechanic by class and method name at runtime, so
a feature that names the wrong class installs nothing and rolls back. That
failure is only visible in game, which is a bad place to find it: the port
shipped 20 of 21 features with correct targets and one (`element`) pointed at a
class that does not declare the method at all.

This tool closes that gap offline. It reads the port's hook specifications
straight out of `crates/wcore/src/faunas.rs` and checks every one of them
against an exported metadata snapshot, then reports:

  * a target class that does not exist in the snapshot
  * a method the target class does not declare
  * an argument count the target class does not declare
  * a (class, method) pair claimed by more than one feature, which the hook
    engine refuses because a MethodInfo has exactly one owner

Usage:
    python scripts/faunas_target_audit.py
    python scripts/faunas_target_audit.py --metadata <path> --source <path>

The metadata snapshot is produced by `scripts/export-metadata-symbols.ps1`.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import struct
import sys
from collections import defaultdict
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_SOURCE = REPO_ROOT / "crates" / "wcore" / "src" / "faunas.rs"
DEFAULT_LEGACY_SOURCE = REPO_ROOT / "crates" / "wcore" / "src" / "features.rs"
DEFAULT_METADATA = (REPO_ROOT / "work" / "blind-spot-audit-20261021"
                    / "metadata-symbols.json")


def current_build_dump() -> Path | None:
    """The downloaded metadata dump of the installed build.

    This file was supplied as a reference rather than produced by the game.
    Its download origin is recorded and the publisher and the tool that made
    it are not established, so it is treated as a declaration and address
    reference and nothing more. It is a fixed artifact: it does not refresh
    when the game updates, so it is only used while it still corresponds to
    the installed module.
    """
    candidates = sorted(REPO_ROOT.glob("PC_v*_dump.cs"),
                        key=lambda path: path.stat().st_mtime, reverse=True)
    return candidates[0] if candidates else None


DUMP_VERIFICATION = (REPO_ROOT / "work" / "dump-verification-20261022"
                      / "verification.json")
INSTALLED_MODULE = Path(r"C:\Asobimo\AsobimoGames\Toram\GameAssembly.dll")


def mapped_size(path: Path) -> int | None:
    """The size of a module as it is mapped, from its PE header.

    The file on disk and the mapped image are different sizes: the mapped
    image carries the uninitialized sections. The verification record pins
    the mapped size, so that is the number that has to be compared.
    """
    try:
        with path.open("rb") as handle:
            header = handle.read(0x400)
    except OSError:
        return None
    if len(header) < 0x40 or header[:2] != b"MZ":
        return None
    pe = struct.unpack_from("<I", header, 0x3C)[0]
    if header[pe:pe + 4] != b"PE\0\0":
        return None
    optional = pe + 24
    if optional + 60 > len(header):
        return None
    return struct.unpack_from("<I", header, optional + 56)[0]


def dump_correspondence(dump: Path) -> tuple[bool, str]:
    """Whether a downloaded dump still describes the installed module.

    The dump is a fixed artifact and the game is updated on its own
    schedule, so a dump that matched the module yesterday may not match it
    today. The verification record pins the dump's digest and the mapped
    size of the module it was checked against; both are re-checked here.
    """
    if not DUMP_VERIFICATION.exists():
        return False, "no verification record at %s" % DUMP_VERIFICATION
    try:
        record = json.loads(DUMP_VERIFICATION.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        return False, "verification record unreadable: %s" % error
    expected = str(record.get("sha256", "")).lower()
    if expected:
        actual = hashlib.sha256(dump.read_bytes()).hexdigest()
        if expected != actual:
            return False, "the dump's digest does not match the verification record"
    module = record.get("module") or {}
    size = module.get("size")
    if size and INSTALLED_MODULE.exists():
        actual_size = mapped_size(INSTALLED_MODULE)
        if actual_size is None:
            return False, "the installed module's PE header could not be read"
        if actual_size != size:
            return False, ("the installed module maps to %d bytes, the dump was "
                           "verified against %d" % (actual_size, size))
    return True, "verified against the installed module"

# The ported feature table, in the order that fixes the command band. The
# order is a wire contract: entries are only ever appended.
FEATURES: list[tuple[int, str]] = [
    (0, "crit"), (1, "motion"), (2, "stable"), (3, "cspd"), (4, "range"),
    (5, "penet"), (6, "resist"), (7, "mob_no_flee"), (8, "mob_no_guard"),
    (9, "mob_no_invincible"), (10, "mob_no_move"), (11, "skill_aoe"),
    (12, "skill_no_aoe"), (13, "skill_burst"), (14, "prorate"), (15, "element"),
    (16, "critdmg"), (17, "critmgcdmg"), (18, "skip_battle"),
    (19, "no_domination"), (20, "no_drop_visual"), (22, "mp_cost"),
    (23, "skill_delay"),
]

# Features that own no hook of their own.
VALUELESS_FEATURES = {21: "one_click"}


class Metadata:
    """The declaration inventory of one metadata snapshot."""

    def __init__(self, payload: dict):
        self.version = payload.get("version")
        self.counts = payload.get("counts", {})
        self.declared_by: dict[str, list[tuple[str, int]]] = defaultdict(list)
        self.type_methods: dict[str, set[str]] = defaultdict(set)
        for entry in payload.get("types", []):
            name = entry.get("name", "")
            for method in entry.get("methods", []):
                self.declared_by[method.get("name", "")].append((name, method.get("argc", 0)))
                self.type_methods[name].add(method.get("name", ""))

    def has_type(self, name: str) -> bool:
        return name in self.type_methods

    def declares(self, method: str, cls: str) -> list[int]:
        return sorted({argc for owner, argc in self.declared_by.get(method, [])
                       if owner == cls})

    def owners(self, method: str) -> list[str]:
        return sorted({owner for owner, _ in self.declared_by.get(method, [])})

    @classmethod
    def load(cls, path: Path) -> "Metadata":
        with path.open(encoding="utf-8") as handle:
            return cls(json.load(handle))

    @classmethod
    def load_dump(cls, path: Path) -> "Metadata":
        """Read the downloaded metadata dump of the installed build.

        The dump lists each class and its methods with the address of each
        method. It is a declaration and address reference for the build it
        was verified against, so it is the better source while it still
        corresponds; `dump_correspondence` is what decides that.
        """
        class_re = re.compile(r"^\s*class\s+([A-Za-z_][\w.<>`]*)\s*:\s*(.*)$")
        method_re = re.compile(
            r"^\s*([^;{}]*?)\s+([A-Za-z_][\w]*)\s*\(([^)]*)\)\s*;\s*//\s*0x([0-9a-fA-F]+)\s*$")
        classes: dict[str, list[tuple[str, int]]] = defaultdict(list)
        current: str | None = None
        with path.open(encoding="utf-8", errors="replace") as handle:
            for line in handle:
                match = class_re.match(line)
                if match:
                    current = match.group(1)
                    continue
                if current is None:
                    continue
                match = method_re.match(line)
                if match:
                    argc = 0 if not match.group(3).strip() else len(
                        [part for part in match.group(3).split(",") if part.strip()])
                    classes[current].append((match.group(2), argc))
        payload = {"version": None, "counts": {},
                   "types": [{"name": name,
                              "methods": [{"name": method, "argc": argc}
                                          for method, argc in methods]}
                             for name, methods in classes.items()]}
        return cls(payload)


class Spec:
    """One hook specification: where it points and what it replaces."""

    __slots__ = ("feature", "feature_id", "cls", "method", "argc", "detour")

    def __init__(self, feature: str, feature_id: int, cls: str, method: str,
                 argc: int, detour: str = ""):
        self.feature = feature
        self.feature_id = feature_id
        self.cls = cls
        self.method = method
        self.argc = argc
        self.detour = detour

    def __repr__(self) -> str:  # pragma: no cover - diagnostics only
        return "Spec(%s, %s.%s, argc=%d)" % (self.feature, self.cls, self.method, self.argc)

    @property
    def key(self) -> tuple[str, str, int]:
        return (self.cls, self.method, self.argc)


def _rust_string_list(source: str, const_name: str) -> list[str]:
    """Read a `const NAME: [&str; N] = [ "a", "b" ];` list."""
    match = re.search(r"const\s+%s\s*:\s*\[&str;\s*\d+\]\s*=\s*\[(.*?)\];" % const_name,
                      source, re.S)
    if not match:
        return []
    return re.findall(r'"([^"]+)"', match.group(1))


def _rust_inline_list(source: str, anchor: str) -> list[str]:
    """Read the `for class in [ "a", "b" ]` list that follows an anchor."""
    start = source.find(anchor)
    if start < 0:
        return []
    tail = source[start:start + 400]
    match = re.search(r"\[(.*?)\]", tail, re.S)
    if not match:
        return []
    return re.findall(r'"([^"]+)"', match.group(1))


def parse_specs(source: str) -> list[Spec]:
    """Read every hook specification out of the port's source."""
    # feature index -> the spec builder it uses
    builder = {}
    start = source.find("fn specs_for")
    if start < 0:
        return []
    window = source[start:start + 6000]
    for match in re.finditer(r"(\d+)\s*=>\s*(\w+)\s*\(", window):
        builder[int(match.group(1))] = match.group(2)

    # literal HookSpec blocks, grouped by the function that holds them
    blocks: dict[str, list[tuple[str, str, int]]] = defaultdict(list)
    positions = [(m.start(), m.group(1)) for m in
                 re.finditer(r"fn\s+(\w+_specs)\s*\(", source)]
    for index, (pos, name) in enumerate(positions):
        end = positions[index + 1][0] if index + 1 < len(positions) else len(source)
        for block in re.finditer(r"HookSpec\s*\{(.*?)\}", source[pos:end], re.S):
            body = block.group(1)
            named = re.search(r'HookTargetClass::Named\(\s*"([^"]+)"\s*\)', body)
            if named:
                cls = named.group(1)
            else:
                target = re.search(r"HookTargetClass::(\w+)", body)
                if not target:
                    continue
                cls = target.group(1)
            method = re.search(r'method:\s*"([^"]+)"', body)
            argc = re.search(r"argc:\s*(\d+)", body)
            detour = re.search(r"detour:\s*([^\n]+)", body)
            if not method or not argc:
                continue
            blocks[name].append((cls, method.group(1), int(argc.group(1)),
                                 _detour_text(detour)))

    specs: list[Spec] = []
    for feature_id, feature in FEATURES:
        fn = builder.get(feature_id)
        if fn is None:
            continue
        items = list(blocks.get(fn, []))
        if fn == "skill_aoe_specs":
            items = [(c, "CheckRangeHit", 3, "hk_true") for c in
                     _rust_string_list(source, "SKILL_AOE_CLASSES")]
        elif fn == "skill_no_aoe_specs":
            items = [(c, "CheckRangeHit", 3, "hk_false") for c in
                     _rust_string_list(source, "SKILL_NO_AOE_CLASSES")]
        elif fn == "skill_burst_specs":
            items = [(c, "NextRangeHit", 0, "hk_skip") for c in
                     _rust_string_list(source, "SKILL_BURST_CLASSES")]
        elif fn == "prorate_specs":
            classes = _rust_inline_list(source, "fn prorate_specs")
            methods = _rust_inline_list(source, "fn prorate_specs")
            # the class list comes first, the method list second
            tail = source[source.find("fn prorate_specs"):][:400]
            lists = re.findall(r"\[(.*?)\]", tail, re.S)
            classes = re.findall(r'"([^"]+)"', lists[0]) if lists else []
            methods = re.findall(r'"([^"]+)"', lists[1]) if len(lists) > 1 else []
            items = [(c, m, 0, "hk_const") for c in classes for m in methods]
        for cls, method, argc, detour in items:
            specs.append(Spec(feature, feature_id, cls, method, argc, detour))
    return specs


def _detour_text(match) -> str:
    """The detour expression, without the cast or the trailing comma.

    A detour can carry a comma inside a template argument, so it is read to
    the end of the line rather than to the next comma.
    """
    if not match:
        return ""
    text = match.group(1).strip().rstrip(",").strip()
    return re.sub(r"\s+as usize$", "", text)


def _forced_value(detour: str, slot_values: dict[str, int]) -> int | None:
    """The value a detour forces, when the source states one.

    Two features that claim the same method only conflict when they force
    different values. A detour that returns a constant, or that forces a
    known slot, states its value; anything else is unknown.
    """
    name = detour.strip()
    port = re.fullmatch(r"port::(\w+)::<(\d+)>", name)
    if port:
        if port.group(1) in ('cast_rate', 'mp_cost'):
            return None  # Native-dependent positive-rate cap, not a constant.
        slot = int(port.group(2))
        # Stable callback identities from the typed Windows port. Value ladders
        # remain unknown here because they can change while enabled.
        return {0: 1, 1: 1, 66: 1, 2: 60, 3: 60, 4: 1000, 5: 1000,
                6: 0, 7: 0, 8: 1, 9: 0, 10: 0, 13: 65535, 14: 65535,
                15: 0, 16: 0, 18: 0, 60: 1, 64: 1, 65: 1, 63: 0, 67: 62,
                68: 1000, 69: 1000}.get(slot)
    if name.startswith("local_stat::<"):
        match = re.search(r"local_stat::<\s*\d+\s*,\s*(-?\d+)\s*>", name)
        return int(match.group(1)) if match else None
    if name.startswith("hk_const::<"):
        match = re.search(r"hk_const::<\s*(\w+)\s*>", name)
        if match:
            return slot_values.get(match.group(1))
        return None
    if name in ("hk_true", "hk_true_0arg", "hk_true_2arg", "hk_true_3arg"):
        return 1
    if name in ("hk_false", "hk_false_0arg", "hk_false_2arg"):
        return 0
    if name.startswith("local_motion::<"):
        # The companion's motion detour forces 60 while instant-attack is on,
        # the same value the port's motion feature forces (MOTION_VALUE).
        return 60
    return None


def _slot_values(source: str) -> dict[str, int]:
    """The constant each forced-value slot starts at.

    The slots are declared in order and the forced-value table is
    initialised in the same order, so the two lists pair up.
    """
    constants: dict[str, int] = {}
    for match in re.finditer(r"pub const (\w+): i32 = (-?\d+);", source):
        constants[match.group(1)] = int(match.group(2))
    order = [match.group(1) for match in
             re.finditer(r"const (SLOT_\w+): usize = (\d+);", source)]
    forced = re.search(r"static FORCED[^=]*=\s*\[(.*?)\];", source, re.S)
    initialisers: list[str] = []
    if forced:
        for entry in forced.group(1).split(","):
            match = re.search(r"AtomicI32::new\(([^)]*)\)", entry)
            if match:
                initialisers.append(match.group(1).strip())
    values: dict[str, int] = {}
    for slot, entry in zip(order, initialisers):
        if entry in constants:
            values[slot] = constants[entry]
        elif re.fullmatch(r"-?\d+", entry):
            values[slot] = int(entry)
    return values


def parse_legacy_specs(source: str) -> list[tuple[str, str, str, int, str]]:
    """Read every hook specification out of the rest of the core.

    The ported features are not the only things that hook methods: the
    companion's own feature tables claim methods too, and a MethodInfo has
    exactly one owner. The two sets are read apart so a collision between
    them can be reported.
    """
    specs: list[tuple[str, str, str, int, str]] = []
    for block in re.finditer(r"HookSpec\s*\{(.*?)\}", source, re.S):
        body = block.group(1)
        named = re.search(r'HookTargetClass::Named\(\s*"([^"]+)"\s*\)', body)
        target = re.search(r"HookTargetClass::(\w+)", body)
        method = re.search(r'method:\s*"([^"]+)"', body)
        argc = re.search(r"argc:\s*(\d+)", body)
        detour = re.search(r"detour:\s*([^\n]+)", body)
        if not method or not argc:
            continue
        if named:
            cls = named.group(1)
        elif target:
            cls = target.group(1)
        else:
            continue
        specs.append((_enclosing_function(source, block.start()), cls,
                      method.group(1), int(argc.group(1)),
                      _detour_text(detour)))
    return specs


def _enclosing_function(source: str, offset: int) -> str:
    """The name of the function a specification sits in."""
    matches = list(re.finditer(r"\bfn\s+(\w+)", source[:offset]))
    return matches[-1].group(1) if matches else "<unknown>"


def audit(source: str, metadata: Metadata, legacy_source: str | None = None) -> dict:
    """Check every specification and every ownership claim."""
    specs = parse_specs(source)
    problems: list[dict] = []
    owners: dict[tuple[str, str, int], list[str]] = defaultdict(list)

    for spec in specs:
        owners[spec.key].append(spec.feature)
        if not metadata.has_type(spec.cls):
            problems.append(dict(kind="class_missing", feature=spec.feature,
                                 feature_id=spec.feature_id, cls=spec.cls,
                                 method=spec.method, argc=spec.argc,
                                 detail="the snapshot has no type %s" % spec.cls))
            continue
        if spec.method not in metadata.type_methods[spec.cls]:
            elsewhere = metadata.owners(spec.method)
            problems.append(dict(kind="method_not_on_class", feature=spec.feature,
                                 feature_id=spec.feature_id, cls=spec.cls,
                                 method=spec.method, argc=spec.argc,
                                 detail="declared on: %s" % ", ".join(elsewhere) or "nowhere"))
            continue
        argcs = metadata.declares(spec.method, spec.cls)
        if spec.argc not in argcs:
            problems.append(dict(kind="argc_mismatch", feature=spec.feature,
                                 feature_id=spec.feature_id, cls=spec.cls,
                                 method=spec.method, argc=spec.argc,
                                 detail="%s declares argc %s" % (spec.cls, argcs)))

    conflicts: list[dict] = []
    for key, claimants in sorted(owners.items()):
        if len(claimants) > 1:
            conflicts.append(dict(cls=key[0], method=key[1], argc=key[2], claimants=claimants))

    # A ported feature and a companion feature cannot own the same method
    # either: the hook engine refuses the second detour, so the feature that
    # loses is refused instead of installing. It only bites when the two
    # force different values; when they force the same one they agree.
    cross: list[dict] = []
    if legacy_source:
        slot_values = _slot_values(source)
        legacy = parse_legacy_specs(legacy_source)
        for spec in specs:
            for owner, cls, method, argc, detour in legacy:
                if cls != spec.cls or method != spec.method or argc != spec.argc:
                    continue
                mine = _forced_value(spec.detour, slot_values)
                theirs = _forced_value(detour, slot_values)
                if mine is not None and theirs is not None:
                    kind = "same value" if mine == theirs else "different value"
                else:
                    kind = "unknown"
                cross.append(dict(cls=cls, method=method, argc=argc,
                                  faunas=spec.feature, legacy=owner,
                                  faunas_value=mine, legacy_value=theirs,
                                  kind=kind))
    return dict(specs=specs, problems=problems, conflicts=conflicts, cross=cross)


def format_report(report: dict) -> str:
    lines = []
    problems = report["problems"]
    lines.append("Faunas target audit: %d specs" % len(report["specs"]))
    lines.append("")
    if problems:
        lines.append("RESOLUTION PROBLEMS (%d)" % len(problems))
        for problem in problems:
            lines.append("  [%2d] %-18s %s.%s argc=%d"
                         % (problem["feature_id"], problem["feature"],
                            problem["cls"], problem["method"], problem["argc"]))
            lines.append("        -> %s" % problem["detail"])
    else:
        lines.append("RESOLUTION PROBLEMS: none")
    lines.append("")
    if report["conflicts"]:
        lines.append("OWNERSHIP CONFLICTS (%d)" % len(report["conflicts"]))
        for conflict in report["conflicts"]:
            lines.append("  %s.%s  <-  %s" % (conflict["cls"], conflict["method"],
                                              ", ".join(conflict["claimants"])))
    else:
        lines.append("OWNERSHIP CONFLICTS: none")
    cross = report.get("cross") or []
    lines.append("")
    if cross:
        biting = [row for row in cross if row.get("kind") == "different value"]
        lines.append("CROSS-MODULE CONFLICTS (%d, %d with different values)"
                     % (len(cross), len(biting)))
        lines.append("  a ported feature and a companion feature claim the same method;")
        lines.append("  the hook engine gives it to one of them and refuses the other.")
        lines.append("  It only bites when the two force different values.")
        for row in cross:
            lines.append("  %s.%s argc=%d  [%s]"
                         % (row["cls"], row["method"], row["argc"], row.get("kind")))
            lines.append("      faunas: %-18s companion: %s"
                         % (row["faunas"], row["legacy"]))
            if row.get("faunas_value") is not None or row.get("legacy_value") is not None:
                lines.append("      values: %s vs %s"
                             % (row.get("faunas_value"), row.get("legacy_value")))
    else:
        lines.append("CROSS-MODULE CONFLICTS: none")
    return "\n".join(lines)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--metadata", type=Path, default=DEFAULT_METADATA,
                        help="the tracked metadata export")
    parser.add_argument("--source", type=Path, default=DEFAULT_SOURCE)
    parser.add_argument("--dump", type=Path, default=None,
                        help="a downloaded metadata dump to check against")
    parser.add_argument("--legacy", type=Path, default=DEFAULT_LEGACY_SOURCE,
                        help="the companion's own feature tables, which claim "
                             "methods the ported features also claim")
    args = parser.parse_args(argv)

    legacy_source = args.legacy

    if not args.source.exists():
        print("port source not found: %s" % args.source, file=sys.stderr)
        return 2

    dump = args.dump if args.dump is not None else current_build_dump()
    source = args.source.read_text(encoding="utf-8")

    if dump is not None and dump.exists():
        corresponds, reason = dump_correspondence(dump)
        if corresponds:
            metadata = Metadata.load_dump(dump)
            print("metadata source: %s (%s)" % (dump.name, reason))
        else:
            print("metadata source: %s is not usable as the installed build's "
                  "reference (%s)" % (dump.name, reason), file=sys.stderr)
            print("falling back to the tracked export, which may describe an "
                  "older build", file=sys.stderr)
            if not args.metadata.exists():
                return 2
            metadata = Metadata.load(args.metadata)
    else:
        if not args.metadata.exists():
            print("metadata snapshot not found: %s" % args.metadata, file=sys.stderr)
            print("run scripts/export-metadata-symbols.ps1 first", file=sys.stderr)
            return 2
        metadata = Metadata.load(args.metadata)
        print("metadata source: %s (the tracked export)" % args.metadata.name)

    report = audit(source, metadata,
                   legacy_source.read_text(encoding="utf-8")
                   if legacy_source and legacy_source.exists() else None)
    print(format_report(report))
    return 1 if (report["problems"] or report["conflicts"] or report.get("cross")) else 0


if __name__ == "__main__":
    raise SystemExit(main())
