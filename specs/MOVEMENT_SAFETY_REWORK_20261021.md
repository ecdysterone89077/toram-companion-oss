# Movement Safety Rework — 2026-10-21

## 1. What was actually wrong

Earlier repair notes described a movement and
no-collision fix as applied. It was not. Verified against the working tree before
this rework:

| Claimed in the spec | Actual in `crates/wcore/src/features.rs` |
| --- | --- |
| `MOVEMENT_SPEED` reduced to `6.0f` | `const MOVEMENT_SPEED: f32 = 20.0;` (line 43) |
| `get_IsGravity` returns `true` | slot 3 returned `false` (line 91) |
| `get_IgnoreHeight` returns `false` | slot 4 returned `true` (line 91) |
| `get_MoveHeightRangeCheck` returns `true` | slot 5 returned `false` (line 91) |
| — | no-collision ON also wrote `isGravity=0`, `ignoreHeight=1`, `MoveHeightRangeCheck=0`, `EnableFloorHitCheck=0`, `EnableOnGroundMove=0`, `rad=0.001`, `height=0.001` |

The canonical `src/wcore.dll` therefore still carried the exact physics and
movement behaviour that preceded the server disconnect. Two of the three
claimed fixes were applied:
the account reset registry filter (`crates/companion/src/lib.rs:514`) and the
removal of `scrub_module_pe_headers` / `install_watchdog_shield` from
`crates/wcore/src/runtime.rs`.

The 2026-10-21 build's behaviour was therefore: a flat `20.0f` move speed
replacing every native speed reading, the GM dash flag forced on, and gravity,
height range checks, floor hit checks and the collision radius all disabled.

## 2. Evidence available, and its limits

The current `GameAssembly.dll` (2026-10-10 14:26, 24 380 352 bytes) has an
encrypted `.text` section:

```
.text  rva=0x03822000 vsize=0x01556000 rsize=0x01556000 entropy=7.999
```

`dump/dump.cs` and `dump/script.json` were produced on 2026-10-02 from a
different, larger build. Their RVAs are past the end of the current file, and
byte-pattern scans for the known accessor encodings (`8B 41 3C C3` for
`moveSpeed`, `0F B6 41 48 C3` for `isGravity`) return zero hits. No offline
static analysis of the movement path is possible on this build, and the
IL2CPP metadata confirms the class layout is unchanged but the code is not
readable.

What the metadata does establish, and what the rework relies on:

- `CharacterActionManagerBase.get_MoveSpeed` is `virtual` (slot 10);
  `PlayerActionManager.get_MoveSpeed` is an `override` of it (slot 10);
  `CharacterMove.get_MoveSpeed` is a plain getter over the `moveSpeed` field.
  A single native speed query can therefore re-enter the hooked getters.
- `CharacterMove` owns `defaultMoveSpeed` (0x38), `moveSpeed` (0x3C),
  `targetMoveSpeed` (0x40), `gravity` (0x44), `isGravity` (0x48),
  `ignoreHeight` (0x13E) and `<MoveHeightRangeCheck>k__BackingField` (0x13F).
- `FieldRayPick` owns `<EnableWallHitCheck>` (0x5C), `<EnableFloorHitCheck>`
  (0x5D), `<EnableAllFloorMove>` (0x5E), `<EnableOnGroundMove>` (0x60),
  `rad` (0x18) and `height` (0x10).

Consequence: the implementation cannot depend on knowing how the native chain
derives the final value. It has to be correct for every chain topology.

## 3. The rework

### 3.1 Speed: a ratio against native, bounded by the native dash envelope

- `SPEED_MULTIPLIER = 1.25`, applied to the value the game itself reports.
- `SPEED_CEILING = 15.0`. The native dash envelope is roughly 12.0f..15.0f, so
  the ceiling never exceeds a speed the server already has to accept from a
  legitimate native dash. It is strictly below the `20.0f` that preceded the
  enforcement event.
- The GM dash flag (`get_GmMoveSpeedDush`) is no longer hooked. A debug
  movement mode has no server-side validation counterpart and is a plausible
  contributor to the delta-displacement breach.
- The movement fields (`moveSpeed`, `defaultMoveSpeed`, `targetMoveSpeed`) are
  no longer written. The engine rewrites them every frame from the character
  state, so a one-shot write is transient; combined with a scaled getter it
  would also compound the multiplier.
