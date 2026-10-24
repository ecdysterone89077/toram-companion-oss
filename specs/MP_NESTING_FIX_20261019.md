# v95.7 MP nesting fix — bonus applies exactly once (13 -> 23, never 33)

Date: 2026-10-19, Asia/Jakarta. Workspace 0.95.7, telemetry 956 (unchanged).

## Operator evidence that resolved it

- +10: 7 hits per 100 MP. Native/+25/+50: 8 hits. (Native +13/hit baseline.)
- Live `%TEMP%/wcore.log` lines from the v95.6 core:
  `[mpdiag] owned=1 native=13 bonus=10 effective=23` (correct, once), then
  `[mpdiag] owned=1 native=23 bonus=10 effective=33` (the bug), and the
  enable-time readout showing tinted `battle=Some(33) secondary=Some(23)`.

## Root cause: reentrant reads compounded the bonus

One calculator reads its native AMPR through the other hooked calculator, so
`mp_recovery_value` re-entered itself and added the bonus per nesting level:
13 -> 23 -> 33 -> ... With +10 the first hits looked slightly faster while the
compounding tail reconciled; with +25/+50 the values exploded immediately and
the server discarded everything above tolerance, landing exactly on native.
The crit-damage path had this identical disease and already carried the cure
(thread-local reentrancy guard); the MP path was missing it.

## Fix

- `MpNativeRead` thread-local guard + pure `mp_apply_bonus` mapping: nested
  reads return native, the bonus applies exactly once, saturating arithmetic.
- Enable-time readout moved BEFORE hook installation, so it reports true
  native scale that no selected bonus or nesting can tint.
- `[mpdiag]` now logs `nested=` per hit; getter-call + boost counters kept.
- Removed `MpBinding::refill` (the localMp=max writer) entirely; hit path and
  tests use read-only `sample()`. Production can no longer manufacture MP.

## Retest (restart normally; diag cap is 24 lines per lifetime)

1. F2 ON + F4 ON, select +10 [86]. Hit counts should now be STABLE: every hit
   grants the same accepted amount (no first-hit boost with tail fallback).
2. Repeat the 100-MP counts at +10/+25/+50, fresh AND after basic spam.
3. Report counts + any fallback + the new `[mpdiag nested=]` lines.
4. Expectation: stable 23/38/63-per-hit predictions; server keeps whatever
   fits its band. If +25/+50 still land on native, their band verdict is final
   and +10 (or hit rate via 72) is the accepted fast MP.

## Verification

83 workspace tests pass (protocol 4, wcore 56, companion 9, wboot 11, observer 3),
zero warnings. Release staged + hash-verified under
`work/release-v95.7-mp-nesting-fix/`:

- companion.exe `6E73BF48CC92C7177AD2910391C908732B7909E49A6FF988C81414474539AE14`
- src/wcore.dll `87B86FA1A27C9523601FFED52C8D570FA1F5B21C76E439DB3A2D124F1D1BDF7C`
- src/wboot.exe `CAE5E6BD8904A1F0B8D65D32FB070EC37733DEF00FBACD4FCD2431DB7A97C655`

## Follow-up: invisible toggle (same day)
The readout surgery had dropped `set_toggle_bit(TOGGLE_UNLIMITED_MP)`: status
was 0 while the badge stayed OFF, so F4 presses silently toggled hidden state
(log proves final state DISABLED). Line restored; the hash above is the fixed DLL.
Restart the game — the old process still runs pre-guard code.
