# v94 validation record — 2026-10-18

## Result

The combined 15:21 correction candidate compiles and passes **49 tests**: companion 6, protocol 4, wboot 10, wcore 29. The simulated worker panic printed during the test run is the expected panic-isolation test; the test passes. Release compilation succeeds with Rust/Cargo 1.99.1, target `x86_64-pc-windows-msvc`.

The tested build is promoted to canonical `companion.exe`, `src/wcore.dll`, and `src/wboot.exe`, matching `work/release-v94` by SHA-256. Launch normally with `start_companion.bat`; `start_companion_v94.bat` still launches the identical staged copy. The previous v93 binaries remain backed up in `work/pre-v94-20261018`.

The build includes typed `CheckHit` output, derived normal-attack slots and independent timing controls. The latest revision corrects the output's semantic value: a clean hit clears IsScratch. Its regression starts with a set output, checks the one-byte clear, adjacent bytes and null-output failure. The combined revision adds scoped native normal-attack critical selection, nine tests for guards/forwarding/thread scope, and automatic staged SHA-256 manifests. The PID 4600 gameplay check below passed for the requested Boss Colon/Hard Hit case. UI command 73 remains "Stability + Crit Settings" and explains that enemy resistance still applies.

## Verified corrections

- Exact overload lookup distinguishes the two five-argument damage functions using their parameter and return types. Missing reflection fails instead of selecting an arbitrary overload.
- A callback's original is published before activation. Duplicate/self hooks and externally overwritten restoration targets are rejected.
- Critical-mode installation rolls back on failure and reports an error instead of displaying successful activation. The stability hook additionally requires a derived vtable slot.
- The normal attack's inherited `CalcStable` is reached through slot 87 in current native code; its unit is a fraction. The replacement returns `1.0`, while integer stability getters retain percentage units.
- Active forwarding callbacks no longer relabel already-calculated hit/reaction fields. Packet `HitType` getter overrides and ActionHit stamping are absent from the active critical table. Damage forwarding preserves the native critical Boolean and argument widths.
- Multiplier requests feed the native calculation getters. Native reset forwards to the original getter instead of assuming a universal 150% value. Repeated getter calls do not compound a multiplier.
- Stopping the v94 worker restores owned callback pointers and reports failed restoration. This does not undo every direct field edit from other features, so a full clean baseline still requires a game restart.
- The loader refuses conflicting or mixed core generations, validates content identity, and serializes dispatch by PID. Both GUI and CLI use verified shadow copies, including when an explicit source DLL is supplied.
- The menu prefers the adjacent release directory and canonical pair. Worker restart stops on errors; different builds require a game restart. Telemetry rejects old versions and clears previous-process state.
- Telemetry unit tests use private test memory, not the running game's shared mapping.

## Live smoke check completed

The v94 loader was invoked against the existing PID 11508. Before remote dispatch it identified five older hashed core modules plus `wcore.dll` and rejected the command with **Restart required**. No new core was loaded and no command ran in the game. This proves the existing mixed-process guard, not combat behavior.

The attempted canonical file copy also failed because `src/wcore.dll` is mapped by the running process. The separate release staging path avoids overwriting that file. The build script now reports this lock before stopping a menu and accepts `-StageDirectory`.

## Remaining live acceptance

### Clean-session results (PID 4672, started 14:40:19)

The operator restarted Toram normally and confirmed the character was in a field. The v94 CLI attached successfully. Module inventory showed exactly one core, `dxgi_ext_95e8004e7f7db5de.dll`. The packaged v94 GUI also dispatched probe command 1 successfully to this same core.

With all combat options off, the battle log against a level-4 Shell Mask recorded normal attacks including 32, 24, 34, 32, 21 and 19 damage. Enabling command 73 installed all 18 entries successfully. A fresh level-4 Shell Mask received 41, 32 and 24 damage and died normally; the probe then showed `stable_calls=3`, `normal_calculations=3`, and `base_damage_calls=0`. This verifies the stability and normal-calculation callbacks execute. It does not establish full critical-hit coverage or eliminate damage decay.

Disabling command 73 restored all 18 entries. Another level-4 Shell Mask received native damage including 52, 27 and 22; the counters stayed at 3, demonstrating those callbacks no longer execute after disabling. Enabling again and stopping the worker with command 0 restored all owned entries and logged `complete=true`. Command 21 then restarted the same core successfully. All feature toggles were left off.

Native inspection identified a remaining local-player critical gap: the normal attack calls `PlayerSecondaryStatus.GetCrtRate`, `GetCrtConstant`, `CalcCritical`, and `PlayerAttackBase.checkCriticalPercent` directly. The installed `get_Critical` and MethodInfo-only `checkCriticalPercent` replacements do not cover that sequence. Investigation continues; the critical-mode label is not proof that every hit is critical.

Session log: `work/v94-live-4672.log`. Release hashes: `work/release-v94/sha256.json`. The operator confirmed the reported HP restoration occurs with **Hard Hit against Boss Colon**. A normal restart has been requested for the revised candidate; the currently loaded earlier DLL must not be mixed with it.

### Hard Hit observations in PID 16588

