# Range target candidate — 2026-10-18

## Live result: failed

The operator's final corrected report for BOTH comparisons is that the target
stayed at least 8 m away, distant attacks did not produce lasting HP reduction,
and close attacks worked. Earlier positive answer selections were mistakes and
must not be used as evidence of a working range mode.

The corrected candidate loaded alone in PID 2260. Its SHA-256 matched both staging
and the loaded shadow module:
5721F0D46536182D43DF3853707E53E31588D5F75B2DFC8B6C23E05ABA5F1B51.
All seven range hooks installed and activation returned status 0.

- Mixed comparison: work/range-candidate-2260.jsonl, 1,763 rows / 89.952 seconds.
- Distant-only comparison: work/range-distant-only-2260.jsonl, 1,468 rows / 74.993
  seconds. Same-identity samples for target 12449 include 91 -> 43 -> 91,
  91 -> 46 -> 91 and later 47 -> 0 -> 47. There are 13 sampled upward local
  corrections, generally 0.10–0.155 seconds after the local reduction.
- One cached HP change from 91 to 47 occurs in the distant-only recording.
  Distance is operator-reported, not independently recorded. That isolated
  cache change does not establish a reliable distant attack or accepted kill.
- Final counters: range_preparations=28, range_patches=28, range_hit_checks=0,
  range_hit_extensions=0, battle_target_fallbacks=0. The corrected target resolver
  was not exercised in the tested normal attacks, so it cannot explain success.
- Read-only inspection confirms NormalAttackAction's inherited CheckRangeHit
  slot 59 still pointed to 0x7FFBA5A978B0 while the base MethodInfo was replaced.
  Original bytes b0 01 c3 decode to mov al,1;ret. The native check already succeeds;
  merely covering this derived slot cannot establish accepted distant damage.

Range OFF restored all seven owned hooks and snapshots. Worker stop logged
owned hook restoration complete=true; same-core restart/probe succeeded. Final
state is all modes OFF, HP 397/397, MP 319/319. Canonical binaries remain unchanged;
the candidate was not promoted. Session log: work/range-candidate-live-2260.log.
Enhanced MP remains unresolved and was not retested without a new mechanism.

## Status

The complete requested MP/range fix remains unresolved. This is a staged range
target-resolution correction that failed its live extended-melee comparison.
The installed canonical release remains the previously validated v95.1.

Candidate artifacts: `work/range-target-candidate`, with a SHA-256 manifest and
`run_candidate.bat`. The source version remains 0.95.1; distinguish this candidate
by its core hash and additional probe counters. A normal game restart is needed
before loading this different core. Do not stack it with the existing core.

## Reproduced source defect

The earlier range recorder contains 1,176 rows. Of 118 rows with a target,
95 have `selected_target_present=false` and use the active battle's main target;
23 have an explicit TargetManager selection. The local HP reductions at elapsed
15.113 and 19.179 seconds occur while selection is absent. A reduction at 17.119
seconds occurs with explicit selection present and still rebounds. Consequently
the selection defect cannot by itself explain every rejected ranged attack.

Before this change, RangeBinding::selected_target accepted only
TargetManager.actionManager. It rejected the still-active battle target when
that transient selection pointer cleared.

The revised resolver uses the main battle target only when explicit selection
is absent, the actor and battle classes match, and IsBattleActive is exactly true.
It validates TargetData and the target action/transform. A different explicitly
selected enemy takes precedence. Inactive retained targets and other actors are
rejected. Normal-attack preparation now checks the exact local actor identity,
in addition to its runtime class.

Additional probe counters report range_hit_checks, range_hit_extensions and
battle_target_fallbacks. These establish dispatch and selection behavior only.
If range_hit_checks stays zero, the MethodInfo replacement is not a demonstrated
interception of the relevant native call sites. No claim of server acceptance
follows from preparation counts, extended geometry or a local damage number.

## Native MP/range observations

Read-only inspection of the existing PID 19224 used PROCESS_VM_READ and bounded
ReadProcessMemory calls. No network requests were forged or replayed during this
investigation. GameAssembly base was 0x7FFBA5800000. Session addresses below are
observations, not constants for implementation.

- The current live metadata places PlayerAttackBase.SkillIndividualFlag at
  0x104. The native outgoing AttackStartData builder copies that value into
  its SkillIndividualFlag field at 0x64. The suspected flag-field mismatch was
  disproved; the candidate makes no flag-field change.
- AttackStartData's live fields include skill/level/local ID, target identity,
  target and actor positions, hit data, flags, timing, element and append data.
  Its direct field list has no MP recovery amount. This does not prove that all
  possible append-data/native skill paths lack resource effects.
- GameManager.UpdatePlayerStatusFromDamage reads received HP/MP values and
  forwards them through the player-data update path. GameMain's natural-recovery
  handler constructs NaturalRecoveryEvent from received event data and dispatches
  it to the local archetype. PlayerDataManager.OnNaturalRecovery forwards those
  received values to the main-player interface. No client-controlled authoritative
  MP recovery path was established.
- Earlier MP getter replacements recorded zero recovery calls during normal
  attacks. The existing local refill still does not demonstrate usable remote MP.
  MP mode remains experimental and must not be reported as fixed.

Raw native exports remain outside the repository under the temporary directories
combat-audit-19224, combat-native-paths-19224 and combat-network-paths-19224.
Ignored work/native_paths.py exports bounded native regions and method metadata;
an explicit --span includes adjacent code and must not be treated as a recovered
method length. The current unwind table can split one method into multiple ranges.

The operator confirmed that only the current sword character is available; no
native Power Wave or bow/bowgun comparison can be performed at this checkpoint.

## Verification

- Baseline: 58 workspace tests passed.
- Revised suite: 60 tests passed (40 core, 10 loader, 6 menu, 4 protocol).
- New regressions cover transient selection loss with active/inactive battle
  targets, explicit-target precedence, invalid target data, transform scoping,
  absent selection manager and another actor with the same runtime class.
- Release build succeeded and was staged separately; canonical binaries were
  not replaced. No new core was loaded into PID 19224.
- The operator restarted into PID 16272. First candidate loaded alone and probed
  valid HP/MP. Range activation returned -8 before installing hooks because
  TargetData was looked up as a top-level class. Read-only metadata established
  its declaring class as BattleManagerBase. Corrected candidate uses nested
  class resolution, again passes all 60 tests, and was rebuilt separately.
- Live candidate acceptance: FAILED; see the corrected result at the top.

## Comparison procedure

1. Restart Toram normally and enter a field. Open the candidate menu from its
   own directory. Confirm exactly one core whose hash matches the candidate.
2. Keep MP and other combat modes OFF. Probe, enable range only, and probe again.
3. Compare normal sword attacks near the target and at a measured extended
   distance while recording target identity, local HP and received HP for longer
   than the previously observed correction interval.
4. Inspect range_hit_checks/extensions/fallbacks. An unused geometry hook requires
   a dispatch correction before attributing a failed comparison to that filter.
5. Disable range and verify all hooks/snapshots restore. Do not promote the
   candidate or declare it fixed without lasting damage and an accepted kill.
