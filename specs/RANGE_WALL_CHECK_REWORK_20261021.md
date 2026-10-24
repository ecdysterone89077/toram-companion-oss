# Range and wall-check rework — 2026-10-21

Three live symptoms drove this rework:

1. **No-collision made the character stick when it touched a wall.**
2. **Attack range could attack from a distance but delivered no damage.**
3. **Using a skill before triggering a basic attack against a distant enemy
   killed the character instantly.**

All three have one root cause, and it is the one the validated-dimension model
predicts.

---

## 1. The instant death

The server validates the distance of a damage claim. The range feature inflated
the *client's* view of the range so that the client would send a claim the
server cannot accept. The consequences are asymmetric and both were observed:

- An out-of-range **basic** attack is silently dropped. The animation plays,
  the number appears, the target keeps its HP. That is symptom 2: "can attack
  from distance but not deliver any damage".
- An out-of-range **skill** is treated as an invalid claim and sanctioned. The
  character dies. That is symptom 3.

The order mattered — "use skill before trigger basic attack" — because the
basic attack is what starts the approach. Skipping it meant the skill was
cast from wherever the character stood, which is out of range, which is a
sanction. The 2026-10-20 capture already showed the same asymmetry from the
other side: at 250% the two Hard Hits came back with zero received HP while
the normal attacks in the same rotation landed 191, 229 and 267.

**Fix.** The range inflation is retired. `RangeBinding::pending_range_override`
and `pending_range_override_for_action` now always return `None`, and the
feature installs no hook that widens a range check, a hit size or an action
range. A regression test, `the_range_feature_never_installs_a_hook_that_widens_a_range_check`,
pins that list so the inflation cannot come back unnoticed.

The client can no longer make an out-of-range claim, so it cannot be
sanctioned for one. A skill used at distance now behaves like a skill used at
distance in a normal game: it is out of range and is not cast.

## 2. What command 71 is now

Command 71 is **Engage**. It installs three assist-move getters and nothing
else:

| Hook | Class | Effect |
| --- | --- | --- |
| `get_IsShukuchiAssistMove` | `PlayerAttackBase` | the action may use assist-move |
| `get_IsActiveAssistMove` | `PlayerAttackBase` | the assist-move is active |
| `get_IsMoveAssistContinue` | `NormalAttackAction` | the assist-move keeps going |

The native range checks stay in place, so the client never claims more than it
legitimately can. The character closes the distance to the target instead, and
the attack is then made from a real range and lands on its own terms. This is
the route the server has always accepted, and the one the earlier
"hyper assist move" sessions proved.

The assist-move value is forced to true **only for the local battle manager's
queued action**. The scope is
`RangeBinding::is_local_pending_action`, which compares the action against
the live `pending_action` field of the local battle manager. Another actor's
action is never affected.

**Honest limit.** Whether the game reads those flags at a moment when the
action is already the queued one is not known. The build counts matched and
foreign queries separately (`RANGE_ASSIST_QUERIES` and
`RANGE_ASSIST_FOREIGN`) so the first live run answers it. If the matched
count stays at zero, the feature is inert and the character approaches
natively, which is slower but still safe.

## 3. The sticky wall

The no-collision build overrode the wall check at two levels: the movement-level
decision (`CharacterMove.get_EnableWallHit`) and the ray picker's wall
measurement (`FieldRayPick.get_EnableWallHitCheck`).

The ray picker's wall check is not only a wall check. The same picker
produces the floor contact and the movement normals that the resolution
depends on, and the 2026-10-21 build also had gravity and the floor hit
checks enabled. Disabling the measurement while keeping the ground
authorities on is what left the character stuck against a wall.

**Fix.** Only the decision is overridden. The measurement, the gravity, the
height range checks, the floor hit checks, the ground move flags and the
collision radius are all native, and no field is written at all. The
hook coverage in `no_collision_hook_specs()` and the movement-callback test
pins the list.

**Honest limit.** If `get_EnableWallHit` alone does not let the character
through, the character will simply hit the wall as normal. That is a safe
failure: it costs the feature, not the account. The next lever, if it is
needed, is the collision radius, which was shrunk to 0.001 in the old build
and was left native here because it moves the ground contact point.

## 4. What the operator should run

1. `--reset-account`, then start the game normally and log in.
2. **No collision.** Walk into a wall. Report whether the character passes through
   or stops, and whether it stays on the floor. The character's ground
   behaviour is the thing to watch: it must not sink, fall or float.
3. **Engage with a basic attack only.** Select a mob at about 20 metres and
   attack with a normal attack. The character should close the distance and
   the attack should land. Report whether it closed and whether damage landed.
4. **Engage with a skill.** Only after step 3 succeeded. Report whether the
   character survived, whether it closed the distance, and whether the skill
   fired. **If the character dies, stop immediately** and report the exact
   sequence; that would mean an out-of-range claim is still being sent.

Step 4 is the one that matters most. It is the case that killed the character
before, and the rework is specifically aimed at it.

---

## 5. What was not changed

- The critical damage ceiling and the Safe loadout are untouched.
- The speed rework is untouched.
- The MP ladder is still inert and still retired from the Safe loadout.
- No new command was added; command 71 keeps its id and its hotkey, and saved
  plans that use it keep working.
