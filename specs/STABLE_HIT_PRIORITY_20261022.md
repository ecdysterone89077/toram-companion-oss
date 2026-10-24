# Faunas wins stable/get_Hit over always-critical — 2026-10-22

## Decision

He approved: when the Faunas port and the companion both claim
`PlayerBattleStatus.get_Stable` / `get_Hit`, the port's values win
(stable=1000, hit=65535). Companion `job_always_crit` keeps its critical
getters; only its stable/hit sidecars (100/10000) yield.

## Mechanism (first-come ownership)

`hook.rs::install_resolved` grants a `MethodInfo` to the first installer
via `ACTIVE_METHODS`; the second install is refused. `apply_hook_specs`
already tolerates refusal on non-core hooks (logs "optional auxiliary hook
skipped", marks partial). What was missing: order-independence. Whoever was
ON second silently ran partial with no path back to whole.

New behavior, mirroring the `crit_dmg` shared-owner pattern:

- Port `stable`/`mob_no_flee` ON: `always_crit_yield_stable_hit` restores
  the companion's entries first; abort (-8) if a live entry won't restore.
- Port `stable`/`mob_no_flee` OFF while companion still on:
  `always_crit_reclaim_stable_hit` reinstalls the companion's two specs.
- Companion ON while port owns: unchanged (skip + partial, returns -11
  via `feature_setup_result`). The reclaim on port-OFF heals it.
- Shutdown unchanged: `faunas::disable_all` runs before the companion
  restore, so reclaim is unnecessary there (inactive entries restore as
  no-ops returning true).

## Files

- `crates/wcore/src/features.rs`: `always_crit_conflict_specs` (single source
  of truth, also used by the enable table), index consts (1, 3),
  `always_crit_yield_stable_hit`, `always_crit_reclaim_stable_hit`,
  one unit test.
- `crates/wcore/src/faunas.rs`: `STABLE_FEATURE_INDEX=2`,
  `MOB_FLEE_FEATURE_INDEX=7`, yield on enable, reclaim on disable;
  fixed stale docstring (65504 -> 65535, matches `HIT_VALUE`).

## Verification (today's tree)

- `cargo test -p wcore`: 119/119 pass (118 + 1 new). Exit 0.
- `cargo clippy -p wcore -- -D warnings`: clean. Exit 0.
- `faunas_target_audit_checks`: 23/23 pass.
- `faunas_target_audit`: same 5 cross rows; the 2 biting rows now label
  the companion side `always_crit_conflict_specs` (the helper is the spec
  source — traceable by construction).
- `cargo fmt --check`: FAILS on pre-existing toolchain drift
  (faunas.rs 15 hunks, hook.rs, lib.rs, scanner.rs — all in untouched
  regions; HEAD content fails identically). `features.rs` fully clean
  including new code. World reformat deliberately NOT done (scope).

## Not verified

No game session: game not running at inspection time. Order-independence
(yield/reclaim across all four ON/OFF orders) is proven by unit scope +
hook-engine ownership rules, not by a live toggle sequence.

## Follow-up: overlap_excused (verified live PID 21016)

Ordering B first ran live as -8, not the planned -11: `get_Stable` is on
the core-hooks list, where any refusal fails closed. Correct for genuinely
missing methods, wrong for a refusal caused by our own port holding the
method by design. `apply_hook_specs` now excuses refusals the other side
demonstrably owns (`overlap_excused`: port flags or companion
`INSTANT_ATK_ACTIVE` for motion) into skip + partial; everything else
still fails closed. Single-hook redundant second-ON still fails safe -8
(zero hooks would be a lie; the value is already served).
121/121 wcore tests pass. Ordering B re-verified live: -11, 22 hooks,
partial bit honest.
