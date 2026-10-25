# Scripts

Run commands from the repository root. Python tools use the standard library;
`workflow.py` requires Python 3.11 or newer. PowerShell packaging requires 7+.

## Repository checks

```powershell
python -B -m unittest discover -s scripts -p '*_checks.py'
pwsh -NoProfile -File scripts/package_release_checks.ps1
```

Python discovery runs the acceptance, correlation, target-audit, incident, and
workflow regressions. Tests requiring local metadata, dumps, or incident archives
report a skip when that input is absent. Packaging regressions create synthetic
text fixtures under `work/package-release-checks/` and verify archives, manifests,
checksums, and preservation of existing outputs.

The complete CI command list is in [Testing](../README.md#testing).

## Change-scoped workflow

```powershell
python -B scripts/workflow.py brief
python -B scripts/workflow.py plan
python -B scripts/workflow.py check --path scripts/workflow.py
```

- `brief` shows source version, branch, commit, and changed-file count. A local
  `CURRENT.md` checkpoint is displayed when present; it is optional.
- `plan` previews checks for tracked and untracked changes against `HEAD`.
- `check` runs the selected checks and writes receipts under `work/workflow/`.
- Repeat `--path` to select files or directories; use `--base` to compare against
  another local Git ref. Unmapped files are reported for manual verification.

The helper launches Git, Cargo, Python, and PowerShell subprocesses as needed.

## Analysis utilities

| Tool | Input and purpose |
| --- | --- |
| [acceptance_report.py](acceptance_report.py) | Summarizes captured event and mob records. |
| [correlation_audit.py](correlation_audit.py) | Audits archived capture correlations and writes a report. |
| [incident_attribution.py](incident_attribution.py) | Analyzes recorded incident and shadow-module evidence. |
| [blind_spot_audit.py](blind_spot_audit.py) | Compares local symbol declarations, hook tables, and archived responses. |
| [faunas_target_audit.py](faunas_target_audit.py) | Checks source target declarations against supplied metadata and optional dumps. |
| [faunas_port_map.py](faunas_port_map.py) | Maps reference declarations against a local dump. |
| [faunas_imm_decode.py](faunas_imm_decode.py) | Decodes recorded ARM64 immediate values. |

Consult each script's module docstring for its invocation and expected inputs.
Metadata, dumps, and local capture evidence are not bundled with the repository.
For a Windows release bundle, use the matching source archive to access the scripts.

## External-tool utilities

These utilities are separate from the repository checks:

| Tool | Requirements and effects |
| --- | --- |
| [export-metadata-symbols.ps1](export-metadata-symbols.ps1) | Requires metadata and a supplied dumper assembly; writes a symbol export. |
| [rea_targets.py](rea_targets.py) | Checks or writes pins for local REA analysis targets. |
| [patch_rea_ghidra_bridge.py](patch_rea_ghidra_bridge.py) | Inspects, patches, or restores a local REA/Ghidra bridge installation and checks compilation. |
| [tcp_write_probe.py](tcp_write_probe.py) | Opens a local TCP listener and client connections to measure write delivery. |
