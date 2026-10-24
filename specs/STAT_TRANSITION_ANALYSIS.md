# Stat transition investigation — current Windows metadata

## Outcome

The current Windows metadata confirms a richer stat-update interface than the nine direct writes in the mod. It contains player-specific status operations, a corresponding response carrying primary status and HP/MP, and separate status-point/update methods. This strengthens the hypothesis that direct field mutation omits a required state transition. It does **not** prove the executed call graph, thread, violated invariant, or shutdown initiator. No replacement stats implementation is justified yet.

No production source, menu, DLL, or game process was changed in this investigation. Only this report, the focused read-only metadata inspection script, and its text output were added.

## Updated control evidence

The user reports having tested the invincibility command independently of stats: enabling, playing, and disabling all worked normally, without freezes or crashes. Accept this as the reported passing control. It narrows investigation toward the stats-specific path; do not portray invincibility-only testing as absent.

The recorded first-plus50 failure still had invincibility enabled immediately beforehand. The passing invincibility-only control does not separately establish the result of a stats-only run or rule out an interaction. No additional gameplay experiment was performed here.

## Current-build evidence, not Android RVAs

Read-only input:
`C:\Asobimo\AsobimoGames\Toram\ToramOnline_Data\il2cpp_data\Metadata\global-metadata.dat`

- SHA-256: `f0cf212a7c7dca1973a401bda6d208549c13f57f0bbdb8782e5d607ed7e8789f`
- Metadata version31: 20163 type definitions, 157413 method definitions, 96686 field definitions.
- Parsed using the already-installed `il2cppdumper/Il2CppDumper.dll`, SHA-256 `8d5be1c629e7474f3bfd3285a6596ee46abdc4f0ac58d4fe4a8b1df13aa03d59`.
- The parser requires a writable stream. The source was read into a writable in-memory copy; the installed metadata file was not opened for writing. No game executable or native game DLL was loaded or run.
- The older `dump/dump.cs` was used only to find candidate names. Its type indices differ from the Windows metadata (for example, `PlayerPrimaryStatus`:1460 versus1461). Its RVAs, method bodies, constants, and field types were not promoted to current Windows evidence.

## Verified declarations

These are metadata declarations and parameter **names**, not resolved runtime signatures or observed calls:

| Owner | Declared method | Parameter names |
|---|---|---|
| `PlayerDataManager` | `StatusUp` | `status` |
| `PlayerDataManager` | `OnStatusUp` | `response` |
| `Operations` | `StatusUp` | `game`, `avatarUuid`, `archetypeType`, `primaryStatus` |
| `IAvatarListener` | `OnStatusUp` | `response` |
| `MobaPlayer` | `ConnectionUpdateStatus` | `data` |
| `PlayerStatus`, `MobaPlayerStatus`, `PlayerStatusBase` | `UpdatePrimaryStatus` | `primaryStatus` (two declarations per owner) |
| `PlayerPrimaryStatus`, `PlayerDataManager`, `PlayerStatusBase` | `UpdateStatusPoint` | `point` |

`Toram.Common.Operations.Main.Status.StatusUp` has backing fields for `AvatarUuid`, `ArchetypeType`, and `PrimaryStatus`.

`Toram.Common.Operations.Main.Status.StatusUpResponse` has backing fields for those three values plus `Hp`, `Mp`, `ExHp`, and `ExMp`.

`PlayerPrimaryStatus` declares the nine modified stats alongside `lv`, `statusPoint`, `skillPoint`, `personality`, `moveSpeed`, and `checkValue`. Its available methods include creation/conversion, point updates, `GetAllStatusPoint`, `Check`, and `GetCRC32`; the nine stat properties have getters in the inspected method list, not individual stat setters. Method names alone reveal neither integrity algorithms nor failure behavior.

No edges between these declarations have been proved. In particular, a diagram claiming `StatusUp -> network -> OnStatusUp -> UpdatePrimaryStatus` would currently be a hypothesis, not an observed execution trace. Nothing here establishes the callback thread or the server's acceptance rules.

## Comparison with the two mod paths

