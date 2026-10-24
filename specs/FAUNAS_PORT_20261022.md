# Faunas -> Windows companion port

Date: 2026-10-22
Source of truth: `work/reference/faunas_release.lua` (Faunas 4.0.62, Android
Toram, Lua reference script, 138,076 bytes / 4,846 lines).
Target: the Windows companion, `crates/wcore` (in-process core) and
`crates/companion` (control window), against Toram Online 0.96.3 on Windows 11
x64.

This document is the port's adaptation matrix. The original is the spec: every
mechanic Faunas implements is listed here with the Windows target it lands on
and the verdict. Nothing is invented, and nothing is imported from a later
rewrite of the original.

---

## 1. Why the mechanism changes

Faunas patches raw ARM64 instruction bytes inside `libil2cpp.so`. The Windows
build cannot be patched that way: the AppSign driver rejects writes to code
pages, so this port drives every mechanic through the managed dispatch door
instead — the `MethodInfo` pointer swap in `crates/wcore/src/hook.rs`.

The original uses two patch shapes:

| Shape | Original bytes | Port |
| --- | --- | --- |
| Prologue override | `mov w0, #N ; ret` at the method entry | The detour returns `N` and never runs the native body. Exact. |
| In-body constant rewrite | a constant or branch inside the body | Not expressible through the dispatch door, because the door replaces the whole method. Recorded as a deviation in section 5. |

Argument handling: every detour in `crates/wcore/src/faunas.rs` uses the
zero-argument shape `(this, method)` and ignores both parameters. The caller
still passes the real arguments in the real registers; the detour simply does
not read them, so the position of the trailing `MethodInfo*` argument does not
matter. The *hook resolution* still uses the exact name, argument count and
signature, so the correct overload is always the one replaced.

### Verification of the values

The value ladders are not transcribed from the menu. `scripts/faunas_imm_decode.py`
decodes the ARM64 `MOVZ Wd, #imm16, LSL #hw` instruction word out of every
byte string the original writes, and `crates/wcore/src/faunas.rs` carries a
test that re-derives every ladder from the original byte strings. A change to
a constant without the matching byte string fails the test.

---

## 2. Analysis provenance

| Question | Method | Result |
| --- | --- | --- |
| Which Windows class/method owns each Faunas target? | `scripts/faunas_port_map.py` against `dump/dump.cs` | All 12 named player/mob/damage targets and all 32 skill targets resolved. |
| Is the current Windows build natively analysable? | REA MCP, Ghidra 12.1.4 provider, section entropy of `C:\Asobimo\AsobimoGames\Toram\GameAssembly.dll` | No. `.text` entropy is 7.9991 and the saved `dump/dump.cs` predates the build, so its RVAs point past the end of the file. Native decompilation of the current build is impossible from disk. |
| Does the REA Ghidra bridge work now? | `open_binary` on `baselib.dll` | Yes. The bridge hang recorded in `HANDOFF.md` is gone in REA 6.7.0. |
| Are Rizin, Hopper, IDA or Kuna usable here? | `binary_session` provider inventory | No. Rizin is not installed on this Windows host, Hopper is macOS-only, IDA is not configured, Kuna is not configured. |
| Can Faunas' Android offsets be verified against a Toram Android build? | byte comparison against every `libil2cpp.so` on this machine | No. The available Toram Android builds are a different version, and the `old_dump/*` dumps are a different game with obfuscated names. Faunas' offsets are therefore taken from the script's own comments, cross-checked against the Windows metadata by method name. |
| Is the current IL2CPP metadata readable? | `scripts/export-metadata-symbols.ps1` on the installed `global-metadata.dat` | No. The installed metadata is encrypted, so the parser returns null. The Windows `dump/dump.cs` remains the declaration source. |

Consequence: the port resolves every target **by class and method name at
runtime**, never by RVA. That is what the existing companion already does, it
survives a game update, and it removes the stale-RVA problem entirely.

---

## 3. Adaptation matrix — player and mob