The operator confirmed Hard Hit against Boss Colon left HP reduced both with all combat options off and with Stability + Crit Settings on, while Instant Attack and MP overrides remained off. These are operator reports. The revised CheckHit installed successfully with its exact byref signature; probes counted 14 each for stability, normal calculations and hit checks. The process contained one revised core, `dxgi_ext_97e85ada64578f79.dll`.

Attempting Instant Attack failed because the earlier table requested `GetNextAtkTime(0)`. It rolled back with status -8, leaving timing changes off. Current metadata and native forwarding confirm both `GetNextAtkTime(int)` and `GetMotionSpeed(int)` use one argument. The source and 15:05 binaries now correct this; no high-speed result was observed from that failed attempt. The audit of all 52 original entries found no other argument-count mismatches. A separate MP-mode follow-up remains: `RecalcCostMp` returns void while its callback returns an integer; MP mode was not used in these comparisons.

`work/sample_target_hp.py` observes fields by names/types from live class metadata with read-only process access. A 120-second bounded recording, `work/hp-stability-16588.jsonl`, contained 416 valid samples for Boss Colon identity MobId 10 / LocalId 1 / UniqueId 11226 before target loss/unavailable states. It saw local HP 209 -> 194 -> 179 -> 164 -> 149 -> 134 -> 119; the client's cached serverHp matched each reduction about 103–255 ms later. The last valid observation was local 0 / cached server 119 before the target became unavailable. No upward local correction appears in valid samples. This trace does not establish the final server-acknowledged death, and the reported earlier 660-damage event was not captured. Session log: `work/v94-live-16588.log`.

### Timing and graze results in PID 13520

The 15:05 candidate loaded as the only core, `dxgi_ext_4c47e769b8782c90.dll`. All five timing hooks installed. With modes 72/73 on and MP/Anti-Rigidity off, the operator reports faster attacks and Boss Colon HP staying down after normal attacks and Hard Hit. Final counters were next_attack=28, motion_speed=68, and stability/normal/hit_check=28 each. Turning both modes off restored all five timing and eighteen combat entries. Session log: `work/v94-live-13520.log`.

The two read-only HP attempts in this session recorded no target then a transient unavailable state, so they do not corroborate the fight's HP result. The sampler now continues transient failures within its existing bounded duration; it still cannot assume a saved PlayerStatus root survives a scene change.

The operator also reports normal attacks are not always critical and show Graze. Native inspection confirms the revised CheckHit callback wrote true to the Boolean copied into `DamageData.IsScratch`; the clean native branch writes false. Source and regression expectation now clear that byte while preserving successful return and adjacent bytes. The combined 15:21 build includes this correction and scoped critical selection, passes all 49 tests and awaits live validation. Core SHA-256: `EA012E56ADE407A9AE6FE25403EA5741EE1E98A5329CC2D15942DE3D186F10DE`.

### Final combined comparison in PID 4600

The operator restarted normally. The process loaded exactly one new core, `dxgi_ext_82bd963c60e922d6.dll`. Command 73 passed all native pattern/class/call-target guards and installed 19 hooks, including the secondary-status getter. Command 72 installed five timing hooks. MP overrides and Anti-Rigidity stayed OFF, multiplier Native.

The operator reports **all tested normal hits critical, no Graze, and HP stayed down**. Probe counters independently recorded 13 each for normal calculations, stability, corrected hit checks and scoped critical fallbacks; timing counters were next_attack=13 and motion_speed=31. The operator further confirmed **Boss Colon died and victory/rewards appeared**. These are gameplay observations supplied by the operator; the counters establish execution of the corrected paths.

The completed 120-second read-only recording `work/hp-final-4600.jsonl` contains 173 valid samples for Boss Colon MobId 10 / LocalId 1 / UniqueId 11379. Local HP decreased 1000 -> 954 -> 913 -> 877 -> 845 -> 818 -> 795 -> 772 -> 749 -> 726 -> 470 -> 227. The client's cached serverHp matched those decreases approximately 102–203 ms afterward. A final local 0 / cached server 227 sample was followed about 204 ms later by 227/227, then target loss. **This is an observed upward local correction**, despite no visible rebound reported by the operator and confirmed victory/rewards. Its cleanup/reconciliation cause was not traced, and the sampler did not observe cached serverHp=0. Do not describe this trace as containing no corrections or as a direct server query.

Worker command 0 restored all 24 owned hooks, including the secondary getter, with `complete=true`. Same-core restart and both mode re-enables succeeded. Final state: modes 72/73 ON; MP/Anti-Rigidity OFF; Native multiplier. Canonical promotion preserved the v93 backup, verified all three binary hashes against staging, and successfully dispatched a probe from the canonical root pair without loading another core. Session log: `work/v94-live-4600.log`.

### Scope and remaining limits

This verifies the requested normal-attack/Hard Hit comparison on Boss Colon for this game build. Native proration still changes damage across consecutive hits; stability does not remove it. Different enemies/skills and custom multiplier values require their own comparisons. MP mode was not enabled; its separately documented RecalcCostMp return-contract issue remains a follow-up. No server cap or attack-acknowledgement algorithm was established. HP reconciliation and server fields are unchanged, and no universal 100% claim is supported.

Native evidence and signature provenance: COMBAT_EVIDENCE_20261018.md (development repository). General direct/virtual dispatch background: [Unity IL2CPP method calls](https://unity.com/blog/engine-platform/il2cpp-internals-method-calls).
