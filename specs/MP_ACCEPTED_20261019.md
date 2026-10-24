# v95.6 accepted MP engine — truthful sampling, calibrated AMPR ladder

Date: 2026-10-19, Asia/Jakarta. Workspace 0.95.6, telemetry 956.

## Operator report (the bug)

MP filled to full after 1 attack with the engine on, then fell back to the
native recovery trajectory. Bar moves fast, server does not follow.

## Root cause

cmd 74 installed two client-side lies on top of a server-authoritative value:

- `get_AtkMpRecovery` returned a flat **2000** (native grants ~+13 server MP
  per accepted normal hit), so the client predicted a full bar after one hit.
- `NormalAttackAction.ActionHit` wrote `localMp = max` directly and published
  it as telemetry.

`PlayerGameStatus` keeps `localMp` (prediction) separate from `serverMp` (last
authoritative value from `AttackResponseData`). The server recomputes MP
deterministically from real stats, so every inflated prediction reconciled on
the next response. Worker status 0 proved dispatch, never acceptance — the
exact twin of the +1000 damage failure.

## Fix

- Hit path is now **read-only**: `sample()` instead of `refill()`; telemetry
  publishes truthful local/received values. No more fill-then-snapback.
- AMPR override is now **native + bonus** for the owned calculator only, with
  a ladder: **+10 [86] / +25 [87] / +50 [88]** (0 = native). Unowned
  calculators, missing binding, or engine OFF all fail closed to native.
- New `mp_ampr_bonus` IPC field (telemetry 956) and menu row + dashboard readout.
- Fast *accepted* MP still comes the proven way too: every accepted normal hit
  grants server MP, so F2 accepted crits + Instant Attack (72) multiplies the
  acknowledged gain with zero desync.

## Calibration (one hit per step)

1. Restart normally (v95.6 core), menu v95.6, F2 ON + F4 ON, select +10 [86].
2. Note server MP (menu MP readout after it settles), land ONE normal attack.
3. Accepted = server MP rises by clearly more than the native ~+13 and STAYS
   (no fall-back over the next seconds). Rejected = spike then fall-back.
4. Repeat for +25 [87], then +50 [88]. Keep the highest step with zero fallback
   across several enemies. If +10 falls back, stay Native (bonus 0) and use
   hit rate (72) for speed.

## Verification

82 workspace tests pass (protocol 4, wcore 55, companion 9, wboot 11, observer 3):
bonus mapping 86/87/88 (+10/+25/+50), invalid-cmd rejection, dispatch paths,
native fail-closed without binding, sample-only hit path, IPC roundtrip, menu
labels. Release staged + hash-verified under `work/release-v95.6-mp-accepted/`:

- companion.exe `1B98A7E78302339E5D40E38748C8A0C5FFF77CF291EE201191147EA478910171`
- src/wcore.dll `6153701EAEE0D351D6C0C4E330EFFEA9E9A39D69D033A62E6C1C173ACBFED027`
- src/wboot.exe `9ED28CF0D1C052D64FF0D9E9CE49FCE672DDDC4910DEB49B777FD981DE8C1978`

Build note: two mid-edit brace slips (a stray `]` in the observe guard, brace
drift while inserting tests) were caught by a delimiter-balance scan plus
`cargo test`; the suite passes 55/55 in wcore with the new MP coverage.