| # | Faunas feature | Android target (from the script) | Windows target | Verdict |
| --- | --- | --- | --- | --- |
| 0 | Always Critical | `checkCriticalPercent` PlayerAttackBase, `CheckMagicCritical`, plus one unlabelled third | `PlayerAttackBase.checkCriticalPercent(int, MobActionManagerBase) -> bool`, `PlayerAttackBase.CheckMagicCritical(PlayerStatusBase, MobActionManagerBase) -> bool` | adapt — two of the three Android targets have one Windows counterpart each; the third has no separate Windows method |
| 1 | Max Motion | `GetMotionSpeed` player secondary status, value 60 | `PlayerBattleStatus.GetMotionSpeed(int aspd) -> int`, forced to 60 | keep — same method, same value |
| 2 | Max Stability | `get_Stable` player secondary, value 1000 | `PlayerBattleStatus.get_Stable() -> int`, forced to 1000 | keep |
| 3 | No Cast Time | `GetSkillDelayRate1` secondary status, value 0 | `PlayerBattleStatus.GetSkillDelayRate1(int cspd) -> int`, forced to 0 | keep |
| 4 | Long Range Attack | `CheckActionRange` BattleManagerBase, value 1 | `BattleManagerBase.CheckActionRange() -> bool`, forced to true | adapt — see the original's bug in section 6 |
| 5 | Always Pierce | `CalcPowerResistDamage`, `CalcMagicResistDamage`, value 0 | `PlayerAttackBase.CalcPowerResistDamage(PlayerStatusBase, int, float) -> int` and `CalcMagicResistDamage(...)`, both forced to 0 | keep |
| 6 | Disable Resist | `AbnormalPopLabelPlayerToEnemy`, `AbnormalResistPopUpLabel`, value 0 | `UI3DLabelManager.AbnormalPopLabelPlayerToEnemy(AbnormalType, int, Vector3) -> void` and `AbnormalResistPopUpLabel(bool, int, Vector3, float) -> void`, both bodies skipped | keep |
| 7 | Mob No Flee | `get_Hit` player secondary status, value 65535 | `PlayerBattleStatus.get_Hit() -> int`, forced to 65535 | keep — the value is decoded from `E0FF9F52`, not assumed |
| 8 | Mob No Guard / Evade | `checkMobReaction` playerattackbase, value 0 | `PlayerAttackBase.checkMobReaction(int guard, int avoid) -> SkillHitReactionType`, forced to 0 | keep |
| 9 | Mob No Invincible | `get_SystemInvincible` enemymobaction, value 0 | `EnemyMobActionManagerBase.get_SystemInvincible() -> bool`, forced to false | keep |
| 10 | Mob No Move | `get_Range` mob act, `CreateMobAction`, value 0 | `MobActionPattern.get_Range() -> int` forced to 0, `MobBattleSystemManager.CreateMobAction(MobActionPattern) -> MobAttackBase` forced to null | keep |

## 4. Adaptation matrix — skill AOE and burst

Faunas exposes 40 skill rows, but they are two mechanics applied across 18
skill action classes:

* `CheckRangeHit` forced to true — "Full AOE": the skill's own range test
  passes for every mob in the scene.
* `CheckRangeHit` forced to false — "No AOE": the skill never hits.
* `NextRangeHit` replaced with an immediate return — "Burst": the chain step
  is skipped.

The Windows `NextRangeHit` returns `void`, so the original's "return true" is
expressed here as **skip the body** — which is exactly the operation the
original performs, because its patch replaces the whole prologue with an
immediate return. Faunas writes a bare `ret` for `SatelliteArrowAction`
(`C0035FD6`) and `mov w0, #1 ; ret` for the rest; both skip the body.

| Group | Classes | Windows target | Forced |
| --- | --- | --- | --- |
| Skill Full AOE | MeteoBreakerAction, AuraBladeAction, CrossFireAction, SatelliteArrowAction, MagicWallAction, MagicStormAction, MagicFinawAction, MagicCannonAction, MagicFallAction, DiveImpactAction, DimensionTillAction, DragonicChargeAction, LunaDitherStarAction, TwinBusterBladeAction, NemesisAction, SoulHuntAction (16) | `CheckRangeHit(Transform, Transform, float) -> bool` on each class, vtable slot | true |
| Skill No AOE | MagicImpactAction, AirSlideAction (2) | same method | false |
| Skill Burst | the 14 AOE classes that the original also patches with `NextRangeHit` | `NextRangeHit() -> void` on each class, vtable slot | body skipped |

`MagicFinawAction` is the Windows class name; it keeps the original's own
"magicfinaw" spelling, which is how the two were matched.

## 5. Adaptation matrix — damage, proration, element and the rest

