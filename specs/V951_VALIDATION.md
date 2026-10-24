# v95.1 validation — 2026-10-18

## Result

The requested complete combat fix has **not** been achieved. The revised core
passes 58 tests and a clean live melee comparison, but enhanced MP recovery and
extended melee basic attacks failed their live acceptance tests. Both are OFF.
The menu identifies them as experimental. Earlier notes claiming that local
`serverMp` writes create real server MP, or that Power Wave guarantees accepted
ranged packets, are superseded by this record.

## Source and build verification

- Read HANDOFF.md, STATE.md and TASK.md in full, then inspected the combat,
  reflection, chain, hook, worker, telemetry and menu paths relevant to the failures.
- Baseline: 52 passing workspace tests. Revised suite: 58 passing tests
  (wcore 38, wboot 10, companion 6, protocol 4).
- Release build succeeds with the existing Rust/MSVC toolchain and Thin-LTO.
  Build version is 0.95.1; telemetry compatibility version is 951.
- Final rustfmt normalization changes whitespace only. Installed artifacts remain
  byte-identical to the release used in the live comparisons.
- Staging: `work/release-v95.1`; the SHA-256 manifest identifies all three artifacts.
- Canonical binaries and manifest now match staging; normal entry point is
  `start_companion.bat`. Exact prior artifacts/manifest are preserved under
  `work/pre-v95.1-20261018-session-19224`.
- Final menu SHA-256: `DACEB29EC7A65DA758C6C5927F615C0A67B1BF8FA68FE710E43C2CDEFBEEE163`.
- Loader SHA-256: `24BCD8199153F96E89EE63B0593EF8D37A3722C9ECF00E5794297F709A93E6AB`.
- Native disassembly and third-party source inspection remain outside the repository.
  Diagnostic recordings and scripts are ignored under `work/`.

## Corrections implemented

1. Critical/hit/damage callbacks no longer refill MP or HP unconditionally.
   The reintroduced ActionHit damage relabeling was removed. Native critical
   arguments remain unchanged in forwarding callbacks.
2. MP mode owns its ActionHit hook independently of critical mode. The typed
   callback forwards the original first, validates the actor/status/resource
   chain, then changes only local MP up to the native maximum. Its getter
   overrides are limited to the configured local player's calculators.
   Native skill payment and received resource caches remain intact.
3. Probe reads actual primary-status fields and PlayerGameStatus local HP/MP,
   rather than nonexistent PlayerBattleStatus NowHp/NowMp getters. It logs
   the cached serverMp separately and counts actual callbacks/refills.
4. Normal range preparation now hooks the reachable virtual ActionPreparation.
   It calls the native projectile-take builder after preparation and resolves
   fields dynamically. The old private PowerWaveCheck and static GetWeaponRange
   hook entries were removed: direct calls/inlining miss their MethodInfo swaps.
5. Range snapshots root managed objects and restore owned numeric values and
   managed take references using the IL2CPP write barrier. Later native flags
   and replacement take references are preserved. Extended hit geometry is
   restricted to the selected target and local actor.
6. Vtable lookup scans only the current runtime's declared VirtualInvokeData
   entries. It no longer searches arbitrary header/adjacent allocation words.
   Derived hooks publish their original callback before replacing the slot.
   In-flight forwarding originals remain available during restoration.
7. Essential timing/combat hooks fail the installation transaction when missing.
   The local chain prefers PlayerDataManager.activePlayer and its real references
   over broad manager hunting.

## Native observations before the clean test

Read-only process 15456 inspection used bounded ReadProcessMemory calls.
The current x64 class header exposes vtable_count at 0x12A and vtable entries
at 0x138; these are runtime ABI offsets, not hardcoded gameplay slot indices.

The inspected AddLocalMp body adds/clamps local MP and ends at SetLocalMp,
whose body writes only `[this + 0x1C]`. It does not itself update a remote server
or trigger the UI update claimed in earlier notes.

NormalAttackAction.ActionPreparation contains the Power Wave distance branch
inline. OnInitialize directly calls GetWeaponRange. Installing MethodInfo
replacements for those private/static helpers does not intercept these sites.
These observations establish a dispatch gap, not a server-acceptance mechanism.

## Clean live session: PID 19224

The operator restarted normally and entered a field. Exactly one core was loaded:
`dxgi_ext_f4efed3405e4d960.dll`, core SHA-256
`5E5CAF28B5FC74425D18F806C739FD483C2D6E03B5B463E65F25EA64F48024B6`.
The initial probe read HP 397/397, MP 319/319 and valid primary values.

### MP acceptance: failed

Only MP mode was ON. All three entries installed. The operator reported that
the bar refilled but the subsequent skill failed or damage reset.

Counters: `mp_hit_calls=5`, `mp_refills=5`, `mp_recovery_calls=0`.
The 1,178-sample read-only recording `work/v951-mp-19224.jsonl` captures local
MP becoming 319 while cached serverMp is 219, followed about 153 ms later
by both becoming 232. This is normal-sized recovery, not the requested full refill.
The recording also contains a skill paid from real remaining MP and target loss;
it does not independently isolate every failed cast in the operator report.
All three MP hooks restored successfully. MP mode remains OFF.

### Extended basic range acceptance: failed

Only range mode was ON. All seven entries installed. Counters reached
`range_preparations=4`, `range_patches=4`, proving that the new preparation
callback and patch executed. The operator still reported animated attacks
without lasting HP reduction.

The 1,176-sample recording `work/v951-range-19224.jsonl` captures target
UniqueId 12265 at cached HP 91. Local HP changes 91 -> 47 -> 91,
91 -> 46 -> 91 and 91 -> 27 -> 91. Those upward corrections occur about
152–154 ms after local damage. A later local-zero/target-loss event is not
proof of a server-acknowledged kill. The remote validation rule was not recovered.
All seven range hooks and the owned action snapshots restored successfully.
Range mode remains OFF.

### Melee regression acceptance: passed for this comparison

With modes 72/73 ON and modes 71/74 OFF, the operator reported faster attacks,
no Graze and HP staying reduced after melee attacks/Hard Hit.
Counters independently recorded six normal calculations, six stability calls,
six hit checks, six critical fallbacks, six timing calls and twelve motion calls.
This verifies the tested context; it is not a universal critical or damage guarantee.

Worker stop returned status 0 and logged `owned hook restoration complete=true`.
Same-core restart, re-enabling 72/73, and another probe all succeeded.
Final live state: 72/73 ON, native multiplier, MP/range and other modes OFF.
The canonical v95.1 menu was opened and inspected. It shows READY, the expected
ON/OFF labels and valid HP/MP. Its Probe Status command returned status 0,
and a module inventory still found exactly the same single core.
Session log: `work/v951-live-19224.log`.

## Remaining work

- Enhanced MP must alter a recovery path that actually executes and produces
  lasting resource recovery. The tested getters were never called during attacks;
  changing the local prediction or its cached server-labelled field is insufficient.
- Extended melee needs a demonstrated accepted native action/validation path.
  Changing an animation/take and range fields did not provide one in this test.
  A native Power Wave/bow comparison is unavailable: the operator has neither.
- Invincibility, movement, no-collision, anti-rigidity, custom multipliers and additional
  enemies/skills were not revalidated by this focused session.
- Server logic and authoritative acceptance formulas are not available here.
  Do not replace these open tasks with claims of complete implementation or
  assume a named client field is the remote server's memory.
