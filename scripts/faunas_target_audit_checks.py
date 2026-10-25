"""Regression checks for the Faunas port's hook target audit.

The audit is the thing that stands between a mis-named target class and a
feature that silently refuses to install in game. These checks prove the audit
actually catches each way a target can be wrong, and that the shipped port
still resolves against the installed metadata snapshot.
"""

import json
import os
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from faunas_target_audit import (
    DEFAULT_LEGACY_SOURCE,
    DEFAULT_METADATA,
    DEFAULT_SOURCE,
    FEATURES,
    Metadata,
    _detour_text,
    _forced_value,
    audit,
    current_build_dump,
    dump_correspondence,
    format_report,
    parse_legacy_specs,
    parse_specs,
)


def _usable_dump():
    """The downloaded dump, when it still describes the installed module."""
    dump = current_build_dump()
    if dump is None or not dump.exists():
        return None
    corresponds, _reason = dump_correspondence(dump)
    return dump if corresponds else None


PORT_SOURCE = r'''
// A trimmed but structurally faithful copy of the port's source.
const SKILL_AOE_CLASSES: [&str; 2] = ["MeteoBreakerAction", "AuraBladeAction"];
const SKILL_NO_AOE_CLASSES: [&str; 1] = ["MagicImpactAction"];
const SKILL_BURST_CLASSES: [&str; 1] = ["MeteoBreakerAction"];

fn crit_specs() -> [HookSpec; 1] {
    [HookSpec {
        target: HookTargetClass::PlayerSecondaryStatus,
        method: "get_Critical",
        argc: 0,
        detour: hk_true_0arg as usize,
        orig_sink: None,
    }]
}

fn element_specs() -> [HookSpec; 1] {
    [HookSpec {
        target: HookTargetClass::PlayerStatusBase,
        method: "GetEquipElement",
        argc: 0,
        detour: hk_const::<SLOT_ELEMENT> as usize,
        orig_sink: None,
    }]
}

fn critdmg_specs() -> [HookSpec; 1] {
    [HookSpec {
        target: HookTargetClass::PlayerSecondaryStatus,
        method: "get_CriticalDmg",
        argc: 0,
        detour: hk_const::<SLOT_CRITDMG> as usize,
        orig_sink: None,
    }]
}

fn skill_aoe_specs() -> Vec<HookSpec> {
    SKILL_AOE_CLASSES
        .iter()
        .map(|class| HookSpec {
            target: HookTargetClass::Named(class),
            method: "CheckRangeHit",
            argc: 3,
            detour: hk_true as usize,
            orig_sink: None,
        })
        .collect()
}

fn skill_no_aoe_specs() -> Vec<HookSpec> {
    SKILL_NO_AOE_CLASSES
        .iter()
        .map(|class| HookSpec {
            target: HookTargetClass::Named(class),
            method: "CheckRangeHit",
            argc: 3,
            detour: hk_false as usize,
            orig_sink: None,
        })
        .collect()
}

fn skill_burst_specs() -> Vec<HookSpec> {
    SKILL_BURST_CLASSES
        .iter()
        .map(|class| HookSpec {
            target: HookTargetClass::Named(class),
            method: "NextRangeHit",
            argc: 0,
            detour: hk_skip as usize,
            orig_sink: None,
        })
        .collect()
}

fn specs_for(index: usize) -> Option<Vec<HookSpec>> {
    Some(match index {
        0 => crit_specs().to_vec(),
        11 => skill_aoe_specs(),
        12 => skill_no_aoe_specs(),
        13 => skill_burst_specs(),
        15 => element_specs().to_vec(),
        16 => critdmg_specs().to_vec(),
        _ => Vec::new(),
    })
}
'''


def metadata_payload(types):
    return {
        "version": 31.0,
        "counts": {"types": len(types), "methods": 0, "fields": 0},
        "types": types,
    }


def type_entry(name, methods):
    return {"index": 0, "namespace": "", "name": name,
            "methods": [{"name": m, "argc": a, "token": 0} for m, a in methods],
            "fields": []}


