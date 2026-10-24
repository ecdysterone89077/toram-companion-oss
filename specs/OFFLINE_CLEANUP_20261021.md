# Offline cleanup repair - 2026-10-21

This pass changes only cleanup behavior in
`crates/wcore/src/features.rs`. No Toram process was launched, inspected,
or sent commands. No watchdog or integrity-check behavior was modified.

## Defect and changes

Range and MP previously resolved the current player before attempting OFF.
A missing player during disconnect or scene transition could therefore prevent
restoration of state already owned by the mod. MP also ignored observer-hook
restoration failures.

- OFF now restores the saved hook tables and range snapshots before any player
  lookup. Successfully completed cleanup releases its binding.
- Callback effects are disabled before restoration. Failure retains ownership,
  snapshots, and a cleanup-pending flag, returns -8, and marks partial status.
- Another absolute OFF retries pending cleanup instead of reporting success
  because the active flag is already false. An ON request while cleanup is
  pending returns -8. Legacy toggle commands also retry cleanup.
- MP OFF includes its observer hooks. Worker shutdown uses the same cleanup
  routines and reports incomplete restoration as failure.

This is a source-only cleanup patch. It does not establish gameplay correctness,
identify an enforcement trigger, lift an existing sanction, or establish that the
remaining features work. The prior release candidate and canonical binaries remain
unchanged; no updated release package was staged.

## Verification

- `cargo test -p wcore --lib`: 93 passed, including three new regression tests.
- `cargo test --workspace`: 148 passed plus the separate-process reader probe;
  no failed tests. The worker panic-isolation test intentionally logs a simulated
  fault and passes.
- `cargo check -p wcore --release`: passed without compiler warnings.
- `rustfmt --edition 2021 --check crates/wcore/src/features.rs`: passed.
- `git diff --check -- crates/wcore/src/features.rs`: passed.
- Ten saved hashes match: four canonical binaries, four prior candidate binaries,
  and both incident evidence manifests.

The new tests use local synthetic hook records. They cover OFF without resolving
a player, failed range restoration with another OFF retry, and partial MP cleanup
where the main hook restores but an observer hook needs a retry. They do not
attach to Toram or exercise an account.

Logs, the previous source copy, patch, and hash receipts are in
`work/cleanup-20261021/`. Original incident material remains in
`work/incident-20261021-0306/` and `work/live-repair-20261021-1032/`.
