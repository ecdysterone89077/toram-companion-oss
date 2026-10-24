# Geometry baseline - 2026-10-19

> **Tooling note.** The Python sensor this record describes
> (`scripts/toram_geometry.py`, driven through `scripts/combat_observer.py
> capture --geometry`) was removed when the project was rewritten as a pure
> Rust workspace. The recovered formulas in this record still describe the
> game's own behaviour and remain the reference for the current observation
> stream, which reports the target distance from the game's `PlayerDistance`
> cache with an explicit source label. The current procedure is
> `specs/LIVE_TEST_RUNBOOK_20261021.md`.

The distance ambiguity is resolved for the inspected build. Extended sword range
and enhanced MP remain unresolved; no gameplay code or canonical binaries changed.

## Recovered calculation

Read-only live method metadata and bounded x64 disassembly establish:

```
PlayerDistance = max(0, sqrt(dx*dx + dz*dz) - virtual get_Size())
PlayerMeterDistance = ceil(PlayerDistance * 0.5)
```

`EnemyMobActionManagerBase.UpdateDist` ignores vertical separation. Its virtual
size slot resolves to `get_Size`, which reads `mobStatusMaster.size` when the master
exists. The observed Shell Mask has size 2. The meter getter calls a multiplier
whose constant is 0.5, then the same native target as `System.Math.Ceiling`.
Thus the HUD integer, target-adjusted cache, and centre separation are different
quantities. A displayed value must not be substituted for centre distance.

Current process addresses, useful only within PID 2260 / creation FILETIME
134368076866930507:

| Method | Address |
| --- | --- |
| EnemyMobActionManagerBase.UpdateDist | 0x7ffba63d1cc0 |
| EnemyMobActionManagerBase.get_Size | 0x7ffba63d3070 |
| EnemyMobActionManagerBase.get_PlayerMeterDistance | 0x7ffba63d3000 |
| Meter multiplier | 0x7ffba5b0d4e0 |
| Transform.get_position_Injected | 0x7ffba7e78840 |
| Resolved Unity native bridge | 0x7ffb9a511a00 |
| Native world-position evaluator | 0x7ffb9aa74000 |

Disassembly remains outside the repository under
`<temp-dir>/toram-geometry-20261019-2260`.
The UpdateDist unwind entry covered only its first region; `UpdateDist_full.txt`
includes the bounded continuation through its return. No method was invoked.

## Observer implementation

New `scripts/toram_geometry.py`, opt-in through `combat_observer.py capture
--geometry`, implements only the verified root-transform case. Sensor version is 3.

Managed `CharacterActionManagerBase.charaTransform` resolves by metadata; the
Transform object's typed `m_CachedPtr` points to its native object. The inspected
evaluator reads its access structure at +0x38, with data pointer and index. Data
+0x18/+0x20 contain 48-byte TRS entries and signed parent indexes. A parent of -1
allows the entry's first three floats to be read as root world position.
Any other parent is rejected. Index, read size, coordinates and metadata walks
are bounded. Native pointer, access structure, data header, parent and position
are reread to reject changes. The whole combat observation is read twice.

The managed thunk, resolved bridge and native evaluator must match SHA256 code
fingerprints before layout access. Each encountered target class must match the
inspected UpdateDist and virtual size getter. Fingerprints and profile identity
are in source; addresses are resolved from live metadata, not copied from this
document. Unknown code fails binding, and target/position failures report an
explicit unavailable value. The existing process-generation checks remain active.

The evaluator normally synchronizes a native job fence. The observer does not
call that synchronization or any game method. It retains `job_fence_nonzero` and
labels values as sampled storage without an engine update timestamp. Matching
reads do not establish an atomic game frame, job completion, or callback time.
Computed distances use Python arithmetic; small floating-point differences from
the native single-precision calculation are expected. Cache residuals may also
include update timing differences. The generic cache is not relabelled in old
recordings and the new geometry is not promoted into `distance.metres`.