SAMPLE_METADATA = metadata_payload([
    type_entry("PlayerStatusBase", [("GetEquipElement", 0),
                                    ("get_IsInvincibility", 0)]),
    type_entry("PlayerStatus", [("get_IsInvincibility", 0)]),
    type_entry("PlayerSecondaryStatus", [("get_CriticalDmg", 0),
                                        ("get_Critical", 0)]),
    type_entry("PlayerBattleStatus", [("get_CriticalDmg", 0), ("get_Critical", 0)]),
    type_entry("MeteoBreakerAction", [("CheckRangeHit", 3), ("NextRangeHit", 0)]),
    type_entry("AuraBladeAction", [("CheckRangeHit", 3)]),
    type_entry("MagicImpactAction", [("CheckRangeHit", 3)]),
])


def kinds(report):
    return sorted(problem["kind"] for problem in report["problems"])


class TargetAuditChecks(unittest.TestCase):
    def test_clean_source_reports_no_problems(self):
        report = audit(PORT_SOURCE, Metadata(SAMPLE_METADATA))
        self.assertEqual(report["problems"], [])
        self.assertEqual(report["conflicts"], [])

    def test_method_on_the_wrong_class_is_reported(self):
        broken = PORT_SOURCE.replace(
            "target: HookTargetClass::PlayerStatusBase,\n        method: \"GetEquipElement\"",
            "target: HookTargetClass::PlayerBattleStatus,\n        method: \"GetEquipElement\"")
        report = audit(broken, Metadata(SAMPLE_METADATA))
        self.assertIn("method_not_on_class", kinds(report))
        problem = [p for p in report["problems"]
                   if p["feature"] == "element"][0]
        self.assertIn("PlayerStatusBase", problem["detail"])

    def test_missing_target_class_is_reported(self):
        broken = PORT_SOURCE.replace("HookTargetClass::PlayerSecondaryStatus",
                                    "HookTargetClass::NoSuchClass")
        report = audit(broken, Metadata(SAMPLE_METADATA))
        self.assertIn("class_missing", kinds(report))

    def test_argument_count_mismatch_is_reported(self):
        broken = PORT_SOURCE.replace(
            'method: "get_CriticalDmg",\n        argc: 0,',
            'method: "get_CriticalDmg",\n        argc: 2,')
        report = audit(broken, Metadata(SAMPLE_METADATA))
        self.assertIn("argc_mismatch", kinds(report))

    def test_two_features_claiming_one_method_is_a_conflict(self):
        broken = PORT_SOURCE.replace(
            'method: "get_CriticalDmg",\n        argc: 0,',
            'method: "get_Critical",\n        argc: 0,')
        report = audit(broken, Metadata(SAMPLE_METADATA))
        self.assertTrue(report["conflicts"])
        conflict = report["conflicts"][0]
        self.assertEqual(conflict["method"], "get_Critical")
        self.assertIn("critdmg", conflict["claimants"])

    def test_named_loop_specs_expand_to_every_class(self):
        specs = parse_specs(PORT_SOURCE)
        targets = {(s.cls, s.method) for s in specs if s.feature == "skill_aoe"}
        self.assertIn(("MeteoBreakerAction", "CheckRangeHit"), targets)
        self.assertIn(("AuraBladeAction", "CheckRangeHit"), targets)

    def test_every_specified_feature_is_covered_by_the_parser(self):
        specs = parse_specs(PORT_SOURCE)
        covered = {s.feature_id for s in specs}
        self.assertEqual(covered, {0, 11, 12, 13, 15, 16})

    def test_report_is_stable_across_repeated_runs(self):
        metadata = Metadata(SAMPLE_METADATA)
        first = format_report(audit(PORT_SOURCE, metadata))
        second = format_report(audit(PORT_SOURCE, metadata))
        self.assertEqual(first, second)


@unittest.skipUnless(DEFAULT_METADATA.exists(),
                     "the installed metadata snapshot is not present")
