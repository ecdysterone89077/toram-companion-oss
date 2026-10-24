# GitHub movement restoration — 2026-10-22

Source/candidate version: 0.96.4-rc.8. Telemetry remains 962.
Baseline: the operator's v0.96.1 ZIP, SHA-256
`471F9A866313D7E41D6DA14E67D6DCF7503B7902F9549CA21F84C97A04968CC5`.
Its checked principal sources match commit `4dd9546` after newline normalization.

## Change

The intervening movement rewrite replaced fixed speed with a 1.25 multiplier,
removed direct field writes, removed the dash override, and reduced no-collision to
one getter. This repair restores the baseline's six speed and six no-collision hook
targets and their forced values while retaining local-actor scoping and native
forwarding for every unbound, disabled or cleared instance.

Speed's five float getters return 15.0, its dash getter returns true, and its
three stored speed fields are 20.0. Metadata resolution uses `moveSpeed` and
`defaultMoveSpeed` on CharacterMove, and the inherited
`<DefaultMoveSpeed>k__BackingField` on PlayerActionManager. The original ZIP
attempted several nonexistent capitalized field names; this repair uses the
actual backing fields rather than reproducing silent failed writes.

No collision restores the six baseline decisions: FieldRayPick wall checks false,
all-floor movement true, CharacterMove wall collision false, gravity false,
ignore-height true, and movement-height checks false. Ten owned field edits
also set the corresponding native wall/floor/ground flags, gravity, radius
and ray height. The radius and height are 0.001, as in the supplied baseline.
Properties that forward to the same FieldRayPick field are captured once.

All fields are looked up by metadata name and checked for their exact type.
No new hardcoded field offsets or writes to native executable bytes are used.

## Lifecycle

Every movement hook is required. Capture and instance rooting precede mutation;
the active flag is published only after all hooks and field writes succeed.
Failure clears callback bindings, restores hooks and all successfully owned
field edits, and returns an error. Incomplete cleanup keeps the saved state,
sets pending/partial status, blocks explicit ON, and makes OFF retry without
needing to resolve a new player. OFF and worker shutdown restore exact saved
values only while those fields still contain this activation's written values.
Later native updates are preserved.

## Verification

Evidence directory: `work/movement-restore-20261022/`.

- 180 workspace Rust tests and the additional child-process reader passed.
- Eight movement tests cover callback scoping, exact float/boolean restoration,
  replacement actors, native updates, capture races, partial writes and retry.
  A separate speed/dash test verifies the baseline constants and native
  forwarding; the active local speed path does not execute the native getter.
- `field-audit.json`: all thirteen field names/types exist in PC_v226_dump.cs,
  including the inherited PlayerActionManager backing field.
- `baseline-hook-comparison.json`: both sets of six class/method targets match
  the supplied v0.96.1 ZIP.
- `workspace-tests.log` and `clippy.log` record passed tests and zero warnings.

Reproduction:

```powershell
cargo test --workspace --locked --offline
cargo clippy --workspace --all-targets --locked --offline -- -D warnings
cargo xtask build --no-test --stage-dir work/movement-restore-20261022/candidate
```

## Live validation and promotion

PID 24928 was a newly restarted client, with no valid companion telemetry
before loading. The rc.8 core loaded at `0x7FFA97CC0000`. Speed installed all
six hooks and three field edits. The operator reported **Clearly faster**;
the speed callback execution bit was 2. OFF restored all six hooks and owned
fields, with toggle and partial masks both zero.

No collision installed all six hooks and ten field edits. The operator reported
**Walks through the wall**. Its callback-observation bit remained zero, so
this is operator-observed gameplay through the field mechanism, not evidence
of the getter detours being called. OFF restored its hooks and owned fields.

Combined ON produced mask 6 and no partial state. Combined OFF returned to
mask 0 and no partial state. A final probe succeeded. This verifies the
reported movement effects and cleanup in this session; map transitions and
long-session behavior were not measured.

The canonical `companion.exe`, `src/wcore.dll`, `src/wboot.exe`, `observer.exe`
and `sha256.json` were backed up under
`work/movement-restore-20261022/canonical-backup-before-rc8/` and replaced with
the tested candidate. Their hashes match staging, the packaged release folder
and the Cargo target artifacts. A Cargo freshness check found all eight
compiler artifacts fresh. The root loader's probe reused the same single core.
The reopened root menu reports `Game Companion (v0.96.4-rc.8)`.

Final state: worker active, speed OFF, no-collision OFF, active mask 0, partial mask 0.
F5/command 68 toggles speed; F6/command 69 toggles no-collision. Other gameplay
features were not tested by this repair.

The receipt is `work/movement-restore-20261022/receipt.json`; the session log
and phase status files are in its `live-24928/` subdirectory. Archives under
`dist/Toram-Companion-v0.96.4-rc.8-*` were packaged before the live test and
retain their original `candidate_gameplay_pending` receipt. This later
verification record supersedes that status for the two tested movement
features only. No archive was overwritten or published externally.
