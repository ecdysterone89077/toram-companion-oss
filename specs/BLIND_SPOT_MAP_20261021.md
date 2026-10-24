# Blind spot map — evidence and open coverage — 2026-10-21

Continuation: [action/result correlation and calculator ownership](CORRELATION_OWNERSHIP_AUDIT_20261021.md)
traces ID allocation/sending, omitted response keys, concrete Hard Hit and
temporary caches. Six archived logs still provide no originating network join.
Use that continuation for the latest priorities and evidence corrections.

The earlier claim that the complete server-blind set was exhausted is withdrawn.
This revision inventories installed metadata and current source, keeping local
control, observed outcomes and remote validation separate. See
[the detailed audit](BLIND_SPOT_AUDIT_20261021.md) for findings and provenance.

## Inventory and evidence

The offline export of installed metadata version 31 contains **20,163 types,
157,413 methods and 96,686 fields**. Every method owner and member range was
checked and the totals reconcile. This enumerates declarations, not executed
paths, remote code or remotely accepted modifications.

`IPlayerStatusCalculator` declares **51 methods**. Six concrete types declare
all 51 name/argument-count pairs: `PlayerBattleStatus`, `PlayerSecondaryStatus`,
`TemporaryPlayerStatus`, `MercenaryBattleStatus`, `PetStatus` and
`MobaPlayerSecondaryStatus`. This matching is not an interface-dispatch proof.

The eight current feature-installation tables contain **45 hook entries**.
Across the two player calculators, those tables name **9 distinct contract
methods**; **42 have no direct status hook in those tables**. These are source
installation counts, not runtime coverage or counts of usable new features.

| Evidence | Establishes | Does not establish |
| --- | --- | --- |
| Declaration | Named type/member exists | An action executes it |
| Source | Hook/calculation installation intent | Installation or remote acceptance |
| Archived native body | Flow in the identified client snapshot | Current server rules or all derived actions |
| Archived observation | Outcome for that configuration | Universal limits, compatibility or future acceptance |
| Missing observation | A coverage gap | Impossibility or a server-blind surface |

No area below is proven universally server-blind. An accepted outcome can
coexist with checks, thresholds, context or delayed enforcement.

## Existing feature areas

| Area | Current source behavior | Recorded evidence / open boundary |
| --- | --- | --- |
| Accuracy/stability/critical selection | HIT 10000, stability/substability 100, critical predicates and normal critical fallback | Earlier successful melee comparisons; weapon, skill, resistance and target coverage remains incomplete. A top roll is finite. |
| Critical magnitude | Two `get_CriticalDmg` hooks, 135% implementation ratio ceiling | Original native **168**; three Hard Hit HP deltas **0, 0, 1566** at request-at-response 250. Effective skill values were absent; 135% is not a measured remote ceiling. |
| Magic critical | Two `CheckMagicCritical` overloads overridden | `get_CriticalMagicDmg` unhooked; comprehensive magic outcomes unmeasured. |
| Survival | Six local invincibility/damage/death hooks | Earlier invincibility reports; explicit server/local HP and received settlement exist. Ailments, scripted defeat, extra pools and transitions need separate coverage. |
| Attack timing | Five next-attack/motion-speed/ASPD hooks | Earlier callback counters and combat observations; cast, channel, action-lock and remote cadence coverage remains open. |
| Anti-rigidity | Three boolean hooks and local timer/lock clearing | Source and earlier reports; callback scope and non-normal/ailment locks need verification. |
| Speed | 1.25x native getter result, cap 15.0, nested-query guard | Source policy; current displacement and context-dependent acceptance unverified. |
| No collision | One `CharacterMove.get_EnableWallHit` hook | Source; preserving floor logic does not prove a valid or accepted traversal. |
| Engage | Three assist predicates; installer contains no range-extension hook | Source and historical approach observations; current action scope and delivered distance await measurement. |
| MP | Two AMPR hooks and an action-hit observer | Historical hit callbacks with zero intercepted AMPR calls; direct calls, caches and temporary calculators remain unresolved. |

## All 51 status-contract methods

Counts are methods, not independent gameplay quantities. This grouping covers
the entire installed interface.

| Group | Methods | Count | Direct status hooks in feature tables |
| --- | --- | ---: | --- |
| Level/primary | `get_Lv`, `get_Str`, `get_Int`, `get_Vit`, `get_Agi`, `get_Dex`, `get_Crt`, `get_Luk`, `get_Men`, `get_Tec` | 10 | None |
| Resource maxima | `get_MaxHp`, `get_MaxMp` | 2 | None |
| Attack inputs | `get_EqAtk`, `get_SubEqAtk`, `get_Atk`, `get_Matk`, `get_SubAtk`, `get_SubMatk` | 6 | None |
| Accuracy/variance | `get_Stable`, `get_Hit`, `get_Flee`, `get_SubStable` | 4 | First, second, fourth |
| Defense | `get_Def`, `get_Mdef`, `get_EqDef`, `GetDefFact`, `GetMdefFact`, `GetDamageCut` | 6 | None |
| Timing | `get_Aspd`, `get_Cspd`, `GetNextAtkTime`, `GetMotionSpeed`, `GetSkillDelayRate1`, `GetSkillDelayRate2` | 6 | ASPD, next-attack time, motion speed |
| Recovery | `get_HpRecovery`, `get_MpRecovery`, `get_AtkMpRecovery` | 3 | AMPR only; effective execution unestablished |
| Critical/element | `get_Critical`, `get_CriticalDmg`, `get_CriticalMagicDmg`, `get_ElementPower` | 4 | Critical rate, physical critical magnitude |
| Resistance | `get_AntiVirus` | 1 | None |
| Guard | `get_GuardDelay`, `get_GuardPower`, `get_GuardSpeed`, `CalcBaseGuardPower` | 4 | None |
| Avoid | `get_AvoidSpeed`, `get_AvoidStack`, `get_AvoidDelay` | 3 | None |
| Respawn | `get_Respawn` | 1 | None |
| Guild contribution | `SetGuildStatusBoost` | 1 | None |

