# Changelog

## 0.96.4-rc.8 - 2026-10-22

- Restore the GitHub v0.96.1 speed and no-collision targets and values: fixed 15.0
  speed getters, dash enabled, 20.0 movement fields, and all six no-collision hooks.
- Resolve the thirteen edited fields by their real metadata names and types.
  Preserve exact originals instead of resetting to guessed defaults.
- Require all movement hooks and fields. Roll back failed activation, retain
  failed cleanup for OFF to retry, and keep callbacks inactive while pending.
- Preserve local-actor scoping and native forwarding for other instances.
- 180 Rust tests plus the child reader, thirteen metadata field checks and the
  baseline hook comparison pass; clippy has no warnings. The operator confirmed
  faster movement and walking through walls in the live rc.8 client. Combined
  ON/OFF restoration passed, and canonical root binaries now match the candidate.


## 0.96.4-rc.7 - 2026-10-22

- Fix the observer's capture and watch refusing to read the worker log. The
  log is appended by the worker and can carry bytes that are not valid
  UTF-8, and the reader gave up on the whole file because of them, hiding
  every record in it. The bytes are now decoded lossily.
- Tell the operator that a running capture is still in memory. Its records
  reach the log only when the capture is stopped, so a capture taken while
  it runs reads back empty and looks like a broken capture.
- Check the port's hook targets against the build that is actually
  installed. A metadata dump of the installed build is present in the
  workspace; it was supplied as a downloaded reference rather than produced
  by the game, and it describes the build it was verified against, which the
  tracked export does not, because the game is updated independently of
  this repository. The audit uses the dump only while it still corresponds:
  its digest and the installed module's mapped size are checked against the
  verification record, and a mismatch falls back to the export. A new check
  asserts that every ported target resolves in it. All 63 specifications
  resolve, including the two gates, with no ownership conflicts.
- Ignore the downloaded metadata dumps in git. They are large and are not
  ours to track.

182 Rust tests plus the child reader, the optimized worker probe and the
isolated DLL bootstrap passed, with 0 clippy warnings under `--all-targets`.
Release package Toram-Companion-v0.96.4-rc.7 was built and packaged with all
54 packaging assertions passing.

## 0.96.4-rc.6 - 2026-10-22

- Resolve the two gates the port record had given up on. Both were recorded
  as "not identifiable in the installed metadata"; a search of the metadata
  by concept, not by the original's Android offsets, found them.
  - `gacha` — `GameUserDataManager.get_IsBeginner`, forced true. Faunas
    writes `mov w0, #1` over a boolean getter, so this is a port of the
    original's shape: a boolean getter replaced with an immediate true.
  - `unlock_map` — `UIWorldMapPanel.get_IsTapLock`, forced false. The
    original rewrites a register inside a method body rather than replacing
    a return value, so this one is a reading of it rather than a port, and
    it stays out of the one-click preset until the map is seen to open with
    every area reachable.
- Add the two features to the companion window, so the Faunas row now
  carries all 24 features.
- Extend the target audit to cover both gates: 63 specifications, no
  resolution problems, no ownership conflicts.

182 Rust tests plus the child reader, the optimized worker probe and the
isolated DLL bootstrap passed, with 0 clippy warnings under `--all-targets`.
Release package Toram-Companion-v0.96.4-rc.6 was built and packaged with all
54 packaging assertions passing.

## 0.96.4-rc.5 - 2026-10-22

- Record the critical-damage ladder's claim in the response capture. The
  ladder returns an absolute value, and the capture previously carried only
  the server's verdict, so a claim the server capped and a claim that never
  reached the calculation looked identical. The native value is now read
  under the same guard the rest of the critical path uses, and the capture
  carries the claim, the native value and the server's verdict together.
- Require a ladder to be armed before it can force a value, so a ladder
  feature that somehow reached its enabled state without a rung cannot apply
  the forced-value slot's placeholder.

181 Rust tests plus the child reader, the optimized worker probe and the
isolated DLL bootstrap passed, with 0 clippy warnings under `--all-targets`.
Release package Toram-Companion-v0.96.4-rc.5 was built and packaged with all
54 packaging assertions passing.

## 0.96.4-rc.4 - 2026-10-22

