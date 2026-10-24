"""Audit archived combat-response formats 1..3 without inventing network joins.

Python 3.10+, standard library only. Local call grouping is separate from
received HP differences. No process access or game modification is performed.
"""

import argparse
import collections
import hashlib
import json
import sys
from pathlib import Path


PREFIX = "[combat-response] "
CONTEXT = ("battle", "target", "target_actor", "action", "action_class")


def integer(record, name, minimum=None, maximum=None):
    value = record.get(name)
    if type(value) is not int:
        raise ValueError(f"{name} must be an integer")
    if minimum is not None and value < minimum:
        raise ValueError(f"{name} is below {minimum}")
    if maximum is not None and value > maximum:
        raise ValueError(f"{name} exceeds {maximum}")
    return value


def boolean(record, name):
    if type(record.get(name)) is not bool:
        raise ValueError(f"{name} must be boolean")
    return record[name]


def parse_sessions(text):
    sessions = []
    current = None
    for line_number, line in enumerate(text.splitlines(), 1):
        if PREFIX not in line:
            continue
        try:
            record = json.loads(line.split(PREFIX, 1)[1])
            if not isinstance(record, dict):
                raise ValueError("record must be an object")
            kind = record.get("kind")
            if kind == "begin":
                integer(record, "version", 1, 3)
                integer(record, "pid", 1)
                integer(record, "frequency", 1)
                integer(record, "capacity", 1, 1_000_000)
                integer(record, "max_mobs", 1, 4096)
                current = {"begin": record, "events": [], "mobs": {}, "end": None,
                           "records": [record], "start_line": line_number}
                sessions.append(current)
                continue
            if current is None:
                raise ValueError("record outside a capture")
            if kind == "event":
                if record.get("pid") != current["begin"]["pid"]:
                    raise ValueError("event PID differs from capture")
                index = integer(record, "event", 0)
                if index != len(current["events"]):
                    raise ValueError("event IDs must be contiguous from zero")
                if index >= current["begin"]["capacity"]:
                    raise ValueError("event count exceeds capacity")
                integer(record, "stage", 1, 4)
                integer(record, "ticks", 0)
                integer(record, "thread", 1)
                boolean(record, "valid")
                integer(record, "captured", 0, current["begin"]["max_mobs"])
                integer(record, "mob_count", record["captured"], 4096)
                if record["stage"] == 2:
                    integer(record, "actor_type", 0, 255)
                    integer(record, "actor_id")
                    integer(record, "skill")
                    integer(record, "damage_id", 0, 255)
                    integer(record, "game_code")
                for key in ("local_response", "action_exit", "network_correlated"):
                    if key in record:
                        boolean(record, key)
                if "local_call" in record:
                    integer(record, "local_call", 0)
                current["events"].append(record)
                current["mobs"][index] = []
            elif kind == "mob":
                index = integer(record, "event", 0)
                if not current["events"] or index != current["events"][-1]["event"]:
                    raise ValueError("mob must follow its owning event")
                if current["events"][-1]["stage"] != 2:
                    raise ValueError("mob belongs to a non-response event")
                mobs = current["mobs"][index]
                if integer(record, "index", 0) != len(mobs):
                    raise ValueError("mob indices must be contiguous from zero")
                boolean(record, "valid")
                for key in ("mob_id", "unique_id", "hp", "code", "allowed", "violation"):
                    integer(record, key)
                integer(record, "local_id", 0, 255)
                mobs.append(record)
            elif kind == "end":
                if record.get("pid") != current["begin"]["pid"]:
                    raise ValueError("end PID differs from capture")
                if integer(record, "events", 0) != len(current["events"]):
                    raise ValueError("end count differs from captured events")
                integer(record, "dropped", 0)
                boolean(record, "restored")
                current["end"] = record
            else:
                raise ValueError(f"unsupported record kind: {kind}")
            current["records"].append(record)
            if kind == "end":
                current = None
        except (ValueError, KeyError, TypeError) as error:
            raise ValueError(f"line {line_number}: {error}") from error
    if not sessions:
        raise ValueError("no combat-response captures found")
    for session in sessions:
        for event in session["events"]:
            actual = len(session["mobs"][event["event"]])
            if actual > event["captured"] or (session["end"] and actual != event["captured"]):
                raise ValueError("mob count differs from event's captured count")
    return sessions