- The five hooked getters (`PlayerActionManager.get_MoveSpeed`,
  `CharacterActionManagerBase.get_MoveSpeed`,
  `CharacterActionManagerBase.get_DefaultMoveSpeed`,
  `CharacterMove.get_MoveSpeed`, `CharacterMove.get_DefaultMoveSpeed`) apply
  the ratio through a thread-local chain guard, so the ratio is applied exactly
  once to the outermost query of a native chain. This is the same doctrine as
  the MP nesting fix in `specs/MP_NESTING_FIX_20261019.md`, where an unguarded
  bonus compounded 13 -> 23 -> 33.
- Every emitted speed is clamped to `SPEED_CEILING`. Clamping is idempotent,
  so it can never compound, and it is a hard guarantee that the client never
  reports a speed above the native dash envelope.

A ratio keeps the native walk / sprint / dash relationships intact. A flat
constant destroyed them: the character moved at `20.0f` whether it was
walking, sprinting or dashing, and the server saw a velocity profile unlike
anything a legitimate client produces.

### 3.2 Wall collision only

- Overridden: `FieldRayPick.get_EnableWallHitCheck` and
  `CharacterMove.get_EnableWallHit`, plus the single
  `<EnableWallHitCheck>k__BackingField` write.
- Native: gravity, `ignoreHeight`, `MoveHeightRangeCheck`,
  `EnableFloorHitCheck`, `EnableAllFloorMove`, `EnableOnGroundMove`, and the
  `rad` / `height` collision radius.

The removed writes were the coordinate-desync mechanism. Disabling gravity and
the floor hit check detached the player from the floor raycast, so the
coordinates transmitted to the server no longer correlated with the terrain
navmesh. The removed `rad` / `height` shrink to 0.001 turned the collision
sphere into a 1 mm sliver whose ground contact point could differ entirely
from the real one. Wall collision is the only thing that has to be overridden
for a wall-clip; the floor anchor has to survive.

## 4. Verification performed

- `cargo test --workspace`: 151 tests pass across all six crates
  (`companion` 16 + 2, `observer` 3, `protocol` 13 + 1 child reader, `wboot` 19,
  `wcore` 95, `xtask` 2), zero failures.
- `cargo xtask build --no-test`: release build stages cleanly.
- New regression coverage:
  - `move_speed_multiplier_applies_once_per_nested_getter_chain` — a native
    body that re-enters a second hooked getter proves the ratio is applied
    once across the chain, not once per level.
  - `move_speed_never_exceeds_the_native_dash_ceiling` — the clamp holds for
    readings at, above and below the ceiling, and non-finite or non-positive
    native readings pass through untouched.
  - `movement_callbacks_scope_speed_and_flags_and_forward_original_arguments`
    — updated for the five speed getters and the two wall-only no-collision getters;
    foreign instances and disabled state still forward native.

No live game operation was performed for this rework.

## 5. Live test plan and rejection conditions

This rework changes behaviour that has never been observed in the game. The
first live run must be a controlled one.

Preconditions: run `.\companion.exe --reset-account`, then start the game
normally, log in, and take a clean baseline with no core loaded. A fresh
guest session is required to test at all.

Stage 1 — speed, no movement. Enable speed only, stand still for 60 seconds,
refresh the log, and report the `[speed68]` sample lines. The expectation is
`native=` values inside the native envelope and `scaled=` never above the
ceiling. If no `[speed68]` line appears, the hooked getters are not on the
movement path, the feature is inert, and the alternative is scaling the
`CharacterMove` fields instead. That is a safe negative: the emitted speed
stays native.

Stage 2 — speed, short walk. Walk in a straight line for about 20 metres and
watch for `Server Disconnected`. Any disconnect ends the test immediately.

Stage 3 — no-collision. Enable no-collision only, walk into a wall, and confirm the
character passes through while remaining on the floor. The player must not
fall, and the server must not object.

Rejection conditions. The build is rejected, and the ratio lowered or the
feature withdrawn, if any of the following occur:

- the client reports a speed above `SPEED_CEILING` (a guard failure);
- the server disconnects during any movement;
- the character falls out of bounds or loses its floor anchor with no-collision enabled;
- any server-side enforcement is observed.

Accepted state for a first live run is: speed visibly faster than native, no
disconnect, no restriction, no collision passing walls while staying grounded.

## 6. Still open after this rework

- The exact native speed derivation chain is unverified; the chain guard makes
  the result correct for any topology but does not prove the feature engages.
  Stage 1 resolves this.
- The server's movement tolerance is unknown. The ceiling is derived from the
  native dash envelope, not from a measured server limit.
- Effective MP recovery, accepted distant basic damage and the Hard Hit
  rollback remain unresolved and are unaffected by this rework.
- The 03:06 "Unauthorized Action Detected" trigger is still unidentified. The
  memory-integrity fix is applied but unproven.