| # | Faunas feature | Android target | Windows target | Verdict |
| --- | --- | --- | --- | --- |
| 14 | Mob Proration | `ExpDefNormal` / `ExpDefSkill` / `ExpDefMagic` on `MobBattleStatus` and `BossMobBattleStatus` | the same six getters | keep — absolute replacement of the mob's defence, ladder 150/200/225/250/275/300/350 |
| 15 | Equip Element | `GetEquipElement` player status | `PlayerStatusBase.GetEquipElement() -> ElementType` | keep — ladder None/Fire/Water/Wind/Earth/Light/Dark = 0..6, matching the `ElementType` enum |
| 16 | Critical Damage | `get_CriticalDmg` player secondary status | `PlayerBattleStatus.get_CriticalDmg() -> int` | keep — absolute ladder 225..450 |
| 17 | Critical Magic Damage | `get_CriticalMagicDmg` player secondary status | `PlayerBattleStatus.get_CriticalMagicDmg() -> int` | keep — absolute ladder 125..170, including the original's rung-9 typo |
| 18 | Skip Battle | `get_IsBattleEnd` | `RoomManager.get_IsBattleEnd() -> bool` | keep |
| 19 | No Domination Bonus | `SetBonusGauge` | `PopAreaBonusSender.SetBonusGauge(int)` and `MobPopAreaGaugeManager.SetBonusGauge(int)`, bodies skipped | keep |
| 20 | No Drop Visual | a drop-visual method | `EffectDropItem.Update() -> void`, body skipped | adapt — the Android target is unnamed in the script; `EffectDropItem.Update` is the drop visual's own update |
| 21 | One-Click Loadout | Faunas' "One Clik" preset | applies critical, stability, range, mob-no-invincible, mob-no-move and skip-battle | adapt — Faunas' preset also enables the Magic Fall AOE and burst, the long-range skill constant and the animation constant; the AOE and burst groups are left out of the preset because the skill AOE group is the one mechanic in this port with recorded map-wide desync, and the other two are in-body rewrites (section 7) |
| 22 | Unlock Gacha | a gacha gate | `GameUserDataManager.get_IsBeginner() -> bool`, forced true | keep — Faunas writes `mov w0, #1` over a boolean getter, so the account reads as a beginner and the beginner gacha is offered. The same shape applies on Windows: a boolean getter replaced with an immediate true. |
| 23 | Unlock World Map | a world-selection gate | `UIWorldMapPanel.get_IsTapLock() -> bool`, forced false | adapt — the original rewrites a register inside a method body rather than replacing a return value, so it has no direct counterpart. The Windows candidate is the world map panel's tap lock. This is a reading of the original rather than a port of it, and it stays out of the one-click preset until the map is seen to open with every area reachable. |

---

## 6. Bugs in the original that the port does not reproduce

These were found while decoding the original and are recorded rather than
silently copied.

1. **"Long Range Attack" ON patches the wrong address.** `range1` writes
   `mov w0, #1 ; ret` at `baseAddress[2].start` — the first byte of the
   library's executable segment — instead of at `CheckActionRange`
   (`RangeHit`, `0x1416C5C`). `range2` (OFF) restores `CheckActionRange`,
   which ON never touched. The toggle is therefore a no-op that also
   corrupts an unrelated function. The port implements the mechanic the
   script *intends* — `CheckActionRange` forced to true — and the deviation
   from the original's bytes is recorded here.

2. **"ATK No Delay" OFF does not turn the feature off.** The ON path blanks
   12 bytes at one address with `1F2003D5` (the ARM64 `NOP`). The OFF path
   restores the original bytes at three addresses that are all the same
   address, then writes the NOPs again at the last one, so the NOP wins. The
   port does not reproduce a toggle that cannot be turned off.

3. **Rung 9 of the critical-magic-damage ladder is a typo.** The original
   writes `000280D2`, which decodes to `mov w0, #16` where 165 was intended.
   The port preserves the value so the mechanic matches the original byte for
   byte; it is a one-line change in `CRITMAGICDMG_LADDER`.

---

## 7. Deviations: mechanics the dispatch door cannot express

These are the original's in-body constant rewrites. Each one is recorded
rather than approximated, and each has an existing companion feature or a
documented reason.

