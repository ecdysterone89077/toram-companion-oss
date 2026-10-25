# Design records

These are dated research and maintenance records. Read correction sections and
verification status before using a record as a description of the current build.
Referenced local captures, metadata, and historical artifacts may not be included
in this public repository. Build and test instructions live in the [root README](../README.md).

## Navigation

| Topic | Records |
| --- | --- |
| Movement and restoration | [Movement rework](MOVEMENT_SAFETY_REWORK_20261021.md), [source/spec reconciliation](GITHUB_MOVEMENT_RESTORE_20261022.md), [geometry baseline](GEOMETRY_BASELINE_20261019.md) |
| Range and targeting | [Range candidate](RANGE_TARGET_CANDIDATE_20261018.md), [wall-check rework](RANGE_WALL_CHECK_REWORK_20261021.md) |
| Critical damage and stability | [Historical ladder and correction](SAFE_CRIT_LADDER_20261019.md), [+1000 failure](CRIT_DAMAGE_1000_20261019.md), [stability analysis](STABILITY_CRIT_ANALYSIS_20261019.md), [stable/hit priority](STABLE_HIT_PRIORITY_20261022.md) |
| MP and action state | [MP observations](MP_ACCEPTED_20261019.md), [AMPR observations](MP_AMPR_OBSERVE_20261019.md), [nesting fix](MP_NESTING_FIX_20261019.md), [passive actions](PASSIVE_ACTION_20261019.md), [skill clamp](SKILL_CLAMP_20261019.md), [stat transitions](STAT_TRANSITION_ANALYSIS.md) |
| Combat observations | [Native baseline](NATIVE_BASELINE_20261019.md), [formula review](COMBAT_FORMULA_REVIEW_20261020.md), [feature behavior](FEATURE_BEHAVIOR_ANALYSIS_20261020.md), [dimension model](VALIDATED_DIMENSION_MODEL_20261021.md), [correlation ownership](CORRELATION_OWNERSHIP_AUDIT_20261021.md) |
| Coverage and investigation | [Blind-spot map](BLIND_SPOT_MAP_20261021.md), [audit](BLIND_SPOT_AUDIT_20261021.md), [RE trace](RE_TRACE_20261020.md), [TCP write loss](TCP_WRITE_LOSS_20261021.md) |
| Port and metadata | [Port record](FAUNAS_PORT_20261022.md), [PC v226 metadata verification](PC_V226_METADATA_VERIFICATION_20261022.md) |
| Reliability and maintenance | [Reliability repair](RELIABILITY_REPAIR_20261020.md), [security fixes](SECURITY_FIXES_20261020.md), [runtime repair](RUNTIME_REPAIR_RC2_20261021.md), [offline cleanup](OFFLINE_CLEANUP_20261021.md), [offline maintenance](OFFLINE_MAINTENANCE_20261021.md) |
| Historical validation | [v0.94](V94_VALIDATION.md), [v0.95.1](V951_VALIDATION.md), [v0.96 upgrade](V96_UPGRADE_20261020.md) |

## Bug records

- [Registry](bugs/registry.yaml) — recorded status and links to incident reports.
- [Worker lifecycle incident](bugs/BUG-2026-10-16-v49-lifecycle.md).
- [Critical-damage acceptance incident](bugs/BUG-2026-10-19-critical-damage-acceptance.md).