- Make a value ladder a value, not a switch. The four ladder features draw
  their value from a forced-value slot, and those slots start at zero, so
  turning one of them on from the window applied the placeholder: the
  critical-damage and critical-magic-damage ladders forced zero damage, and
  the element ladder forced "None". A ladder feature now refuses to turn on
  until a rung has been chosen, and its button picks the rung — the next one
  when the feature is already on, the last one chosen when it is off. The
  ladder commands are unchanged and still pick a rung directly.
- Name the element each rung selects in the log, so a rung reads as "Fire"
  rather than as the number one.

181 Rust tests plus the child reader, the optimized worker probe and the
isolated DLL bootstrap passed, with 0 clippy warnings under `--all-targets`.
Release package Toram-Companion-v0.96.4-rc.4 was built and packaged with all
54 packaging assertions passing.

## 0.96.4-rc.3 - 2026-10-22

- Fix the Faunas `element` feature's target class. It named
  `PlayerBattleStatus.GetEquipElement`, but that class is a different
  hierarchy — it implements `IPlayerStatusCalculator` and does not declare
  the method at all, so the hook never resolved and the feature rolled
  itself back in every live session. `GetEquipElement` is declared by
  `PlayerStatusBase`; `PlayerStatus` derives from it without overriding it,
  so the base declaration is the one to replace.
- Let the Faunas `critdmg` ladder and always-critical share the two
  `get_CriticalDmg` declarations instead of colliding over them. A
  `MethodInfo` has exactly one owner and the hook engine refuses a second
  detour on a method that is already hooked, so the ladder used to roll
  itself back every time always-critical was on. The shared detour reads
  whichever channel is active: the ladder's absolute value wins while it is
  on, exactly as the original's prologue patch did, and the hooks are
  restored only when the last owner lets go. The ladder is not part of the
  one-click preset, so the default loadout keeps the ratio cap.
- Add `scripts/faunas_target_audit.py` and its 12 regression checks. The
  audit reads every hook specification out of the port's source and checks it
  against the installed metadata snapshot, so a target class that does not
  declare its method is caught offline instead of in game. The shipped port
  audits clean at 61 specifications.
- Build and package Toram-Companion-v0.96.4-rc.3 with all 54 packaging
  assertions passing.

176 Rust tests plus the child reader, the optimized worker probe and the
isolated DLL bootstrap passed. The live client PID 7868 still runs the rc.2
core: the two fixes need a game restart.

## 0.96.4-rc.2 - 2026-10-22

- Add native NT path fallback (PROCESS_NAME_NATIVE) to client process identity
  resolution (identity_from_handle). Win32 DOS path resolution fails with
  error 31 (ERROR_GEN_FAILURE) under the Asobimo launcher and driver
  environment, which previously prevented companion, wboot, and CLI batches
  from discovering live ToramOnline client processes.
- Synchronize Faunas One-Click Loadout state with shared telemetry: publish
  bit(ONE_CLICK_INDEX) on toggle, auto-clear the preset bit if any constituent
  feature is individually disabled, and clear it on worker shutdown (disable_all).
- Resolve all compiler and clippy warnings across the entire workspace (0 warnings
  under --all-targets), including method hook Default implementation, alignment
  assertions, collapsible conditionals, and relocated test module definitions.
- Build and package Toram-Companion-v0.96.4-rc.2 with all 54 packaging assertions
  passing.

174 Rust tests plus the child reader, the optimized worker probe and the
isolated DLL bootstrap passed. Live client PID 24376 discovered and responsive.

## 0.96.4-rc.1 - 2026-10-22

- Port the Faunas Android release's mechanics to the Windows companion. Every
  prologue override in the original becomes a managed-dispatch detour that
  returns the same value without running the native body.
- Add 21 ported features with their own command band, telemetry bits and
  window buttons: always critical, max motion, max stability, no cast time,
  long range attack, always pierce, disable resist, the four mob controls,
  the skill AOE and burst groups, mob proration, equip element, the two damage
  ladders, skip battle, no domination, no drop visual and a one-click loadout.
- Decode the original's value ladders out of its ARM64 instructions instead of
  transcribing the menu, and pin every constant with a test that re-derives it
  from the original byte strings.