| Faunas mechanic | What the original does | Port status |
| --- | --- | --- |
| ATK No Delay | blanks three instructions in the attack-delay computation | covered by the existing `cmd 72` Instant Attack, which clears the delay through `GetNextAtkTime` and the motion-speed getters |
| Fast Animation V1 / V2 | rewrites an animation constant from 65 to 500 | not expressed. No Windows target is named in the original. |
| Long Range Skill | rewrites a range constant inside the skill code | not expressed. The companion retired range widening on purpose: the server validates the distance of a damage claim, an out-of-range basic attack is dropped and an out-of-range skill is sanctioned. The companion's `cmd 71` is Engage, which closes the distance instead. |
| Instant / 1-second Finale cast | rewrites a branch and an immediate in `InitializeOthers` | not expressed as a body rewrite. The playable effect is cast time, which the port reaches through `GetSkillDelayRate1` forced to 0. |
| Brave Aura +100..+800 | rewrites the subtracted constant of a comparison | not expressed. The comparison lives in a method the original does not name. |
| Quick -> Brave Aura | rewrites a branch target | not expressed, same reason. |
| Spinning Slash 100% Blind | rewrites an immediate | not expressed, same reason. |
| Snipe 100% Break | rewrites an immediate | not expressed, same reason. |
| Shell Break 100% Break | rewrites an instruction 0x1AB into the method body | not expressed, same reason. |
| Satellite Arrow instant | rewrites two instructions in `OnInitialize` | not expressed, same reason. |
| Tembus / Blink / Hayabusa | locates a struct by a memory scan and writes five fields | not expressed. A memory scan is not reproducible through the dispatch door. |
| Wide View | scans the whole process for the float 7 and writes 18 | not expressed, same reason. |
| Fast Run (Safe) | rewrites the whole `get_MoveSpeed` body | replaced by the existing `cmd 68` Speed, which scales the native value by 1.25 with a 15.0 ceiling instead of writing a flat constant. |
| Invincibility | forces `get_IsInvincibility` true | replaced by the existing `cmd 32` invincibility, which does the same thing through the same method. |

---

## 8. Risk notes

* **Skill Full AOE is the one mechanic with recorded desync.** Forcing
  `CheckRangeHit` to true makes every mob loaded in the scene pass the
  skill's range test, which this project has already recorded as map-wide
  AOE aggro and a server desync. The mechanic is implemented because it is in
  the original, and it is kept out of the one-click preset.
* **The damage and proration ladders replace values the server can
  recompute.** `get_CriticalDmg`, `get_CriticalMagicDmg` and the mob's
  `ExpDef*` are absolute overrides, not additions. The companion's existing
  adaptive critical-damage ladder and its 135% of native cap do not apply to
  them, because the value no longer derives from the native one.
* **The Faunas command band is separate from the core band.** Features
  occupy telemetry bits 8..=31, so no existing toggle, hotkey or command id
  changes meaning. The band sits above the core range so the operator phase
  marker keeps its id.

---

## 9. Command surface

| Range | Meaning |
| --- | --- |
| `200..=247` | explicit OFF/ON for a ported feature; even is OFF, odd is ON. The order matches the window's Faunas row. |
| `300..=306` | proration ladder rung; sets the value and turns the feature on |
| `310..=319` | critical damage ladder rung |
| `320` | critical damage back to native |
| `330..=339` | critical magic damage ladder rung |
| `340` | critical magic damage back to native |
| `350..=356` | equip element rung |

`244/245` (gacha) and `246/247` (world map) were originally reported as
"not identifiable in the installed metadata". Both now resolve: the gacha
gate to `GameUserDataManager.get_IsBeginner`, the world map gate to
`UIWorldMapPanel.get_IsTapLock`. The gacha one is a port of the original's
shape; the world map one is a reading of it, because the original rewrites a
register inside a method body rather than replacing a return value.

---

## 10. Verification status

| Check | Result |
| --- | --- |
| Workspace tests | 174 passed, 0 failed |
| Ladder constants re-derived from the original byte strings | covered by `crates/wcore/src/faunas.rs` tests |
| Target resolution against the installed metadata | 12 named targets and 32 skill targets resolved, 0 failures (`work/faunas-port-map.json`) |
| Release build and packaging | see `CHANGELOG.md` for the staged candidate |
| Live gameplay acceptance | **not established.** No live test was run in this pass. Every ported feature is verified to resolve and install; none is verified to be accepted by the server. |

The distinction matters: an installed hook is local evidence that the mechanic
runs, never evidence that the server accepted the result.