## Recorded native run

Session: `work/observations/geometry-baseline-20261019-2260`.
Raw trace SHA256:
`3c95558aad62afeb7d0741b7d64552eb2396f3d84013fa6402682f3a13781b93`.

The immutable manifest records source hashes and on-disk game identities. UnityPlayer.dll
SHA256 is `43fb0aea1b80c74963dae88ec7fc1c3c1993d893ce94d12972000fcc9b653ad1`;
GameAssembly.dll is `a3faf7156734768a3ff7349d4b94de029b68b823d974bf52f92bbc77f93f46dd`.
The observer's report wording was clarified after capture; its capture-time hash
remains preserved. The same failed candidate core is loaded, SHA256
`5721f0d46536182d43df3853707e53e31588d5f75b2dfc8b6c23e05aba5f1b51`.
This is an all-features-OFF control with that core present, not a pristine client.

| Measurement | Result |
| --- | ---: |
| Rows / duration | 1,800 / 90 s |
| Source rejections | 169: 161 changed paired reads, 8 bounded-read failures |
| Valid action/resource rows | 1,631 |
| Geometry samples | 1,026 |
| Geometry unavailable on otherwise valid rows | 3 structural changes |
| Comparable target rows / identities | 50 / 1 |
| Pending negative target-ID rows excluded | 36 |
| Process-generation verified rows | 1,800 |
| IPC masks / command sequence | All 1,800: mask 0 / sequence 7 |
| Median / maximum interval | 49.995 / 53.563 ms |
| Median / maximum read window | 1.1721 / 14.5407 ms |
| Absolute cache residual median / p95 / maximum | 0.00000108 / 0.033331 / 0.150407 world units |
| Absolute cache residual below 0.0001 | 618 / 1,026 |

Errors break association and are retained. The 169 rejections are material gaps;
they are not repaired by interpolating movement or treating the cache as current.

The UI showed a selected Lv4 Shell Mask at 9 m before the normal-attack command.
Around 51 s the sampled centres were 20.954 world units apart. Native assist
movement approached to 8.964 at 54 s and 6.497 at 57 s. The first sampled normal
play, 363, begins at 57.351 s with `startTargetDist` 5.987463, `ActionRange` 4.0,
current centres 5.771503 and computed adjusted distance 3.771503. The target kept
moving during this interval; the stored start distance and sampled position are
not simultaneous. Play 366 begins at 59.350 s with centres 4.971083 and adjusted
distance 2.971083. There are 19 and 20 sampled active rows respectively.

The native attack begins near 6 centre units for a size-2 target, consistent with
the action's 4-unit adjusted range. This observation does not prove which exact
callback made the decision or establish remote acceptance of an extended range.

Target 14995's local HP changes 91 -> 43 at 57.800 s, returns to 91 at 57.901 s,
then local and received HP both become 43 at 57.951 s, all during play 363.
The temporary local rebound therefore also occurs in this OFF baseline; by itself
it is not proof of feature-specific rejection. Play 366 changes local HP 43 -> 0
at 59.750 s, followed by target loss without a sampled received zero. Neither
target loss nor a local zero is an independent kill receipt. MP stays at its 319
cap throughout, so this run supplies no new AMPR comparison.

## Validation and next decision

58 Python tests pass; all four Python files compile. New regressions cover
horizontal versus spatial distance, size subtraction/clamping, unsupported code,
invalid indexes, parented transforms, non-finite positions, changed pointers,
target association, visible job-fence uncertainty and cleanup after binding failure.
No Rust changes or rebuild were needed. Canonical hashes remain unchanged.
Game left idle, target list closed, HP 397/397; the capture process exited.

Use the adjusted metric and centre metric explicitly in future experiments.
Callback receipts and synchronized callback-time geometry remain outstanding.
Do not redeploy the unchanged range candidate or infer successful MP/range behavior
from these observations. The next mechanism needs evidence of the actual decision
and hit path, bounded by play ID, before feature A/B validation.