- Record three defects in the original that the port does not reproduce, the
  mechanics the dispatch door cannot express, and the two features whose
  targets are not identifiable in the installed metadata.
- Add a per-class hook target so a skill class can be resolved by name at
  install time, and make every ported hook a core hook so a half-installed
  feature rolls back instead of running.

174 Rust tests plus the child reader, the optimized worker probe and the
isolated DLL bootstrap passed. No live gameplay test was run: every ported
feature is verified to resolve and install, none is verified to be accepted by
the server. See [the port record](specs/FAUNAS_PORT_20261022.md).

## 0.96.3-rc.3 - 2026-10-22

- Preserve pending cleanup when range or MP activation fails and rollback
  cannot restore an installed hook. Explicit OFF retries restoration and ON
  stays blocked until cleanup succeeds.
- Add a regression that reproduces the lost-cleanup state before the fix and
  verifies both features recover afterward.
- Record rc.2 live lifecycle results and the depleted-MP observation separately
  from rc.3 verification. Correct unsupported MP and range comments.

166 Rust tests plus the child reader, 54 packaging assertions, the optimized
worker probe and isolated DLL bootstrap passed. Rc.3 gameplay testing is
deferred at the operator's request. See the validation record in the
development repository.

## 0.96.3-rc.2 - 2026-10-21

- Keep repeated derived-only hook activation from overwriting shared methods;
  reject null original dispatch pointers.
- Recover worker admission after timed-out commands complete, consume stale
  completion events, and serialize concurrent stop/dispatch handle cleanup.
- Enable Rust panic unwinding in optimized builds and require an isolated
  release-mode lifecycle/panic check before staging artifacts.
- Restore invincibility/timing/critical/anti-rigidity hooks without resolving the player;
  preserve failed cleanup for retries and block reactivation while pending.
- Resolve the Cargo workspace from member directories, build fresh binaries
  before `xtask run`, and forward arguments and executable exit codes.
- Package a separate artifact directory without replacing old root binaries;
  replace obsolete development commands and unsupported threshold comments.

165 Rust tests plus the child reader and 54 package assertions passed.
Gameplay and incident causality remain open. See
[the repair record](specs/RUNTIME_REPAIR_RC2_20261021.md).

## Unreleased maintenance — 2026-10-21

- Replace zero-risk/known-good menu claims with Legacy preset and request labels;
  keep `safe` as a CLI alias and preserve profile commands.
- Fix package-relative checksum paths; reject existing release outputs and
  invalid version path components instead of deleting prior artifacts.
- Make packaging manifests report their actual verification scope; include
  the packager and its synthetic regression checker in source archives.
- Reconcile current documentation with verified evidence and unresolved incident
  causes. See [the maintenance record](specs/OFFLINE_MAINTENANCE_20261021.md).

The older entries below record historical development statements. Exhaustive
blind-spot coverage, zero risk, optimal rotation and specific sanction-cause
claims are superseded by the later audits and maintenance record.

## 0.96.3-rc.1 - 2026-10-21

### Blind spot map and the rotation doctrine

- Add `specs/BLIND_SPOT_MAP_20261021.md`: the complete inventory of the
  quantities the client can change, taken from the IL2CPP metadata of
  `PlayerBattleStatus` and `PlayerSecondaryStatus`, each classified as
  server-blind, server-checked or server-tracked, with the evidence.
- The blind set is finite and already fully utilized. There is no further
  client-side quantity that adds damage without a ceiling, which is the
  answer to "find more server blind variables".
- The remaining power is in the rotation, not the values. The 2026-10-20
  capture shows the server's damage allowance is a function of the attack
  history it tracks: the rejected Hard Hits both followed a run of basic
  attacks, and the third Hard Hit in the same rotation landed 1566. The
  highest-damage, lowest-risk rotation is therefore **skills only, no
  basic spam**, which costs nothing and needs no client change.

### Observation

- `observer capture` now extracts the response stream as well as the action
  stream, so one file holds the phase, the feature mask, the distance, the
  server's verdict, the received HP, the allowance, the violation and the
  three received proration bytes. It reports the proration sample count.
- `observer watch` follows the response stream too.

### Range and wall-check rework