class ShippedPortChecks(unittest.TestCase):
    """The gate: the port as it ships must resolve against the real metadata."""

    def setUp(self):
        self.metadata = Metadata.load(DEFAULT_METADATA)
        self.source = DEFAULT_SOURCE.read_text(encoding="utf-8")
        self.legacy = DEFAULT_LEGACY_SOURCE.read_text(encoding="utf-8")
        self.report = audit(self.source, self.metadata, self.legacy)

    def test_every_spec_resolves(self):
        self.assertEqual(self.report["problems"], [],
                         format_report(self.report))

    def test_the_cross_module_collisions_are_reported(self):
        """A ported feature and a companion feature cannot own one method.

        The hook engine gives a MethodInfo to one owner and refuses the
        other. This is not a resolution problem: every one of these methods
        exists. It is an ownership problem, and it is only visible in game,
        where the feature that loses is refused instead of installing.
        """
        cross = self.report["cross"]
        pairs = {(row["cls"], row["method"], row["argc"], row["faunas"])
                 for row in cross}
        self.assertIn(("PlayerBattleStatus", "GetMotionSpeed", 1, "motion"), pairs)
        self.assertIn(("PlayerAttackBase", "checkCriticalPercent", 2, "crit"), pairs)
        self.assertIn(("PlayerAttackBase", "CheckMagicCritical", 2, "crit"), pairs)
        self.assertIn(("PlayerBattleStatus", "get_Stable", 0, "stable"), pairs)
        self.assertIn(("PlayerBattleStatus", "get_Hit", 0, "mob_no_flee"), pairs)

    def test_motion_and_crit_claims_agree_with_the_companion(self):
        """Same-value overlaps are shared mechanics, not fights.

        Companion `local_motion` forces 60, the port's motion value; the two
        crit claims force 1 on both sides. Only the stable/hit rows bite.
        """
        by_pair = {(r["cls"], r["method"], r["argc"]): r
                   for r in self.report["cross"]}
        motion = by_pair[("PlayerBattleStatus", "GetMotionSpeed", 1)]
        self.assertEqual(motion["kind"], "same value")
        self.assertEqual(motion["faunas_value"], motion["legacy_value"])
        for key in (("PlayerAttackBase", "checkCriticalPercent", 2),
                    ("PlayerAttackBase", "CheckMagicCritical", 2)):
            self.assertEqual(by_pair[key]["kind"], "same value")

    def test_the_port_covers_every_feature_with_a_spec(self):
        covered = {s.feature_id for s in self.report["specs"]}
        for feature_id, name in FEATURES:
            self.assertIn(feature_id, covered,
                          "feature %d (%s) was not parsed" % (feature_id, name))

    def test_no_feature_claims_a_method_twice(self):
        self.assertEqual(self.report["conflicts"], [],
                         format_report(self.report))

    def test_element_targets_the_class_that_declares_the_method(self):
        element = [s for s in self.report["specs"] if s.feature == "element"]
        self.assertEqual(len(element), 1)
        self.assertEqual(element[0].cls, "PlayerStatusBase")
        self.assertEqual(element[0].method, "GetEquipElement")
        self.assertIn("GetEquipElement",
                      self.metadata.type_methods["PlayerStatusBase"])


@unittest.skipUnless(_usable_dump() is not None,
                     "the downloaded dump does not correspond to the installed build")
class InstalledBuildChecks(unittest.TestCase):
    """The gate that matters: the port must resolve against the build that runs.

    The game is updated independently of this repository, so the tracked
    export can describe a build that is no longer installed. The downloaded
    dump is a fixed artifact, so it is only used while it still corresponds:
    its digest and the installed module's size are checked against the
    verification record before it is trusted.
    """

    def setUp(self):
        self.dump = _usable_dump()
        self.metadata = Metadata.load_dump(self.dump)
        self.source = DEFAULT_SOURCE.read_text(encoding="utf-8")
        self.report = audit(self.source, self.metadata)

    def test_every_target_resolves_in_the_installed_build(self):
        self.assertEqual(self.report["problems"], [],
                         format_report(self.report))

    def test_no_feature_claims_a_method_twice(self):
        self.assertEqual(self.report["conflicts"], [],
                         format_report(self.report))

    def test_the_two_gates_are_present_in_the_installed_build(self):
        for cls, method in (("GameUserDataManager", "get_IsBeginner"),
                            ("UIWorldMapPanel", "get_IsTapLock")):
            self.assertIn(method, self.metadata.type_methods[cls],
                          "%s.%s is missing from the installed build"
                          % (cls, method))


