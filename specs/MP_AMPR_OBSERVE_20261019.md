# v95.8 AMPR observer — finding which calculator feeds per-hit MP

Date: 2026-10-19, Asia/Jakarta. Workspace 0.95.8, telemetry 956 (unchanged).

## The verdict that forced this build

Fresh PID 8320 probe after 44 landed normal attacks with F4 ON:
`mp_hit_calls=44 mp_samples=44 mp_recovery_calls=0 mp_getter_calls=0`.
The hooked `get_AtkMpRecovery` getters on PlayerBattleStatus /
PlayerSecondaryStatus are NEVER consulted in the combat loop — not a guard
problem, not an envelope problem. The +10/+25/+50 ladder cannot work through
a dead path (the earlier owned=1 diag hits were non-combat recalc/UI reads).
Per-hit MP most likely flows from a cached field or another calculator class;
dump.cs shows get_AtkMpRecovery on six classes plus atkMpRecovery data fields
on action/buff objects.

## What v95.8 does (pure observation, zero behavior change)

- Best-effort observer hooks for get_AtkMpRecovery on MercenaryBattleStatus,
  PetStatus, MobaPlayerSecondaryStatus: forward native unchanged, count hits,
  rate-limited `[mpdiag] observe=<class> native=<v>` lines. Missing classes or
  failed slots log-and-skip; F4 enable never depends on them.
- New `mp_getter_other` counter in the combat summary line.
- Existing boost hooks/sampling untouched; menu title v95.8 for honesty.

## The one round-trip that ends the hunt

Restart normally, F4 ON, attack normally ~30s, Refresh Log. Verdict table:

- `mp_getter_other` > 0 with a class name: THAT class feeds combat. Next step
  moves the (guarded, single-application) bonus onto it.
- Still all-zero while hits land: no calculator getter feeds per-hit MP — the
  gain comes from a cached field or server grant. Next step is field-level
  tracing, and the honest ship is hit-rate-driven accepted MP (F2 + 72).

## Verification

84 workspace tests pass (protocol 4, wcore 57, companion 9, wboot 11, observer 3).
Release staged + hash-verified under `work/release-v95.8-ampr-observe/`:

- companion.exe `EA16E1AF20F90BB47D1B138DDB107999538AF793E498065971ED7EE055A8635B`
- src/wcore.dll `1F848C221960F7EA9475FCA8545A9912C47E6255B81A3016ABD84DC75A776859`
- src/wboot.exe `1EA84700D92B1B340CB527B5D1E19100D8E282D89752745CE3361E45015F86D8`
