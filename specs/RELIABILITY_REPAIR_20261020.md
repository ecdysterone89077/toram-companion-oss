# Reliability repair - v0.96.2

Date: 2026-10-20. Base commit: 8ab322ad653dd0d74239d28273ec757d0dbf42bc.
Scope: repair confirmed local defects, build and test locally, leave gameplay
validation for the operator. Toram was not running during this validation.

## Changes

- Commands 94/95 now enable/disable the adaptive skill damage cap. Disabled
  mode stays at +100 and ignores advancement feedback; repeated enable preserves
  current progress. Native profiles disable it; other profiles enable it after
  starting feedback. Stopping feedback also disables adaptation.
- Commands 96..99 explicitly return unavailable. Their old flags had no pacing
  or target-acquisition implementation. Unknown command holes fail validation,
  including before worker startup or batch dispatch.
- Tracked feature badges distinguish ARMED from CALLBACK SEEN. Uninstrumented
  features say ON / UNMEASURED; missing hook slots or failed activation writes
  report PARTIAL and return -11. A partially applied feature may retain hooks;
  disable it before retrying. This does not establish gameplay effectiveness.
- MP diagnostics reset on activation. Hit sampling publishes received MP and a
  timestamp separately from local resources. Combat hits without any bonus getter
  invocation show NO BONUS CALLS. Local refill and recovery controls now describe
  their limited/experimental behavior.
- Shared telemetry layout is 962. Menu status JSON and the observer expose
  callback/partial masks, adaptive state, received MP and its sample timestamp.
- Build tooling stages and hashes menu, core, loader and observer together;
  supports an explicit staging directory; uses locked/offline Cargo commands;
  writes valid JSON paths; and no longer stops every running menu.

## Local verification

- Rust/Cargo 1.99.1, x86_64-pc-windows-msvc, configured x86-64-v2 CPU baseline.
- `cargo test --workspace --locked --offline`: 119 tests passed, plus the
  separate-process reader probe. The intentional worker-panic test passed.
- `cargo xtask build --no-test --stage-dir work/release-v96.2`: optimized
  workspace build passed after the test run.
- Staged CLI: help/version, empty JSON status, invalid command 4, unavailable
  commands 96/98, balanced-plan save with command 94, and rejection of an invalid
  saved plan all passed. Observer geometry smoke check passed.
- Windows menu opened as v0.96.2; no-client state, labels and controls were
  visually inspected. The test menu was closed afterward.
- All four installed files match the staged SHA-256 manifest.

Evidence: `work/reliability-v96.2/tests.log`, `build.log`, `cli-checks.json`,
individual CLI logs, `observer-geometry.log`, and `installed-sha256.json`.
Prior canonical binaries are preserved in
`work/reliability-v96.2/previous-release/`.

## Remaining gameplay work

MP bonus getters were not called in the prior recorded combat. This repair
exposes that fact; it does not add a proven MP-recovery path. Extended range also
retains its earlier failed distant-damage acceptance. Automatic pacing and
target acquisition remain unimplemented. Prior local damage rejection and the
original protection trigger are not resolved by these local tests.

PARTIAL reflects setup completeness. CALLBACK SEEN means a local callback ran,
not that its result was accepted by the game server. Field restoration in the
older movement modes still uses existing defaults rather than a captured
per-instance snapshot; those modes need live enable/disable checks.

For the deferred check, use a fresh game session and this matched release.
Compare a native baseline with one feature at a time. For MP, record local and
received MP, hit/getter counts, and whether another skill can actually be used.
For range, record distance and lasting target HP changes. For adaptation, compare
94 with 95 and observe cap/feedback changes. Check disable and scene transitions.
No claim of flawless operation or a new live pass is made.
