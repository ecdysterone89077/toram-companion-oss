#!/usr/bin/env python3
"""Map the Faunas (Android Toram 4.0.62) gameplay mechanics onto Windows IL2CPP targets.

Faunas patches raw ARM64 instruction bytes inside `libil2cpp.so`. This tool
keeps the *mechanics* (the original is the spec) and resolves each one to the
Windows-side IL2CPP class/method declared in an Il2CppDumper `dump.cs`, so the
port can be implemented through the managed dispatch door instead of raw code
patching.

Usage:
    python scripts/faunas_port_map.py [--dump dump/dump.cs] [--json out.json]
                                      [--md out.md]
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from dataclasses import dataclass, field, asdict
from pathlib import Path

# --------------------------------------------------------------------------
# Faunas reference table.
#
# kind:
#   ret_true  -> prologue replaced with `mov w0, #1 ; ret`   (bool true)
#   ret_zero  -> prologue replaced with `mov w0, #0 ; ret`   (bool false / int 0)
#   ret_const -> prologue replaced with `mov w0, #N ; ret`   (int / float N)
#   nop       -> N instructions blanked inside the body
#   imm       -> a constant inside the body is rewritten
#   branch    -> a branch target inside the body is rewritten
#   scan      -> a value is located by memory scan, not by a fixed offset
# --------------------------------------------------------------------------

@dataclass
class FaunasEntry:
    feature: str
    menu: str
    offset: int
    claimed: str
    kind: str
    forced: object = None
    windows_class: str | None = None
    windows_method: str | None = None
    candidates: list = field(default_factory=list)
    note: str = ""


def E(feature, menu, offset, claimed, kind, forced=None,
      windows_class=None, windows_method=None, note=""):
    return FaunasEntry(feature, menu, offset, claimed, kind, forced,
                       windows_class, windows_method, note)


FAUNAS: list[FaunasEntry] = [
    # ---------------- player ----------------
    E("invincibility", "Player", 0x1062D20, "get_IsInvincibility PlayerStatus",
      "ret_true", 1, "PlayerStatus", "get_IsInvincibility",
      "Menu label is 'Mob ATK Always Miss'; the patched getter is the invincibility flag."),
    E("crit", "Player", 0x106DD54, "checkCriticalPercent PlayerAttackBase",
      "ret_true", 1, "PlayerAttackBase", "checkCriticalPercent"),
    E("crit", "Player", 0x107F718, "CheckMagicCritical",
      "ret_true", 1, "PlayerAttackBase", "CheckMagicCritical"),
    E("crit", "Player", 0x106E4A8, "unlabelled third critical check",
      "ret_true", 1, None, None,
      "Faunas names no method here; it is the physical counterpart of the two above."),
    E("aspd", "Player", 0x105F108, "PlayerSecondaryStatus attack-delay block",
      "nop", None, "PlayerSecondaryStatus", None,
      "12 bytes blanked; the port reuses the existing instant-attack delay path."),
    E("moremotion", "Player", 0x14B9B38, "animation-speed constant 65 -> 500",
      "imm", 500, None, None),
    E("moremotionb", "Player", 0x14B9B34, "animation constant + motion block",
      "imm", 500, None, None),
    E("cspd", "Player", 0x105F3E8, "GetSkillDelayRate1 secondarystatus",
      "ret_zero", 0, "PlayerSecondaryStatus", "GetSkillDelayRate1",
      "Zero cast delay."),
    E("motion", "Player", 0x105F128, "GetMotionSpeed playersecondarystatus",
      "ret_const", 60, "PlayerSecondaryStatus", "GetMotionSpeed"),
    E("stable", "Player", 0x105B20C, "get_Stable Player secondary",
      "ret_const", 1000, "PlayerSecondaryStatus", "get_Stable"),
    E("longrange", "Player", 0xEF877C, "long range skill constant",
      "imm", None, None, None,
      "Skill range widening; the Windows build retired range widening after the server "
      "distance validation was found."),
    E("range", "Player", 0x1416C5C, "CheckActionRange BattleManagerBase",
      "ret_true", 1, "BattleManagerBase", "CheckActionRange"),
    E("unlock", "Player", 0x1E6A310, "unlock main quest gate",
      "imm", None, None, None),
    E("resist", "Player", 0x9A5188, "AbnormalPopLabelPlayerToEnemy",
      "ret_zero", 0, None, None),
    E("resist", "Player", 0x9A63B8, "AbnormalResistPopUpLabel",
      "ret_zero", 0, None, None),
    E("penet", "Player", 0x1079644, "CalcPowerResistDamage",
      "ret_zero", 0, None, None),
    E("penet", "Player", 0x1079710, "CalcMagicResistDamage",
      "ret_zero", 0, None, None),

    # ---------------- mob ----------------
    E("mob_no_flee", "Mob", 0x105B578, "get_Hit player secondary status",
      "ret_const", 65504, "PlayerSecondaryStatus", "get_Hit"),
    E("mob_no_move", "Mob", 0xF141D4, "get_Range mob act",
      "ret_zero", 0, None, None),
    E("mob_no_move", "Mob", 0xCAE82C, "CreateMobAction",
      "ret_zero", 0, None, None),
    E("mob_no_guard", "Mob", 0x106CD70, "checkMobReaction playerattackbase",
      "ret_zero", 0, "PlayerAttackBase", "checkMobReaction"),
    E("mob_no_invincible", "Mob", 0xEF8AA4, "get_SystemInvincible enemymobaction",
      "ret_zero", 0, None, None),

    # ---------------- damage ----------------
    E("brave_aura", "DMG", 0x12B455C, "brave aura threshold",
      "imm", "100..800", None, None,
      "Faunas rewrites the subtracted constant of a comparison, not a return value."),
    E("critdmg", "DMG2", 0x105D594, "get_CriticalDmg PlayerSecondaryStatus",
      "ret_const", "1..10 ladder", "PlayerSecondaryStatus", "get_CriticalDmg"),
    E("critmgcdmg", "DMG2", 0x105D8B4, "get_CriticalMagicDmg PlayerSecondaryStatus",
      "ret_const", "1..10 ladder", "PlayerSecondaryStatus", "get_CriticalMagicDmg"),
    E("element", "Element", 0x1064B74, "GetEquipElement player status",
      "ret_const", "0..6", None, None),
    E("quick_aura", "BraveAura", 0x120FD08, "Quick -> Brave aura branch",
      "branch", None, None, None),

    # ---------------- proration ----------------
    E("prorate", "Prorate", 0xF1516C, "ExpDefNormal MobBattleStatus",
      "ret_const", "302/400/450/500/550/600/700", None, None),
    E("prorate", "Prorate", 0xF15184, "get_ExpDefSkill MobBattleStatus",
      "ret_const", "302/400/450/500/550/600/700", None, None),
    E("prorate", "Prorate", 0xF1519C, "get_ExpDefMagic MobBattleStatus",
      "ret_const", "302/400/450/500/550/600/700", None, None),
    E("prorate", "Prorate", 0xEED9B8, "get_ExpDefNormal BossMobBattleStatus",
      "ret_const", "302/400/450/500/550/600/700", None, None),
    E("prorate", "Prorate", 0xEED9D0, "get_ExpDefSkill BossMobBattleStatus",
      "ret_const", "302/400/450/500/550/600/700", None, None),
    E("prorate", "Prorate", 0xEED9E8, "get_ExpDefMagic BossMobBattleStatus",
      "ret_const", "302/400/450/500/550/600/700", None, None),

    # ---------------- other ----------------
    E("gacha", "Other", 0x11D7074, "unlock beginner gacha",
      "ret_true", 1, None, None),
    E("skip_button", "Other", 0x129995C, "skip button",
      "imm", None, None, None),
    E("skip_battle", "Other", 0x11A4DE8, "get_IsBattleEnd",
      "ret_true", 1, None, None),
    E("unlock_map", "Other", 0x9FC6A0, "unlock world selection",
      "imm", None, None, None),
    E("no_domination", "Other", 0xF60A2C, "SetBonusGauge",
      "ret_zero", 0, None, None),
    E("no_domination", "Other", 0xF60A24, "SetBonusGauge",
      "ret_zero", 0, None, None),
    E("unlimited_buff", "Other", 0x12EBCFC, "Update SKILLBUFFERMANAGER",
      "ret_true", 1, None, None),
    E("no_drop_visual", "Other", 0x12F76A8, "drop visual",
      "ret_zero", 0, None, None),
    E("fast_run", "Other", 0x10261FC, "get_MoveSpeed PlayerActionManager",
      "ret_const", None, "PlayerActionManager", "get_MoveSpeed"),
    E("unlock_mq", "Other", 0x14CA578, "unlock main quest",
      "imm", None, None, None),
    E("unlock_mq", "Other", 0x14CA5D8, "unlock main quest (second site)",
      "imm", None, None, None),
    E("skip_mq", "Other", 0x11686F0, "skip main quest",
      "imm", None, None, None),
    E("skip_item_mq", "Other", 0x1F3A504, "skip item main quest and event",
      "ret_zero", 0, None, None),
    E("skip_item_mq", "Other", 0x1F3C828, "skip item event quest",
      "ret_zero", 0, None, None),
    E("skip_mob_mq", "Other", 0x1F3AAE8, "skip mob main quest and event",
      "ret_zero", 0, None, None),
    E("skip_mob_mq", "Other", 0x1F3CE0C, "skip mob event quest",
      "ret_zero", 0, None, None),
    E("skip_quest", "Other", 0x1F3CA9C, "skip quest",
      "imm", None, None, None),
    E("skip_tutorial", "Other", 0x1F380DC, "skip tutorial",
      "ret_zero", 0, None, None),

    # ---------------- movement (value scan) ----------------
    E("tembus", "Tembus", 0, "scanned DashOption struct",
      "scan", "fastrun 2 / blink 0 / antigravity 0 / framesec 1 / ignoreheight 1",
      None, None),
    E("blink", "Tembus", 0, "scanned DashOption struct",
      "scan", "fastrun 2 / blink 0 / antigravity 1 / framesec 0 / ignoreheight 0",
      None, None),
    E("hayabusa", "Tembus", 0, "scanned DashOption struct",
      "scan", "fastrun 1000 / blink 0 / antigravity 0 / framesec 0 / ignoreheight 1",
      None, None),
    E("wide_view", "Other", 0, "memory scan for float 7 -> 18",
      "scan", "FOV 7 -> 18", None, None),
]

# Skill action classes Faunus patches. Each contributes a CheckRangeHit (AOE)
# and/or a NextRangeHit (burst) override that returns true.
SKILL_CLASSES: list[tuple[str, str, str, str]] = [
    # (faunas key, windows class, AOE method, burst method)
    ("meteo", "MeteoBreakerAction", "CheckRangeHit", "NextRangeHit"),
    ("aura_blade", "AuraBladeAction", "CheckRangeHit", "NextRangeHit"),
    ("cross_fire", "CrossFireAction", "CheckRangeHit", "NextRangeHit"),
    ("satellite_arrow", "SatelliteArrowAction", "CheckRangeHit", "NextRangeHit"),
    ("magic_wall", "MagicWallAction", "CheckRangeHit", "NextRangeHit"),
    ("magic_storm", "MagicStormAction", "CheckRangeHit", "NextRangeHit"),
    ("magic_impact", "MagicImpactAction", "CheckRangeHit", None),
    ("magic_finale", "MagicFinalAction", "CheckRangeHit", "NextRangeHit"),
    ("magic_cannon", "MagicCannonAction", "CheckRangeHit", "NextRangeHit"),
    ("magic_fall", "MagicFallAction", "CheckRangeHit", "NextRangeHit"),
    ("dive_impact", "DiveImpactAction", "CheckRangeHit", "NextRangeHit"),
    ("dimension_till", "DimensionTillAction", "CheckRangeHit", "NextRangeHit"),
    ("dragonic_charge", "DragonicChargeAction", "CheckRangeHit", "NextRangeHit"),
    ("lunar_misfortune", "LunaDitherStarAction", "CheckRangeHit", "NextRangeHit"),
    ("twin_buster_blade", "TwinBusterBladeAction", "CheckRangeHit", "NextRangeHit"),
    ("air_slide", "AirSlideAction", "CheckRangeHit", None),
    ("nemesis", "NemesisAction", "CheckRangeHit", None),
    ("soul_hunt", "SoulHuntAction", "CheckRangeHit", None),
    ("shell_break", "ShellBreakAction", None, None),
]

RVA_RE = re.compile(r"//\s*RVA:\s*(0x[0-9A-Fa-f]+)")
SIG_RE = re.compile(r"^\s*(public|private|protected|internal)?\s*"
                    r"(static\s+|virtual\s+|override\s+|abstract\s+|sealed\s+|new\s+)*"
                    r"(?P<ret>[\w\.<>\[\],\? ]+?)\s+(?P<name>[\w\.]+)\s*\((?P<args>[^)]*)\)")


class Dump:
    """A parsed Il2CppDumper dump.cs."""

    def __init__(self) -> None:
        self.by_method: dict[str, list[dict]] = {}
        self.by_class: dict[str, list[dict]] = {}
        self.types = 0

    def load(self, path: Path) -> None:
        current = ""
        pending_rva = None
        pending_slot = None
        with path.open("r", encoding="utf-8", errors="replace") as fh:
            for line in fh:
                stripped = line.rstrip("\n")
                m = RVA_RE.search(stripped)
                if m:
                    pending_rva = int(m.group(1), 16)
                    continue
                if stripped.startswith("public class ") or stripped.startswith("public static class ") \
                        or stripped.startswith("public abstract class ") \
                        or stripped.startswith("internal class ") \
                        or stripped.startswith("public sealed class ") \
                        or stripped.startswith("public interface "):
                    current = stripped.split("class ", 1)[-1].split(":", 1)[0].strip()
                    current = current.split("<", 1)[0].strip()
                    self.types += 1
                    continue
                if not stripped or stripped.startswith("//"):
                    continue
                ms = SIG_RE.match(stripped)
                if not ms:
                    continue
                name = ms.group("name")
                if "." in name:
                    continue
                args = ms.group("args").strip()
                ret = ms.group("ret").strip()
                argc = 0 if not args else len([a for a in args.split(",") if a.strip()])
                rec = {
                    "class": current,
                    "method": name,
                    "args": args,
                    "argc": argc,
                    "returns": ret,
                    "rva": pending_rva,
                }
                pending_rva = None
                self.by_method.setdefault(name, []).append(rec)
                self.by_class.setdefault(current, []).append(rec)

    def find(self, method: str, klass: str | None = None) -> list[dict]:
        recs = self.by_method.get(method, [])
        if klass:
            exact = [r for r in recs if r["class"] == klass]
            if exact:
                return exact
            loose = [r for r in recs if r["class"].endswith(klass)]
            if loose:
                return loose
        return recs


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--dump", default="dump/dump.cs")
    ap.add_argument("--json", default=None)
    ap.add_argument("--md", default=None)
    args = ap.parse_args()

    dump_path = Path(args.dump)
    if not dump_path.is_file():
        print(f"error: dump.cs not found: {dump_path}", file=sys.stderr)
        return 2

    print(f"parsing {dump_path} ...", file=sys.stderr)
    dump = Dump()
    dump.load(dump_path)
    print(f"  types={dump.types} distinct methods={len(dump.by_method)}", file=sys.stderr)

    rows = []
    for entry in FAUNAS:
        rec = asdict(entry)
        if entry.windows_method:
            hits = dump.find(entry.windows_method, entry.windows_class)
            rec["candidates"] = [
                {"class": h["class"], "method": h["method"], "args": h["args"],
                 "argc": h["argc"], "returns": h["returns"], "rva": h["rva"]}
                for h in hits[:6]
            ]
            rec["resolved"] = bool(hits)
        else:
            rec["candidates"] = []
            rec["resolved"] = None
        rows.append(rec)

    skill_rows = []
    for key, klass, aoe, burst in SKILL_CLASSES:
        for role, method in (("aoe", aoe), ("burst", burst)):
            if not method:
                continue
            hits = dump.find(method, klass)
            skill_rows.append({
                "key": key,
                "role": role,
                "class": klass,
                "method": method,
                "resolved": bool(hits),
                "candidates": [
                    {"class": h["class"], "method": h["method"], "args": h["args"],
                     "argc": h["argc"], "returns": h["returns"], "rva": h["rva"]}
                    for h in hits[:4]
                ],
            })

    result = {
        "source": str(dump_path),
        "faunas_entries": rows,
        "skill_entries": skill_rows,
        "summary": {
            "faunas_total": len(rows),
            "faunas_resolved": sum(1 for r in rows if r["resolved"]),
            "faunas_unresolved": sum(1 for r in rows if r["resolved"] is False),
            "faunas_unmapped": sum(1 for r in rows if r["resolved"] is None),
            "skill_total": len(skill_rows),
            "skill_resolved": sum(1 for r in skill_rows if r["resolved"]),
        },
    }

    if args.json:
        Path(args.json).parent.mkdir(parents=True, exist_ok=True)
        Path(args.json).write_text(json.dumps(result, indent=2), encoding="utf-8")
        print(f"wrote {args.json}", file=sys.stderr)

    if args.md:
        lines = ["# Faunas -> Windows target map", ""]
        lines.append("| Faunas feature | Menu | Android claim | Patch kind | Forced |"
                     " Windows class | Windows method | Resolved | Windows candidates |")
        lines.append("| --- | --- | --- | --- | --- | --- | --- | --- | --- |")
        for r in rows:
            cands = "; ".join(
                f"`{c['class']}.{c['method']}({c['args']}) -> {c['returns']}`"
                for c in r["candidates"][:3]) or "-"
            state = {True: "yes", False: "NO", None: "-"}[r["resolved"]]
            lines.append(
                f"| {r['feature']} | {r['menu']} | {r['claimed']} | {r['kind']} |"
                f" {r['forced']} | {r['windows_class'] or '-'} |"
                f" {r['windows_method'] or '-'} | {state} | {cands} |")
        lines.append("")
        lines.append("## Skill AOE / burst targets")
        lines.append("")
        lines.append("| Faunas key | Role | Windows class | Method | Resolved | Signature |")
        lines.append("| --- | --- | --- | --- | --- | --- |")
        for s in skill_rows:
            sig = "; ".join(f"`{c['class']}.{c['method']}({c['args']})`"
                             for c in s["candidates"][:2]) or "-"
            lines.append(f"| {s['key']} | {s['role']} | {s['class']} | {s['method']} |"
                         f" {'yes' if s['resolved'] else 'NO'} | {sig} |")
        Path(args.md).parent.mkdir(parents=True, exist_ok=True)
        Path(args.md).write_text("\n".join(lines) + "\n", encoding="utf-8")
        print(f"wrote {args.md}", file=sys.stderr)

    print(json.dumps(result["summary"], indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
