"""Read a response capture and report what the client claimed against what the
server did with it.

The response capture is the only record that carries both sides of a damage
claim. An event holds the client's side: `request` is the requested value,
`native` is the value the game itself reported and `effective` is the value
that was applied. The mob records hold the server's side: `hp` is the
received HP, `previous_hp` is the HP before the hit, `allowed` is the
server's damage allowance and `violation` is the server's violation amount.

The critical-damage ladder is an *absolute* value, so the claim it makes is
not the same number as the hit that lands: the critical value is one term of
the damage calculation and the server applies its own proration, defence
and allowance to it. The claim and the received HP are therefore reported
side by side rather than compared as if they were the same quantity. What
settles acceptance is the violation amount and the received HP against a
round with a different claim on the same target.

Usage:
    python scripts/acceptance_report.py <capture.jsonl>
"""

from __future__ import annotations

import json
import sys
from collections import defaultdict
from pathlib import Path


def load(path: Path) -> tuple[list[dict], list[dict]]:
    """Return the events and the mob records of a capture."""
    events: list[dict] = []
    mobs: list[dict] = []
    with path.open(encoding="utf-8") as handle:
        for line in handle:
            line = line.strip()
            if not line:
                continue
            try:
                record = json.loads(line)
            except json.JSONDecodeError:
                continue
            kind = record.get("kind")
            if kind == "event":
                events.append(record)
            elif kind == "mob":
                mobs.append(record)
    return events, mobs


def mobs_by_event(mobs: list[dict]) -> dict[int, list[dict]]:
    grouped: dict[int, list[dict]] = defaultdict(list)
    for mob in mobs:
        grouped[int(mob.get("event", -1))].append(mob)
    return grouped


def received_delta(mob: dict) -> int | None:
    """The HP the mob lost, when the record carries both sides of it."""
    hp = mob.get("hp")
    previous = mob.get("previous_hp")
    if not isinstance(hp, int) or not isinstance(previous, int):
        return None
    if hp < 0 or previous < 0:
        return None
    return previous - hp


def summarise(events: list[dict], mobs: list[dict]) -> dict:
    """Summarise a capture: what was claimed, and what the server said."""
    grouped = mobs_by_event(mobs)
    rows: list[dict] = []
    violations = 0
    claims: dict[int, int] = defaultdict(int)

    for event in events:
        if not event.get("valid"):
            continue
        claim = event.get("effective")
        native = event.get("native")
        if not isinstance(claim, int):
            continue
        captured = grouped.get(int(event.get("event", -1)), [])
        for mob in captured:
            if not mob.get("valid"):
                continue
            violation = mob.get("violation")
            if isinstance(violation, int) and violation > 0:
                violations += 1
            if claim != 0:
                claims[claim] += 1
            rows.append(dict(
                event=event.get("event"),
                scope=event.get("calculation_scope"),
                claim=claim,
                request=event.get("request"),
                native=native,
                delta=received_delta(mob),
                hp=mob.get("hp"),
                allowed=mob.get("allowed"),
                violation=violation,
                proration=(mob.get("normal_exp"), mob.get("physical_exp"), mob.get("magic_exp")),
                mob_id=mob.get("mob_id"),
            ))

    deltas_by_claim: dict[int, list[int]] = defaultdict(list)
    for row in rows:
        if row["claim"] and row["delta"] is not None:
            deltas_by_claim[row["claim"]].append(row["delta"])

    return dict(rows=rows, violations=violations, claims=dict(claims),
                deltas_by_claim={k: v for k, v in deltas_by_claim.items()},
                events=len(events), mobs=len(mobs))


def format_report(summary: dict) -> str:
    lines: list[str] = []
    lines.append("capture: %d events, %d mob records, %d joined rows"
                 % (summary["events"], summary["mobs"], len(summary["rows"])))
    lines.append("")
    lines.append("%-6s %-10s %8s %8s %8s %10s %10s  %s"
                 % ("event", "scope", "claim", "native", "delta", "allowed", "violation", "proration"))
    for row in summary["rows"]:
        proration = "/".join(str(part) for part in row["proration"])
        lines.append("%-6s %-10s %8s %8s %8s %10s %10s  %s"
                     % (row["event"], row["scope"], row["claim"], row["native"],
                        row["delta"], row["allowed"], row["violation"], proration))
    lines.append("")
    lines.append("claims seen: %s" % (summary["claims"] or "none"))
    for claim, deltas in sorted(summary["deltas_by_claim"].items()):
        deltas = sorted(deltas)
        middle = deltas[len(deltas) // 2]
        lines.append("  claim %-6s n=%-4d received delta min=%-8s median=%-8s max=%s"
                     % (claim, len(deltas), deltas[0], middle, deltas[-1]))
    lines.append("server violations recorded: %d" % summary["violations"])
    return "\n".join(lines)


def main(argv: list[str] | None = None) -> int:
    args = list(sys.argv[1:] if argv is None else argv)
    if not args:
        print(__doc__.strip(), file=sys.stderr)
        return 2
    path = Path(args[0])
    if not path.exists():
        print("capture not found: %s" % path, file=sys.stderr)
        return 2
    events, mobs = load(path)
    print(format_report(summarise(events, mobs)))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