def local_groups(events):
    grouped = collections.defaultdict(list)
    for event in events:
        if event.get("local_call", 0) > 0:
            grouped[event["local_call"]].append(event)
    results = []
    for call, rows in sorted(grouped.items()):
        entries = [e for e in rows if e["stage"] == 4 and e.get("action_exit") is False]
        exits = [e for e in rows if e["stage"] == 4 and e.get("action_exit") is True]
        calculations = [e for e in rows if e["stage"] == 1]
        reasons = []
        if len(entries) != 1 or len(exits) != 1:
            reasons.append("missing_or_duplicate_action_bracket")
        if len({e["thread"] for e in rows}) != 1:
            reasons.append("call_id_spans_threads")
        if any(not e["valid"] for e in rows):
            reasons.append("invalid_local_record")
        if any(any(e.get(k) in (None, "0x0", 0) for k in CONTEXT) for e in rows):
            reasons.append("missing_context_identity")
        if len({tuple(e.get(k) for k in CONTEXT) for e in rows}) != 1:
            reasons.append("context_identity_changed")
        if len(entries) == len(exits) == 1:
            start, end = entries[0], exits[0]
            if start["ticks"] > end["ticks"] or start["event"] >= end["event"]:
                reasons.append("reversed_action_bracket")
            if any(not (start["event"] < e["event"] < end["event"] and
                        start["ticks"] <= e["ticks"] <= end["ticks"]) for e in calculations):
                reasons.append("calculation_outside_action_bracket")
        results.append({"local_call": call, "event_ids": [e["event"] for e in rows],
                        "calculation_events": [e["event"] for e in calculations],
                        "status": "local_callback_group" if not reasons else "unknown",
                        "reasons": reasons, "network_join": "unavailable"})
    return results


