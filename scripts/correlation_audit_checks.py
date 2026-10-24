"""Regression checks for false attribution and corrupt/incomplete captures."""

import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

from correlation_audit import PREFIX, audit_files, audit_session, parse_sessions


def begin(version=3, pid=42):
    return dict(kind="begin", pid=pid, version=version, frequency=10_000_000,
                capacity=256, max_mobs=8)


def event(index, stage=2, **values):
    row = dict(kind="event", pid=42, event=index, stage=stage, ticks=100 + index,
               thread=7, request=200, native=-1, effective=-1, valid=True,
               game_code=0, actor_type=0, actor_id=99, skill=100, damage_id=9,
               mob_count=0, captured=0)
    row.update(values)
    return row


def mob(index, hp, **values):
    row = dict(kind="mob", event=index, index=0, valid=True, mob_id=100,
               local_id=1, unique_id=321, hp=hp, code=0, allowed=1000, violation=0)
    row.update(values)
    return row


def end(count, dropped=0):
    return dict(kind="end", pid=42, events=count, dropped=dropped, restored=True)


def text(records):
    return "\n".join(PREFIX + json.dumps(record) for record in records)


def audit(records):
    return [audit_session(s) for s in parse_sessions(text(records))]


def response_pair(**values):
    return [begin(), event(0, captured=1, mob_count=1, **values), mob(0, 100),
            event(1, captured=1, mob_count=1, **values), mob(1, 70), end(2)]


def local_capture():
    context = dict(local_call=1, battle="0x100", target="0x200", target_actor="0x300",
                   action="0x400", action_class="0x500", calculation_scope="skill")
    return [begin(), event(0, 4, action_exit=False, **context),
            event(1, 1, native=168, effective=226, **context),
            event(2, 4, action_exit=True, **context), event(3), end(4)]


