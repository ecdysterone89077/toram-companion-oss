# Native baseline — 2026-10-19

## Scope and setup

The operator authorized bringing Toram forward and operating the all-features-OFF
baseline. UI inspection and inputs used the bundled computer-use skill after
the game-automation CLI was found unavailable. No experimental mode was enabled,
no worker/probe command was dispatched, and no runtime binary was replaced.

This is an all-features-OFF comparison with the existing candidate core present,
not a new no-core game process. PID 2260 retains creation FILETIME
134368076866930507. The OS module inventory contains one core whose on-disk
SHA-256 is 5721F0D46536182D43DF3853707E53E31588D5F75B2DFC8B6C23E05ABA5F1B51.
This identifies the file corresponding to the module path, not mapped bytes.

UI-observed setup: level 19 character `crit`, Longsword ATK 17 / stability 80%,
MaxHP +50 / Accuracy +1; Buckler DEF 6 / Guard Recharge and Power +15%; Leather
Armor DEF 11 / MaxHP +50; Hachigane DEF 8 / VIT +1 / MaxHP +50, with Accuracy +2
also displayed. Other buffs were not fully characterized. Equipment was not
changed. Hard Hit Lv1 shows MP cost 100; 28 unspent skill points were left intact.
The bag was full; no inventory deletion was performed.

## Captures and controls

The bounded, read-only local `work/sample_combat.py` producer was preflighted
against PlayerStatus 0x22b1802b1b0 in the verified same process. It reads typed
resource/target fields without invoking game methods. The observer wrapped its
JSONL, read the existing IPC mapping with FILE_MAP_READ and checked the bound OS
process lifetime before and after each IPC observation.

| Session under work/observations | Rows | Resource/mode rows | Rejected source reads |
| --- | ---: | ---: | ---: |
| baseline-20261019-2260 | 2348 | 2346 | 2 |
| native-mp-20261019-2260 | 1172 | 1170 | 2 |
| native-ampr-20261019-2260 | 1761 | 1759 | 2 |

Total: 5,281 rows. Process-generation checks passed for every row; valid mode
observations report mask 0 throughout. Source errors break event matching. Exact
input hashes, descriptive transitions and remaining gaps are in each session's
`analysis-v2.json`; original reports and raw data are preserved.

Distance/action/outcome annotations are timestamped operator markers based on
screenshots and tool actions. They are not a per-frame distance sensor, action
callback stream or exported video. The first screenshot capture showed the
foreground chat rather than Toram and was not used as game evidence; the game
was then explicitly brought forward with operator authorization.

## Native observations

1. Close normal attacks: selected Lv6 Colon was visibly at 1 m. Target 14056
   changed from received/local HP 102/102 to local 65 and received 65, then local
   27 and received 27, then local zero/target loss. The UI subsequently displayed
   EXP +3 and Colon Leaf. This supplies independent reward evidence beyond
   disappearance alone.
2. Far selection: a Lv4 Colon selection read 9 m, then 11 m during movement.
   Native attack initiation moved the character into melee. A later screenshot
   showed damage 42 and the next-attack timer at HUD distance 1 m. Target 14062
   had received HP decreases 91 -> 43 -> 1 before local zero/target loss. This
   demonstrates approach, not a successful sword hit at the initial far distance.
3. Hard Hit payment: the skill-info UI specifies 100 MP. In the first MP capture,
   local MP changed 319 -> 219 at elapsed 26.489 s; received-cache MP followed at
   26.592 s. A separate native cast repeated 319 -> 219 at 29.841/29.944 s.
   Shell Mask was used for these MP controls, distinct from the Colon comparison.
4. Idle recovery: after battle, paired local/received MP increased by 4 at roughly
   three-second intervals once recovery resumed. The first MP session contains
   nine such changes. Its planned later normal-attack phase was not executed;
   a clarification marker records this. These rises must not be called AMPR.
5. Normal-attack recovery below maximum: target 14083's local HP fell at 53.410
   and 55.352 s. Received HP followed at 53.564 and 55.504 s; paired MP changed
   243 -> 256 -> 269. After the final local reduction/target loss, paired MP
   became 282 at 57.555 s. Three +13 received-MP steps align with this native
   attack sequence. Subsequent no-target recovery returned to +4 steps. This is
   a control for this setup, not a universal recovery formula or callback proof.

Successful normal hits in these recordings have approximately 0.10–0.155 s
prediction/received-cache convergence. That interval overlaps the earlier failed
range corrections. Timing alone therefore cannot distinguish acceptance from
reversion or identify a specific remote validation rule. The direction and
persistence of the observed state changes matter.

## Instrumentation findings and next work

IPC `now_mp` remained 319 in every valid observation, including while the typed
resource reader observed 219 and subsequent recovery. Existing IPC HP/MP are not
a demonstrated continuous combat sensor. The current mapping lacks a producer
timestamp, and no probe was issued during these runs. Do not substitute those
IPC values for the independently sampled resource fields.

The observer now guards capture by process lifetime and reports local/received
MP transitions separately. All 32 Python regressions pass; both Python files
compile. Gameplay source and canonical artifact hashes are unchanged by this
work. No enhanced-MP or extended-range implementation was established.

Next work is passive, executing-path evidence: correlate actual normal attack
and received-resource updates, and establish fresh timestamped callback/resource
observations. Per-sample distance, action identity and callback counters remain
missing. Do not repeat unused getter replacements, client-cache refills or the
unchanged failed range candidate based on this baseline. Remote rules and the
feasibility of the full requested effects remain unresolved.
