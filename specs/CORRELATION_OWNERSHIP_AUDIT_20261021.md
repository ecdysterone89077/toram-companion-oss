# Action/result correlation and calculator ownership — 2026-10-21

## Result

The offline continuation locates the missing joins and recovers a concrete
Hard Hit calculation path. None of the archived captures can identify an
originating critical calculation for a received damage result. The original
Hard Hit HP differences remain **0, 0, 1566**; their cause remains unresolved.

New findings:

- The response logger omits `AttackResponseData.LocalId` and `AttackCount`,
  even though both are also declared on `AttackData`.
- A damage ID is a byte backed by a reusable range counter. Allocation and
  sending are separate steps; an allocated ID does not prove a send occurred.
- `OnSkillActionHit` can process previously populated damage records and
  multiple targets. Its callback ID is not a per-calculation or per-send ID.
- The archived concrete Hard Hit method is identified by matching metadata
  token, virtual slot and a native call to its template wrapper. Earlier
  statements that this method had not been recovered are now superseded.
- Temporary battle status caches AMPR and critical magnitude independently.
  Its cache misses dispatch through the saved base calculator's interface;
  cache hits return the stored field. Actual execution in the failed captures
  is still unknown.
- Six archived logs contain 16 capture sections, all format 1. Five groups
  have identical record streams across files. These copies add no independent
  observations. No archived format-3 gameplay capture was found in these logs.

Tools and documentation are the deliverables of this pass. Runtime features,
packages and current account/process state were not tested or changed.

## Evidence and reproduction

The input inventory and original comparison hashes remain those in
[the full blind-spot audit](BLIND_SPOT_AUDIT_20261021.md). New local results:

- `scripts/correlation_audit.py`: strict format-1..3 reader, local-call
  grouping, HP-snapshot replay, completeness/loss annotations and archive
  duplication detection. Python 3.10+, standard library only.
- `scripts/correlation_audit_checks.py`: synthetic regression checks for false
  attribution, malformed input, mixed log encodings and output preservation.
- `work/correlation-audit-20261021/audit.json`: full six-file replay.
- `work/correlation-audit-20261021/verification.json`: hashes and check receipt.
- `specs/verification/correlation-ownership-20261021.yaml`: tracked receipt.

```powershell
python scripts/correlation_audit_checks.py
python scripts/correlation_audit.py `
  --capture work/gameplay-20261020-16932/comparison-session.log `
  --capture work/gameplay-20261020-16932/proration-response.log `
  --capture work/crit-response-18552/comparison.log `
  --capture work/incident-20261020-17024/wcore-before.log `
  --capture work/incident-20261020-17024/wcore-live-v961-14440.log `
  --capture work/incident-20261021-0306/wcore-preserved.log `
  --output work/correlation-audit-20261021/audit.json
```

Native evidence stays outside the repository under
`%TEMP%/combat-decompile-2260/`. The archived image SHA-256 is
`a2d9a3b246aae49eef1505d5e58aea7b7c55b2621fa4763d69025598fd10ee7c`,
recorded base `0x7ffba5800000`. Addresses below apply only to that image.
The current installed metadata hash matches the earlier inventory; this is
not a claim that the archived native image is the currently running build.

New exports are in `correlation-review-20261021/`:

- `export_slices.py`, `slices.json` and five bounded assembly listings;
  reproduced with Python 3.14.8 and Capstone 5.0.7 after verifying image hash.
- `CorrelationReview.java`, four `.c` exports, reference lists and logs;
  Ghidra 12.1.4, JDK 21, existing project opened with `-readOnly -noanalysis`.

The Java script has its exact four target addresses. Reproduction uses:

```powershell
$archive = Join-Path $env:TEMP 'combat-decompile-2260'
$evidence = Join-Path $archive 'correlation-review-20261021'
python (Join-Path $evidence 'export_slices.py')
& "$env:GHIDRA_INSTALL_DIR/support/analyzeHeadless.bat" `
  $archive CombatNative -process GameAssembly.live.dll -readOnly -noanalysis `
  -scriptPath $evidence -postScript CorrelationReview.java $evidence
