"""Regression checks for the acceptance report.

The report is what turns a capture into a reading of the server's verdict, so
it has to join the event rows to the mob rows correctly, ignore records that
do not carry both sides of a claim, and count a violation only when the
server actually recorded one.
"""

import json
import tempfile
import unittest
from pathlib import Path

from acceptance_report import format_report, load, received_delta, summarise


def event(index, claim=225, native=168, scope="normal_attack", valid=True):
    return {"kind": "event", "event": index, "valid": valid, "request": claim,
            "native": native, "effective": claim, "calculation_scope": scope}


def mob(index, hp=775, previous=1000, allowed=42720, violation=0, valid=True):
    return {"kind": "mob", "event": index, "valid": valid, "hp": hp,
            "previous_hp": previous, "allowed": allowed, "violation": violation,
            "normal_exp": 0, "physical_exp": 0, "magic_exp": 0, "mob_id": 42}


def write_capture(records):
    handle = tempfile.NamedTemporaryFile("w", suffix=".jsonl", delete=False,
                                         encoding="utf-8")
    try:
        for record in records:
            handle.write(json.dumps(record) + "\n")
        handle.write("not json\n")
    finally:
        handle.close()
    return Path(handle.name)


class AcceptanceReportChecks(unittest.TestCase):
    def test_events_join_to_their_mobs_by_event_index(self):
        path = write_capture([event(0), mob(0), event(1), mob(1)])
        events, mobs = load(path)
        summary = summarise(events, mobs)
        self.assertEqual(len(summary["rows"]), 2)
        self.assertEqual([row["event"] for row in summary["rows"]], [0, 1])

    def test_the_received_delta_is_the_hp_the_mob_lost(self):
        self.assertEqual(received_delta(mob(0, hp=775, previous=1000)), 225)
        self.assertIsNone(received_delta(mob(0, hp=-1, previous=1000)))
        self.assertIsNone(received_delta({"hp": 10}))

    def test_a_violation_is_only_counted_when_the_server_recorded_one(self):
        path = write_capture([event(0), mob(0, violation=0),
                              event(1), mob(1, violation=1158)])
        summary = summarise(*load(path))
        self.assertEqual(summary["violations"], 1)

    def test_invalid_records_are_left_out(self):
        path = write_capture([event(0, valid=False), mob(0), mob(1, valid=False)])
        summary = summarise(*load(path))
        self.assertEqual(summary["rows"], [])

    def test_claims_are_counted_by_value(self):
        path = write_capture([event(0, claim=225), mob(0),
                              event(1, claim=225), mob(1),
                              event(2, claim=450), mob(2)])
        summary = summarise(*load(path))
        self.assertEqual(summary["claims"], {225: 2, 450: 1})

    def test_the_deltas_are_grouped_by_claim(self):
        path = write_capture([event(0, claim=225), mob(0, hp=775, previous=1000),
                              event(1, claim=225), mob(1, hp=800, previous=1000),
                              event(2, claim=450), mob(2, hp=550, previous=1000)])
        summary = summarise(*load(path))
        self.assertEqual(sorted(summary["deltas_by_claim"][225]), [200, 225])
        self.assertEqual(summary["deltas_by_claim"][450], [450])

    def test_the_report_reads_the_claim_and_the_verdict_together(self):
        path = write_capture([event(0, claim=225, native=168), mob(0, hp=775)])
        report = format_report(summarise(*load(path)))
        self.assertIn("225", report)
        self.assertIn("168", report)
        self.assertIn("violations recorded: 0", report)

    def test_a_capture_with_nothing_in_it_still_reports(self):
        path = write_capture([])
        summary = summarise(*load(path))
        self.assertEqual(summary["rows"], [])
        self.assertIn("claims seen: none", format_report(summary))

    def test_malformed_lines_are_skipped(self):
        path = write_capture([event(0), mob(0)])
        events, mobs = load(path)
        self.assertEqual(len(events), 1)
        self.assertEqual(len(mobs), 1)


if __name__ == "__main__":
    unittest.main()
