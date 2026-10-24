#!/usr/bin/env python3
"""Decode the ARM64 immediates Faunas writes, so the port's value ladders are
taken from the original bytes rather than from memory.

Faunas writes four-byte instruction words in memory byte order. The prologue
override it uses is `mov w0, #imm` (`MOVZ Wd, #imm16, LSL #hw`), optionally
followed by `ret` (`0xD65F03C0`).

Usage:
    python scripts/faunas_imm_decode.py [reference.lua]
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

MOVZ32 = 0x52800000
RET = 0xD65F03C0
MOVK32 = 0x72800000
MOVN32 = 0x12800000


def word_from_hex(text: str) -> int:
    raw = bytes.fromhex(text)
    if len(raw) != 4:
        raise ValueError(f"not a 32-bit word: {text}")
    return int.from_bytes(raw, "little")


def decode_movz(word: int) -> tuple[int, int] | None:
    """Returns (immediate, destination) for a 32-bit MOVZ, else None."""
    if word & 0x7F800000 != MOVZ32:
        return None
    imm16 = (word >> 5) & 0xFFFF
    hw = (word >> 21) & 3
    rd = word & 0x1F
    return imm16 << (hw * 16), rd


def main() -> int:
    path = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(
        "work/reference/faunas_release.lua"
    )
    text = path.read_text(encoding="utf-8", errors="replace")
    # Every literal the script writes.
    values = re.findall(r"value\s*=\s*'([0-9A-Fa-f]{8})\s*r?'", text)
    decoded: dict[str, int] = {}
    unknown: list[str] = []
    for value in values:
        word = word_from_hex(value)
        result = decode_movz(word)
        if result is None:
            if word == RET:
                decoded[value] = -1  # ret
                continue
            unknown.append(value)
            continue
        decoded[value] = result[0]

    print("== every distinct immediate the script writes ==")
    for value in sorted(decoded, key=lambda v: decoded[v]):
        immediate = decoded[value]
        shown = "ret" if immediate == -1 else str(immediate)
        print(f"  {value}  ->  {shown}")

    if unknown:
        print("\n== words that are not a MOVZ immediate ==")
        for value in sorted(set(unknown)):
            print(f"  {value}  ->  0x{word_from_hex(value):08X}")

    # The three ladders the port reproduces.
    print("\n== ladders ==")
    ladders = {
        "prorate": ["C01280D2", "001980D2", "201C80D2", "401F80D2",
                    "602280D2", "802580D2", "C02B80D2"],
        "critdmg": ["201C80D2", "401F80D2", "802580D2", "602780D2", "A02880D2",
                    "C02B80D2", "E02E80D2", "003280D2", "203580D2", "403880D2"],
        "critmgcdmg": ["A00F80D2", "401080D2", "E01080D2", "801180D2", "201280D2",
                       "C01280D2", "601380D2", "001480D2", "000280D2", "401580D2"],
        "element": ["00008052", "20008052", "40008052", "60008052",
                    "80008052", "A0008052", "C0008052"],
        "motion": ["800780D2"],
        "stable": ["007D80D2"],
        "hit": ["E0FF9F52"],
    }
    for name, entries in ladders.items():
        values = []
        for entry in entries:
            result = decode_movz(word_from_hex(entry))
            present = entry in decoded or entry in text.upper()
            values.append((result[0] if result else None, present))
        print(f"  {name}: {values}")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
