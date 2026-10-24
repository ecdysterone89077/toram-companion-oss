"""Audit local declarations, installed hook tables and archived responses offline.

Python 3.10+, standard library only. Inputs are never modified. This tool does
not open a process, load the game, send commands, or assert server authority.
"""

import argparse
import collections
import hashlib
import json
import re
import sys
from pathlib import Path


FEATURE_FUNCTIONS = {
    "invincibility": "job_invincibility_toggle",
    "speed": "job_speed_boost",
    "no_collision": "no_collision_hook_specs",
    "rigidity": "job_clear_delay",
    "engage": "range_hook_specs",
    "timing": "job_instant_attack",
    "critical": "job_always_crit",
    "mp": "job_unlimited_mp",
}

DOMAINS = {
    "status_calculation": r"^(?:IPlayerStatusCalculator|Player(?:Battle|Secondary|Primary|Status)|TemporaryPlayerStatus|MobaPlayerSecondaryStatus|MercenaryBattleStatus|PetStatus)",
    "damage_and_hit": r"(?:Attack|Damage|Critical|Stable|HitType)",
    "skills_and_combo": r"(?:Skill|Combo|Cast|Chant|Charge)",
    "resources_and_recovery": r"(?:Recovery|Recover|Revive|Respawn|Resurrection|ExHp|ExMp)",
    "status_effects_and_resistance": r"(?:Buff|Debuff|Ailment|Abnormal|Guard|Avoid|Barrier|Resistance|Rigidity)",
    "movement_and_geometry": r"(?:CharacterMove|FieldRayPick|MoveAssist|Shukuchi|Warp|Teleport|NavMesh|Collision|Position)",
    "targeting_and_ai": r"(?:Target|Aggro|Hate|Monster|MobAI|MobBrain|MobAction|EnemyAction)",
    "equipment_and_elements": r"(?:Equipment|Equip|Weapon|Armor|Element|Refine|Enhance|Crystal|Enchant)",
    "inventory_and_items": r"(?:Inventory|Bag|Storage|Warehouse|Item|Drop|Loot)",
    "crafting_and_production": r"(?:Craft|Produce|Production|Smith|Alchemy|Synthesis|Cooking|Material)",
    "progression_and_quests": r"(?:Quest|Mission|Achievement|Reward|Experience|StatusUp|SkillUp|LevelUp)",
    "economy_and_transactions": r"(?:Trade|Consignment|Auction|Market|Shop|Purchase|Currency|Money|Spina)",
    "companions_and_party": r"(?:Mercenary|Pet|Partner|Party|Guild|Friend)",
    "world_and_instance": r"(?:Dungeon|Instance|Difficulty|Boss|Raid|Spawn|MapChange|FieldChange)",
    "network_and_settlement": r"(?:Response|Request|Operations|Listener|Network|Connection|Photon)",
    "presentation_and_input": r"(?:Camera|Minimap|MiniMap|Hud|HUD|Input|Shortcut|KeyConfig)",
}


