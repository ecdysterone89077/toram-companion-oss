# Offline maintenance checkpoint — 2026-10-21

The menu wording and release-tool defects below are fixed and verified offline.
The overall project is **incomplete**: no operational guarantee and no new
gameplay validation follows from this work.

## Changes

- The UI calls the former zero-risk profile **Legacy preset**. CLI `legacy`
  is its current name; the case-insensitive `safe` alias and the existing Rust
  variant remain compatible. The profile's command sequence is unchanged.
- Critical-value buttons describe requests rather than a universal known-good
  result. The profile comments no longer assert unlimited server-blind values.
- The packager calculates checksum paths relative to the package directory.
  The original function returned
  `dist/Toram-Companion-v0.96.3-rc.1-win64/README.md` where the extracted
  checksum file requires `README.md`; the defect was reproduced independently.
- Release outputs are never deleted or overwritten. Existing output paths
  fail preflight, and version values cannot contain path separators.
- Manifest schema 2 records that the packager does not run tests, compile
  binaries, or verify correspondence between current source and supplied
  binaries. The unconditional `152 passed` and build-pass claims are removed.
- Source packages include the packaging script and its regression checker.
  ZIP creation includes dotfiles, and each archived entry is read back and
  compared with its staging file.
- Current release notes, validation, README and continuation records separate
  source changes, historical packages and unproven incident explanations.

## Verification

Windows x64, Rust/Cargo 1.99.1, Python 3.14.8, PowerShell 7.

| Check | Result | Evidence under `work/maintenance-20261021/` |
| --- | --- | --- |
| Profile regression before change | Failed on `legacy`, as expected | `profile-before.log` |
| Original checksum-path function | Defect reproduced | `checksum-before.json` |
| `cargo test --workspace --locked --offline` | 158 tests plus one child-process probe passed | `workspace-tests.log` |
| `python scripts/correlation_audit_checks.py` | 19 passed | `correlation-tests.log` |
| `pwsh -NoProfile -File scripts/package_release_checks.ps1` | 45 assertions passed using text placeholders | `package-checks.log` |
| `cargo build -p companion --release --locked --offline` | Passed without compiler warnings | `menu-release-build.log` |
| Compiled CLI help and save-plan commands | Passed; `legacy` and `safe` produce byte-identical plans | `cli-check.json`, `menu-help.log` |
| Preserved files | 20 hashes unchanged | `preservation-check.json`, `preserved-artifacts.json` |

The Rust count is `companion` 17 library + 2 binary, `observer` 6,
`protocol` 13, `wboot` 19, `wcore` 99, and `xtask` 2. The child-process
reader separately runs one probe. This explains the 159 successful test
executions in the full log; earlier counts belong to earlier snapshots.
The worker panic-isolation test intentionally emits a simulated panic and passes.

The regression fixture archives contain text placeholders, not functioning
runtime binaries. The package checks verify extracted checksum paths, manifest
truthfulness, required source files, unchanged outputs on a repeat invocation,
and rejection of version traversal/separators. The fixture directories remain
available under the evidence folder.

The optimized menu is a local build output under
`target/x86_64-pc-windows-msvc/release/companion.exe`. It was exercised only with
`--help` and `--save-plan`. It was not promoted over the canonical menu.

## Incident conclusions and remaining work

| Item | Supported status | Remaining gap |
| --- | --- | --- |
| 03:06 warning | Preserved screenshot and shutdown records exist | Enabled features, mechanism and cause are not established |
| PID 1032 lifecycle check | Candidate load, speed/no-collision commands and the resulting disconnect are recorded | Exact onset and causal trigger remain unknown |
| MP/range OFF cleanup | Existing repair's regressions pass in the current workspace suite | An offline cleanup pass does not establish accepted gameplay results |
| Current movement and critical policy | Source policies are documented | Neither the 15.0 speed cap nor 135% critical cap is a measured universal acceptance threshold |
| MP, distant damage and Hard Hit | Earlier failed/unresolved results remain | No new correlated gameplay evidence |
| Commands 96–99 | Unimplemented | No automatic pacing or targeting implementation |
| Strix rescan | Previous run stopped on provider billing failure | No new scan or coverage |
| Release archives | Existing archives and canonical binaries preserved | They retain their historical source/docs; this maintenance is not packaged as a new release |

The incident records are kept in the development repository. They do not
establish a causal trigger or identify a mechanism. No live test was performed
in this continuation. No concealment or integrity-check change was implemented.

## Reproduction and handoff

Run the commands in the verification table from the repository root. The
packaging checker uses a new synthetic fixture directory on every invocation.
The production packaging script now requires PowerShell 7 and refuses an
existing version's output paths. It packages supplied artifacts; it does not
build or certify them.

The canonical binaries, existing archives, incident evidence manifests and
`crates/wcore/src/features.rs` were unchanged. Pre-existing working-tree edits
were retained; their starting diff is `preexisting.patch` in the evidence folder.
No game was launched or attached to, no commands were sent to a game, and no
external publication occurred. The repository must not be marked fully complete.
