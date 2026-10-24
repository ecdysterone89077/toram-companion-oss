# Runtime repair candidate 0.96.3-rc.2

Historical checkpoint. Subsequent rc.2 gameplay observations, the rc.3 rollback
repair and current testing status are in the October 22 record, which lives in
the development repository. The original rc.2 receipt and archives remain unchanged.

The broad repair request produced reproducible fixes to hook ownership,
worker recovery, feature shutdown and build/staging. Gameplay acceptance and
incident causality remain unresolved.

## Fixed defects

1. Repeating installation of an active derived-only hook could overwrite the
   shared base MethodInfo after that base hook had been restored. Repeated
   activation now validates ownership without writing any pointers. A derived
   hook with a null original dispatch is rejected. Both regressions failed on
   the original implementation and pass with the repair.
2. A command timeout left worker admission permanently closed, even after the
   job completed, so shutdown returned busy. The runtime now distinguishes a
   timed-out caller from a live one and consumes the old completion before
   admitting another command. Stop operations serialize handle cleanup with
   dispatch and each other. Regressions cover shutdown after a timeout,
   next-command result isolation, and simultaneous stop calls.
3. Invincibility, timing, critical and anti-rigidity OFF paths attempted to resolve the
   current player before restoring saved hooks. They now restore independently
   of scene/player resolution. Scoped callbacks deactivate before restoration;
   incomplete restoration retains its saved entries and blocks ON until OFF
   succeeds. The same paths run during worker shutdown. Failed activation
   rollback also records pending cleanup.
4. `xtask` treated a member crate as the workspace root, and `run` launched
   existing stale binaries without rebuilding. Cargo now resolves the workspace,
   child Cargo commands run there, builds explicitly select the Windows x64
   output directory, and `run` rebuilds before launching one of the three
   supported executables. Arguments after `--` and exit status propagate.
5. Packaging accepts `-ArtifactRoot` for a separate candidate without replacing
   canonical artifacts. New fixture checks distinguish candidate bytes from
   old root binaries and verify preservation of the latter. CONTRIBUTING names
   the actual Cargo commands instead of missing scripts.
6. Source comments and CLI help no longer describe configured movement and
   critical limits as verified server thresholds, or mislabel the fixed
   critical ratio as a +100 bonus. Feature values are unchanged.
7. Release builds used `panic=abort`, disabling `catch_unwind` recovery in
   the worker despite passing debug tests. The profile now uses `panic=unwind`.
   `cargo xtask build` runs the release-mode `worker_lifecycle` example in
   its own process before staging. It exercises timeout recovery, a deliberate
   Rust panic and handle cleanup without accessing any game. This is Rust
   worker panic recovery, not recovery from access violations or arbitrary
   native faults.

## Verification

Evidence directory: `work/fix-all-20261021/`.

- `workspace-tests.log`: 165 workspace tests plus the separate-process reader
  probe passed. Target counts: companion 17 + 2, observer 6, protocol 13,
  wboot 19, wcore 105, xtask 3. The worker fault test deliberately panics and
  verifies isolation.
- `hook-regressions-before.log`, `timeout-before.log`, `cleanup-before.log`,
  `workspace-root-before.log` and `release-worker-before.log` reproduce the
  original defects/configuration mismatch.
- `package-checks.log`: 54 assertions passed using synthetic text artifacts.
- Earlier in this repair, the 19 correlation and 13 incident-attribution
  regression tests passed. Those tools were not changed by this candidate.
- `build.log`: optimized workspace build and four-artifact staging passed.
  Tests ran separately; staging used `--no-test`. Candidate hashes are in
  `candidate/sha256.json`. The final source/binary identity and check results
  are recorded in `receipt.json` and `source-sha256.json`.
- The final optimized worker check passed: normal result 42, timeout -3,
  busy -2, recovered next result 42, caught Rust panic -6, stop 0, all worker
  handles released. The intentional panic message is part of this check.
- `bootstrap.log`: host exception filter and PE headers preserved, exports
  present, zero core-started threads before explicit dispatch. The isolated
  host loaded and unloaded the candidate without accessing the game.
- Compiled CLI help/version, byte-identical legacy/safe plans, invalid-batch
  and invalid-PID rejection passed. Touched Rust files pass rustfmt checks.

Final core SHA-256:
`DF33012BC29751679ED3CF8B9AE1AAAC6000D4A28CA4AD7441DB0582020267C2`.
Canonical root binaries remain the old build. Use the separate candidate or
the new rc.2 package for review; older archives are not updated in place.

## Current game session

The operator corrected the earlier account answer: a new account is in-game.
The supplied screenshot showed Marbaro Forest: Deepest Part. Read-only process
inspection found PID 4580, created 2026-10-21 13:04:43 +07:00, already loaded
with the canonical old core. Its file hash matched `src/wcore.dll`. Telemetry
reported all eight toggles enabled; the resource sample was stale.

Before any candidate loading, the existing binary sent command 0 to that
exact process. It returned status 0, the log recorded owned hook restoration
complete, and telemetry reported worker=-1, toggles=0, partial_toggles=0,
feedback=false and adaptive_enabled=false. No candidate or feature ON command
was sent by this repair. The old DLL remains mapped; a restart is required
before loading a different core. This session is not an unmodified baseline.
The operator chose to finish local repairs first and restart later; live
candidate testing is deferred accordingly.

Saved evidence: `client-baseline.json`, `modules-before-stop.json`,
`status-before-stop.json`, `worker-stop.log`, `status-after-stop.json`, and
`Player-before-stop.log`.

## Remaining work

- Fresh unmodified session baseline and bounded candidate gameplay checks.
- Exact causes of the recorded warning and disconnect.
- Server-accepted MP recovery, distant basic damage and Hard Hit rollback,
  with the missing calculation-to-response correlation.
- Automatic pacing and target acquisition (reserved commands 96..99).
- The earlier Strix scan's provider billing failure.

This repair is not an account repair, proof of server
acceptance, or completion of the entire project.