- Retire the range inflation. The server validates the distance of a damage
  claim: an out-of-range basic attack is dropped, and an out-of-range skill is
  sanctioned and kills the character. The feature no longer widens any range
  check, hit size or action range, so the client cannot make a claim the
  server will reject.
- Command 71 becomes **Engage**. It installs the three assist-move getters
  and nothing else: the native range checks stay in place, the character
  closes the distance, and the attack lands from a legitimate range. The
  assist-move value is forced to true only for the local battle manager's
  queued action, so another actor's action is never affected.
- No-collision overrides only the movement-level wall decision. The ray picker's
  wall measurement also produces the floor contact and the movement normals,
  and disabling it left the character stuck against a wall. No field is
  written at all now.
- Rename the menu control from "Range 30m" to "Engage (assist-move)".
- Two regression tests pin the change: the range feature must never install a
  hook that widens a range check, and the no-collision feature must touch only
  `get_EnableWallHit`.

### Movement safety rework

- Apply move speed as a ratio against the value the game itself reports
  (1.25x) instead of a flat `20.0f` constant, and clamp every emitted speed
  to 15.0, the top of the native dash envelope.
- Stop forcing the GM dash flag. A debug movement mode has no server-side
  validation counterpart.
- Leave the movement fields native. A scaled field combined with a scaled
  getter compounds the multiplier, and the engine rewrites those fields every
  frame anyway.
- Apply the ratio exactly once per native getter chain through a thread-local
  guard, following the MP nesting doctrine.
- Reduce no-collision to wall collision only. Gravity, height range checks, floor hit
  checks, ground move flags and the collision radius stay native so the player
  keeps a floor anchor and the server keeps receiving navmesh-valid
  coordinates.

### Observation

- Add command 116 as an operator phase marker. Every later observation sample
  carries the phase id, so one capture can be split into named phases.
- Stamp each sample with the active feature mask and the observer callback
  counter, so a sample states the conditions it was taken under.
- Record the target distance from the game's own `PlayerDistance` cache, with
  an explicit source label and a HUD-metre conversion. An unresolved reading
  is reported as unavailable rather than as zero.

### The validated-dimension model

