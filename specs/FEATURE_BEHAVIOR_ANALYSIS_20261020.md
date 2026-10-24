# Feature behavior analysis — 2026-10-20

Source checkpoint: `8ab322ad653dd0d74239d28273ec757d0dbf42bc`.
Packaged release: v0.96.1; telemetry layout 960.

This review inspected source, prior live-test records, and release files. It
reran the current offline Windows test suite. It did not perform a new live
gameplay comparison. Historical gameplay observations below remain scoped to
their recorded builds, characters, actions, and test intervals.

## Finding

The project contains real game modifications, experiments with failed or
unresolved gameplay outcomes, and three newly added controls with no behavior
implementation. These coexist because command delivery, hook installation,
callback execution, local calculation, and lasting gameplay outcome are
different success conditions. The menu's ON badge primarily represents
activation state.

## How the implemented path works

1. The menu turns buttons, profiles, or saved plans into command IDs. It pins
   client identity and serializes delivery (`crates/companion/src/control.rs`).
2. The loader locates the chosen process, loads or reuses the core DLL, invokes
   its command export, and reports the returned status
   (`crates/wboot/src/lib.rs:938`).
3. The core starts its worker on explicit dispatch, attaches that thread to the
   IL2CPP domain, and runs `dispatch_game_command`
   (`crates/wcore/src/runtime.rs:79`, `:339`).
4. Feature handlers resolve game objects and methods. The hook manager changes
   selected MethodInfo/virtual dispatch pointers, stores originals, and
   restores owned hooks on disable. Other feature code also writes local
   movement or action fields (`crates/wcore/src/hook.rs:119`,
   `crates/wcore/src/features.rs:1453`).
5. A gameplay change occurs only when the game executes an affected path with
   the relevant objects. Some callbacks return altered timing/stat values;
   others preserve native calculation while changing critical selection,
   stability, or local checks.
6. Received combat state can confirm or correct the local prediction. The
   response observer reads received data and forwards the original callback;
   it does not control the remote result
   (`crates/wcore/src/combat_response.rs:432`).

Consequently, successful client modification does not imply that every changed
quantity is authoritative. The saved experiments demonstrate that distinction;
they do not recover the complete server validation formula.

## Feature-by-feature evidence

| Feature | Actual implementation | Evidence and present limitation |
| --- | --- | --- |
| Stability / critical selection | Local stat callbacks, normal attack calculation scope, stability and hit hooks. | Historical comparisons record lasting normal/Hard Hit damage and no Graze. The v96.1 comparison recorded normal/stability/hit callbacks. This is evidence for the tested settings, not every multiplier. |
| Instant attack | Local next-attack callbacks return zero delay; local motion callbacks return 60; attack-speed getter is modified. | Historical comparison reports faster melee. v96.1 recorded 60 next-attack and 188 motion callbacks. Callback counts alone do not measure a DPS multiplier. |
| Invincibility | Local invincibility/death callbacks and suppression of the selected local DamageHp call. | Implemented and included in the bounded v96.1 combination test. That test is not an independent demonstration against every damage or death mechanism. |
| Speed / no-collision | Movement getters, local movement fields, wall/floor/gravity checks. | Real implementations. The latest v96.1 four-feature comparison did not exercise these modes. Some writes ignore their Boolean failure result; installation success need not mean every sub-operation succeeded. |
| Anti-rigidity | Delay/rigidity getters and one-time clearing of local delay/action flags. | Real implementation and included in combat profiles. The latest four-feature comparison did not separately establish this feature's outcome. |
| MP recovery bonus | Hit hook samples MP; bonus is applied by two hooked AtkMpRecovery getters. | Recorded combat reaches the hit sampler but not the bonus getters. Latest validation records 58 MP hit samples and zero AMPR getter calls. No added per-hit recovery is established. |
| Extended range | Local range/assist hooks, normal-action preparation and geometry changes. | Earlier candidate failed the distant-melee comparison despite successful setup. Latest validation retains unresolved extended-range acceptance. |
| Large critical boost | Changes the requested local critical-damage value; skills are additionally capped. | +1000 failed the recorded gameplay comparison. Smaller settings have conditional operator observations; basic and skill outcomes differ. Exact remote limits are unknown. |
| Full heal, command 2 | Writes localHp/localMp to their maxima and publishes those values. | Explicitly a local state write; received state remains unchanged. The command does not establish a lasting server-confirmed heal. |
| New adaptive ladder / cadence / target lock, commands 94–99 | Three AtomicBool fields, store operations, log messages and status 0. | No reads of those flags exist anywhere in crates. No stepping, pacing, or target-acquisition behavior consumes them. |

## Concrete reasons features can appear enabled without working

### 1. The newest three controls have no effect beyond storing state

`crates/wcore/src/features.rs:155` declares the three flags. Handlers at `:3085`
only store true/false, log an enabled/disabled message, and return zero.
A repository search finds declarations and stores, but no loads.

The existing adaptive skill cap is separate and real: `features.rs:175` changes
the cap using classified skill responses from `combat_response.rs:337`.
The new ladder flag neither enables nor disables that existing mechanism.
Profiles and the eight-feature active mask also do not include the new flags.

### 2. MP modifies a path not observed during the tested combat

`features.rs:2862` installs ActionHit and two AtkMpRecovery hooks.
`features.rs:1150` forwards native ActionHit and samples MP; it no longer refills
the bar. The bonus arithmetic is in `features.rs:1251`, reached through the
getters.