def audit_session(session):
    events, end = session["events"], session["end"]
    dropped = end["dropped"] if end else None
    previous = {}
    responses = []
    identities = collections.Counter()
    for event in events:
        if event["stage"] != 2:
            continue
        raw_key = (event["actor_type"], event["actor_id"], event["skill"], event["damage_id"])
        identities[raw_key] += 1
        rows = []
        for mob in session["mobs"][event["event"]]:
            key = (mob["mob_id"], mob["local_id"], mob["unique_id"])
            valid = (event["valid"] and event["game_code"] == 0 and mob["valid"] and
                     mob["code"] == 0 and mob["mob_id"] >= 0 and mob["unique_id"] >= 0 and mob["hp"] >= 0)
            prior = previous.get(key)
            delta = None
            reasons = []
            if not valid:
                reasons.append("invalid_or_error_response")
                previous.pop(key, None)
            elif prior is None:
                reasons.append("no_prior_snapshot")
            elif event["ticks"] <= prior["ticks"]:
                reasons.append("non_increasing_timestamp")
            else:
                delta = prior["hp"] - mob["hp"]
                if prior["local"] is False or event.get("local_response") is False:
                    reasons.append("foreign_actor_snapshot")
            if valid and (prior is None or event["ticks"] > prior["ticks"]):
                previous[key] = {"hp": mob["hp"], "ticks": event["ticks"],
                                 "local": event.get("local_response")}
            outcome = ("unknown" if delta is None else "hp_decrease" if delta > 0 else
                       "unchanged" if delta == 0 else "hp_increase")
            rows.append({"target": list(key), "hp": mob["hp"], "snapshot_hp_delta": delta,
                         "snapshot_outcome": outcome, "observation_notes": reasons,
                         "damage_attribution": "unknown"})
        gaps = ["no_outgoing_request_record", "no_calculation_to_request_join",
                "response_local_id_not_recorded", "response_attack_count_not_recorded",
                "origin_configuration_not_recorded", "exclusive_hp_contribution_not_established"]
        if "local_response" not in event:
            gaps.append("local_actor_membership_not_recorded")
        if dropped is None:
            gaps.append("capture_incomplete_drop_count_unknown")
        elif dropped:
            gaps.append("capture_reports_unlocated_losses")
        if event["captured"] < event["mob_count"]:
            gaps.append("target_list_truncated")
        if len(rows) != event["captured"]:
            gaps.append("target_records_missing")
        if not event["valid"] or event["game_code"] != 0:
            gaps.append("invalid_or_error_response")
        responses.append({"event": event["event"], "actor": list(raw_key[:2]),
                          "skill": event["skill"], "damage_id": event["damage_id"],
                          "request_at_response": event.get("request"),
                          "originating_calculation": None, "network_join": "unavailable",
                          "gaps": gaps, "targets": rows})
    groups = local_groups(events)
    calculations = [e for e in events if e["stage"] == 1]
    paired = {event for group in groups if group["status"] == "local_callback_group"
              for event in group["calculation_events"]}
    signature = hashlib.sha256(json.dumps(session["records"], sort_keys=True,
                                         separators=(",", ":")).encode()).hexdigest()
    return {"version": session["begin"]["version"], "pid": session["begin"]["pid"],
            "start_line": session["start_line"], "record_stream_sha256": signature,
            "complete": end is not None, "reported_dropped": dropped,
            "restored": end["restored"] if end else None,
            "event_count": len(events), "stage_counts": dict(sorted(collections.Counter(e["stage"] for e in events).items())),
            "local_groups": groups,
            "calculations_without_local_group": [e["event"] for e in calculations if e["event"] not in paired],
            "repeated_response_keys": [{"actor_type": k[0], "actor_id": k[1], "skill": k[2],
                                        "damage_id": k[3], "count": n} for k, n in sorted(identities.items()) if n > 1],
            "responses": responses,
            "network_joined_responses": 0,
            "snapshot_outcomes": dict(sorted(collections.Counter(t["snapshot_outcome"] for r in responses for t in r["targets"]).items()))}


def audit_files(paths):
    reports = []
    identical = collections.defaultdict(list)
    for path in paths:
        data = path.read_bytes()
        # Historical console logs mix encodings in unrelated lines. Decode only
        # tagged records strictly; damaged JSON must still fail the audit.
        lines = []
        for line in data.splitlines():
            if PREFIX.encode() in line:
                lines.append(PREFIX + line.split(PREFIX.encode(), 1)[1].decode("utf-8"))
            else:
                lines.append("")
        sessions = [audit_session(s) for s in parse_sessions("\n".join(lines))]
        for index, session in enumerate(sessions):
            identical[session["record_stream_sha256"]].append({"path": str(path), "session": index})
        reports.append({"path": str(path), "sha256": hashlib.sha256(data).hexdigest(), "sessions": sessions})
    return {"schema": 1, "captures": reports,
            "identical_record_streams": [v for _, v in sorted(identical.items()) if len(v) > 1],
            "limits": [
                "Formats 1..3 contain no outgoing-request record; no network join can be recovered.",
                "Local call grouping establishes only callback membership, not a wire action or target hit.",
                "HP differences are observations between snapshots, never exclusive damage attribution.",
                "The end counter combines some losses; zero does not establish complete observation coverage.",
                "Identical record content across archives is not independent supporting evidence."]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--capture", action="append", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    output = args.output.resolve()
    if output in [p.resolve() for p in args.capture] or output == Path(__file__).resolve():
        raise ValueError("output would overwrite an input or the tool")
    result = audit_files(args.capture)
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8", newline="\n")
    print(json.dumps({"captures": len(result["captures"]), "sessions": sum(len(c["sessions"]) for c in result["captures"]),
                      "identical_record_stream_groups": len(result["identical_record_streams"]),
                      "network_joined_responses": 0, "output": str(output)}, indent=2))


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, KeyError, TypeError) as error:
        print(f"Correlation audit failed: {error}", file=sys.stderr)
        raise SystemExit(2)
