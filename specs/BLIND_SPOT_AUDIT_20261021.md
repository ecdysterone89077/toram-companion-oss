# Blind spot audit — full local inventory — 2026-10-21

Follow-up: [correlation and calculator ownership](CORRELATION_OWNERSHIP_AUDIT_20261021.md)
recovers the archived concrete Hard Hit method and template consumer, traces
temporary caches and sending identities, and audits copied/incomplete logs.
It supersedes this audit's statement that concrete Hard Hit was unrecovered;
the historical zero-result cause and runtime correlation remain unresolved.

## Result

Substantial client behavior remains unexplored. The previous claims
"complete blind set", "no ceiling", "no eleventh variable", "0 risk" and
"skills only is always best" are not established by project evidence.
Neither is the opposite: a newly found declaration is not a verified useful
modification or remotely accepted effect.

This offline audit replaces the map, adds reproducible inventory/replay tools
and corrects the project handoff. Runtime hooks, feature values, installed
binaries and account state were left unchanged.

## Inputs and reproduction

| Input | Identity |
| --- | --- |
| Installed metadata | `C:/Asobimo/AsobimoGames/Toram/ToramOnline_Data/il2cpp_data/Metadata/global-metadata.dat`, version 31 |
| Metadata SHA-256 | `f0cf212a7c7dca1973a401bda6d208549c13f57f0bbdb8782e5d607ed7e8789f` |
| Parser | Existing `il2cppdumper/Il2CppDumper.dll`; hash in local symbol export |
| Feature tables | `crates/wcore/src/features.rs`; hash and hook source lines in `audit.json` |
| Current diagnostics | `crates/wcore/src/combat_response.rs`, format 3 |
| Original comparison | `work/gameplay-20261020-16932/comparison-session.log` |
| Comparison SHA-256 | `02c734b77c371240bd2df027c57ba6c3a74a3c6591f1f42754ad404c725c548f` |
| Archived native evidence | `%TEMP%/combat-decompile-2260/`; saved client snapshot, not current server code |

```powershell
& ./scripts/export-metadata-symbols.ps1 `
  -MetadataPath 'C:/Asobimo/AsobimoGames/Toram/ToramOnline_Data/il2cpp_data/Metadata/global-metadata.dat' `
  -DumperAssembly './il2cppdumper/Il2CppDumper.dll' `
  -OutputPath './work/blind-spot-audit-20261021/metadata-symbols.json'

py -3.14 scripts/blind_spot_audit.py `
  --symbols work/blind-spot-audit-20261021/metadata-symbols.json `
  --output work/blind-spot-audit-20261021/audit.json
