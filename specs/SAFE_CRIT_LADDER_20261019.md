# Critical-damage ladder — historical v0.95.4 record

## Correction — 2026-10-20

The universal "safe" claims and inferred validation mechanism below are
superseded by the current gameplay findings (development repository).
In the new capture, AllowDamage stays 42720 while individual HP drops vary or
are zero. It must not be interpreted as damage dealt. Zero ViolationDamage
does not prove success, and distributing a boost across several stat terms
has not been tested. Historical observations below do not establish a global
limit or rule out other effective calculation paths.

Date: 2026-10-19, Asia/Jakarta.

## Why +1000 failed (root cause)

Command 81 adds 1,000 percentage points to the native `get_CriticalDmg` result.
Native ~150 becomes ~1150, roughly a 7.6x critical multiplier at the inspected
native multiplication stage. The client predicts the inflated hit locally
(1,358 Critical + death animation in PID19500), but the server independently
holds the authoritative combat state (`MobResponseData.Hp / ReturnCode /
`AllowDamage / ViolationDamage`, observed via command 82). The overkill hit is
rejected server-side: received HP stays full and the client reconciles back,
leaving a living, unresponsive enemy. Worker status 0 only proves the setting
was dispatched, never that damage was accepted.

No client-side multiplier edit can force the server to accept 7.6x damage.
The fix is therefore NOT a bigger multiplier — it is boosts that stay inside
the server-accepted envelope, verified per-setting with the response observer.

## Fix: safe additive ladder + accepted DPS combo

New commands 83/84/85 add +100/+200/+300 percentage points to the CURRENT
native value (native 150 -> 250/350/450, i.e. 1.66x-3x crit instead of 7.6x).
They reuse the exact F2 getter hooks, nested-read guard, saturation and reset
semantics as the +1000 path. Command 81 (+1000) is retained for compatibility
but relabeled UNSAFE in the menu and telemetry; do not use it for real kills.

For actual killing power, combine a safe boost (or Native) with the proven
accepted DPS path: Stability + Crit (73) + Instant Attack (72) + Anti-Rigidity
(70). Each hit stays a legitimate max-roll crit the server already accepts;
speed delivers the damage instead of one rejected overkill hit.

## Calibration workflow (per-setting acceptance proof)

1. Restart the game normally so the v95.4 core loads. Open menu v95.4.
2. Enable Stability + Crit (F2 / 73). Select a safe boost: +100 [83] first.
3. Enable the response observer: Run Cmd 82 (logs request/native/effective per
   calculation plus per-hit MobResponseData Hp/AllowDamage/ViolationDamage/).
4. Land ONE normal attack on a fresh enemy, then Run Cmd 82 again to stop and
   flush `%TEMP%/wcore.log`.
5. Read the `[combat-response]` lines: accepted = received Hp drops and stays
   dropped, AllowDamage~=dealt, ViolationDamage=0, ReturnCode=success. Any
   ViolationDamage>0 or HP snap-back = rejected: drop to a lower step or Native.
6. Repeat for +200 [84], then +300 [85]. Use the highest step that shows zero
   violation across several fresh enemies. If +100 already violates, stay Native.
7. If a mob ever goes invulnerable after a test, select Native [79] and use a
   fresh enemy (confirmed recovery procedure).

## Implementation

- `protocol`: `CRIT_DAMAGE_ADD_100/200/300` (-100/-200/-300),
  `CommandId::CritDamageAdd100/200/300` (83/84/85), MAX=85, telemetry v954.
- `wcore`: `critical_damage_value` handles -100/-200/-300 via saturating_add;
  `job_set_crit_multiplier` maps 83/84/85; dispatch covers `83..=85`.
- `companion`: two-row crit layout (presets+Native / safe ladder+UNSAFE +1000),
  warning line, telemetry labels (`Native +100 safe`, ..., `Native +1000 UNSAFE`),
  window height 595->627. Custom Cmd entry follows MAX automatically (0..85).
- `wboot`: CLI usage 0..85 (range-checked via shared `CommandId::MAX`).
- Workspace version 0.95.4.

## Verification

`cargo test` per crate, all pass (80 total, 0 failed):

- protocol 4 (range incl. 83/84/85, MAX=85, additive IPC roundtrip).
- wcore 54 (new `safe_additive_ladder_adds_small_bonus_without_double_apply`:
  per-calculator +100/+200/+300, no stored-stat mutation, dispatch 84 path,
  Native reset; existing +1000/nested/overflow/forwarding tests still pass).
- companion 8 (v95.4 title, safe/UNSAFE labels).
- wboot 11, observer 3.

`cargo xtask build --no-test`: release built, staged, hash-verified:

- companion.exe `E94D13913AD67540A38E672FB3566335DB065B26568C44A0D7E342222E7D3088`
- src/wcore.dll `ECFB05DD1085965C29A6612D8877209E2D978CA9EA7C85157B0782CF9CE2A6BD`
- src/wboot.exe `4BDF01CEB097A97EC3C03DC95A5FA25A5F77B545AE08DB90F0F21894FA81AAA2`

Binary-string check confirms `+100 safe [83]`, `UNSAFE`, `v95.4` in companion.exe
and the new log text in wcore.dll. Copies + manifest under
`work/release-v95.4-safe-ladder/`.

## Limits

- Unit tests prove arithmetic/dispatch/labels, NOT server acceptance. The safe
  steps are candidates sized near the accepted envelope (+100 ~= existing 250%
  preset territory); live cmd-82 calibration per setting is still required.
- If all safe steps violate on the operator's build, the answer is Native +
  attack speed, not a larger multiplier.
- Prior +1000 failure record: `specs/bugs/BUG-2026-10-19-critical-damage-acceptance.md`.