class DumpCorrespondenceChecks(unittest.TestCase):
    """The gate that keeps a stale dump from being trusted.

    The dump is a fixed artifact and the game updates on its own schedule,
    so the audit has to refuse a dump that no longer matches the record.
    """

    def test_a_dump_that_does_not_match_the_record_is_refused(self):
        with tempfile.TemporaryDirectory() as tmp:
            dump = Path(tmp) / "fixture_dump.cs"
            dump.write_text("class Nope { }\n", encoding="utf-8")
            verification = Path(tmp) / "verification.json"
            verification.write_text(json.dumps({"sha256": "0" * 64}), encoding="utf-8")
            with patch("faunas_target_audit.DUMP_VERIFICATION", verification):
                corresponds, reason = dump_correspondence(dump)
        self.assertFalse(corresponds)
        self.assertIn("digest", reason)

    def test_the_shipped_dump_is_accepted_when_it_matches(self):
        dump = current_build_dump()
        if dump is None or not dump.exists():
            self.skipTest("no downloaded dump is present")
        corresponds, _reason = dump_correspondence(dump)
        self.assertTrue(corresponds)


class DetourValueChecks(unittest.TestCase):
    """Same-method claims only bite when they force different values."""

    def test_true_and_false_force_one_and_zero(self):
        self.assertEqual(_forced_value("hk_true", {}), 1)
        self.assertEqual(_forced_value("hk_false", {}), 0)

    def test_unknown_detour_stays_unknown(self):
        self.assertIsNone(_forced_value("hk_skip", {}))

    def test_native_cast_adapter_is_not_reported_as_constant_zero(self):
        self.assertIsNone(_forced_value("port::cast_rate::<6>", {}))
        self.assertIsNone(_forced_value("port::cast_rate::<7>", {}))

    def test_same_value_claims_agree(self):
        faunas_source = PORT_SOURCE.replace(
            'detour: hk_true as usize,', 'detour: hk_true as usize,', 1)
        legacy_source = (
            "fn companion_feature() { let _ = HookSpec {"
            ' target: HookTargetClass::Named("MeteoBreakerAction"),'
            ' method: "CheckRangeHit", argc: 3,'
            " detour: hk_true as usize, }; }"
        )
        report = audit(faunas_source, Metadata(SAMPLE_METADATA), legacy_source)
        rows = [r for r in report["cross"]
                if (r["cls"], r["method"]) == ("MeteoBreakerAction", "CheckRangeHit")]
        self.assertTrue(rows)
        self.assertEqual(rows[0]["kind"], "same value")

    def test_different_value_claims_bite(self):
        legacy_source = (
            "fn companion_feature() { let _ = HookSpec {"
            ' target: HookTargetClass::Named("MeteoBreakerAction"),'
            ' method: "CheckRangeHit", argc: 3,'
            " detour: hk_false as usize, }; }"
        )
        report = audit(PORT_SOURCE, Metadata(SAMPLE_METADATA), legacy_source)
        rows = [r for r in report["cross"]
                if (r["cls"], r["method"]) == ("MeteoBreakerAction", "CheckRangeHit")]
        self.assertTrue(rows)
        biting = [r for r in rows if r["kind"] == "different value"]
        self.assertTrue(biting)

    def test_detour_text_strips_cast_and_comma(self):
        class Match:
            def group(self, _index):
                return "hk_true as usize,"
        self.assertEqual(_detour_text(Match()), "hk_true")


if __name__ == "__main__":
    unittest.main()
