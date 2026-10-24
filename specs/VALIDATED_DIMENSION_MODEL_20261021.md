# The Validated-Dimension Model — 2026-10-21

**Historical model, superseded as an authority classification.** The claims
of exhaustive blind dimensions, no ceiling, zero risk, native 150 calibration,
skills-only optimality and identified incident causality below are not
established. Read [the blind-spot audit](BLIND_SPOT_AUDIT_20261021.md) and
[the revised map](BLIND_SPOT_MAP_20261021.md) first. Current code still applies
its configured caps; this notice does not change runtime behavior.

Why some aggressive features have worked flawlessly for a hundred sessions
while others ended in a disconnect or a character kill. The answer is not
"too much value". It is **whether the server
can independently recompute the number the client is claiming**.

This document replaces the earlier "the server validates a balance threshold"
framing, which fits the damage data and misfits everything else.

---

## 1. The model

The server recomputes some of the client's numbers and compares its own
result with the client's claim. When they disagree, it reacts. When it cannot
recompute a number, it has nothing to compare, and it accepts whatever the
client sends.

| Feature | What the client changes | Server can recompute? | Observed result |
| --- | --- | --- | --- |
| `get_Hit` = 10000 | which outcome the client rolls | No — it is the client's RNG | Accepted. Uncapped. |
| `get_Stable` / `get_SubStable` = 100 | the damage variance band | No — the client's RNG | Accepted. Uncapped. |
| `get_Critical` = 1000, `checkCriticalPercent` → true | the crit rate | No — the client's RNG | Accepted. Uncapped. |
| `get_IsInvincibility` | the player's own HP | No — it tracks damage to *others* | Accepted. Uncapped. |
| `GetMotionSpeed`, `get_Aspd`, delay timers | attack cadence | Loosely — a high-ASP build looks the same | Accepted. |
| `get_IsHitRigidity` → false | flinch and lock state | No — client animation state | Accepted. |
| `get_CriticalDmg` | the damage magnitude | Yes — ATK x skill x proration | Rollback past the ceiling. |
| move speed | displacement per tick | Yes — kinematic plausibility | Disconnect. |
| no-collision that leaves the floor | the position | Yes — navmesh containment | Disconnect. |
| MP balance | the MP available to spend | Yes — it gates skill costs | Reconciled. |
| primary stats (`cmd 44`) | the basis of every damage formula | Yes, and strictly | **HP = 0.** Sanction kill. |

The accuracy path and the damage path are **separate hooks in the source**.
`job_always_crit` installs `get_Critical`, `get_Stable`, `get_SubStable` and
`get_Hit`; `critical_damage_value` is a different function reached through
`get_CriticalDmg`. The first group changes *which* result the client rolls.
The second changes *how large* the result is. The server can audit the second
and cannot audit the first.

That is the whole explanation, and it accounts for every anomaly in the
project's history, including the ones that looked most contradictory.

---

## 2. Why the old no-collision mode "worked without issues"

It did not. It was **conditionally** safe, and the condition was the
environment.

The old no-collision mode disabled gravity, the height range checks, the floor hit
checks, the ground move flags, and shrank the collision radius and height to
0.001. On flat ground none of that moves the character, so the coordinates
stay on the navmesh and the server sees nothing wrong. The failure needs a
ledge, a slope, an airborne moment, or a multi-level area — a case where the
lost floor anchor changes the transmitted height.

The 2026-10-21 session was in the Underground Channel, which has all four.
That is the entire difference between "flawless for a hundred sessions" and
"disconnect". The feature was never safe; it was safe
*somewhere*.

---

## 3. The sanction ladder

The severity of the response tracks how far outside the model the claim is,
and how long it persists.

1. **Roll back the claim.** The server substitutes its own value. The client
   sees the damage, the target keeps its HP. This is the critical-damage
   failure: 1358 displayed, target alive at full.
2. **Drop the connection.** The server severs the session. This is the
   movement and navmesh failure.
3. **Restrict the account.** A repeated or severe violation is escalated to
   the account database.
4. **Kill the character.** The server rejects a primary-stat claim and
   executes a sanction on the player. This is why `cmd 44` is blocked.

The important consequence: **a small overshoot is survivable and a large one
is not.** The 2026-10-21 speed was 20.0 against a native dash envelope of
roughly 12 to 15 — a third above the maximum speed any player can ever
produce, sustained. There was no legitimate case for it, so nothing in the
server's model could absorb it.

A near-boundary claim has somewhere to land. That is the safe operating
region, and it is reachable.

---

## 4. What this changes about the critical damage ladder

The 2026-10-20 comparison produced the calibration point:

- absolute 200% at a native value of 150 — **accepted**
- absolute 250% at the same native — **rolled back**
- native +100, which is also 250% at that native — **rolled back**
- native +200 and native +300, and +1000 — **rolled back**

The old build expressed the ladder as *points over native*. That is the
defect: `native + 100` silently became 250%, and the operator had no way to
know that. The ladder's first rung was already a known-failing value.

Two things follow.

**The bound is a ratio, not a number.** The native value varies by character
and build. An absolute cap of 200 would push a character whose native value
is 212 below its own value. The bound is therefore a ratio against native:
`CRIT_DAMAGE_MAX_RATIO = 135`, which is 1.35x, just above the observed
accepted 1.333x and well below the observed rejected 1.667x. At a native of
150 the ceiling is 202, so the accepted 200 is left exactly intact and the
rejected 250 is bounded to 202.

