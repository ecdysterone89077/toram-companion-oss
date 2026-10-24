# PC v226 dump verification — 2026-10-22

The checked file is `PC_v226_dump.cs`, SHA-256
`DF48CC4E5C0C32E41F912BE102B165E1875FCDBB08447D73A23BC64B25CBAC20`.
It is 20,003,769 bytes, with 362,709 lines, 16,238 class headers and
156,264 method declarations carrying 89,269 distinct relative addresses.

## Evidence

- The running Toram client was PID 14912. Windows reported its installed
  `GameAssembly.dll` base as `0x7FFA42D90000`, size `0x4F70000`.
- All 23 original-pointer records in that client's latest core-log segment
  matched this base plus the dump's method RVA. These cover 20 distinct
  addresses, including both status classes' critical/stability getters,
  damage calculation and Photon response handlers.
- All 89,269 distinct method RVAs fall in committed executable/readable
  memory in that process. This is an address-placement check, not proof that
  every method has the correct name or signature.
- Read-only native-byte samples agree with two field relationships in the
  dump: `CharacterMove.get_MoveSpeed` reads the float at `this + 0x3C`;
  `CharacterMove.get_EnableWallHit` reads its `FieldRayPick` at `this + 0x30`
  and that object's boolean at `+0x5C`.
- The current port's 63 hook specifications pass class-name, method-name and
  argument-count checks against this file, with no ownership conflicts in
  the port specification table. This does not validate interception or a
  gameplay effect.

Machine-readable evidence: `work/dump-verification-20261022/verification.json`.
No game-memory writes or gameplay commands were performed for this audit.

## Provenance and interpretation

Windows' Zone.Identifier records `web.telegram.org` as the download source.
The original publisher and generating tool are not established. The creation
time in this workspace is a download/file-creation time, not proof that the
game generated the dump. The existing auditor's “game-generated” description
must not be treated as evidence of provenance.

The evidence supports using this file as a declaration/address reference for
the checked running PC build. It is not recovered C# method-body source or
proof that every field, signature and feature is correct. A game update
requires checking correspondence again. Earlier warnings about the older
`dump/dump.cs` do not establish that this separate file is stale.

## Companion comparison baseline

The supplied `toram-companion-main.zip` is
version 0.96.1, SHA-256
`471F9A866313D7E41D6DA14E67D6DCF7503B7902F9549CA21F84C97A04968CC5`.
Its manifest and five checked principal Rust sources match commit `4dd9546`
after newline normalization. It is extracted separately under
`work/reference/github-v0.96.1-471f9a86/`.

The operator's corrected baseline report is: speed and no-collision work in that
version; MP recovery and attack range fail there. The current local version
also has reported movement regressions and inactive Android-port features.
Source comparison confirms that later movement changes removed the older
direct field writes and substantially reduced the no-collision hooks. This audit
does not mark those features repaired or gameplay-verified.
