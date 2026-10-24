# Security remediation — 2026-10-20

The five findings in strix_runs/crates_8356 are patched in the working tree.
Existing uncommitted gameplay changes were retained. Pre-change source copies,
test/build logs and the focused scan instruction are in
work/security-fixes-20261020/.

| Finding | Implemented change | Windows verification |
| --- | --- | --- |
| M1: GameChain fallback | Validated acquisition helpers check object headers before retention/use; singleton and FindObjectOfType results, BFS enqueue/dequeue, raw pam+0xA0, and terminal success use is_valid_il2cpp_obj. Both GC pin entry points reject invalid objects before calling the native GC API. | Readable invalid-header arenas fail acquisition, pinning and terminal acceptance; all 60 wcore tests pass. |
| M2: forged receipts | Only the child stdout pipe supplies receipts. Disk logs are display-only. The parent passes the selected PID, creation time and image path to the child. Receipts require that PID and command; foreign, embedded and duplicate receipts fail closed. | Receipt regressions and all eight companion tests pass. |
| L1: IPC squat/spoof | Mapping names include PID and creation time. An explicit protected DACL grants only read/control-read to the user, SYSTEM and OWNER RIGHTS; the creator retains its original writable handle. Existing mappings are rejected. Consumers open/map read-only and authenticate custody by duplicating the claimed handle from the target process, then validate the target-held view. Writer APIs verify their creator PID. | Same-user write and WRITE_DAC opens are denied; existing mappings and forged handles are rejected; a separate consumer process reads the authentic mapping. |
| L3: torn snapshots | Private telemetry fields are mutated only inside a serialized odd/even epoch guard. Initialization, command receipts, hook/resource updates, settings and toggles all participate. Field and epoch atomics use sequential consistency. Readers make bounded retries and return invalid if a writer stalls. Command sequence remains separate. | Two concurrent writers perform 39,998 resource/receipt updates without mixed valid snapshots; stalled-writer regression passes. |
| L2: stale PID | Capture handle-derived PID, creation time and full image path before staging. Repeat the identity/liveness check on the final OpenProcess handle before remote allocation/write/thread creation. Fan-out captures all selected identities before staging; subprocess dispatch preserves the parent's selection. | Reused PID/lifetime/path cases fail comparison; live handle query works on the test process; a non-game target is rejected before staging or remote writes. All 17 wboot tests pass. |

## Validation

Primary code locations:

- M1: crates/wcore/src/chain.rs (validated acquisition helpers, singleton resolver and terminal gate); crates/wcore/src/il2cpp.rs (GC pin entry points).
- M2: crates/companion/src/lib.rs (parse_target_receipt and run_subprocess); crates/wboot/src/main.rs (pinned target CLI arguments).
- L1: crates/protocol/src/ipc.rs (security descriptor, creation, writer authorization and authenticated reader).
- L3: crates/protocol/src/lib.rs (SharedTelemetry update/snapshot); crates/wcore/src/features.rs and runtime.rs (all publisher transactions).
- L2: crates/wboot/src/lib.rs (identity_from_handle, same_process, dispatch_to_toram_expected and dispatch_to_all_toram).

- Rust 1.99.1 (58cf8f9c3 2026-10-13), Windows x64 MSVC target.
- cargo test --workspace: 98 top-level tests pass, plus the child-process reader test. No compiler warnings.
- cargo build --release --workspace: passes.
- Final logs: work/security-fixes-20261020/tests.log and build.log.
- Focused Strix verification completed as strix_runs/crates_1985: five coverage entries, zero coverage gaps, zero new findings. The reviewer judged all five original flaws closed in source, with the bounded limitations recorded below. This is a model-judged static review, separate from the executed Windows tests.
- Earlier run crates_0e07 exhausted its budget; resuming only produced an incomplete report with no coverage. Its zero-finding result is not evidence of closure. The later single-verifier run supplies the actual per-finding review.

Current source entry points (one-based lines):

| Area | Entry points |
| --- | --- |
| GameChain | crates/wcore/src/chain.rs:148 and :640; crates/wcore/src/il2cpp.rs:188 |
| Receipts | crates/companion/src/lib.rs:112 and :493 |
| IPC | crates/protocol/src/ipc.rs:167, :250, :304 and :354 |
| Epochs | crates/protocol/src/lib.rs:167 and :248 |
| Process identity | crates/wboot/src/lib.rs:695, :938 and :1152 |

The review report is strix_runs/crates_1985/source_review_report.md;
its recorded per-area evidence is in coverage.json. The entry points above were
checked against the final local files rather than relying on the reviewer's
line offsets.

## Runtime and compatibility

The workspace version remains 0.95.8; the changed telemetry layout is version
957. Use the matching core, loader, menu and observer together. Older telemetry
readers/layouts are incompatible. Candidate binaries are staged separately under
work/release-v95.8-security/, with SHA-256 manifests.

No game was launched, restarted or controlled for this remediation.
Combat/MP acceptance remains at its previous checkpoint. The pointer validator
checks structural readability/alignment/header validity; it does not prove GC
liveness or the provenance of deliberately fabricated class metadata. Process
path comparison binds a selection across staging; it does not establish a
separate installation allowlist.

## Win32 references

- [CreateFileMapping: new handles, zero-filled paging mappings, existing-object detection](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-createfilemappinga)
- [Well-known SIDs, including OWNER RIGHTS](https://learn.microsoft.com/en-us/windows/win32/secauthz/well-known-sids)
- [QueryFullProcessImageNameW](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-queryfullprocessimagenamew)