def sha256(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def feature_hooks(source):
    starts = list(re.finditer(r"(?m)^(?:pub(?:\([^\n]*\))? )?(?:unsafe )?fn (\w+)[^\n]*", source))
    functions = {}
    for index, match in enumerate(starts):
        end = starts[index + 1].start() if index + 1 < len(starts) else len(source)
        functions[match[1]] = (match.start(), source[match.start():end])
    hooks = []
    pattern = re.compile(
        r"HookSpec\s*\{\s*target:\s*HookTargetClass::(\w+),\s*"
        r'method:\s*"([^"]+)",\s*argc:\s*(\d+),\s*detour:\s*([^\n]+)',
        re.S,
    )
    for feature, function in FEATURE_FUNCTIONS.items():
        if function not in functions:
            raise ValueError(f"Missing feature function: {function}")
        offset, body = functions[function]
        matches = list(pattern.finditer(body))
        if not matches:
            raise ValueError(f"No hook declarations found in {function}")
        for match in matches:
            hooks.append({
                "feature": feature,
                "function": function,
                "owner": match[1],
                "method": match[2],
                "argc": int(match[3]),
                "detour": match[4].strip().rstrip(","),
                "source_line": source.count("\n", 0, offset + match.start()) + 1,
            })
    return hooks


def replay_capture(path):
    events, prior, rows, calculations, endings = {}, {}, [], [], []
    for line in path.read_text(encoding="utf-8-sig").splitlines():
        if "[combat-response] " not in line:
            continue
        record = json.loads(line.split("[combat-response] ", 1)[1])
        kind = record.get("kind")
        if kind == "begin":
            events, prior = {}, {}
        elif kind == "event":
            events[record["event"]] = record
            if record.get("stage") == 1:
                calculations.append(record)
        elif kind == "mob":
            event = events.get(record["event"])
            if event is None:
                raise ValueError("Mob record without preceding event")
            key = (record["mob_id"], record["local_id"], record["unique_id"])
            valid = event.get("valid") and record.get("valid") and key[-1] >= 0 and record["hp"] >= 0
            previous = prior.get(key)
            delta = previous - record["hp"] if valid and previous is not None else None
            if valid:
                prior[key] = record["hp"]
            outcome = "no_baseline" if delta is None else "hp_drop" if delta > 0 else "unchanged" if delta == 0 else "hp_increase"
            rows.append({
                "event": record["event"], "target": key, "skill": event["skill"],
                "damage_id": event["damage_id"], "request_at_response": event["request"],
                "hp": record["hp"], "hp_drop": delta, "outcome": outcome,
                "allowed": record["allowed"], "violation": record["violation"],
                "game_code": event["game_code"], "mob_code": record["code"],
                "proration_recorded": all(k in record for k in ("normal_exp", "physical_exp", "magic_exp")),
            })
        elif kind == "end":
            endings.append(record)
    if not rows or not endings:
        raise ValueError("Expected response rows and a completed capture")
    return {
        "endings": endings,
        "counts": dict(collections.Counter(row["outcome"] for row in rows)),
        "native_calculation_values": sorted({entry["native"] for entry in calculations}),
        "calculation_count": len(calculations),
        "responses": rows,
        "limits": ["Request sampled at response time is not the originating calculation value.",
                   "HP deltas are between snapshots; attribution requires a correlated action and exclusion of other actors.",
                   "Zero violation and positive allowance do not prove that a hit landed."],
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--symbols", type=Path, required=True)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    root = args.root.resolve()
    symbols = json.loads(args.symbols.read_text(encoding="utf-8-sig"))
    if symbols.get("schema") != 1:
        raise ValueError("Unsupported symbol inventory schema")
    types = symbols["types"]
    counts = {"types": len(types), "methods": sum(len(t["methods"]) for t in types), "fields": sum(len(t["fields"]) for t in types)}
    if counts != symbols["counts"]:
        raise ValueError("Symbol inventory totals do not reconcile")
    source_file = root / "crates/wcore/src/features.rs"
    source = source_file.read_text(encoding="utf-8")
    hooks = feature_hooks(source)
    methods = collections.defaultdict(set)
    for entry in types:
        if not entry["namespace"]:
            methods[entry["name"]].update((m["name"], m["argc"]) for m in entry["methods"])
    for hook in hooks:
        hook["declared_on_owner"] = (hook["method"], hook["argc"]) in methods[hook["owner"]]
    calculators = [t for t in types if t["name"] == "IPlayerStatusCalculator"]
    if len(calculators) != 1:
        raise ValueError("Expected exactly one IPlayerStatusCalculator declaration")
    contract = calculators[0]
    covered = {(h["method"], h["argc"]) for h in hooks if h["owner"] in ("PlayerBattleStatus", "PlayerSecondaryStatus")}
    contract_rows = [{**m, "status_hook_installed_by_feature_table": (m["name"], m["argc"]) in covered} for m in contract["methods"]]
    domains = {}
    for name, expression in DOMAINS.items():
        selected = [t for t in types if re.search(expression, t["name"], re.I)
                    or re.search(expression, t["namespace"] + "." + t["name"], re.I)]
        domains[name] = {"type_count": len(selected), "method_count": sum(len(t["methods"]) for t in selected),
                         "type_references": [{"index": t["index"], "namespace": t["namespace"], "name": t["name"]} for t in selected]}
    capture = root / "work/gameplay-20261020-16932/comparison-session.log"
    report = {
        "schema": 1,
        "inputs": {"metadata": {"path": symbols["metadata_path"], "sha256": symbols["metadata_sha256"], "version": symbols["version"]},
                   "symbol_export_sha256": sha256(args.symbols), "source_sha256": sha256(source_file),
                   "capture_sha256": sha256(capture)},
        "metadata_counts": counts,
        "feature_hook_counts": dict(collections.Counter(h["feature"] for h in hooks)),
        "feature_hooks": hooks,
        "status_contract": {
            "method_count": len(contract_rows),
            "hooked_method_count": sum(m["status_hook_installed_by_feature_table"] for m in contract_rows),
            "methods": contract_rows,
            "full_contract_declarations": [
                {"index": t["index"], "namespace": t["namespace"], "name": t["name"]}
                for t in types
                if {(m["name"], m["argc"]) for m in contract["methods"]}
                <= {(m["name"], m["argc"]) for m in t["methods"]}
            ],
        },
        "domains": domains,
        "comparison_replay": replay_capture(capture),
        "limits": ["Metadata declarations do not identify executed paths, remote validation, or usable modifications.",
                   "Domain tags are overlapping lexical navigation aids, not semantic or ownership classifications.",
                   "Hook tables describe source installation intent, not successful runtime installation.",
                   "A method absent on an owner may be inherited; absence here does not establish a broken hook.",
                   "This inventory does not enumerate native-only behavior or remote server implementation."],
    }
    output = args.output.resolve()
    if output in (args.symbols.resolve(), source_file.resolve(), capture.resolve()):
        raise ValueError("Output would overwrite an input")
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({"metadata": counts, "hooks": report["feature_hook_counts"],
                      "status_contract": {k: report["status_contract"][k] for k in ("method_count", "hooked_method_count")},
                      "replay": report["comparison_replay"]["counts"], "output": str(output)}, indent=2))


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, KeyError) as error:
        print(f"Audit failed: {error}", file=sys.stderr)
        raise SystemExit(2)