The v95.8 observer record reports 44 hit callbacks and zero recovery/getter
callbacks. The later v96.1 validation again records zero AMPR getter calls.
For those runs, changing the selected bonus cannot affect combat through the
unexecuted getter callbacks.

Earlier direct local-MP refills caused the bar to fill and then fall back after
received updates. Removing those writes corrected misleading presentation; it
did not discover a working source of extra authoritative MP.

Sources: `specs/MP_AMPR_OBSERVE_20261019.md`,
`specs/MP_ACCEPTED_20261019.md`, `VALIDATION.md`.

### 3. Range activation changes client state without proving lasting damage

`features.rs:2402` installs range-related hooks, then sets its active bit.
The failed candidate record reports 28 preparations/patches but zero
range-hit checks/extensions. Distant local HP sequences included
`91 -> 43 -> 91` and `47 -> 0 -> 47`; the operator's corrected report says
distant attacks did not produce lasting damage while close attacks worked.

This establishes failed gameplay outcome for that candidate and an unobserved
geometry callback. It does not establish the complete remote distance rule or
prove that adding one more hook would solve it.

Source: `specs/RANGE_TARGET_CANDIDATE_20261018.md`.

### 4. Damage arithmetic and gameplay acceptance are separate

The +1000 experiment displayed 1,358 critical damage and a death animation while
the enemy remained alive. Native reset restored normal kills according to the
operator's controlled comparison.

Current source applies the full selected request inside the normal-attack
scope, but caps other critical-damage reads using the adaptive skill cap
(`features.rs:503`). Thus selecting +300 does not promise +300 on every skill.
The skill cap responds to observations when feedback is enabled.

The accepted/rejected telemetry is a classifier over decoded response fields
(`combat_response.rs:298`), not independently measured per-feature DPS or proof
of a kill/reward. It should not replace target-correlated outcome evidence.

Sources: `specs/bugs/BUG-2026-10-19-critical-damage-acceptance.md`,
`specs/SKILL_CLAMP_20261019.md`, `MODLOG.md`.

### 5. Success reporting is weaker than feature effectiveness

- Menu badges read `active_toggles` (`companion/src/main.rs:545`).
- Feature handlers generally set the bit after their setup succeeds.
- Optional auxiliary hooks can be skipped while overall installation succeeds
  (`features.rs:1669`); zero total installed hooks and failed critical hooks do fail.
- Several speed/no-collision/anti-rigidity field operations discard failure results.
- The protocol accepts a numeric interval containing unused IDs
  (`protocol/src/lib.rs:107`); the game dispatcher has a default branch that
  logs an executed diagnostic/legacy command and returns zero
  (`features.rs:3115`). For example, an otherwise unblocked command 4 can reach
  this no-op branch after worker initialization.

The existing command receipts correctly establish delivery to a selected
process. Their status is insufficient as an effectiveness indicator.

## Verification performed in this review

`cargo test --workspace --locked --offline` passed:

| Test target | Passed |
| --- | ---: |
| companion library | 14 |
| companion executable | 2 |
| observer | 3 |
| protocol | 12 |
| wboot | 17 |
| wcore | 64 |
| Total | 112 |

The separate child-reader probe also passed. The simulated worker panic is an
expected passing fault-isolation test.

These tests exercise arithmetic, guards, synthetic callback objects, dispatch,
hook ownership and IPC. For example, the MP test at `features.rs:3336` supplies
a synthetic native getter; `:3368` checks bonus arithmetic directly. They cannot
establish that real combat calls the getter or retains the resulting MP.
The new command-ID coverage likewise does not test the advertised gameplay.

## Source and binary identity

SHA-256 was compared directly against entries in the release ZIPs.

| File | Matches packaged v0.96.1 |
| --- | --- |
| root companion.exe | Yes |
| src/wcore.dll | Yes |
| src/wboot.exe | Yes |
| root observer.exe | No |
| current protocol source | No |
| current feature source | No |

The current source includes commit 8ab322a; the canonical menu/core/loader still
match the earlier release. Source edits do not automatically change those
binaries. No claim is made here about which module is currently loaded in a
running game process.

Core release SHA-256:
`D429663B1A66CC8C066F5FA1EF7B891873BA251A55B54EB456AF59C2C9DF1AA7`.

Root observer SHA-256:
`F4D0562252FC18775C357A28180493085C020126B72425A6FE7B23A477830E8D`.

Packaged observer SHA-256:
`4B2FCCA7A7939437DC68D8231355954AAB4DD27CC1DCACCEFF5DD9AB2BD0CABA`.

## Engineering priorities

1. Reject unimplemented commands and expose the three new controls as unavailable
   until they have behavior. Keep existing adaptive feedback distinct.
2. Separate requested state, installed hooks, observed callback activity and
   measured gameplay outcome. ON should not imply all four.
3. Make required field writes and hook coverage part of the setup result; report
   partial optional coverage explicitly.
4. Identify the loaded build by hash/capabilities, and reconcile the observer
   discrepancy before attributing new observations to a specific release.
5. Give each experimental feature an outcome criterion: MP gained per comparable
   hit and still usable after received updates; persistent same-target damage
   at recorded distance; damage/kill results distinguished by action and setting.
6. Preserve the working baseline and tie each outcome record to its exact build
   and conditions. Passing local tests and a no-warning interval cannot promote
   an unresolved feature to verified.