Outside the interface, `PlayerSecondaryStatus` has 75 methods, including the
metadata spelling **`get_CurrectHit`**, `GetParam`, `GetItemBonusParam`,
`GetDurationBonusTime`, `GetExpBonus`, `GetDropBonus`,
`GetDisplayBonusCalcHate`, `CalcHpRecovery` and `CalcMpRecovery`.
`TemporaryPlayerStatus` has 48 fields, including `baseStatus`, `_motionSpeed`,
`_damageCut`, `_damageReductionRate`, `_criticalMagicDmg` and `_atkMpRecovery`.
Their presence is a data-flow lead, not proof of effective control.

## Coverage beyond status getters

| Surface | Concrete navigation points | Evidence boundary |
| --- | --- | --- |
| Skills/cost/combo | `CalcCastTime`, `CalcCostMp`, `SetComboMp`, `CorrectComboRate`, skill cast/delay/attack-count properties | Concrete skill dispatch and lasting effects need tracing. |
| Temporary effects | `TemporaryStatus`, `RestoreStatus`, temporary-buffer activation/invalidation | Archived dispatch uses temporary buffers; live calculator consumption unmeasured. |
| Final damage/limits | `CalcActualDamage`, `CalcMultiDamage`, `GetLimitDamage`, `CheckDamageLimit`, target-limit map | Archived local branches can produce zero; not attributed to failed Hard Hits. |
| Element/distance/resistance | `CalcElementBonus`, `CalcElementKillerBonus`, `CalcDistanceResistRate`, `CalcSkillTypeBonusRate`, resist-damage methods | Separate declared terms; combined accepted modifications unproven. |
| Ailments/guard/reaction | `PossibilityAbnormalState`, `CalcHitReaction`, abnormal/buff lists and guard/avoid methods | Local and received paths exist; dismissal as cosmetic was unsupported. |
| Proration | `MobStatus` local/server normal/skill/magic fields, `CalcExpDef`, `GetExp`, received DefExp bytes | Original comparison lacked these bytes; universal skills-only rotation unsupported. |
| Targeting/aggro/AI | `UpdateHate`, hate lists, main-target/hit-target methods | Ownership and synchronization unmeasured. |
| Movement/geometry | `CharacterMove`, `FieldRayPick`, height/angle/range gates, assist movement | Local gates and transmitted position/path must be distinguished. |
| Resource pools/gauges | Server/local HP, MP, ExHP, ExMP, respawn and magic gauge | Multiple settlement channels, not one ownership model. |
| Equipment/refinement | Equipment calculators, refinement result/items/gold/cost response | Persistent result is separate from a local stat or UI change. |
| Inventory/storage/rewards | Storage/item-use responses, `RewardResponseDatav2`, `DropData` | A display/bonus getter does not prove item or reward creation. |
| Crafting/production | Craft/produce/synthesis/material types and responses | Declaration inventory; effective mutations unestablished. |
| Progression/quests | `StatusUpResponse`, quest/mission rewards, EXP/skill/combo state | Received and persistent state require separate attribution. |
| Currency/trade/market | Server/local gold pairs and trade/shop responses | No blind economic surface demonstrated. |
| Companions/party/guild | Additional calculators, actions and status paths | Actor ownership differs; three AMPR observer classes are not full coverage. |
| World/instances/bosses | Field/room/dungeon/spawn/phase declarations | Not covered by melee stat comparisons. |
| Camera/HUD/input | Presentation/input declarations | Establish whether a local value feeds gameplay or stays presentation-only. |
| Mechanism/lifecycle | MethodInfo/vtable writes, module/mapping/thread presence, scopes and restoration | Independent from accepted gameplay values; integrity behavior unmeasured. |

## Investigation order

1. Correlate originating calculations with received results, effective skill
   values, configuration, actor/target identity and dropped-event counts.
2. Trace ordinary versus temporary/buff calculators and direct versus virtual
   consumers before treating zero intercepted calls as a closed branch.
3. Separate normal timing, cast/channel time, locks and remote settlement.
4. Observe local zero-damage branches and per-target limits alongside received
   HP rather than assigning every zero to remote rejection.
5. Separate survival, extra pools, recovery, ailments, guard/avoid and respawn.
6. Compare actual proration categories, MP costs and received damage per time;
   the old capture does not establish an optimal rotation.
7. Study persistence/world domains through their operation/result flows;
   local presentation is insufficient evidence of durable state.

The audit used files and archived data only. It did not launch the game,
attach to a process, load a core, change an account, send gameplay commands
or produce a new runtime package.
