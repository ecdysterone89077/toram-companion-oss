# HISTORICAL — pre-rewrite C++/Python lineage

> This record covers the v49 lifecycle work from the C++/Python era
> (`build_v49.ps1`, `tests/test_companion.py`, `C:\Python315\python.exe`). None
> of those exist in the current pure Rust workspace.
>
> The underlying worker-lifecycle finding is still relevant to the current
> build, and the recorded cause of the game freeze was never established. The
> current state is in `STATE.md` and `HANDOFF.md`.

# BUG-2026-10-16-v49: Bounded worker attachment and cooperative lifecycle

## Approved scope
User approved the proposed lifecycle-only next version. This repairs the verified source ownership omission and contains the known-failing stats action. It does not claim to identify the game's shutdown initiator or prove the recorded wait targets the worker. No live game process, module loading, debugging, protection modifications, stat algorithm changes, or network game-service requests.

Project survey: existing native C++/Python project, no Git repository and no previous specs/state.yaml. The existing root-cause report remains outside this repository. Preserve earlier binaries/evidence and baseline exact edited files; do not initialize Git or install skills/dependencies.

## Verified source cause / contract
A successful worker attachment currently spans indefinite idle OS waits and has no stop/detach/normal return path. The previous exact-source isolated characterization and current RED run demonstrate that behavior. It is a source lifecycle omission, not a proved cause of either recorded game freeze.

Security impact: LOW; no security exploit path identified in this lifecycle task. Runtime integration risk: HIGH because actual engine teardown timing and stat-associated quit initiation remain unknown.

## Design
- Keep one native worker and existing dispatch semantics. Scope attachment to bootstrap and each job, release temporary GC handles and detach on that same worker before readiness/completion. Wait idle detached.
- Add terminal command0 for explicit stop using existing transport; never send it automatically from menu close. Stop closes admission, drains already accepted work, then exits; no forced thread termination or restart.
- Serialize publication/stop with a short state lock. Preserve fail-closed command timeout admission. Protect handles used by exported-call waiters; close only after worker signaled and all such waiters leave. Pin module outside loader lock for process lifetime to prevent unloading executing code; stop is not module unloading.
- Keep DllMain free of joining, runtime calls and teardown waiting.
- Disable stat command44 in UI and native public command boundary, without altering stat calculations. Keep the invincibility algorithm unchanged.
- Extract only the lifecycle block into an internal production header so isolated tests execute production lifecycle logic rather than a replacement fake worker. No test-only branches in production.

## TDD / verification plan
1. RED: actual-source v48 characterization must fail the now-approved idle-detached contract. GREEN: actual production lifecycle tests cover attachment/detachment ordering, terminal stop and failures.
   verify: C:\Python315\python.exe -m unittest discover -s tests -p test_worker_runtime.py -v
2. RED: menu command44 remains reachable/re-enabled. GREEN: reject before any thread/process and keep disabled through completions.
   verify: C:\Python315\python.exe -m unittest discover -s tests -p test_companion.py -v
3. Preserve arithmetic, admission and loader contracts; adapt fixtures for intentional command0/44 semantics.
   verify: C:\Python315\python.exe -m unittest discover -s tests -v
4. Build separate v49 artifacts without overwriting previous DLL/EXE; inspect expected exports/dependencies and review concurrency/cleanup changes.
   verify: powershell -NoProfile -ExecutionPolicy Bypass -File .\build_v49.ps1

## Acceptance
- [x] Detached idle and same-thread balanced attachment on normal and C++ exception paths (isolated runtime API doubles).
- [x] Accepted work completes cleanup before receipt; terminal stop rejects new work and joins without premature resource release (offline integration tests).
- [x] Timeout does not cancel, retry or overwrite work; shutdown still available.
- [x] Stats action unavailable through menu and command44.
- [x] Fresh offline tests/build pass; parent source-level review completed. Independent final implementation review did not complete (agent usage limit).
- [x] Limits explicitly disclosed: no real IL2CPP/gameplay or freeze-fix validation.

## Resolution
Lifecycle-only implementation and offline acceptance complete: 41 tests pass, separate v49 artifacts built, exports/imports checked, earlier binaries preserved. Final evidence: specs/verification/v49-full-suite.txt, build-v49-final.txt, v49-artifact-check.txt. Behavioral notes and remaining runtime limits: specs/MENU_V49.md.

Parent review also found and regression-tested two implementation issues: failed-worker admission had to close before completion signaling, and a successfully started legacy init job had to clear the pending status sentinel. Both are covered by the final passing suite.

Incident root cause and real-runtime repair remain unverified. Prior failed root-cause experiments are not counted as a verified runtime repair; no live verification was performed.
