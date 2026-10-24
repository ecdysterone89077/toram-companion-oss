# v95.5 skill-path clamp — historical implementation record

## Correction — 2026-10-20

The explanation and universal acceptance claims below are superseded by
the current capture (development repository). Repeated basics decrease
normal-attack proration and raise physical/magic proration; the earlier claim
that this lowers the following skill's expected damage was incorrect. The
capture has two unchanged-HP Hard Hit responses at 250%, despite zero violation
flags, and a later HP decrease at the same setting. It does not establish the
server's validation formula or prove native +100 reliable in every condition.
The original implementation/test history is retained below.

Date: 2026-10-19, Asia/Jakarta. Workspace 0.95.5, telemetry 955.

## Operator testimony (maps the server envelopes)

- Native: flawless.
- +100: flawless on basics and skills.
- +200: basics + fresh skills fine; skill AFTER basic spam (proration active)
  shows damage but enemy HP does not drop.
- +300: basics fine; skills rejected even fresh.

## Model

One unified crit-damage boost feeds both paths, but the server validates them
against different bands: basics carry stability variance, so the band is wide
(holds to +300); skills have stat-derived power the server can recompute, so
the band is tight — and proration shrinks it further mid-rotation (hence +200
skills passing fresh but failing after spam). A fixed +points bonus is a larger
relative inflation exactly when prorated expectation is smallest.

## Fix

Basics (inside the normal-attack calc scope) receive the full selected boost,
unchanged. The skill path (every other `get_CriticalDmg` read: skills, UI,
foreign) is clamped to native + `SKILL_CRIT_CAP_POINTS` (100) — the highest
level proven flawless on skills in every condition. Absolute presets clamp the
same way (e.g. 300 -> 250 at native 150). Crit classification for skills is
untouched: every skill still crits, at the capped multiplier.

Trade stated plainly: +200 fresh-skill hits drop from +200 to the +100 cap —
that hit only ever passed while the proration band happened to be wide, and it
provably fails once spam narrows the band. Deterministic flawless beats
sometimes-bigger. If live testing later proves a higher skill ceiling (e.g.
+150 holds prorated), the cap is one constant.

## Retest procedure

1. Restart the game normally (v95.5 core), menu v95.5, F2 ON.
2. Select +300 [85] (strongest basics). Spam basics on a fresh enemy: every
   number must move the HP bar; kill must complete with drops/EXP.
3. Without changing the setting, cast skills fresh AND after long basic spam:
   every skill number must move the HP bar (skills run at the +100 cap).
4. Repeat step 2-3 at +200 [84]. Optional cmd-82 capture on any failure.
5. Pass = all numbers move HP in both rotations. Native [79] still resets.

## Verification

80 workspace tests pass (protocol 4, wcore 54, companion 8, wboot 11, observer 3).
New/extended wcore coverage: full boost inside scope for 75-78/81/83-85,
skill-path clamp to native+100 for every additive level and absolute 300/200,
cap scaling with native (212 -> 312), nested-read single application, Native
reset. Release staged + hash-verified under `work/release-v95.5-skill-clamp/`:

- companion.exe `3C0A0021A01ED28B985AE85D0C81C5F05403E54791DB3ECBF742A3338FA2A67D`
- src/wcore.dll `9A65AC8AFAB25ACF1204BD0DFCB60ECBD69FB3A88907FFA786F35D6DAE6E24CF`
- src/wboot.exe `695998879D58F896C468C1B5EA1FF70F0864C71B1E26215471C13112757A2165`

Note: during this build a mid-edit file truncation dropped the nested/classification
tests; both were reconstructed (nested from the verified pre-edit body plus the
scope guard; classification from git HEAD plus the get_CriticalDmg assert) and the
suite passes 54/54, matching the pre-incident count with strictly more assertions.
