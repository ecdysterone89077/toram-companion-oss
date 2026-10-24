"""Regression checks for the incident attribution tool.

These guard the reasoning, not just the code: a future change that breaks the
append-only property of the global log, or that makes a core-free process look
core-loaded, must fail here rather than silently weaken an attribution.
"""

from __future__ import annotations

import json
import os
import tempfile
import unittest
from pathlib import Path

import incident_attribution as ia


class ShadowNaming(unittest.TestCase):
    def test_fnv1a64_matches_the_known_candidate_pair(self):
        core = ia.REPO / "work" / "repair-20261021" / "candidate" / "src" / "wcore.dll"
        self.assertTrue(core.exists(), "candidate core missing")
        self.assertEqual(ia.shadow_name_for(core), "dxgi_ext_ea4e2587f1b98a6e.dll")

    def test_fnv1a64_is_stable_and_distinguishes_binaries(self):
        self.assertEqual(ia.fnv1a64(b""), 0xCBF29CE484222325)
        self.assertEqual(ia.fnv1a64(b"a"), 0xAF63DC4C8601EC8C)
        self.assertNotEqual(ia.fnv1a64(b"a"), ia.fnv1a64(b"b"))

    def test_shadow_name_is_content_addressed(self):
        with tempfile.TemporaryDirectory() as tmp:
            core = Path(tmp) / "wcore.dll"
            core.write_bytes(b"payload-one")
            first = ia.shadow_name_for(core)
            core.write_bytes(b"payload-two")
            second = ia.shadow_name_for(core)
            self.assertNotEqual(first, second)
            self.assertTrue(first.startswith(ia.SHADOW_PREFIX))
            self.assertTrue(first.endswith(".dll"))


class LogContinuity(unittest.TestCase):
    def test_preserved_log_is_a_prefix_of_the_live_log(self):
        preserved = ia.INCIDENT / "wcore-preserved.log"
        live = ia.TEMP / "wcore.log"
        if not live.exists():
            self.skipTest("live global log not present on this machine")
        append_only, old_len, new_len = ia.log_is_append_only(preserved, live)
        self.assertTrue(append_only, "global log was rotated or truncated")
        self.assertLess(old_len, new_len)
        self.assertEqual(old_len, 2077053)

    def test_prefix_check_rejects_a_different_log(self):
        with tempfile.TemporaryDirectory() as tmp:
            live = Path(tmp) / "live.log"
            live.write_bytes(b"completely different content")
            preserved = ia.INCIDENT / "wcore-preserved.log"
            append_only, _, _ = ia.log_is_append_only(preserved, live)
            self.assertFalse(append_only)


class LoadMarkerParsing(unittest.TestCase):
    def test_both_marker_formats_are_recognised(self):
        text = (
            "=== wcore94 (Rust) attached PID=17024 base=0x7ff9f5a70000 ===\n"
            "=== core 0.96.3-rc.1 worker PID=1032 base=0x7FFABD210000; headers preserved ===\n"
        )
        self.assertEqual(ia.loaded_pids(text), {17024, 1032})

    def test_a_session_without_a_marker_is_not_counted_as_loaded(self):
        text = "[result] pid=14516 cmd=1 status=0\nnothing else here\n"
        self.assertEqual(ia.loaded_pids(text), set())


class IncidentClassification(unittest.TestCase):
    def test_the_incident_process_has_no_core(self):
        report = ia.classify_incident()
        if report.get("error"):
            self.skipTest(report["error"])
        self.assertTrue(report["log_append_only"])
        self.assertEqual(report["log_occurrences_14516"], 0)
        self.assertFalse(report["incident_has_core_module"])
        self.assertEqual(report["incident_module_count"], 121)

    def test_the_module_collector_has_a_working_positive_control(self):
        control = ia.classify_repair_control()
        self.assertTrue(control["control_has_core"])
        self.assertFalse(control["pre_load_has_core"])
        self.assertEqual(control["control_core_module"], "dxgi_ext_ea4e2587f1b98a6e.dll")

    def test_the_original_warning_session_did_have_the_core(self):
        original = ia.classify_original()
        if original.get("error"):
            self.skipTest(original["error"])
        self.assertTrue(original["has_load_marker"])
        self.assertGreater(original["pid_17024_occurrences"], 0)


class ModuleNamePattern(unittest.TestCase):
    def test_core_module_pattern_matches_known_shadow_names(self):
        for name in ("dxgi_ext_ea4e2587f1b98a6e.dll", "DXGI_EXT_A1B2C3D4E5F60718.DLL"):
            self.assertTrue(ia.CORE_MODULE.search(name))
        for name in ("dxgi.dll", "GameAssembly.dll", "UnityPlayer.dll", "ntdll.dll"):
            self.assertIsNone(ia.CORE_MODULE.search(name))


class ShadowTimeline(unittest.TestCase):
    def test_known_cores_map_to_their_recorded_shadows(self):
        shadows = ia.shadow_timeline()
        self.assertEqual(shadows["v0.96.1"]["shadow"], "dxgi_ext_628b2fbac55147f6.dll")
        self.assertEqual(shadows["v0.96.2"]["shadow"], "dxgi_ext_e178700456260f6f.dll")
        self.assertEqual(shadows["candidate"]["shadow"], "dxgi_ext_ea4e2587f1b98a6e.dll")


class SummaryShape(unittest.TestCase):
    def test_summary_is_json_serialisable(self):
        report = ia.summary()
        json.dumps(report)
        self.assertIn("incident", report)
        self.assertIn("repair_control", report)
        self.assertIn("original_session", report)
        self.assertIn("shadows", report)


if __name__ == "__main__":
    unittest.main()