- `src/wcore.cpp:3010-3079`: the invincibility command invokes the game's add/remove effect methods and keeps the returned effect ID. It does not implement its effect by directly overwriting all HP-related fields. Its successful user control is consistent with using a coherent game-managed operation, but the method internals were not recovered.
- `src/wcore.cpp:3367-3418`: stats preflights nine instance int32 fields, adds50, writes them separately, and reads each back. It invokes none of the status operations above and does not update the other declared fields. This is a definite implementation omission relative to a full allocation transaction; which omitted operation is required, and whether it caused the recorded failure, remain unknown.
- The current mock stat tests model nine integers with direct read/write functions. They verify arithmetic and error handling, not point accounting, response processing, object replacement, or shutdown. Passing those tests cannot validate a full stat transition.
- `il2cpp_thread_attach` is not a main-thread dispatcher. Thus even choosing a plausible method name would not establish correct scheduling or safe object ownership. See [Unity's explanation](https://discussions.unity.com/t/when-are-callback-for-il2cpp-thread-attach-processed/837447).

## Narrowed hypotheses and limits

1. **Highest-value gap to investigate:** a normal allocation may coordinate point accounting and refreshed player/derived status, while direct mutation changes only one representation. The current Windows request/response declarations make this a concrete interface-level hypothesis rather than a generic guess.
2. **Still unproved:** whether `checkValue`, an object-identity change, a concurrent reader, server response, or another condition initiates shutdown. Do not change checksums or call guessed update methods based only on their names.
3. **Separate lifecycle question:** the shared worker's lifecycle hazard can help explain incomplete shutdown, but does not by itself explain the difference between the passing invincibility control and the failing stat operation. v49 is not proof of a stats fix.
4. **No unlimited-stat conclusion:** recovering a normal allocation path would not establish that arbitrary +50 requests or unlimited values are accepted by that path.

## What would resolve the remaining questions

### Completed normal-UI control (2026-10-16)

With no mod activated (user report), the screenshot showed STR/INT/VIT/AGI/DEX all1, 24 available points, HP186, and a separate area marked `Available from Lv40`. The user explicitly selected STR for a one-point native-UI allocation. The subsequent screenshot showed STR2, the other four displayed stats still1, 23 available points, and HP186. The user then confirmed normal movement/camera and unchanged STR2/23 points after closing and reopening the panel.

Result: **PASS for this bounded normal-allocation control**. It establishes a visible +1 STR / -1 available-point transition with continuing reported gameplay. It does not isolate the cause of the mod failure: the native control and old command44 differ in mechanism, increment, number of fields, and point accounting. The old helper explicitly increments nine fields by50 (aggregate +450), but implements no corresponding available-point deduction. The UI's locked area and the helper's extra four stat fields warrant comparison; their exact connection to the failure is not proved.

Read-only process metadata showed the same PID12988 responding before/after. Player.log grew by84 lines; no `StatusUp`, `OnStatusUp`, `PrimaryStatus`, `ShutdownInProgress`, `Exception`, or `Fatal` terms appeared in the appended portion. Its inspected tail contains asset-unloading/GC timings, not an allocation call trace. No debugger attach, game-memory read/write, packet capture, or further point allocation was performed by the assistant. No mod +50 retest occurred.

Evidence: `specs/verification/normal-allocation-control-20261016T063233Z/baseline.md`, `after-str1.md`, and their local before/after/delta log snapshots. The result does not demonstrate map/reconnect persistence, expose method execution order, or prove which invariant failed.

### Remaining evidence boundary

The missing evidence is a version-matched normal allocation trace or method implementation showing the actual order, ownership, and dependent updates, plus a game-owned reason/caller for the observed shutdown. Metadata alone cannot provide those facts. The earlier shutdown dump coverage report also remains a limitation: the relevant recorded shutdown code pages were absent; no new process capture was made.

Prefer existing normal-allocation recordings/logs or a supported diagnostic trace before another modification experiment. Do not spend/reset the user's stat points automatically, attach a debugger that already confounded a control, or re-enable the known-failing +50 command merely to reproduce the same symptom. An owned fixture can test lifecycle or a proposed state model, but cannot establish Toram's implementation by itself.

## Reproduce the static check

From the project directory, in PowerShell7 (the host used here):

```powershell
& .\specs\verification\inspect-stat-transition.ps1
```

Final run: **PASS**, with metadata format, range, owner, parameter-range, and bounded-output checks. Output is saved in `specs/verification/stat-transition-metadata.txt`. The initial script's magic check treated a hexadecimal literal as signed; the corrected unsigned conversion passed. No production test suite was rerun because this investigation made no production changes. No new dependency was installed and no independent reviewer ran in this pass.