class CorrelationChecks(unittest.TestCase):
    def test_local_group_never_becomes_network_join(self):
        result = audit(local_capture())[0]
        self.assertEqual(result["local_groups"][0]["status"], "local_callback_group")
        self.assertEqual(result["calculations_without_local_group"], [])
        self.assertEqual(result["network_joined_responses"], 0)
        self.assertIsNone(result["responses"][0]["originating_calculation"])

    def test_nearby_calculation_and_response_are_unknown(self):
        rows = [begin(1), event(0, 1, native=168, effective=250), event(1), end(2)]
        result = audit(rows)[0]
        self.assertEqual(result["calculations_without_local_group"], [0])
        self.assertIsNone(result["responses"][0]["originating_calculation"])

    def test_unsubstantiated_correlated_flag_is_not_trusted(self):
        result = audit([begin(), event(0, network_correlated=True), end(1)])[0]
        self.assertEqual(result["network_joined_responses"], 0)

    def test_local_call_cannot_span_threads(self):
        rows = local_capture()
        rows[2]["thread"] = 8
        result = audit(rows)[0]
        self.assertIn("call_id_spans_threads", result["local_groups"][0]["reasons"])
        self.assertEqual(result["calculations_without_local_group"], [1])

    def test_missing_exit_and_changed_context_are_unknown(self):
        for change in (lambda rows: rows[3].update(action_exit=False),
                       lambda rows: rows[2].update(action="0x900"),
                       lambda rows: rows[2].update(ticks=500),
                       lambda rows: rows[2].update(valid=False),
                       lambda rows: rows[2].update(target="0x0")):
            with self.subTest(change=change):
                rows = local_capture()
                change(rows)
                self.assertEqual(audit(rows)[0]["local_groups"][0]["status"], "unknown")

    def test_snapshot_delta_is_not_attributed_damage(self):
        result = audit(response_pair())[0]
        target = result["responses"][1]["targets"][0]
        self.assertEqual(target["snapshot_hp_delta"], 30)
        self.assertEqual(target["damage_attribution"], "unknown")

    def test_target_tuple_prevents_respawn_identity_reuse(self):
        rows = response_pair()
        rows[4]["unique_id"] += 1
        target = audit(rows)[0]["responses"][1]["targets"][0]
        self.assertIsNone(target["snapshot_hp_delta"])

    def test_foreign_actor_is_explicit(self):
        rows = response_pair(local_response=True)
        rows[1]["local_response"] = False
        target = audit(rows)[0]["responses"][1]["targets"][0]
        self.assertIn("foreign_actor_snapshot", target["observation_notes"])
        self.assertEqual(target["damage_attribution"], "unknown")

    def test_tied_timestamp_has_no_ordered_delta(self):
        rows = response_pair()
        rows[3]["ticks"] = rows[1]["ticks"]
        target = audit(rows)[0]["responses"][1]["targets"][0]
        self.assertIsNone(target["snapshot_hp_delta"])

    def test_repeated_byte_id_is_reported_not_unique(self):
        result = audit(response_pair())[0]
        self.assertEqual(result["repeated_response_keys"][0]["count"], 2)
        self.assertEqual(result["network_joined_responses"], 0)

    def test_session_restart_resets_baselines_and_call_ids(self):
        result = audit(response_pair() + response_pair())
        self.assertEqual(len(result), 2)
        for session in result:
            self.assertIsNone(session["responses"][0]["targets"][0]["snapshot_hp_delta"])

    def test_incomplete_session_is_preserved(self):
        rows = response_pair()[:-1] + [begin(pid=81)]
        result = audit(rows)
        self.assertFalse(result[0]["complete"])
        self.assertIsNone(result[0]["reported_dropped"])
        self.assertIn("capture_incomplete_drop_count_unknown", result[0]["responses"][0]["gaps"])
        self.assertFalse(result[1]["complete"])

    def test_reported_drops_and_target_truncation_are_separate(self):
        rows = response_pair()
        rows[1]["mob_count"] = 10
        rows[-1]["dropped"] = 3
        result = audit(rows)[0]
        self.assertIn("target_list_truncated", result["responses"][0]["gaps"])
        self.assertIn("capture_reports_unlocated_losses", result["responses"][0]["gaps"])

    def test_missing_mob_is_error_only_for_completed_capture(self):
        rows = [begin(), event(0, captured=1, mob_count=1)]
        self.assertIn("target_records_missing", audit(rows)[0]["responses"][0]["gaps"])
        with self.assertRaisesRegex(ValueError, "mob count"):
            audit(rows + [end(1)])

    def test_malformed_record_types_and_bounds_rejected(self):
        changes = [(0, "version", 4), (0, "capacity", 0), (1, "valid", "false"),
                   (1, "event", True), (1, "damage_id", 256), (1, "captured", 9),
                   (1, "pid", 43), (3, "event", 0), (5, "events", 3),
                   (5, "dropped", -1), (2, "index", 1), (4, "event", 0)]
        for index, key, value in changes:
            with self.subTest(index=index, key=key):
                rows = response_pair()
                rows[index][key] = value
                with self.assertRaises(ValueError):
                    audit(rows)

    def test_invalid_snapshot_does_not_seed_next_delta(self):
        rows = response_pair()
        rows[2]["valid"] = False
        self.assertIsNone(audit(rows)[0]["responses"][1]["targets"][0]["snapshot_hp_delta"])

    def test_identical_archive_content_detected(self):
        with tempfile.TemporaryDirectory() as directory:
            a, b = Path(directory)/"a.log", Path(directory)/"b.log"
            a.write_text(text(response_pair()))
            b.write_text("unrelated log line\n" + text(response_pair()))
            report = audit_files([a, b])
            self.assertEqual(len(report["identical_record_streams"]), 1)
            self.assertNotEqual(report["captures"][0]["sha256"], report["captures"][1]["sha256"])

    def test_unrelated_encoding_bytes_ignored_but_tagged_corruption_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory)/"mixed.log"
            source.write_bytes(b"unrelated degree: \xb0\n" + text(response_pair()).encode())
            self.assertEqual(audit_files([source])["captures"][0]["sessions"][0]["event_count"], 2)
            source.write_bytes(PREFIX.encode() + b"\xff")
            with self.assertRaises(ValueError):
                audit_files([source])

    def test_cli_rejects_overwrite_and_malformed_without_output(self):
        with tempfile.TemporaryDirectory() as directory:
            source, output = Path(directory)/"input.log", Path(directory)/"report.json"
            source.write_text(text(response_pair()))
            original = source.read_bytes()
            command = [sys.executable, str(Path(__file__).with_name("correlation_audit.py")),
                       "--capture", str(source), "--output"]
            result = subprocess.run(command + [str(source)], capture_output=True)
            self.assertEqual(result.returncode, 2)
            self.assertEqual(source.read_bytes(), original)
            source.write_text(PREFIX + "{")
            result = subprocess.run(command + [str(output)], capture_output=True)
            self.assertEqual(result.returncode, 2)
            self.assertFalse(output.exists())


if __name__ == "__main__":
    unittest.main()