**The ladder can no longer climb.** The old adaptive ladder stepped *up*
after eight straight accepts, hunting for a larger value. There is no
headroom above a value the server has already been observed to roll back, so
the ladder now only steps *down*: a rejection lowers the cap one rung, a
fresh target restores it. The cap is expressed in the same absolute units the
server checks.

This is implemented in `effective_critical_damage` in
`crates/wcore/src/features.rs` and covered by
`critical_damage_never_exceeds_the_ratio_ceiling_and_never_drops_below_native`
and `the_ratio_ceiling_preserves_the_observed_accepted_values`.

---

## 5. The maximum power that carries no sanction risk

Read the first table again and the conclusion is uncomfortable for a project
that has been pushing on damage inflation: **the safe power is already the
strongest combat state in the game, and it has no ceiling.**

Always crit, always a max-roll critical, never a graze, never die, never be
slowed down. Every one of those lives in a dimension the server cannot
recompute.

Compare the two ways to spend a hit:

| Configuration | Damage per hit | Uptime | Sanction risk |
| --- | --- | --- | --- |
| Always crit + max stability, native crit damage | 1.0x | 100% | none — the roll |
| 250% crit damage | 2.5x | rejected, 0x | roll-back, escalation |

At 250% the server refused two Hard Hits outright while the normals in the
same rotation landed 191, 229 and 267. **Expected damage strongly favours
the safe path**, and it carries no sanction risk. The old build was
negotiating with the server's model for a second multiplier on top of an
already maximal roll, and that second multiplier was the only thing it was
buying.

### Where the remaining power actually is

**The roll, not the number.** `get_Critical` = 1000, `get_Hit` = 10000 and
`get_Stable` = 100 are unbounded. That is the multiplier.

**The sequence, not the magnitude.** The captured 250% session shows two
Hard Hits at zero received HP and a third at 1566, on the same target, in the
same rotation. Basics raise the proration *against skills* specifically. The
server's allowance for a skill hit is therefore a function of recent attack
history, not only of the magnitude. A client that shapes its own sequence —
firing the big skill when the proration is clean, letting it decay instead of
stacking basics, and never inflating the magnitude — gets more out of the
server's model than one that simply asks for a bigger number.

**The cadence.** Instant attack and anti-rigidity are server-tolerant and
proven.

### The loadout

| At maximum (outside the model, uncapped) | At the calibrated ceiling (server-checked) | Off |
| --- | --- | --- |
| always crit (rate) | critical damage at the 200% known good | the additive ladder beyond it |
| HIT 10000, stability 100 | speed at 1.25x native, capped at 15.0 | the GM dash flag |
| invincibility | no-collision, wall collision only | the MP bonus ladder, proven inert |
| instant attack, anti-rigidity | MP through the server's own grant | primary stats |

This is implemented as the **Safe** profile in the companion window: one
click applies invincibility, always crit, instant attack, anti-rigidity and the
200% critical damage, with MP on native and the server-tracked features off.

---

## 6. The other axis: mechanism

Value consistency and mechanism footprint are independent concerns.
The project has been treating them as one.

**The mechanism did not discriminate.** Invincibility, always crit, HIT 10000 and
instant attack all use the same heap-door method interception as speed and
damage, and all have been flawless across many sessions. The features that
were sanctioned used the same mechanism. Whatever tripped on 2026-10-21 was
not the hook mechanism.

**What was closed.** The PE header clearing and the watchdog shield that
overwrote `HackThreadManager` method pointers with foreign addresses are both
removed from the worker startup. They are the most plausible explanation for
the 03:06 "Unauthorized Action Detected" event, and they were correct changes
regardless.

**What remains, honestly.** The following surfaces are still present and
none of them has been validated against host-side inspection:

1. `MethodInfo->methodPointer` and `virtualMethodPointer` now point into
   `wcore.dll` rather than into `GameAssembly.dll`. A check that validates
   method pointers resolve into the expected module would catch this. **This
   is the largest remaining surface.** It is also the price of the
   features the server cannot recompute, which all require intercepting a getter.
2. The loaded module is present in the process module list.
3. The telemetry shared mapping is present in the process.
4. The worker thread is present.

**The minimal-surface variant.** If the goal were the smallest possible
footprint, the answer is to intercept fewer methods: leave `get_CriticalDmg`
alone and the damage path disappears, and the only remaining hooks are the
accuracy getters and the invincibility getter. That costs the damage
ceiling, which is worth little, and it removes a hook class. The loadout in
section 5 is the version that keeps the damage ceiling; the minimal-surface
version drops to 200% and removes the damage hooks entirely. Both are
defensible. Neither is provably invisible to integrity checks, because those checks are
unknown.

**"Zero surface, 0 risk" is not achievable against unknown integrity checks.**
What is achievable is **no known trigger**: every value inside the server's
model, plus the two known memory-integrity holes closed, plus the remaining
surface enumerated and measured rather than assumed.

---

## 7. Rules that follow

1. **Prefer the dimension the server cannot recompute.** When a feature can deliver the same
   effect through a dimension the server cannot check, it belongs there.
2. **Never express a bound as an offset from a value the operator cannot
   see.** `native + 100` was the defect that put a known-failing 250% in the
   ladder's first rung.
3. **A ceiling is a measured boundary, not a floor to climb.** The adaptive
   ladder steps down, never up.
4. **A rejected claim is a server rule, not a bug to route around.** The
   answer is a lower request or a different sequence, not a cleverer
   encoding.
5. **Separate the two axes.** A value inside the model can still
   be delivered by a mechanism the server can see. Fix them independently.
6. **A feature that only works in flat areas is not a working feature.**
   The old no-collision mode was safe on flat ground and not safe anywhere else.