- Add `specs/VALIDATED_DIMENSION_MODEL_20261021.md`. The root cause of the
  project's failures is not "too much value": it is whether the server can
  independently recompute the number the client claims. The features that
  work flawlessly change numbers it cannot recompute (the accuracy rolls,
  the crit rate, the player's own HP, the attack cadence). The features that
  fail change numbers it can (the damage magnitude, the velocity, the
  position, the MP balance). The document also records the sanction ladder,
  explains why the old no-collision mode was only conditionally safe, and enumerates
  the mechanism surface that remains.

### Critical damage ceiling

- Bound every effective critical damage value to `CRIT_DAMAGE_MAX_RATIO`,
  135% of native. The 2026-10-20 comparison accepted an absolute 200% at a
  native 150 and rolled back 250% and above, including native +100, which is
  also 250% at that native. The bound is a ratio because the native value
  varies by character and build.
- The old ladder expressed its steps as points over native, which put a
  known-failing 250% in the first rung. The bound is now absolute.
- The adaptive ladder steps *down* on a rejection and never climbs: a ceiling
  is a measured boundary, not a floor to be climbed past. A fresh target
  restores it.
- Never reduce the native value. An explicit absolute preset below native is
  honoured, because lowering a claim is not a sanction risk.

### Safe loadout

- Add a **Safe loadout (0 risk)** profile: one click applies invincibility,
  always crit, instant attack, anti-rigidity and the 200% known-good
  critical damage, with MP on native and the server-tracked features off.
- Relabel the critical damage controls so the window tells the truth:
  250% and 300% are shown as capped, the additive ladder collapses to a
  single "Damage max (capped)" button, and 200% is labelled as the known
  good value.

### Capture controls

- Add a Capture row to the companion window: action capture (80), response
  capture (82), the phase marker (116) and a forced stop-and-log (93). The
  observation controls were previously reachable only from the command line.
- Keep the phase marker reachable while a capture is running. It is the one
  command a capture needs, because it is how a capture is split into
  operator-named phases.

### Observer

- Add `observer capture`, which extracts the observation records from the
  worker log into a clean JSONL capture file and reports the event count, the
  phases it saw, how many samples carried a distance, and whether the begin
  and end records were present.
- Add `observer watch`, which follows the worker log for a bounded time and
  prints the diagnostic lines it produces.
- The JSONL recorder was written for this and had never been wired to a
  command.

### Documentation and registry

- Rewrite `specs/state.yaml`. It described v74, `wcore74.dll` and the
  C++/Python lineage, with a maximum command of 63.
- Complete `specs/bugs/registry.yaml` with the records that were missing:
  the critical damage acceptance failure, the 03:06 memory-response event,
  and the spec-versus-source drift that caused the movement fix to be
  missing from the build.
- Mark `MENU_V48.md`, `specs/MENU_V49.md`, the v49 bug record and
  `docs/HANDOFF.md` as historical, so they cannot be mistaken for live
  specifications of the current pure Rust workspace.
- Replace the README's observation section, which documented the removed
  Python observer scripts.
- Add `specs/FEASIBILITY_DECISION_20261021.md` and
  `specs/LIVE_TEST_RUNBOOK_20261021.md`.

### Packaging and validation

- Add `package-release.ps1`: a reproducible release packager that stages the
  Windows package, hashes the source allowlist, builds both archives, reads
  every archive entry back and verifies it against the staged file, and writes
  the checksum list and release receipt.
- 156 workspace tests pass across all six crates, with no compiler warnings.
- Package v0.96.3-rc.1 in `dist/`, both archives read-back verified.
- Correct the release script's registry filter coverage and ignore local
  registry exports, which can embed live session tickets.
- The dedicated Strix rescan was attempted and blocked by the LLM API
  provider returning `402 billing_not_configured`. The run recorded zero
  coverage entries and is not closure evidence. The instruction file is
  preserved so the rescan can be run unchanged once billing is restored.

## 0.96.2 - 2026-10-20

- Wire adaptive-cap commands into combat calculations and profile plans.
- Reject unknown commands and unavailable pacing/target-acquisition commands.
- Report partial activation and distinguish armed hooks from observed callbacks.
- Separate local and received MP; expose missing bonus getter calls.
- Upgrade telemetry to layout 962 and update menu/observer diagnostics.
- Stage/hash all four matched binaries; support explicit release directories.
- 119 workspace tests plus the child reader passed; release build, CLI and
  offline menu checks passed. Gameplay is deferred; MP/range remain unresolved.

## 0.96.1 - 2026-10-20

### Repairs

- Initialize the worker after explicit dispatch, outside DllMain. Preserve the
  host exception filter, thread notifications, PE headers and exports.
- Give nineteen invincibility, timing and stat callbacks their own native originals
  and exact local-instance bindings; forward other instances unchanged.
- Use selected virtual slots for verified CheckHit, CalcStable, ActionHit and
  ActionPreparation paths. Preserve inherited MethodInfo and sibling dispatch.
- Track virtual-slot ownership and restore MP observer hooks during shutdown.
- Stop installing unrelated MP observers on normal feature enable.
- Remove the automatic local resource refill on invincibility enable.
- Correct the Boolean-sized speed-flag write and label stale resource samples.

### Validation

- 112 Windows workspace tests plus a separate-process telemetry reader probe.
- Optimized release build, CLI checks and isolated old/new DLL bootstrap check.
- Original feature combination enabled over eleven minutes, including a
  484-second window after the first confirmed hit: 60 normal calculations and
  58 hit callbacks in mixed combat and idle activity. No engine shutdown logged;
  the operator confirmed no warning and a playable game.
- Live owned-hook restoration and same-core worker restart/re-enable passed.

### Existing limits

The original protection check was not isolated. The live result is a bounded
comparison. AMPR getters still recorded zero calls; MP-bonus effectiveness and
extended-range server acceptance remain unresolved. Telemetry layout stays 960.

## 0.96.0 - 2026-10-20

- Added repeatable profiles, per-client selection, saved command plans,
  serialized cancellable batches, CLI controls and received combat feedback.
- Fixed the menu runner rejecting valid commands above 80.

Earlier implementation and live results are retained in MODLOG.md and specs/.
