# Combat formula review — 2026-10-20

## Conclusion

Combining distinct calculation terms is a reasonable hypothesis for increasing
actual damage. Current presets mainly change critical damage; they do not
implement a coordinated ATK/pierce/element/distance-damage configuration.
However, a larger predicted result is not proof of a larger lasting HP loss.
Splitting the same increase across terms does not by itself resolve the two
unchanged-HP Hard Hit responses.

The six supplied pages are a community stat-calculator guide, including the
author's research and cited community sources. They are not server source code
or documentation of its validation rules. All six article bodies were read;
copies are retained in `work/gameplay-20261020-16932/`.

## Sources and implementation mapping

| Guide section | Relevant rule | Current implementation / implication |
| --- | --- | --- |
| [20: Introduction](https://coryn.club/guide.php?key=status&page=20) | Combine modifiers of the same kind; percentages precede flat additions, with flooring at each stage. | Several separate modifiers of one kind must not be multiplied independently. Basic stats precede derived stats. |
| [21: Basic stats](https://coryn.club/guide.php?key=status&page=21) | STR, DEX, AGI and INT feed different derived values. | Changing a parent stat and replacing its derived getter can double-apply a planned increase. Trace which value is consumed. |
| [22: Equipment stats](https://coryn.club/guide.php?key=status&page=22) | Weapon attack, refinement, stability and element have distinct rules. | Stability 100 sets the normal roll ceiling; more stability is not an unlimited damage multiplier. Refinement is separate from ordinary weapon ATK modifiers. |
| [23: Derived stats](https://coryn.club/guide.php?key=status&page=23) | With STR at least AGI, base critical damage is `150 + floor(STR / 5)`. | STR 93 / AGI 1 gives 168, matching the captured native result. Absolute 250 means 250% critical damage, not an additional 250% damage. |
| [24: Non-derived stats](https://coryn.club/guide.php?key=status&page=24) | Physical pierce reduces the DEF contribution, not physical resistance. Element and short/long-range damage are separate terms. | Current presets do not modify these terms. Their value depends on target and skill eligibility; merely locating a getter is insufficient. Distance damage does not extend attack reach. |
| [25: Special stats](https://coryn.club/guide.php?key=status&page=25) | Base AMPR is `10 + floor(MaxMP / 100)`, before its modifiers. | MaxMP 358 gives base 13. The +50 implementation sits in getters with zero observed calls over 455 hit callbacks, so the current bonus did not execute on that path. |

For the supplied setup, raising critical damage from 200 to 250 is a 1.25x
change to that factor. The low-proration normal hit goes from 153 to 191,
consistent with that ratio after integer rounding. This is a normal-attack
observation, not a prediction of every skill result.

An illustrative 1.25x factor combined with a distinct eligible 1.20x factor
would give 1.50x before applicable flooring/caps. Two bonuses in the same
additive category need not behave this way. This arithmetic is not a complete
Toram damage formula or evidence of remote acceptance.

## What the present hooks actually change

- `get_CriticalDmg`: absolute or native-plus-points presets; outside the normal
  scope, an experimental cap of 100–200 points over native applies. A cap of
  100 with native 168 allows 268; it does not cap absolute damage at 200%.
- Critical classification / HIT / stability: separate hooks, including local
  HIT 10000 and stability 100. Reported success does not establish identical
  treatment for every other calculation stage.
- `calcBaseDamage` / `calcPlayerToMobDamage`: original forwarding remains;
  no coordinated ATK, pierce, element or distance-damage boost is implemented.
- AMPR: `ActionHit` forwards native code and samples resources. It does not
  add MP. The getters containing the bonus were not invoked in this capture.
- Range: the action range and movement-assist path determine approach and
  attack initiation. Short/long-range damage stats cannot replace that path.

The metadata also names separate skill calculation stages for critical,
stability, proration, type damage and final damage. Those names support tracing
the stages, but do not establish their exact algebra or executed order.

## Evidence needed to choose a working combination

1. Keep target, equipment, action and starting proration comparable. Record
   native/current ATK, DEF/pierce, critical value, eligible element/distance
   modifiers, local/received proration and resulting received HP.
2. Observe one candidate term before combining terms. A hit callback or changed
   stat screen alone does not establish that the skill consumes the value.
3. Match calculation and response by actor, target and action identity. The
   current request-at-response value and HP history do not provide that match.
4. Repeat the strongest result after basic spam and after physical-skill spam.
   A result on a fresh enemy alone misses the reported failure condition.
5. Validate MP through subsequent skill spending, and range through damage
   delivered at measured distance without forced approach.

The diagnostic candidate now records received proration and no longer treats
unchanged-HP responses as positive feedback. It has passed local tests/build;
it supplies measurements for this investigation, not a verified combined
modifier repair. See the capture and build record in the development repository.