```

Decompiled signatures are analysis artifacts. Older exports lose by-reference
information and some functions share native bodies; use tokens and native
instructions to resolve ownership, not a decompiler's selected function label.

## P0: exactly what is missing

| Stage | Present in current source | Missing for attribution |
| --- | --- | --- |
| Critical getter (`note_critical_damage`) | Requested/native/effective values, QPC, thread, optional local call/pointers and scope | Calculator instance/class, concrete skill and hit identity, originating configuration epoch |
| Hit callback (`ActionContext`, stage 4) | Local call, battle/target/action pointers, before/after resource samples | Stable actor/target IDs, skill/local ID, per-target calculation ID and outgoing request ID |
| Received attack (stage 2) | Actor type/ID, skill, byte damage ID, response flags, up to eight mob identities/HP/proration, received MP | Attack LocalId/AttackCount, outgoing record, originating calculation/configuration |
| Default response (stage 3) | Action code and return code | Target/skill/damage identity; it cannot be joined to the closest hit |
| Session end | Aggregate `dropped`, stored event count | Loss location and category, target truncation count, unmatched-stage accounting |

Source: `crates/wcore/src/combat_response.rs`, especially `ActionContext`,
`Event`, `Binding::response`, `around_action`, `note_critical_damage`, `attack`
and format-3 serialization. Response events do not inherit `ActionContext`;
their serialized `local_call` remains zero. `network_correlated:false` is
therefore an accurate description of the present format.

Even for a complete stage-4 bracket, grouping a stage-1 event by local call and
thread establishes callback membership only. It does not identify which
target or sent damage record consumed that calculation. Pointers are local
process observations and can be reused across lifetimes.

### Where the byte ID originates

Archived `decompiled/PlayerBattleManager.OnSkillActionDamaged.7ffba651ae80.c`
calls `GameManager.GetNextPlayerDamageId` before later conditional branches.
It passes that byte onward to `GameManager.AttackDamage` in explicit branches;
other branches dispatch through target callbacks. Early-return branches mean
ID allocation alone is insufficient evidence of a transmitted operation.

`GameManager.GetNextPlayerDamageId` at `0x7ffba696b950` delegates to
`0x7ffba5b71210`. The bounded native listing shows that function returns the
previous counter, increments storage, and resets to a configured minimum when
the configured maximum is reached. The counter bounds were not recovered in
this pass. A globally unique or permanently monotone damage ID is unsupported.

Both archived `GameManager.AttackDamage` overloads reach `0x7ffba7c1cf90`.
The four-argument overload forwards the supplied byte together with the
action's discriminator, low byte of its local ID and a target-damage list.
The fresh export of `0x7ffba7c1cf90` shows payload construction, byte-field
assignments, serialization through a virtual call, an action wrapper carrying
code 7, and `Game.SendAction`. This is a concrete outbound observation point;
the method's fully typed signature and the identity of every serialized field
still need verification before any callback implementation.

`AttackData` and `AttackResponseData` both declare `SkillId`, `LocalId`,
`DamageId` and `AttackCount`. `MobDamageData` separately declares
`DamageLocalId`. The target's `LocalId`, the attack's `LocalId`, and the
per-target `DamageLocalId` are different names/locations and must not be
collapsed into one field. The current response log records only target LocalId.

### Existing log replay

| Distinct completed record stream | Events | Calculations | Responses | Snapshot outcomes |
| --- | ---: | ---: | ---: | --- |
| Original PID 16932 comparison | 40 | 19 | 18 | 15 decreases, 2 unchanged, 1 baseline |
| Earlier PID 18552 first section | 2 | 1 | 1 | 1 baseline |
| Earlier PID 18552 second section | 22 | 11 | 11 | 10 unchanged, 1 baseline |
| PID 16932 proration follow-up | 0 | 0 | 0 | No combat observations |

The 16 sections comprise 12 completed and four incomplete sections before
accounting for copies. There are four distinct completed record streams.
Repeated bytes establish duplicate stored evidence; they do not establish a
unique acquisition/session identity for empty logs.

Every archived calculation lacks a local call, actor, concrete skill and
outgoing ID. All **30 responses across the three distinct nonempty completed
streams remain unjoined**. The earlier logger also filtered calculations to
normal-attack scope (`work/finalize-v96.1/source/crates/wcore/src/combat_response.rs`).
A nearby calculation cannot supply a Hard Hit critical value.

All 18 original comparison rows independently match `blind_spot_audit.py`.
The replay deliberately calls differences `snapshot_hp_delta`, never accepted
damage. Local actor membership is not present in format 1; one actor in a
capture does not exclude unobserved contributions from other actors/actions.

### Loss accounting is broader than the end counter

`DROPPED` combines failed buffer writes and some binding/resource lock misses.
It does not count every missing observation:

- `Binding::response` caps the mob list at eight without incrementing it.
- Invalid response objects become invalid events; they are not drop counts.
- Invalid/foreign action context can produce no stage-4 record.
- HP-history lock contention records an internal timestamp gap, not a
  categorized loss in the capture.
- Inactive capture and callbacks outside installed dispatch coverage produce
  no records. Zero reported drops says nothing about those paths.
- The response callback can feed adaptive state after a buffer-write loss.
  A stored log is not necessarily a complete ledger of adaptive decisions.

## P1: recovered calculator ownership

### Concrete Hard Hit and its shared calculation

`mp-dispatch-review/resolved-focused-methods.json` resolves
`HardHitAction.calcPlayerToMobDamage` to `0x7ffba670aea0`, token `0x06005860`,
virtual slot 55, arity 2. The token and arity match the installed declaration.
Slot 55 maps to class `+0x4a8` in this saved layout, the same dispatch used by
`SkillActionBase.CalcDamage`. The archive contains its actual native body and
bounded assembly; it is not the base-class return stub.

Its native prefix reads the battle-calculator interface via status slot 7,
queries interface index 27 (`get_Critical`), and calls template wrapper
`0x7ffba656c7d0`. New bounded disassembly verifies that wrapper calls
`0x7ffba6568d20` and tail-jumps to shared template code `0x7ffba656b680`.
The saved shared-template body reads interface index 28 (`get_CriticalDmg`)
and has separate index-29 (`get_CriticalMagicDmg`) branches. These interface
indices match the installed 51-method contract and the archived interface
dispatch helper at `0x7ffba5802ec0`.

This identifies a concrete static consumer of the selected battle calculator.
It does not identify the calculator object actually used by damage IDs 37/39,
nor the branch/value consumed in those actions. Template/final-limit evidence
must still be connected to a sent record and received result.

### Replacement, flags and cached getters are distinct

| Path | Archived behavior | Implication |
| --- | --- | --- |
| `PlayerStatus.TemporaryPlayerStatus` | Saves battle interface at object `+0x30` to `+0x38`, creates temporary wrapper with previous interface as base, replaces `+0x30` | Consumers of battle status can see a different calculator object |
| `PlayerStatus.RestorePlayerStatus` | Restores saved interface to `+0x30` when present | Restore is an object-selection transition |
| `SkillActionBase.TemporaryStatus/RestoreStatus` | Fresh exports write only action flag `+0x90` to one/zero | These names alone do not imply calculator replacement |
| `TemporaryPlayerStatus.get_AtkMpRecovery` | Flag bit `0x10`; base at `+0x18`; first read uses interface index 26 and stores `+0x30`; later reads return cache | Base-getter count can differ from temporary-getter count |
| `TemporaryPlayerStatus.get_CriticalDmg` | Flag bit `0x200`; first read uses interface index 28 and stores `+0x44`; later reads return cache | Configuration changes can occur after this value was cached |

The cached paths and offsets are corroborated by fresh bounded disassembly.
The constructor stores the supplied base; `CheckInitializeStatus` checks/sets
an initialization bit. This is concrete lazy caching, not just a field-name
hypothesis.

`SkillActionBase.CalcDamage` activates temporary buffers, conditionally calls
status slot 24 (`TemporaryPlayerStatus`), dispatches the concrete calculation,
then invalidates buffers. `OnSkillActionHit` has a later branch that clears
the action flag and calls status slot 25 (`RestorePlayerStatus`). Conditions
matter: this is not proof that every Hard Hit or every normal hit installs the
temporary wrapper. It also does not establish an executed AMPR consumer in
the recorded zero-callback sessions.

## Required observation contract for the next pass

This is a data requirement, not an implemented capture format:

1. **Capture/configuration identity:** unique capture ID, build/metadata hash,
   local actor identity, configuration epoch, feature mask and operator phase.
   Stamp the originating calculation and sending stage, not only the response.
2. **Calculation identity:** sequence, action lifetime, concrete class/skill,
   complete target identity, calculator instance/class/base identity, cache
   state, native/requested/effective values, template flags, final local amount
   and relevant per-target limit. Allow multiple calculations per action.
3. **Allocation and send:** separate records. Capture request sequence,
   attack LocalId/DamageId/AttackCount, all targets and per-target DamageLocalId,
   final serialized amount and the calculation IDs actually consumed.
4. **Received result:** actor and complete attack/target identity, result flags,
   HP/resource/proration state, and send-candidate cardinality. Repeated keys,
   missing records and multiple candidates remain unknown; never choose the
   closest timestamp to resolve ambiguity.
5. **Losses:** categorized counts and sequence gaps for buffer exhaustion,
   contention, invalid data, incomplete target lists, unsupported scope and
   missing boundaries. Bound pending history and expose evictions/ID reuse.
6. **Attribution:** a unique wire-request match still does not establish that
   the whole HP difference came from it. Preserve snapshot deltas separately
   from per-action damage claims and record competing contributions.

Next offline work: resolve the outbound adapter's exact managed/serialized
types and the received LocalId/AttackCount layout, then follow concrete AMPR
consumers and temporary-cache writes. Runtime correlation and the zero-damage
cause stay open until the full chain is observed. The 135% cap remains an
implementation policy, with no new remote acceptance evidence.

## Verification

- 19 Python regression tests pass, including false proximity joins, reused
  damage IDs, context/thread mismatches, missing brackets, partial targets,
  corrupt records, copied archives and input/output preservation.
- Six-file report repeats byte-for-byte; all 18 original rows match the
  independent earlier replay. Hard Hit deltas remain 0/0/1566.
- Archived image hash matches its original receipt. Five bounded disassembly
  exports reproduce; four fresh read-only Ghidra decompilations completed.
- Runtime Rust source was unchanged. The earlier 157-test workspace result
  remains historical; this pass did not rerun it or claim new gameplay tests.
