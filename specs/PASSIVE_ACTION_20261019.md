# Passive action observations - 2026-10-19

> **Tooling note.** The Python sensor this record describes
> (`scripts/toram_sensor.py`, driven through `scripts/combat_observer.py`) was
> removed when the project was rewritten as a pure Rust workspace. The
> observations in this record remain the reference for what the in-game
> observation stream reports, and the current procedure is
> `specs/LIVE_TEST_RUNBOOK_20261021.md`.

The observer now records executing managed action state alongside paired
local/received resources, without enabling a combat feature or installing a hook.
Enhanced MP and extended range remain unresolved. This is a diagnostic result,
not another combat implementation or a pristine client baseline.

## Scope and implementation

`scripts/toram_sensor.py` opens `PROCESS_VM_READ` only. It follows the verified
local player chain and validates live metadata class names, field names and types.
It does not scan ranges, call game functions, allocate in the game or write memory.
Metadata structure layout remains specific to the inspected 64-bit IL2CPP build.
`combat_observer.py capture` requires the process creation identity from session
initialization and a previously verified PlayerStatus pointer. Queries before and
after each paired read bind it to that process lifetime. Two matching observations
are not an atomic game transaction. Read windows over 250 ms are rejected.

The sensor records `SkillActionManager.currentSkillData`, its play/take IDs and
action class, `SkillActionBase.Id`, end/motion flags, range/start-distance fields
and raw `_attackCount` / normal `damageCount` fields. Sampled changes in these
fields do not establish a callback count or identify the writing function.

A null selected mob status now yields `no_mob_status`, retaining independently
valid action/resource values. Battle-target fallback requires active battle.
Pending negative target IDs cannot establish continuous target identity.
Cached PlayerDistance has no verified game update time or units. It is kept
separate from measured distance, including when a HUD distance looks different.

## Captures

All captures use PID 2260, creation FILETIME 134368076866930507 and the existing
failed range-candidate core (on-disk SHA256 5721f0d4...1b51). No replacement core,
worker command, probe or feature toggle was sent. Every IPC mask is 0 and command
sequence stays 7. Native gameplay used the previously approved foreground control.
Equipment and skill points were unchanged; the target list was closed afterwards.

| Session under work/observations | Rows | Rejected source reads | Scope |
| --- | ---: | ---: | --- |
| passive-action-20261019-2260 | 2,400 | 1,314 | Initial prototype; null status bug makes most target-associated rows unusable |
| passive-action-v2-20261019-2260 | 1,800 | 1 | Corrected null-status handling; native normals and Hard Hit |
| passive-final-smoke-20261019-2260 | 100 | 0 | Final sensor v2, idle CLI smoke and game artifact hashes |

The prototype was retained, not overwritten or treated as a clean experiment.
Its errors are 1,313 bounded-read rejections and one changed paired observation.
The corrected run has 1,799 resource/action-state samples, 191 comparable target
samples across three complete identities, 866 distance-cache observations and
1,800 verified process-lifetime observations. Median interval is 50.003 ms,
maximum 54.448 ms; median read window is 0.960 ms, maximum 9.733 ms. One bounded
read at 19.151 s breaks association. There are no gaps above 100 ms.

Corrected raw trace SHA256:
`5bb7e01404b13e0936a4b591b4971d95c88b1376167a32dcd96da657f737c8ab`.
Use `report-final.json` in that session for the current analysis. Earlier reports
are preserved. The first analysis counted pending -1 IDs; later reports exclude
them. Captured source hashes remain in each manifest.

## Observed native sequences

The corrected run records six NormalAttackAction plays (325, 328, 331, 348, 351,
354) and one HardHitAction play (340), totaling 142 active-action samples and 21
state changes including returns to idle. Raw `_attackCount` changes 0 -> 1 in
each observed play. That is a state observation, not a callback receipt.

The first normal sequence began after MP had already recovered to its cap. It
cannot quantify AMPR. The UI later displayed a Nut-use message and visible loot;
that context is retained as a marker rather than assigned a causal explanation.

At 64.450 s, HardHitAction play 340 coincides with local MP 319 -> 219. Received
MP follows at 64.600 s while that same play is still sampled. Target 14834 local
HP becomes zero at 65.000 s, then the target disappears; disappearance is not an
independent kill receipt. Afterward, idle paired MP rises by 4 at 71.600, 74.600,
77.600 and 80.600 s, reaching 235.

The next native normal sequence provides action context for three +13 steps:

| Normal play | Target 14837 local HP change | Received HP change | Paired MP change |
| ---: | --- | --- | --- |
| 348 | 96 -> 53 at 82.100 s | 96 -> 53 at 82.251 s | 235 -> 248 at 82.251 s |
| 351 | 53 -> 17 at 84.051 s | 53 -> 17 at 84.201 s | 248 -> 261 at 84.201 s |
| 354 | 17 -> 0 at 86.050 s | Target loss; no sampled received zero | 261 -> 274 at 86.150 s |

All three MP changes occur while the corresponding NormalAttackAction is still
sampled. This supports alignment to distinct normal plays in this setup. It
does not establish the exact callback, universal recovery formula or remote rules.

IPC `now_mp` remains 319 throughout; it disagrees with typed local MP in 986 of
1,799 comparable rows. The worker waits for commands, and the existing resource
fields are published by probes/feature paths rather than a continuous sampler.

## Display, checks and remaining work

The menu now labels resources `Cache age unknown` and discards telemetry when
the game is offline or the snapshot PID differs from the current game. Seven menu
tests pass, including this process filter. Fifty Python tests pass and all three
Python files compile. A release menu build succeeded and its label was visually
verified. The preview was then closed without dispatching any command.

Preview: `work/telemetry-menu-20261019/companion.exe`, SHA256
`bcd9171891293f562149ba066491f093f72c5c6c74a3a79c36a8ede96e61c8ea`.
This is a menu-only preview, not a full staged release. Canonical menu, loader and
core hashes remain unchanged. No core was built or loaded by this work.

Final smoke also hashes the on-disk game files: ToramOnline.exe
`164a72cb55c32780cf3d1e2d8645b300bbc650ae0c70ef00a0d81de9e8d4a80e`,
GameAssembly.dll `a3faf7156734768a3ff7349d4b94de029b68b823d974bf52f92bbc77f93f46dd`.
These are on-disk identities, not mapped-byte verification.

Next: use play IDs to bound further executing-path evidence, and obtain genuinely
current geometry before another range candidate. Callback publication still needs
an observation-only mechanism and separate deployment; none is claimed here.
Sensor v2 names the literal action Id `id_field`; prototype v1 used `skill_id`,
which must not be interpreted as a verified skill database identifier. Do not
repeat unused MP getters, client-cache refills or the unchanged range patch.