```

Requirements: PowerShell/.NET compatible with the existing Il2CppDumper
assembly and Python 3.10+ standard library; this workspace used Python 3.14.
The parser requires a writable stream, so the exporter gives it a private
in-memory copy. The installed metadata is read only. Outputs cannot replace
the inputs. No game process or network operation is needed.

All **20,163 types, 157,413 methods and 96,686 fields** reconcile with parser
totals. Method owners and member ranges are checked. These checks do not
resolve parameter types, native addresses, bodies, callers or server rules.

## Findings

### 1. Two calculators were never a complete inventory

The 51-method interface has six concrete full name/argument-count matches:
player battle, player secondary, temporary player, mercenary, pet and Moba
secondary status. The old map considered only the first two.
`TemporaryPlayerStatus` has 53 methods and 48 fields, including a base
calculator, reduction/cut, motion-speed and resource-recovery state.
Archived `mp-dispatch-review/PlayerStatus.TemporaryPlayerStatus.asm.txt`
corroborates a status-object replacement path. Consumption remains to be
traced; the declaration is not proof that hooking it produces a useful effect.

Current `PlayerBattleStatus` is metadata index 1455 and `PlayerSecondaryStatus`
is 1466, while the old dump used 1454/1465. Never reuse indices without their
metadata identity. The current `PlayerAttackBase` has 159 methods and
`SkillActionBase` 151; combat extends well beyond the two getter lists.

### 2. Accepted outcomes do not establish remote blindness

Earlier critical/stability comparisons include successful melee hits and
completed fights. Those are scoped observations. They do not reveal all
server checks or guarantee all weapons, actions, targets, values or rates.
Stability 100 is the top of a finite band, not unlimited multiplication.
Normal critical fallback retains a native resistance check. Magic selection
uses different callbacks, and `get_CriticalMagicDmg` is not directly hooked.

Use the classification **local mechanism plus historical outcomes; remote
validation model unknown**. Do not classify an entire dimension as unchecked
because a particular configuration produced no immediate warning.

### 3. The 135% ratio is not a measured server threshold

Independent replay of the original capture produces:

| Measurement | Result |
| --- | --- |
| Completed events | 40, zero reported drops |
| Critical calculations | 19 |
| Recorded native critical value | **168**, not 150 |
| Target response snapshots | 18 |
| Outcomes | 1 baseline, 15 HP decreases, 2 unchanged |
| Hard Hit damage IDs | 37, 39, 40 |
| Their HP decreases | **0, 0, 1566** |
| Request sampled at response | 250 for all three |
| Their effective skill critical values | Not captured |
| Violation | 0 throughout |
| AllowDamage | 42720 throughout |
| Received proration bytes | Absent from this capture |

At native 168, absolute 200 is approximately **1.1905x** and 250 is **1.4881x**
the native critical factor. The source cap gives `floor(168 * 135 / 100) = 226`;
**226 was not the tested setting**. At the later assumed native 150, the cap
gives 202, also not the original tested 200. Interpolation is not a remote
boundary measurement.

`the_ratio_ceiling_preserves_the_observed_accepted_values` constructs native
150. Its passing arithmetic assertions cannot establish that historical
input, effective skill values or server acceptance.

Absolute presets can also reduce a higher native value:
`effective_critical_damage(200, 212, 135, true)` yields 200. The existing
"never drops below native" test checks that property for Native/additive
requests, not every absolute preset. Fixed 200 is not universally maximal.

### 4. The recorded zero-drop symptom does not lower the adaptive cap

`response_signal` returns rejection for explicit violations or zero allowance
against a living mob. It returns acceptance only for a complete valid set of
received HP decreases. Unchanged/increasing HP and incomplete baselines are
**unknown**.

Both original unchanged-HP skills had positive allowance and zero violation,
so they are unknown under the current classifier. Unknown clears the streak
but leaves the cap unchanged. The ladder cannot be claimed to solve those
failures. This uncertainty handling is deliberate; automatically treating
every zero as remote rejection would also overstate the evidence.

A newly seen target can restore a lowered ratio. The cap avoids increases on
ordinary acceptance feedback; it is not globally monotone across targets.
The bounded unique-ID ring is a local freshness heuristic, not a remote model.

### 5. Player HP has a received-state settlement channel

Installed `PlayerGameStatus` contains server/local HP, MP, ExHP and ExMP pairs,
respawn state and magic-gauge fields. Saved
`decompiled/GameManager.UpdatePlayerStatusFromDamage.7ffba6994a60.c` forwards
received HP to `PlayerDataManager.UpdateServerHP`, then received MP to
`UpdateServerMp`.

This contradicts the old blanket claim that the server does not track player
HP. It does not reveal every remote damage rule. The six invincibility hooks suppress
local damage and invincibility/death reads; they do not establish coverage of
ailments, periodic damage, HP-paid skills, scripted defeat, extra pools,
respawn or map-transition settlement.

### 6. Zero intercepted AMPR calls do not close the recovery question

The 455/239 historical hit counts and zero AMPR counts show no completed
intercepted AMPR callback in those captures. `MethodHookEntry` replaces
MethodInfo pointers and a matching virtual slot; compiled direct calls,
inlining, caches, temporary calculators and other derived tables can miss
those locations. No one explanation has been established.

The saved MP settlement path explains replacement of local bar writes, not
the remote recovery formula. Existing non-player observers cover Mercenary,
Pet and Moba calculators, not `TemporaryPlayerStatus`. Identify the executed
owner/caller and usable settled grant before proposing a recovery mutation.

### 7. Normal timing does not cover casting and every action lock

Instant Attack installs five entries: two next-attack-time hooks, two
motion-speed hooks and one ASPD hook. Local callbacks return next-attack time
zero and motion-speed value 60. That is concrete normal-timing behavior.

`get_Cspd`, `GetSkillDelayRate1/2`, `CalcCastTime`, cast/channel properties,
post-cast locks and network settlement are separate declared paths. Their
existence supplies coverage targets; it does not prove zero casting or an
unlimited remote rate would work.

### 8. Local final-damage branches can also explain a zero

Declarations name distinct element, distance, resistance, mastery, combo,
weapon, buffer and final-damage stages. Archived
`skill-review-decompiled/SkillActionBase.CalcActualDamage.7ffba69b66c0.c`
consults a per-target limit map and can return zero. A saved
`templateToDamageData` branch also builds a zero-hit record from local flags.

Neither branch was recorded for damage IDs 37/39. The concrete Hard Hit
implementation was not recovered by examining a base return stub. Trace
concrete action, temporary calculator, template flags, target limits, final
local amount and received outcome before attributing a zero to the server.
Finding separate factors does not establish their algebra or the acceptance
of a combined modification.

### 9. The universal skills-only rotation is unsupported

The comparison shows decreasing basic damage and later recovery after skills.
It did not record received proration transitions. Earlier gameplay findings
described rising physical/magic proration after basics; the later map claimed
basics suppress skills. The later universal conclusion did not follow from
the evidence. The follow-up sampler saw no combat and flushed zero responses.

Keep normal, physical and magic categories distinct, including the skill's
actual proration category, target/party context and MP gain/cost. Neither time
decay nor a globally optimal rotation is established. Compare received damage
per elapsed time and resource budget. This audit found no official formula
source resolving these remote mechanics.

### 10. Range and movement claims require configuration and geometry

Engage installs three assist predicates. Old range-check/hit-size callbacks
remain in the file but are absent from that installation table. Counting
every named range function would overstate current behavior.

An earlier feasibility note called remote skills accepted; the later rework
attributed death to remote skill claims. Reconcile build, action, target and
measured distance before choosing a universal rule. The saved geometry uses
`ceil(adjustedDistance * 0.5)` for HUD metres; raw range 30 is not proof of
30-metre delivery. Target radius and vertical/horizontal distance matter.

Likewise 1.25x/cap 15 is source policy, not measured acceptance. Preserving
floor logic does not establish valid paths or destinations on all maps.
The earlier disconnect trigger remains unestablished.

### 11. Persistence and world behavior were absent from the old map

| Domain | Concrete evidence/navigation | Outstanding question |
| --- | --- | --- |
| Equipment/refinement | `RefiningEquipmentResponse`: result/items/gold/cost/status | Which received outcome becomes persistent equipment state? |
| Inventory/storage | `StoragePutItemResponse`, `ItemUseResponseData` | Does local presentation agree with settled/reloaded inventory? |
| Drop/EXP/rewards | `RewardResponseDatav2`, `GetDropBonus`, `GetExpBonus` | Display, request input or received settlement? |
| Stats/skills/quests | `StatusUpResponse`, quest/mission reward responses | Which owner/result persists each change? |
| Currency/trade | Server/local gold and `TradeResultEvent_` | Does local state match persistent transaction results? |
| Companions | Six calculator families and separate actor actions | Which actor owns each calculation and settlement? |
| Buffs/ailments/defense | Temporary status, guard/avoid, received abnormal/buff lists | Local decision, transmitted result or received replacement? |
| Aggro/AI/targeting | Hate lists, target-selection/action methods | Which decisions are local and which are synchronized? |
| World/instances/bosses | Field/room/dungeon/spawn/phase declarations | What changes at transitions and phases? |
| Camera/HUD/input | Presentation/input declarations | Does the value stay presentation-only or feed an action? |

These are inventory leads, not demonstrations of item creation, economic
blindness, progression modification or persistent mutation. The local JSON
contains 16 overlapping lexical navigation groups. Engine/library names can
match these groups; their counts are not semantic audit or coverage counts.

### 12. The observer has blind spots of its own

| Gap | Source evidence | Practical implication |
| --- | --- | --- |
| Calculation/network join | Format 3 explicitly logs `network_correlated:false` | Local context and timestamp proximity are not an originating damage-ID join. |
| HP attribution | Target-history snapshots, no exclusive contribution ledger | A decrease may include other actions/actors. |
| Observer coexistence | Shared `OnSkillActionHit`; dispatch blocks stacking | Extracting two prefixes does not establish simultaneous capture. |
| Phase/config provenance | Action samples have phase/mask/distance; responses lack that same complete stamp | Settings read later may differ from action-origin settings. |
| Bounds/contention | Response buffer 256 events, at most 8 mobs/event, try-lock drop paths | Missing data must remain unknown; count losses. |
| Cached resources | Historical resource/max-MP readings were stale | Panel values are not a before/after grant measurement. |
| Shared callback scope | Several crit/rigidity callbacks return constants without instance filters | Foreign actors and concrete action coverage need verification. |
| Mechanism footprint | MethodInfo/vtable swaps, module, worker and shared mapping | Accepted gameplay values do not prove integrity behavior. |
| Product claim | UI still says `Safe loadout (0 risk)` | Label unsupported; runtime code/package unchanged by this audit. |

The footprint is recorded to avoid confusing accepted values with integrity
coverage. No concealment or integrity-check changes were made.

## Prioritized investigation and completion criteria

1. **P0: correlation.** Action-origin configuration, actor, full target ID,
   concrete skill, effective values and received result, with drop accounting.
   Complete when results are attributed and ambiguous rows stay unknown.
2. **P1: calculator ownership.** Ordinary/temporary/buff selection and direct,
   cached and virtual consumers. Complete when each candidate has an identified
   executed caller; zero intercepted calls alone cannot close a branch.
3. **P1: concrete skill/final damage.** Flags, temporary buffers and per-target
   limits. Complete when a local-zero or received-zero outcome is tied to the
   originating action.
4. **P1: timing.** Normal cadence, cast/channel time, locks and settlement.
   Complete with action-specific timings and lasting outcomes.
5. **P2: survival/recovery/reaction.** HP/MP/extra pools, damage types, ailments,
   HP-paid skills, guard/avoid and respawn. Complete with local/received
   before-and-after state per event type.
6. **P2: rotation.** Three proration categories, action types, MP and party/target
   context. Complete with reproducible received damage per time/resource.
7. **P2: movement.** Assist decisions, delivery distance, displacement, height
   and path context. Complete with actual outcomes for the identified build.
8. **P3: persistence/world.** Follow operation/result ownership for inventory,
   rewards, equipment, quests, companions and transitions. A local display
   change cannot count as a durable result.

Offline declaration coverage and original-capture replay are complete. New
runtime coverage, remote formulas and usable new features remain unresolved.
No live test, account change or package promotion was performed.

## Verification

- Exported member counts reconcile, with all method owners and ranges checked.
- Repeated audit output is byte-for-byte identical.
- All 18 replay rows match the earlier independent comparison analysis.
- The original native 168 and Hard Hit deltas 0/0/1566 reproduce exactly.
- A malformed inventory is rejected without writing an output report.
- `cargo test --workspace --locked --offline -- --test-threads=1`: all
  **157 workspace tests pass**, including the separate-process telemetry
  probe. These are local software tests, not gameplay acceptance tests.
- `git diff --check` passes. No Rust feature source was changed.

Local machine-readable check receipt:
`work/blind-spot-audit-20261021/verification.json`.
