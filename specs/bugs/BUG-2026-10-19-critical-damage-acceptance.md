# BUG: +1000 critical damage produces failed combat acceptance

## Fix status 2026-10-19 (v0.95.4): safe ladder shipped, +1000 retired as UNSAFE

- Root cause: 7.6x local overkill vs server-authoritative Hp/AllowDamage/
  ViolationDamage. Status 0 = dispatched, never accepted.
- v0.95.4 adds server-envelope-sized boosts +100/+200/+300 (cmds 83/84/85)
  on the same F2 hooks; +1000 (81) kept but labeled UNSAFE. 80 tests pass,
  release staged + hash-verified (`work/release-v95.4-safe-ladder`).
- Live acceptance of each safe step still requires the cmd-82 one-hit
  calibration in `specs/SAFE_CRIT_LADDER_20261019.md`. If every step violates,
  the fixed killing setup is Native + Instant Attack + Anti-Rigidity.

---


Date: 2026-10-19, Asia/Jakarta.
Status: **+1000 unresolved; Native restored and normal kills reported.**

## Report and reproduction context

The operator enabled the v0.95.2 +1000 CD setting in a fresh ToramOnline process,
PID19500, and reported that large damage numbers and a death animation appeared
but enemies became invulnerable. Both supplied screenshots show 1,358 Critical
over a Torpo with a full green HP bar; the second also shows an enemy attack
telegraph. The screenshots do not contain received response values.

The current core file hash matches the staged v95.2 implementation:
`5858CF58B81F1E71FFA0E9C5E5C18FC14E834E80BCC3396CB0121EB45C594D65`.
The preserved log records the fresh PID19500 attach, invincibility (32), Stability +
Crit Settings (73), and `cmd=81 -> requested=-1000`, each with status 0.
Timing, range and MP callbacks are zero in the subsequent probe; normal,
stability, clean-hit and scoped critical-fallback counters each reached 26.

These observations establish active calculation paths and a failed gameplay
outcome. They do not establish a successful kill or remote acceptance merely
because the worker returned status 0.

## Recovery comparison

Preserved the log, then dispatched:

```text
[result] pid=19500 cmd=1 status=0
[result] pid=19500 cmd=79 status=0
```

The probe resolved PlayerStatus `0x1cf349c36c0` and primary values
`[80, 1, 1, 1, 1, 1]`. Native reset set the critical request to zero without
disabling stability/critical selection or changing the other modes.

Asked the operator: "On a fresh enemy, do ordinary attacks now leave its HP
reduced and allow a normal kill?" The answer was **"Yes, fresh enemies work
normally."** This is a controlled operator observation isolating the added
critical-damage override as the failing change in this comparison.

No +1000 re-enable, game restart, new DLL load, response-field edit, HP edit or
death-flag edit was performed during recovery.

## Read-only Native capture

Existing metadata-bound `work/sample_target_hp.py` sampled the same process for
30 seconds at a requested 50 ms interval after the Native receipt. It uses
PROCESS_VM_READ, bounded typed reads and a single bound process handle. It does
not invoke methods, install hooks, drive input or write game memory.

Saved file: `work/crit1000-failure-19500/native-recovery-hp.jsonl`.

| State | Rows |
|---|---:|
| sample | 29 |
| no_target | 519 |
| no_mob_status | 43 |
| unavailable | 1 |
| total | 592 |

Five samples have pending UniqueId -1 and cannot establish continuous target
identity. Positive target IDs 30131 and 30132 each have 12 samples, beginning at
local/received HP 108/108 and ending at 0/108 before target loss. No received
HP=0 was sampled. The capture therefore does not independently prove acknowledged
kills or measure accepted damage for the original failed +1000 hits. The
operator's normal-kill confirmation is separate evidence.

## Mechanism and remaining uncertainty

The implementation changes the local `get_CriticalDmg` calculation result before
native damage multiplication. Its arithmetic, nested-read guard and reset tests
passed. Those tests do not update or verify the remote server's combat state.

Saved native instructions establish the local critical multiplier getter,
division by 100 and numeric multiplication. Existing declarations show
`MobDamageData.Damage` (int) and `SkillCalc` (float array), alongside separate
received `MobResponseData.Hp`, `ReturnCode`, `AllowDamage` and `ViolationDamage`.
Declaration names and old dump offsets are not observed failure responses or a
recovered server formula. No packet/response callback capture was available for
the failed attacks, and those response values were not changed or fabricated.

The combined screenshots, active-setting receipt and Native recovery are
consistent with local lethal prediction disagreeing with accepted combat state.
Whether a server damage check, another response condition or subsequent client
reconciliation is the exact rejection branch remains unverified. No numerical
cap is established. Ordinary critical chance/stability working with Native
damage does not establish that an arbitrary additional multiplier is accepted.

An actual fix still requires identifying an accepted higher-damage mechanism and
verifying lasting damage and normal kills. Rewriting local HP/death state or
suppressing received corrections would not demonstrate that outcome.

## Saved evidence and current state

- `work/crit1000-failure-19500/before-native.log`.
- `work/crit1000-failure-19500/after-native.log`.
- `work/crit1000-failure-19500/native-recovery-hp.jsonl`.
- `work/crit1000-failure-19500/native-recovery-summary.json`.
- Supplied screenshots preserved in the same ignored evidence directory.
- [Original implementation record](../CRIT_DAMAGE_1000_20261019.md).
- Prior native dispatch evidence (development repository).

Gameplay source and canonical binaries remain the tested v95.2 implementation.
The active setting is Native. The 75 earlier workspace tests remain arithmetic
and lifecycle evidence; no new code was changed and no test suite was rerun for
this documentation/recovery-only investigation.
