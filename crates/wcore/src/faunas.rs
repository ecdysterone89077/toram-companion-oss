//! Faunas (Android Toram 4.0.62) mechanics, ported to the Windows companion.
//!
//! # Why the mechanism changes
//!
//! Faunas patches raw ARM64 instruction bytes inside `libil2cpp.so` so a
//! managed method returns a fixed value without running its body. The Windows
//! build cannot be patched that way: the AppSign driver rejects writes to
//! code pages, so this port drives every mechanic through the managed
//! dispatch door instead — the `MethodInfo` pointer swap in `hook.rs`.
//!
//! Two patch shapes exist in the original:
//!
//! * **Prologue override** (`mov w0, #N ; ret`). The detour returns `N` and
//!   never calls the native body. Typed callbacks reproduce supported returns;
//!   Windows adapters retain native paths where a direct constant fails live
//!   checks. Casting uses a positive rate after zero caused unpaid-skill errors.
//! * **In-body constant rewrite**. A constant or branch inside a method body
//!   is edited. That is not expressible through the dispatch door, because
//!   the door replaces the whole method. Those mechanics are either already
//!   covered by an existing companion feature or are recorded as deviations
//!   in `specs/FAUNAS_PORT_20261022.md`.
//!
//! Windows callbacks preserve the exact argument and return types. Local status
//! callbacks bind pinned player instances; skill callbacks exclude other players.
//! OFF deactivates callbacks before restoring pointers, so in-flight calls forward.

#![allow(function_casts_as_integer, dead_code)]

use crate::chain::GameChain;
use crate::faunas_hooks as port;
use crate::features::{
    apply_hook_specs, cleanup_pending, disable_local_feature, feature_setup_result, note_partial,
    HookSpec, HookTargetClass,
};
use crate::hook::MethodHookEntry;
use crate::il2cpp::GcScope;
use crate::win::log_fmt;
use core::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicUsize, Ordering};
use std::sync::Mutex;

#[cfg(not(test))]
use protocol::telemetry;
#[cfg(test)]
fn telemetry() -> Option<&'static protocol::TelemetryWriter> {
    None
}

/// Number of ported Faunas features exposed as individual toggles.
pub const FEATURE_COUNT: usize = 24;

/// Toggle-bit base. Bits 0..=7 belong to the pre-existing companion features;
/// the ported Faunas features occupy bits 8..=31.
const BIT_BASE: u32 = 8;

const fn bit(index: usize) -> u32 {
    1u32 << (BIT_BASE as usize + index)
}

/// Forced-value slots. One slot per distinct value a detour has to read.
const SLOT_MOTION: usize = 0;
const SLOT_STABLE: usize = 1;
const SLOT_HIT: usize = 2;
const SLOT_PRORATE: usize = 3;
const SLOT_ELEMENT: usize = 4;
const SLOT_CRITDMG: usize = 5;
const SLOT_CRITMAGICDMG: usize = 6;
const SLOT_COUNT: usize = 7;

/// The rung each value ladder is on, packed eight bits per ladder.
static LADDER_RUNGS: AtomicU32 = AtomicU32::new(0);
/// Which value ladders have had a rung chosen.
static LADDER_ARMED: AtomicU32 = AtomicU32::new(0);

/// The value ladder a ladder feature draws its value from.
///
/// A ladder feature is a value, not a switch: its mechanic replaces a number,
/// so it has nothing to do until a rung has been chosen. The forced-value
/// slots start at zero, and zero is a usable value for some of them, so a
/// ladder that has never been given a rung must not install its hook.
fn ladder_kind(index: usize) -> Option<usize> {
    match index {
        14 => Some(0),
        15 => Some(3),
        16 => Some(1),
        17 => Some(2),
        _ => None,
    }
}

/// The forced-value slot, the feature index and the ladder of a value ladder.
fn ladder_parts(kind: usize) -> Option<(usize, usize, &'static [i32])> {
    match kind {
        0 => Some((SLOT_PRORATE, 14, &PRORATE_LADDER)),
        1 => Some((SLOT_CRITDMG, 16, &CRITDMG_LADDER)),
        2 => Some((SLOT_CRITMAGICDMG, 17, &CRITMAGICDMG_LADDER)),
        3 => Some((SLOT_ELEMENT, 15, &ELEMENT_LADDER)),
        _ => None,
    }
}

/// The rung a ladder is currently on.
fn ladder_rung(kind: usize) -> usize {
    ((LADDER_RUNGS.load(Ordering::Acquire) >> (kind * 8)) & 0xFF) as usize
}

/// Records the rung a ladder has been given, which also arms the ladder.
fn set_ladder_rung(kind: usize, rung: usize) {
    let mask = 0xFFu32 << (kind * 8);
    let mut current = LADDER_RUNGS.load(Ordering::Acquire);
    loop {
        let next = (current & !mask) | ((rung as u32 & 0xFF) << (kind * 8));
        match LADDER_RUNGS.compare_exchange_weak(current, next, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => break,
            Err(observed) => current = observed,
        }
    }
    LADDER_ARMED.fetch_or(1 << kind, Ordering::AcqRel);
}

/// Whether a ladder has been given a rung.
fn ladder_armed(kind: usize) -> bool {
    LADDER_ARMED.load(Ordering::Acquire) & (1 << kind) != 0
}

/// Whether a ladder's feature is currently on.
fn ladder_feature_active(kind: usize) -> bool {
    let Some((_, feature_index, _)) = ladder_parts(kind) else {
        return false;
    };
    bindings()
        .get(feature_index)
        .map(|binding| binding.active.load(Ordering::Acquire))
        .unwrap_or(false)
}

/// Sustain farming: the MP-cost rung index into `MP_COST_LADDER`.
static MP_COST_RUNG: AtomicUsize = AtomicUsize::new(0);

/// The MP-cost rung value currently applied (percent of native).
fn mp_cost_rung_value() -> i32 {
    MP_COST_LADDER[MP_COST_RUNG
        .load(Ordering::Acquire)
        .min(MP_COST_LADDER.len() - 1)]
}

/// Sustain farming: MP-cost feature index in the feature table.
const MP_COST_FEATURE_INDEX: usize = 22;

/// Steps the MP-cost ladder: ON from OFF reapplies the last rung, ON while ON
/// advances to the next rung (50 -> 25 -> 10 -> 50).
unsafe fn set_mp_cost_on() -> i32 {
    let active = MP_COST_ACTIVE.load(Ordering::Acquire);
    let rung = MP_COST_RUNG.load(Ordering::Acquire);
    let next = if active {
        (rung + 1) % MP_COST_LADDER.len()
    } else {
        rung.min(MP_COST_LADDER.len() - 1)
    };
    MP_COST_RUNG.store(next, Ordering::Release);
    port::set_value(MP_COST_FEATURE_INDEX, MP_COST_LADDER[next]);
    log_fmt(format_args!(
        "[faunas] mp_cost rung {} -> {}% of native",
        next + 1,
        MP_COST_LADDER[next]
    ));
    unsafe { set_feature(MP_COST_FEATURE_INDEX, true) }
}

/// Faunas' "Max Motion" value: `GetMotionSpeed` returns 60.
pub const MOTION_VALUE: i32 = 60;
/// Faunas' "Max Stability" value: `get_Stable` returns 1000.
pub const STABLE_VALUE: i32 = 1000;
/// Faunas' "Mob No Flee" value: `get_Hit` returns 65535.
pub const HIT_VALUE: i32 = 65535;
/// Windows zero-delay casts returned SkillNotCostPayed; keep a positive rate.
pub const CAST_RATE_PERCENT: i32 = 10;

/// Sustain farming: MP-cost percents of native, safest first.
///
/// The detour computes `native * pct / 100` with a floor of 1 and preserves
/// native zero-cost skills at zero, mirroring the Fast Cast positive-rate
/// rule. ON from OFF applies the last rung; ON while ON steps to the next
/// rung so He can probe 50 -> 25 -> 10 without new commands.
pub const MP_COST_LADDER: [i32; 3] = [50, 25, 10];

/// Faunas' proration ladder, decoded from the ARM64 `mov w0, #imm` it writes.
///
/// The original writes `ExpDefNormal` / `ExpDefSkill` / `ExpDefMagic` on both
/// `MobBattleStatus` and `BossMobBattleStatus` with one absolute constant, so
/// the mob's defence is *replaced*, not raised. The values are the menu's own
/// numbers, which is why they read as percentages.
pub const PRORATE_LADDER: [i32; 7] = [150, 200, 225, 250, 275, 300, 350];

/// Faunas' critical-damage ladder, decoded the same way.
///
/// These are absolute `get_CriticalDmg` values, so they replace the native
/// value outright. The native value is around 150 to 168 on this build.
pub const CRITDMG_LADDER: [i32; 10] = [225, 250, 300, 315, 325, 350, 375, 400, 425, 450];

/// Faunas' critical-magic-damage ladder, decoded the same way.
///
/// Rung 9 is `16` in the original: the Lua writes `000280D2`, which decodes
/// to `mov w0, #16` where the author meant 165. The typo is preserved so the
/// port matches the original byte for byte; it is a one-line change in this
/// table.
pub const CRITMAGICDMG_LADDER: [i32; 10] = [125, 130, 135, 140, 145, 150, 155, 160, 16, 170];

/// Faunas' element ladder: None, Fire, Aqua, Wind, Earth, Light, Dark.
pub const ELEMENT_LADDER: [i32; 7] = [0, 1, 2, 3, 4, 5, 6];

/// The element each rung of the element ladder selects, for the log.
pub const ELEMENT_NAMES: [&str; 7] = ["None", "Fire", "Aqua", "Wind", "Earth", "Light", "Dark"];

/// The value each forced-value slot currently holds.
static FORCED: [AtomicI32; SLOT_COUNT] = [
    AtomicI32::new(MOTION_VALUE),
    AtomicI32::new(STABLE_VALUE),
    AtomicI32::new(HIT_VALUE),
    AtomicI32::new(PRORATE_LADDER[0]),
    AtomicI32::new(ELEMENT_LADDER[0]),
    AtomicI32::new(0),
    AtomicI32::new(0),
];

/// Skill action classes whose `CheckRangeHit` Faunas forces to true.
const SKILL_AOE_CLASSES: [&str; 16] = [
    "MeteoBreakerAction",
    "AuraBladeAction",
    "CrossFireAction",
    "SatelliteArrowAction",
    "MagicWallAction",
    "MagicStormAction",
    "MagicFinawAction",
    "MagicCannonAction",
    "MagicFallAction",
    "DiveImpactAction",
    "DimensionTillAction",
    "DragonicChargeAction",
    "LunaDitherStarAction",
    "TwinBusterBladeAction",
    "NemesisAction",
    "SoulHuntAction",
];

/// Skill action classes whose `CheckRangeHit` Faunas forces to false.
const SKILL_NO_AOE_CLASSES: [&str; 2] = ["MagicImpactAction", "AirSlideAction"];

/// Skill action classes whose `NextRangeHit` Faunas replaces with an
/// immediate return, which skips the native body.
const SKILL_BURST_CLASSES: [&str; 14] = [
    "MeteoBreakerAction",
    "AuraBladeAction",
    "CrossFireAction",
    "SatelliteArrowAction",
    "MagicWallAction",
    "MagicStormAction",
    "MagicFinawAction",
    "MagicCannonAction",
    "MagicFallAction",
    "DiveImpactAction",
    "DimensionTillAction",
    "DragonicChargeAction",
    "LunaDitherStarAction",
    "TwinBusterBladeAction",
];

/// A ported Faunas feature.
#[derive(Clone, Copy)]
pub struct FaunasFeature {
    /// Stable machine name, also the CLI keyword.
    pub id: &'static str,
    /// Human label for the companion window.
    pub label: &'static str,
    /// Telemetry toggle bit.
    pub bit: u32,
    /// Whether the feature is resolvable in the installed metadata.
    pub available: bool,
}

const fn feature(
    index: usize,
    id: &'static str,
    label: &'static str,
    available: bool,
) -> FaunasFeature {
    FaunasFeature {
        id,
        label,
        bit: bit(index),
        available,
    }
}

/// The ported feature table. The order fixes the command band and the
/// telemetry bit, so entries are only ever appended.
pub static FEATURES: [FaunasFeature; FEATURE_COUNT] = [
    feature(0, "crit", "Always Critical", true),
    feature(1, "motion", "Max Motion", true),
    feature(2, "stable", "Max Stability", true),
    feature(3, "cspd", "Fast Cast", true),
    feature(4, "range", "Long Range Attack", true),
    feature(5, "penet", "Always Pierce", true),
    feature(6, "resist", "Disable Resist", true),
    feature(7, "mob_no_flee", "Mob No Flee", true),
    feature(8, "mob_no_guard", "Mob No Guard / Evade", true),
    feature(9, "mob_no_invincible", "Mob No Invincible", true),
    feature(10, "mob_no_move", "Mob No Move", true),
    feature(11, "skill_aoe", "Skill Full AOE", true),
    feature(12, "skill_no_aoe", "Skill No AOE", true),
    feature(13, "skill_burst", "Skill Burst", true),
    feature(14, "prorate", "Mob Proration", true),
    feature(15, "element", "Equip Element", true),
    feature(16, "critdmg", "Critical Damage", true),
    feature(17, "critmgcdmg", "Critical Magic Damage", true),
    feature(18, "skip_battle", "Skip Battle", true),
    feature(19, "no_domination", "No Domination Bonus", true),
    feature(20, "no_drop_visual", "No Drop Visual", true),
    feature(21, "one_click", "One-Click Loadout", true),
    feature(22, "mp_cost", "MP Cost 10%", true),
    feature(23, "skill_delay", "Skill Delay Override", true),
];

/// Hook storage per feature. The size is the largest table the module needs.
const HOOK_CAPACITY: usize = 20;

type HookTable = Mutex<[MethodHookEntry; HOOK_CAPACITY]>;

macro_rules! hook_table {
    ($name:ident) => {
        static $name: HookTable = Mutex::new([MethodHookEntry::new(); HOOK_CAPACITY]);
    };
}

hook_table!(CRIT_HOOKS);
hook_table!(MOTION_HOOKS);
hook_table!(STABLE_HOOKS);
hook_table!(CSPD_HOOKS);
hook_table!(RANGE_HOOKS);
hook_table!(PENET_HOOKS);
hook_table!(RESIST_HOOKS);
hook_table!(MOB_FLEE_HOOKS);
hook_table!(MOB_GUARD_HOOKS);
hook_table!(MOB_INVINCIBLE_HOOKS);
hook_table!(MOB_MOVE_HOOKS);
hook_table!(SKILL_AOE_HOOKS);
hook_table!(SKILL_NO_AOE_HOOKS);
hook_table!(SKILL_BURST_HOOKS);
hook_table!(PRORATE_HOOKS);
hook_table!(ELEMENT_HOOKS);
hook_table!(CRITDMG_HOOKS);
hook_table!(CRITMAGICDMG_HOOKS);
hook_table!(SKIP_BATTLE_HOOKS);
hook_table!(NO_DOMINATION_HOOKS);
hook_table!(NO_DROP_HOOKS);
hook_table!(MP_COST_HOOKS);
hook_table!(SKILL_DELAY_HOOKS);
// The one-click preset owns no hooks of its own, but the binding table is
// indexed by the feature index, so it needs a slot.
hook_table!(ONE_CLICK_HOOKS);

static CRIT_ACTIVE: AtomicBool = AtomicBool::new(false);
static MOTION_ACTIVE: AtomicBool = AtomicBool::new(false);
static STABLE_ACTIVE: AtomicBool = AtomicBool::new(false);
static CSPD_ACTIVE: AtomicBool = AtomicBool::new(false);
static RANGE_ACTIVE: AtomicBool = AtomicBool::new(false);
static PENET_ACTIVE: AtomicBool = AtomicBool::new(false);
static RESIST_ACTIVE: AtomicBool = AtomicBool::new(false);
static MOB_FLEE_ACTIVE: AtomicBool = AtomicBool::new(false);
static MOB_GUARD_ACTIVE: AtomicBool = AtomicBool::new(false);
static MOB_INVINCIBLE_ACTIVE: AtomicBool = AtomicBool::new(false);
static MOB_MOVE_ACTIVE: AtomicBool = AtomicBool::new(false);
static SKILL_AOE_ACTIVE: AtomicBool = AtomicBool::new(false);
static SKILL_NO_AOE_ACTIVE: AtomicBool = AtomicBool::new(false);
static SKILL_BURST_ACTIVE: AtomicBool = AtomicBool::new(false);
static PRORATE_ACTIVE: AtomicBool = AtomicBool::new(false);
static ELEMENT_ACTIVE: AtomicBool = AtomicBool::new(false);
static CRITDMG_ACTIVE: AtomicBool = AtomicBool::new(false);
static CRITMAGICDMG_ACTIVE: AtomicBool = AtomicBool::new(false);
static SKIP_BATTLE_ACTIVE: AtomicBool = AtomicBool::new(false);
static NO_DOMINATION_ACTIVE: AtomicBool = AtomicBool::new(false);
static NO_DROP_ACTIVE: AtomicBool = AtomicBool::new(false);
static MP_COST_ACTIVE: AtomicBool = AtomicBool::new(false);
static SKILL_DELAY_ACTIVE: AtomicBool = AtomicBool::new(false);
static ONE_CLICK_ACTIVE: AtomicBool = AtomicBool::new(false);

// ---------------------------------------------------------------------------
// Detours
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Hook tables
// ---------------------------------------------------------------------------

/// The three Android checks are direct native calls in Windows. The normal-
/// attack adapter routes the local calculator through the verified caller scope.
/// Native critical resistance remains in that path; magic still needs live proof.
fn crit_specs() -> [HookSpec; 7] {
    [
        HookSpec {
            target: HookTargetClass::PlayerAttackBase,
            method: "checkCriticalPercent",
            argc: 2,
            detour: port::crit::<0> as usize,
            orig_sink: Some(&port::CALLBACKS[0].original),
        },
        HookSpec {
            target: HookTargetClass::PlayerAttackBase,
            method: "CheckMagicCritical",
            argc: 2,
            detour: port::magic_crit::<1> as usize,
            orig_sink: Some(&port::CALLBACKS[1].original),
        },
        HookSpec {
            target: HookTargetClass::PlayerAttackBase,
            method: "CheckMagicCritical",
            argc: 3,
            detour: port::magic_crit_base::<66> as usize,
            orig_sink: Some(&port::CALLBACKS[66].original),
        },
        HookSpec {
            target: HookTargetClass::PlayerBattleStatus,
            method: "get_Critical",
            argc: 0,
            detour: port::critical_rate::<68> as usize,
            orig_sink: Some(&port::CALLBACKS[68].original),
        },
        HookSpec {
            target: HookTargetClass::PlayerSecondaryStatus,
            method: "get_Critical",
            argc: 0,
            detour: port::critical_rate::<69> as usize,
            orig_sink: Some(&port::CALLBACKS[69].original),
        },
        HookSpec {
            target: HookTargetClass::NormalAttackAction,
            method: "calcPlayerToMobDamage",
            argc: 2,
            detour: port::normal_critical::<70> as usize,
            orig_sink: Some(&port::CALLBACKS[70].original),
        },
        HookSpec {
            target: HookTargetClass::PlayerStatus,
            method: "get_SecondaryStatus",
            argc: 0,
            detour: crate::critical_path::secondary_getter_shim as usize,
            orig_sink: Some(&crate::critical_path::ORIG_SECONDARY_GETTER),
        },
    ]
}

/// `motion`: `PlayerBattleStatus.GetMotionSpeed` returns 60.
fn motion_specs() -> [HookSpec; 2] {
    [
        HookSpec {
            target: HookTargetClass::PlayerBattleStatus,
            method: "GetMotionSpeed",
            argc: 1,
            detour: port::int1::<2> as usize,
            orig_sink: Some(&port::CALLBACKS[2].original),
        },
        HookSpec {
            target: HookTargetClass::PlayerSecondaryStatus,
            method: "GetMotionSpeed",
            argc: 1,
            detour: port::int1::<3> as usize,
            orig_sink: Some(&port::CALLBACKS[3].original),
        },
    ]
}

/// `stable`: `PlayerBattleStatus.get_Stable` returns 1000.
fn stable_specs() -> [HookSpec; 2] {
    [
        HookSpec {
            target: HookTargetClass::PlayerBattleStatus,
            method: "get_Stable",
            argc: 0,
            detour: port::int0::<4> as usize,
            orig_sink: Some(&port::CALLBACKS[4].original),
        },
        HookSpec {
            target: HookTargetClass::PlayerSecondaryStatus,
            method: "get_Stable",
            argc: 0,
            detour: port::int0::<5> as usize,
            orig_sink: Some(&port::CALLBACKS[5].original),
        },
    ]
}

/// Keep native calculation and cap its positive first cast-rate percentage.
fn cspd_specs() -> [HookSpec; 2] {
    [
        HookSpec {
            target: HookTargetClass::PlayerBattleStatus,
            method: "GetSkillDelayRate1",
            argc: 1,
            detour: port::cast_rate::<6> as usize,
            orig_sink: Some(&port::CALLBACKS[6].original),
        },
        HookSpec {
            target: HookTargetClass::PlayerSecondaryStatus,
            method: "GetSkillDelayRate1",
            argc: 1,
            detour: port::cast_rate::<7> as usize,
            orig_sink: Some(&port::CALLBACKS[7].original),
        },
    ]
}

/// Android `RangeHit` returns true; `lngrange` returns enemy size 62.0f.
fn range_specs() -> [HookSpec; 2] {
    [
        HookSpec {
            target: HookTargetClass::BattleManagerBase,
            method: "CheckActionRange",
            argc: 0,
            detour: port::bool0::<8> as usize,
            orig_sink: Some(&port::CALLBACKS[8].original),
        },
        HookSpec {
            target: HookTargetClass::Named("EnemyMobActionManagerBase"),
            method: "get_Size",
            argc: 0,
            detour: port::enemy_size::<67> as usize,
            orig_sink: Some(&port::CALLBACKS[67].original),
        },
    ]
}

/// `penet`: both resistance-damage calculators return 0.
fn penet_specs() -> [HookSpec; 2] {
    [
        HookSpec {
            target: HookTargetClass::PlayerAttackBase,
            method: "CalcPowerResistDamage",
            argc: 3,
            detour: port::resist_damage::<9> as usize,
            orig_sink: Some(&port::CALLBACKS[9].original),
        },
        HookSpec {
            target: HookTargetClass::PlayerAttackBase,
            method: "CalcMagicResistDamage",
            argc: 3,
            detour: port::resist_damage::<10> as usize,
            orig_sink: Some(&port::CALLBACKS[10].original),
        },
    ]
}

/// `resist`: the two abnormal-resistance label calls are skipped, so the
/// resistance popup never appears.
fn resist_specs() -> [HookSpec; 2] {
    [
        HookSpec {
            target: HookTargetClass::Named("UI3DLabelManager"),
            method: "AbnormalPopLabelPlayerToEnemy",
            argc: 3,
            detour: port::label::<11> as usize,
            orig_sink: Some(&port::CALLBACKS[11].original),
        },
        HookSpec {
            target: HookTargetClass::Named("UI3DLabelManager"),
            method: "AbnormalResistPopUpLabel",
            argc: 4,
            detour: port::resist_label::<12> as usize,
            orig_sink: Some(&port::CALLBACKS[12].original),
        },
    ]
}

/// `mob_no_flee`: `PlayerBattleStatus.get_Hit` returns 65535.
fn mob_flee_specs() -> [HookSpec; 2] {
    [
        HookSpec {
            target: HookTargetClass::PlayerBattleStatus,
            method: "get_Hit",
            argc: 0,
            detour: port::int0::<13> as usize,
            orig_sink: Some(&port::CALLBACKS[13].original),
        },
        HookSpec {
            target: HookTargetClass::PlayerSecondaryStatus,
            method: "get_Hit",
            argc: 0,
            detour: port::int0::<14> as usize,
            orig_sink: Some(&port::CALLBACKS[14].original),
        },
    ]
}

/// `mob_no_guard`: `PlayerAttackBase.checkMobReaction` returns the zero
/// reaction, so the mob never guards and never evades.
fn mob_guard_specs() -> [HookSpec; 1] {
    [HookSpec {
        target: HookTargetClass::PlayerAttackBase,
        method: "checkMobReaction",
        argc: 2,
        detour: port::reaction::<15> as usize,
        orig_sink: Some(&port::CALLBACKS[15].original),
    }]
}

/// `mob_no_invincible`: `EnemyMobActionManagerBase.get_SystemInvincible` is false.
fn mob_invincible_specs() -> [HookSpec; 1] {
    [HookSpec {
        target: HookTargetClass::Named("EnemyMobActionManagerBase"),
        method: "get_SystemInvincible",
        argc: 0,
        detour: port::bool0::<16> as usize,
        orig_sink: Some(&port::CALLBACKS[16].original),
    }]
}

/// `mob_no_move`: the mob action is never created and the mob's own range
/// reads zero.
fn mob_move_specs() -> [HookSpec; 2] {
    [
        HookSpec {
            target: HookTargetClass::Named("MobBattleSystemManager"),
            method: "CreateMobAction",
            argc: 1,
            detour: port::create_action::<17> as usize,
            orig_sink: Some(&port::CALLBACKS[17].original),
        },
        HookSpec {
            target: HookTargetClass::Named("MobActionPattern"),
            method: "get_Range",
            argc: 0,
            detour: port::int0::<18> as usize,
            orig_sink: Some(&port::CALLBACKS[18].original),
        },
    ]
}

/// `skill_aoe`: every listed skill's range check passes, so the skill lands on
/// every mob the scene holds.
fn skill_aoe_specs() -> Vec<HookSpec> {
    let detours = [
        port::range::<19> as usize,
        port::range::<20> as usize,
        port::range::<21> as usize,
        port::range::<22> as usize,
        port::range::<23> as usize,
        port::range::<24> as usize,
        port::range::<25> as usize,
        port::range::<26> as usize,
        port::range::<27> as usize,
        port::range::<28> as usize,
        port::range::<29> as usize,
        port::range::<30> as usize,
        port::range::<31> as usize,
        port::range::<32> as usize,
        port::range::<33> as usize,
        port::range::<34> as usize,
    ];
    SKILL_AOE_CLASSES
        .iter()
        .enumerate()
        .map(|(index, class)| HookSpec {
            target: HookTargetClass::Named(class),
            method: "CheckRangeHit",
            argc: 3,
            detour: detours[index],
            orig_sink: Some(&port::CALLBACKS[19 + index].original),
        })
        .collect()
}

/// `skill_no_aoe`: the two skills Faunas forces to fail their range check.
fn skill_no_aoe_specs() -> Vec<HookSpec> {
    let detours = [port::range::<35> as usize, port::range::<36> as usize];
    SKILL_NO_AOE_CLASSES
        .iter()
        .enumerate()
        .map(|(index, class)| HookSpec {
            target: HookTargetClass::Named(class),
            method: "CheckRangeHit",
            argc: 3,
            detour: detours[index],
            orig_sink: Some(&port::CALLBACKS[35 + index].original),
        })
        .collect()
}

/// `skill_burst`: the chain step is skipped, exactly as Faunas' immediate
/// return does.
fn skill_burst_specs() -> Vec<HookSpec> {
    let detours = [
        port::skip::<37> as usize,
        port::skip::<38> as usize,
        port::skip::<39> as usize,
        port::skip::<40> as usize,
        port::skip::<41> as usize,
        port::skip::<42> as usize,
        port::skip::<43> as usize,
        port::skip::<44> as usize,
        port::skip::<45> as usize,
        port::skip::<46> as usize,
        port::skip::<47> as usize,
        port::skip::<48> as usize,
        port::skip::<49> as usize,
        port::skip::<50> as usize,
    ];
    SKILL_BURST_CLASSES
        .iter()
        .enumerate()
        .map(|(index, class)| HookSpec {
            target: HookTargetClass::Named(class),
            method: "NextRangeHit",
            argc: 0,
            detour: detours[index],
            orig_sink: Some(&port::CALLBACKS[37 + index].original),
        })
        .collect()
}

/// `prorate`: the mob's physical, skill and magic defence are replaced on
/// both the normal and the boss status.
fn prorate_specs() -> Vec<HookSpec> {
    let mut specs = Vec::new();
    for class in ["MobBattleStatus", "BossMobBattleStatus"] {
        for method in ["get_ExpDefNormal", "get_ExpDefSkill", "get_ExpDefMagic"] {
            let slot = specs.len();
            let detour = match slot {
                0 => port::int0::<51> as usize,
                1 => port::int0::<52> as usize,
                2 => port::int0::<53> as usize,
                3 => port::int0::<54> as usize,
                4 => port::int0::<55> as usize,
                5 => port::int0::<56> as usize,
                _ => unreachable!(),
            };
            specs.push(HookSpec {
                target: HookTargetClass::Named(class),
                method,
                argc: 0,
                detour,
                orig_sink: Some(&port::CALLBACKS[51 + slot].original),
            });
        }
    }
    specs
}

/// `element`: the equipped element is replaced.
///
/// Faunas patches `GetEquipElement` on the player status. On this build that
/// method is declared by `PlayerStatusBase`; `PlayerStatus` derives from it
/// and never overrides it, so the base declaration is the one to replace.
/// `PlayerBattleStatus` is a different hierarchy entirely — it implements
/// `IPlayerStatusCalculator` — and does not carry the method at all.
fn element_specs() -> [HookSpec; 1] {
    [HookSpec {
        target: HookTargetClass::PlayerStatusBase,
        method: "GetEquipElement",
        argc: 0,
        detour: port::int0::<57> as usize,
        orig_sink: Some(&port::CALLBACKS[57].original),
    }]
}

/// `critdmg`: the absolute critical damage value.
///
/// Faunas patches `get_CriticalDmg` so the method returns a fixed value
/// instead of running its body. Always-critical replaces the same two
/// declarations, and a `MethodInfo` has exactly one owner, so this feature
/// shares that owner instead of installing a second detour: the shared detour
/// reads the ladder value while this feature is on and always-critical's own
/// value path while it is not.
pub(crate) fn critdmg_specs() -> [HookSpec; 2] {
    [
        HookSpec {
            target: HookTargetClass::PlayerBattleStatus,
            method: "get_CriticalDmg",
            argc: 0,
            detour: crate::features::hk_pbs_crit_damage as usize,
            orig_sink: Some(&crate::features::ORIG_PBS_CRIT_DMG),
        },
        HookSpec {
            target: HookTargetClass::PlayerSecondaryStatus,
            method: "get_CriticalDmg",
            argc: 0,
            detour: crate::features::hk_pss_crit_damage as usize,
            orig_sink: Some(&crate::features::ORIG_PSS_CRIT_DMG),
        },
    ]
}

/// The absolute critical damage the ladder currently forces.
///
/// `None` while the feature is off, or while the ladder has never been given
/// a rung, which is what lets always-critical's own value path run instead.
pub(crate) fn critdmg_forced_value(instance: *mut core::ffi::c_void) -> Option<i32> {
    if !CRITDMG_ACTIVE.load(Ordering::Acquire) || !port::critdmg_matches(instance) {
        return None;
    }
    if !ladder_armed(1) {
        return None;
    }
    Some(FORCED[SLOT_CRITDMG].load(Ordering::Acquire))
}

/// `critmgcdmg`: the absolute critical magic damage value.
fn critmgcdmg_specs() -> [HookSpec; 2] {
    [
        HookSpec {
            target: HookTargetClass::PlayerBattleStatus,
            method: "get_CriticalMagicDmg",
            argc: 0,
            detour: port::int0::<58> as usize,
            orig_sink: Some(&port::CALLBACKS[58].original),
        },
        HookSpec {
            target: HookTargetClass::PlayerSecondaryStatus,
            method: "get_CriticalMagicDmg",
            argc: 0,
            detour: port::int0::<59> as usize,
            orig_sink: Some(&port::CALLBACKS[59].original),
        },
    ]
}

/// `skip_battle`: the battle always reports itself finished.
fn skip_battle_specs() -> [HookSpec; 1] {
    [HookSpec {
        target: HookTargetClass::Named("RoomManager"),
        method: "get_IsBattleEnd",
        argc: 0,
        detour: port::bool0::<60> as usize,
        orig_sink: Some(&port::CALLBACKS[60].original),
    }]
}

/// `no_domination`: the domination bonus gauge is never written.
fn no_domination_specs() -> [HookSpec; 2] {
    [
        HookSpec {
            target: HookTargetClass::Named("PopAreaBonusSender"),
            method: "SetBonusGauge",
            argc: 1,
            detour: port::gauge::<61> as usize,
            orig_sink: Some(&port::CALLBACKS[61].original),
        },
        HookSpec {
            target: HookTargetClass::Named("PopAreaBonusSender"),
            method: "SetBonusGauge",
            argc: 2,
            detour: port::gauge2::<62> as usize,
            orig_sink: Some(&port::CALLBACKS[62].original),
        },
    ]
}

/// Android `rop`: EffectPlayer.PlayDropBox returns zero without creating a visual.
fn no_drop_specs() -> [HookSpec; 1] {
    [HookSpec {
        target: HookTargetClass::Named("EffectPlayer"),
        method: "PlayDropBox",
        argc: 1,
        detour: port::drop_box::<63> as usize,
        orig_sink: Some(&port::CALLBACKS[63].original),
    }]
}

/// Sustain `mp_cost`: `PlayerAttackBase.CalcCostMp` shrinks to a percent of
/// native (ladder 50 -> 25 -> 10), floor 1, native zero preserved.
fn mp_cost_specs() -> [HookSpec; 1] {
    [HookSpec {
        target: HookTargetClass::PlayerAttackBase,
        method: "CalcCostMp",
        argc: 1,
        detour: port::mp_cost::<64> as usize,
        orig_sink: Some(&port::CALLBACKS[64].original),
    }]
}

/// Sustain `skill_delay`: `SkillDelayManager.CheckSkillAvailable` returns true
/// while ON (experimental, server reuse-permission still gates acceptance).
/// The server gate is `OnSkillDelayStandbyOk(SkillReusePermissioneEvent)`,
/// which this client gate cannot waive — a live smoke decides.
fn skill_delay_specs() -> [HookSpec; 1] {
    [HookSpec {
        target: HookTargetClass::Named("SkillDelayManager"),
        method: "CheckSkillAvailable",
        argc: 1,
        detour: port::skill_available::<65> as usize,
        orig_sink: Some(&port::CALLBACKS[65].original),
    }]
}

// The former gacha/map gates were removed: the Android offsets never proved a
// gacha or map mechanism on this build. Slots 22/23 now carry sustain farming
// (mp_cost + skill_delay) with verified PC_v226 targets.

// ---------------------------------------------------------------------------
// Feature dispatch
// ---------------------------------------------------------------------------

/// The index of the one-click preset inside the feature table.
const ONE_CLICK_INDEX: usize = 21;

/// The set of features the one-click loadout applies.
///
/// This is Faunas' "One Clik" preset, reduced to the mechanics this port can
/// express. Faunas' preset also turns on the Magic Fall AOE and burst, the
/// long-range skill constant and the animation constant; the AOE and burst
/// groups are left out of the preset on purpose, because the skill AOE group
/// is the one mechanic in this port with recorded map-wide desync, and the
/// other two are in-body constant rewrites this port does not express.
const ONE_CLICK: [usize; 6] = [0, 2, 4, 9, 10, 18];

/// A feature's hook table, activation flag and label.
struct FeatureBinding {
    id: &'static str,
    label: &'static str,
    bit: u32,
    table: &'static HookTable,
    active: &'static AtomicBool,
}

fn bindings() -> [FeatureBinding; FEATURE_COUNT] {
    [
        FeatureBinding {
            id: "crit",
            label: "Always Critical",
            bit: bit(0),
            table: &CRIT_HOOKS,
            active: &CRIT_ACTIVE,
        },
        FeatureBinding {
            id: "motion",
            label: "Max Motion",
            bit: bit(1),
            table: &MOTION_HOOKS,
            active: &MOTION_ACTIVE,
        },
        FeatureBinding {
            id: "stable",
            label: "Max Stability",
            bit: bit(2),
            table: &STABLE_HOOKS,
            active: &STABLE_ACTIVE,
        },
        FeatureBinding {
            id: "cspd",
            label: "Fast Cast",
            bit: bit(3),
            table: &CSPD_HOOKS,
            active: &CSPD_ACTIVE,
        },
        FeatureBinding {
            id: "range",
            label: "Long Range Attack",
            bit: bit(4),
            table: &RANGE_HOOKS,
            active: &RANGE_ACTIVE,
        },
        FeatureBinding {
            id: "penet",
            label: "Always Pierce",
            bit: bit(5),
            table: &PENET_HOOKS,
            active: &PENET_ACTIVE,
        },
        FeatureBinding {
            id: "resist",
            label: "Disable Resist",
            bit: bit(6),
            table: &RESIST_HOOKS,
            active: &RESIST_ACTIVE,
        },
        FeatureBinding {
            id: "mob_no_flee",
            label: "Mob No Flee",
            bit: bit(7),
            table: &MOB_FLEE_HOOKS,
            active: &MOB_FLEE_ACTIVE,
        },
        FeatureBinding {
            id: "mob_no_guard",
            label: "Mob No Guard / Evade",
            bit: bit(8),
            table: &MOB_GUARD_HOOKS,
            active: &MOB_GUARD_ACTIVE,
        },
        FeatureBinding {
            id: "mob_no_invincible",
            label: "Mob No Invincible",
            bit: bit(9),
            table: &MOB_INVINCIBLE_HOOKS,
            active: &MOB_INVINCIBLE_ACTIVE,
        },
        FeatureBinding {
            id: "mob_no_move",
            label: "Mob No Move",
            bit: bit(10),
            table: &MOB_MOVE_HOOKS,
            active: &MOB_MOVE_ACTIVE,
        },
        FeatureBinding {
            id: "skill_aoe",
            label: "Skill Full AOE",
            bit: bit(11),
            table: &SKILL_AOE_HOOKS,
            active: &SKILL_AOE_ACTIVE,
        },
        FeatureBinding {
            id: "skill_no_aoe",
            label: "Skill No AOE",
            bit: bit(12),
            table: &SKILL_NO_AOE_HOOKS,
            active: &SKILL_NO_AOE_ACTIVE,
        },
        FeatureBinding {
            id: "skill_burst",
            label: "Skill Burst",
            bit: bit(13),
            table: &SKILL_BURST_HOOKS,
            active: &SKILL_BURST_ACTIVE,
        },
        FeatureBinding {
            id: "prorate",
            label: "Mob Proration",
            bit: bit(14),
            table: &PRORATE_HOOKS,
            active: &PRORATE_ACTIVE,
        },
        FeatureBinding {
            id: "element",
            label: "Equip Element",
            bit: bit(15),
            table: &ELEMENT_HOOKS,
            active: &ELEMENT_ACTIVE,
        },
        FeatureBinding {
            id: "critdmg",
            label: "Critical Damage",
            bit: bit(16),
            table: &CRITDMG_HOOKS,
            active: &CRITDMG_ACTIVE,
        },
        FeatureBinding {
            id: "critmgcdmg",
            label: "Critical Magic Damage",
            bit: bit(17),
            table: &CRITMAGICDMG_HOOKS,
            active: &CRITMAGICDMG_ACTIVE,
        },
        FeatureBinding {
            id: "skip_battle",
            label: "Skip Battle",
            bit: bit(18),
            table: &SKIP_BATTLE_HOOKS,
            active: &SKIP_BATTLE_ACTIVE,
        },
        FeatureBinding {
            id: "no_domination",
            label: "No Domination Bonus",
            bit: bit(19),
            table: &NO_DOMINATION_HOOKS,
            active: &NO_DOMINATION_ACTIVE,
        },
        FeatureBinding {
            id: "no_drop_visual",
            label: "No Drop Visual",
            bit: bit(20),
            table: &NO_DROP_HOOKS,
            active: &NO_DROP_ACTIVE,
        },
        // The one-click preset owns no hooks; it is routed to its own handler
        // before the binding table is consulted.
        FeatureBinding {
            id: "one_click",
            label: "One-Click Loadout",
            bit: bit(21),
            table: &ONE_CLICK_HOOKS,
            active: &ONE_CLICK_ACTIVE,
        },
        FeatureBinding {
            id: "mp_cost",
            label: "MP Cost 10%",
            bit: bit(22),
            table: &MP_COST_HOOKS,
            active: &MP_COST_ACTIVE,
        },
        FeatureBinding {
            id: "skill_delay",
            label: "Skill Delay Override",
            bit: bit(23),
            table: &SKILL_DELAY_HOOKS,
            active: &SKILL_DELAY_ACTIVE,
        },
    ]
}

/// The hook specs for one feature, or `None` when the feature has no specs.
fn specs_for(index: usize) -> Option<Vec<HookSpec>> {
    Some(match index {
        0 => crit_specs().to_vec(),
        1 => motion_specs().to_vec(),
        2 => stable_specs().to_vec(),
        3 => cspd_specs().to_vec(),
        4 => range_specs().to_vec(),
        5 => penet_specs().to_vec(),
        6 => resist_specs().to_vec(),
        7 => mob_flee_specs().to_vec(),
        8 => mob_guard_specs().to_vec(),
        9 => mob_invincible_specs().to_vec(),
        10 => mob_move_specs().to_vec(),
        11 => skill_aoe_specs(),
        12 => skill_no_aoe_specs(),
        13 => skill_burst_specs(),
        14 => prorate_specs(),
        15 => element_specs().to_vec(),
        16 => critdmg_specs().to_vec(),
        17 => critmgcdmg_specs().to_vec(),
        18 => skip_battle_specs().to_vec(),
        19 => no_domination_specs().to_vec(),
        20 => no_drop_specs().to_vec(),
        22 => mp_cost_specs().to_vec(),
        23 => skill_delay_specs().to_vec(),
        _ => return None,
    })
}

pub(crate) fn feature_bit_for_id(id: &str) -> u32 {
    FEATURES
        .iter()
        .find(|feature| feature.id == id)
        .map_or(0, |feature| feature.bit)
}

fn forced_value(index: usize) -> i32 {
    // Sustain mp_cost reads its rung helper here instead of a `=>` arm so the
    // target audit's `specs_for` map (which scans `N => name(`) keeps pointing
    // index 22 at `mp_cost_specs`.
    if index == 22 {
        return mp_cost_rung_value();
    }
    match index {
        0 | 4 | 11 | 18 => 1,
        1 => MOTION_VALUE,
        2 => STABLE_VALUE,
        3 => CAST_RATE_PERCENT,
        7 => HIT_VALUE,
        23 => 1,
        14 => FORCED[SLOT_PRORATE].load(Ordering::Acquire),
        15 => FORCED[SLOT_ELEMENT].load(Ordering::Acquire),
        17 => FORCED[SLOT_CRITMAGICDMG].load(Ordering::Acquire),
        _ => 0,
    }
}

/// The critical-damage ladder's index in the feature table.
const CRITDMG_FEATURE_INDEX: usize = 16;

/// Feature indices whose values win over always-critical's sidecars.
///
/// `stable` (2) takes `PlayerBattleStatus.get_Stable` and `mob_no_flee` (7)
/// takes `PlayerBattleStatus.get_Hit`; always-critical yields both while
/// these features are on and reclaims them after.
const STABLE_FEATURE_INDEX: usize = 2;
const MOB_FLEE_FEATURE_INDEX: usize = 7;

/// Whether the port currently owns a hook the companion also claims.
///
/// Only the overlaps where the port's value wins by design are listed:
/// stable's `get_Stable`, mob_no_flee's `get_Hit`, motion's `GetMotionSpeed`.
/// Anything else refused is genuinely missing, so the caller keeps failing
/// closed on it.
pub(crate) fn port_owns_hook(target: HookTargetClass, method: &str) -> bool {
    match (target, method) {
        (
            HookTargetClass::PlayerBattleStatus | HookTargetClass::PlayerSecondaryStatus,
            "get_Stable",
        ) => STABLE_ACTIVE.load(Ordering::Acquire),
        (
            HookTargetClass::PlayerBattleStatus | HookTargetClass::PlayerSecondaryStatus,
            "get_Hit",
        ) => MOB_FLEE_ACTIVE.load(Ordering::Acquire),
        (
            HookTargetClass::PlayerBattleStatus | HookTargetClass::PlayerSecondaryStatus,
            "GetMotionSpeed",
        ) => MOTION_ACTIVE.load(Ordering::Acquire),
        _ => false,
    }
}

pub(crate) fn critical_active() -> bool {
    CRIT_ACTIVE.load(Ordering::Acquire)
}

/// Installs or removes the critical-damage ladder.
///
/// The ladder shares the `get_CriticalDmg` hooks with always-critical, so it
/// takes a share of that owner instead of installing hooks of its own.
unsafe fn set_critdmg_feature(enabled: bool) -> i32 {
    let feature_bit = bit(CRITDMG_FEATURE_INDEX);
    if CRITDMG_ACTIVE.load(Ordering::Acquire) == enabled && !cleanup_pending(feature_bit) {
        return feature_setup_result(feature_bit, enabled);
    }
    if !enabled {
        // The ladder stops forcing a value before the hooks go, so a reader
        // already inside the detour never sees a stale absolute value.
        CRITDMG_ACTIVE.store(false, Ordering::Release);
        port::deactivate(CRITDMG_FEATURE_INDEX);
        let result = unsafe {
            disable_local_feature(&CRITDMG_HOOKS, &CRITDMG_ACTIVE, &[], feature_bit, "critdmg")
        };
        if result != 0 {
            return result;
        }
        if !crate::features::crit_dmg_release(crate::features::CRIT_DMG_OWNER_FAUNAS) {
            crate::features::mark_cleanup_pending(feature_bit);
            note_partial(feature_bit, true);
            return -8;
        }
        port::release(CRITDMG_FEATURE_INDEX);
        return 0;
    }
    if cleanup_pending(feature_bit) {
        log_fmt(format_args!(
            "[faunas] critdmg is waiting for a completed restore; retry ON after the OFF finishes"
        ));
        return -8;
    }
    let mut gc = GcScope::new();
    let mut ch = GameChain::new();
    if !unsafe { ch.resolve(&mut gc) } {
        return -1;
    }
    if !unsafe { port::prepare_critdmg(&ch) } {
        return -8;
    }
    if !unsafe { crate::features::crit_dmg_acquire(crate::features::CRIT_DMG_OWNER_FAUNAS, &ch) } {
        port::release(CRITDMG_FEATURE_INDEX);
        log_fmt(format_args!(
            "[faunas] critdmg rolled back after a failed install"
        ));
        return -8;
    }
    CRITDMG_ACTIVE.store(true, Ordering::Release);
    if let Some(shm) = telemetry() {
        shm.set_toggle_bit(feature_bit, true);
    }
    log_fmt(format_args!(
        "[faunas] Critical Damage ENABLED (shared critical damage hooks)"
    ));
    feature_setup_result(feature_bit, true)
}

/// Installs or removes one feature's hooks.
unsafe fn set_feature(index: usize, enabled: bool) -> i32 {
    if index >= FEATURE_COUNT || (enabled && !FEATURES[index].available) {
        return -10;
    }
    if enabled {
        if let Some(kind) = ladder_kind(index) {
            if !ladder_armed(kind) {
                log_fmt(format_args!(
                    "[faunas] {} needs a ladder rung before it can be turned on",
                    FEATURES[index].id
                ));
                return -4;
            }
        }
    }
    if index == CRITDMG_FEATURE_INDEX {
        return unsafe { set_critdmg_feature(enabled) };
    }
    let all = bindings();
    let Some(binding) = all.get(index) else {
        return -4;
    };
    if binding.active.load(Ordering::Acquire) == enabled && !cleanup_pending(binding.bit) {
        return feature_setup_result(binding.bit, enabled);
    }
    if !enabled {
        if ONE_CLICK.contains(&index) {
            ONE_CLICK_ACTIVE.store(false, Ordering::Release);
            if let Some(shm) = telemetry() {
                shm.set_toggle_bit(bit(ONE_CLICK_INDEX), false);
            }
        }
        if index == 0 {
            crate::critical_path::disable();
        }
        port::deactivate(index);
        let status = unsafe {
            disable_local_feature(binding.table, binding.active, &[], binding.bit, binding.id)
        };
        if status != 0 {
            return status;
        }
        port::release(index);
        if (index == STABLE_FEATURE_INDEX || index == MOB_FLEE_FEATURE_INDEX)
            && crate::features::always_crit_active()
        {
            // Always-critical is still on without its sidecars: hand them
            // back. A failed handback only leaves always-critical partial,
            // which is where it already was while the port owned the methods.
            let mut gc = GcScope::new();
            let mut ch = GameChain::new();
            if !unsafe { ch.resolve(&mut gc) } {
                log_fmt(format_args!(
                    "[faunas] {} off, but the chain would not resolve for handback",
                    binding.id
                ));
                return 0;
            }
            if !unsafe { crate::features::always_crit_reclaim_stable_hit(&ch) } {
                log_fmt(format_args!(
                    "[faunas] {} off, but always-critical did not reclaim get_Stable/get_Hit",
                    binding.id
                ));
            }
        }
        return 0;
    }
    if cleanup_pending(binding.bit) {
        log_fmt(format_args!(
            "[faunas] {} is waiting for a completed restore; retry ON after the OFF finishes",
            binding.id
        ));
        return -8;
    }
    if index == 0 && crate::features::always_crit_active() {
        log_fmt(format_args!(
            "[faunas] disable companion critical before selecting the Faunas critical path"
        ));
        return -8;
    }
    let Some(specs) = specs_for(index) else {
        return -4;
    };
    if specs.len() > HOOK_CAPACITY {
        log_fmt(format_args!(
            "[faunas] {} needs {} hook slots, table holds {}",
            binding.id,
            specs.len(),
            HOOK_CAPACITY
        ));
        return -8;
    }
    let mut gc = GcScope::new();
    let mut ch = GameChain::new();
    if !unsafe { ch.resolve(&mut gc) } {
        return -1;
    }
    if !unsafe { port::prepare(index, &specs, &ch, forced_value(index)) } {
        port::release(index);
        return -8;
    }
    if index == 0 {
        if let Err(reason) = unsafe { crate::features::prepare_normal_critical_path(&ch) } {
            log_fmt(format_args!(
                "[faunas] critical caller validation failed: {reason}"
            ));
            port::release(index);
            return -8;
        }
    }
    if index == STABLE_FEATURE_INDEX || index == MOB_FLEE_FEATURE_INDEX {
        // The port's values win: yield always-critical's sidecars first so
        // the install below cannot be refused.
        if !unsafe { crate::features::always_crit_yield_stable_hit() } {
            log_fmt(format_args!(
                "[faunas] {} rolled back: always-critical would not yield get_Stable/get_Hit",
                FEATURES[index].id
            ));
            port::release(index);
            unsafe {
                crate::features::always_crit_reclaim_stable_hit(&ch);
            }
            return -8;
        }
    }
    let Ok(mut hooks) = binding.table.lock() else {
        port::release(index);
        if index == STABLE_FEATURE_INDEX || index == MOB_FLEE_FEATURE_INDEX {
            unsafe {
                crate::features::always_crit_reclaim_stable_hit(&ch);
            }
        }
        return -8;
    };
    let installed =
        unsafe { apply_hook_specs(&mut hooks[..specs.len()], &specs, &ch, true, binding.id) };
    let complete = hooks[..specs.len()].iter().all(|entry| entry.active);
    drop(hooks);
    let path_ready = index != 0 || (installed && complete && crate::critical_path::enable());
    if !installed || !complete || !path_ready {
        if index == 0 {
            crate::critical_path::disable();
        }
        port::deactivate(index);
        let restored = unsafe {
            disable_local_feature(binding.table, binding.active, &[], binding.bit, binding.id)
        };
        if restored == 0 {
            port::release(index);
        }
        if index == STABLE_FEATURE_INDEX || index == MOB_FLEE_FEATURE_INDEX {
            unsafe {
                crate::features::always_crit_reclaim_stable_hit(&ch);
            }
        }
        return -8;
    }
    binding.active.store(true, Ordering::Release);
    if let Some(shm) = telemetry() {
        shm.set_toggle_bit(binding.bit, true);
    }
    port::activate(index);
    log_fmt(format_args!(
        "[faunas] {} ENABLED ({} hooks)",
        binding.label,
        specs.len()
    ));
    feature_setup_result(binding.bit, true)
}

/// Sets one of the value ladders and turns the matching feature on.
unsafe fn set_value(kind: usize, value: i32) -> i32 {
    let Some((slot, feature_index, ladder)) = ladder_parts(kind) else {
        return -4;
    };
    let Some(found) = ladder.iter().position(|entry| *entry == value) else {
        return -4;
    };
    FORCED[slot].store(value, Ordering::Release);
    port::set_value(feature_index, value);
    set_ladder_rung(kind, found);
    if kind == 3 {
        log_fmt(format_args!(
            "[faunas] value ladder {} rung {} -> {} ({})",
            kind,
            found + 1,
            value,
            ELEMENT_NAMES[found]
        ));
    } else {
        log_fmt(format_args!(
            "[faunas] value ladder {} rung {} -> {}",
            kind,
            found + 1,
            value
        ));
    }
    unsafe { set_feature(feature_index, true) }
}

/// The rung a ladder feature turns on with.
///
/// Turning it on while it is already on steps up the ladder; turning it on
/// from off re-applies the rung that was chosen last.
fn ladder_next_rung(ladder_len: usize, current: usize, active: bool) -> usize {
    if ladder_len == 0 {
        return 0;
    }
    let current = current.min(ladder_len - 1);
    if active {
        (current + 1) % ladder_len
    } else {
        current
    }
}

/// Turns a ladder feature on by choosing a rung for it.
///
/// A ladder feature has no mechanic of its own, so turning it on has to pick
/// a value. The next rung is used when the feature is already on, which lets
/// the operator step up the ladder from the window; from off, the rung that
/// was chosen last is re-applied, or the first one if none has been.
unsafe fn set_ladder_on(kind: usize) -> i32 {
    let Some((_, _, ladder)) = ladder_parts(kind) else {
        return -4;
    };
    if ladder.is_empty() {
        return -4;
    }
    let next = ladder_next_rung(ladder.len(), ladder_rung(kind), ladder_feature_active(kind));
    unsafe { set_value(kind, ladder[next]) }
}

/// Restores the native value of a ladder by turning its feature off.
unsafe fn clear_value(kind: usize) -> i32 {
    let feature_index = match kind {
        0 => 14usize,
        1 => 16usize,
        2 => 17usize,
        3 => 15usize,
        _ => return -4,
    };
    unsafe { set_feature(feature_index, false) }
}

/// Handles one Faunas-port command.
///
/// * `200..=247` — explicit ON/OFF for a feature: even is OFF, odd is ON.
/// * `300..=306` — proration ladder rung.
/// * `310..=319` — critical damage ladder rung.
/// * `320` — critical damage back to native.
/// * `330..=339` — critical magic damage ladder rung.
/// * `340` — critical magic damage back to native.
/// * `350..=356` — element ladder rung.
pub unsafe fn dispatch(cmd: i32) -> i32 {
    match cmd {
        200..=247 => {
            let index = ((cmd - 200) / 2) as usize;
            if index >= FEATURE_COUNT {
                return -4;
            }
            let enabled = cmd & 1 != 0;
            if index == ONE_CLICK_INDEX {
                return unsafe { one_click(enabled) };
            }
            if index >= bindings().len() {
                log_fmt(format_args!(
                    "[faunas] feature {} is not resolvable in the installed metadata",
                    FEATURES[index].id
                ));
                return -10;
            }
            // A ladder feature is a value, not a switch: turning it on picks
            // a rung, because the feature has no mechanic without one.
            // Sustain mp_cost steps its own percent ladder (50 -> 25 -> 10).
            if enabled && index == MP_COST_FEATURE_INDEX {
                return unsafe { set_mp_cost_on() };
            }
            if enabled {
                if let Some(kind) = ladder_kind(index) {
                    return unsafe { set_ladder_on(kind) };
                }
            }
            unsafe { set_feature(index, enabled) }
        }
        300..=306 => unsafe { set_value(0, PRORATE_LADDER[(cmd - 300) as usize]) },
        310..=319 => unsafe { set_value(1, CRITDMG_LADDER[(cmd - 310) as usize]) },
        320 => unsafe { clear_value(1) },
        330..=339 => unsafe { set_value(2, CRITMAGICDMG_LADDER[(cmd - 330) as usize]) },
        340 => unsafe { clear_value(2) },
        350..=356 => unsafe { set_value(3, ELEMENT_LADDER[(cmd - 350) as usize]) },
        _ => -4,
    }
}

static ONE_CLICK_OWNED: AtomicU32 = AtomicU32::new(0);

fn preset_transition(
    enabled: bool,
    initial: u32,
    mut owned: u32,
    mut set: impl FnMut(usize, bool) -> i32,
) -> (i32, u32) {
    if !enabled {
        let mut status = 0;
        for index in ONE_CLICK.into_iter().rev() {
            if owned & bit(index) == 0 {
                continue;
            }
            if set(index, false) == 0 {
                owned &= !bit(index);
            } else {
                status = -8;
            }
        }
        return (status, owned);
    }
    for index in ONE_CLICK {
        if initial & bit(index) != 0 {
            continue;
        }
        let status = set(index, true);
        // A failed ON can itself own a partially restored hook.
        owned |= bit(index);
        if status != 0 {
            let (cleanup, remaining) = preset_transition(false, 0, owned, set);
            return (if cleanup != 0 { cleanup } else { status }, remaining);
        }
    }
    (0, owned)
}

unsafe fn one_click(enabled: bool) -> i32 {
    let owned = ONE_CLICK_OWNED.load(Ordering::Acquire);
    if enabled && ONE_CLICK_ACTIVE.load(Ordering::Acquire) {
        return 0;
    }
    if enabled && owned != 0 {
        return -8;
    }
    let (status, remaining) =
        preset_transition(enabled, active_mask(), owned, |index, on| unsafe {
            set_feature(index, on)
        });
    ONE_CLICK_OWNED.store(remaining, Ordering::Release);
    let active = enabled && status == 0;
    ONE_CLICK_ACTIVE.store(active, Ordering::Release);
    note_partial(bit(ONE_CLICK_INDEX), status != 0 && remaining != 0);
    if let Some(shm) = telemetry() {
        shm.set_toggle_bit(bit(ONE_CLICK_INDEX), active);
    }
    status
}

/// The features that are currently active, by telemetry bit.
pub fn active_mask() -> u32 {
    let mut mask = 0;
    for binding in bindings() {
        if binding.active.load(Ordering::Acquire) {
            mask |= binding.bit;
        }
    }
    mask
}

/// Restores every ported feature. Called on worker shutdown.
pub fn disable_all() -> bool {
    let mut restored = true;
    for index in (0..FEATURE_COUNT).rev() {
        if index != ONE_CLICK_INDEX {
            restored &= unsafe { set_feature(index, false) } == 0;
        }
    }
    restored &= unsafe { one_click(false) } == 0;
    restored
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preset_failure_rolls_back_only_new_members_including_failed_on() {
        let initial = bit(0) | bit(9);
        let mut calls = Vec::new();
        let (status, owned) = preset_transition(true, initial, 0, |index, enabled| {
            calls.push((index, enabled));
            if index == 4 && enabled {
                -8
            } else {
                0
            }
        });
        assert_eq!(status, -8);
        assert_eq!(owned, 0);
        assert_eq!(calls, [(2, true), (4, true), (4, false), (2, false)]);
    }

    #[test]
    fn preset_failed_restore_retains_ownership_for_retry() {
        let owned = bit(2) | bit(4);
        let (status, remaining) =
            preset_transition(false, 0, owned, |index, _| if index == 4 { -8 } else { 0 });
        assert_eq!(status, -8);
        assert_eq!(remaining, bit(4));
        let mut calls = Vec::new();
        assert_eq!(
            preset_transition(false, 0, remaining, |index, on| {
                calls.push((index, on));
                0
            }),
            (0, 0)
        );
        assert_eq!(calls, [(4, false)]);
    }

    #[test]
    fn preset_off_preserves_features_enabled_before_the_preset() {
        let initial = bit(0) | bit(2);
        let (status, owned) = preset_transition(true, initial, 0, |_, _| 0);
        assert_eq!(status, 0);
        assert_eq!(owned & initial, 0);
        let mut calls = Vec::new();
        assert_eq!(
            preset_transition(false, 0, owned, |index, enabled| {
                calls.push((index, enabled));
                0
            }),
            (0, 0)
        );
        assert!(!calls.iter().any(|(index, _)| *index == 0 || *index == 2));
    }

    #[test]
    fn each_port_target_has_a_distinct_original_pointer_and_both_calculators() {
        let mut sinks = Vec::new();
        for (index, feature) in FEATURES.iter().enumerate() {
            if index == 16 || index == 21 || !feature.available {
                continue;
            }
            for spec in specs_for(index).unwrap() {
                let sink = spec.orig_sink.unwrap() as *const _ as usize;
                assert!(
                    !sinks.contains(&sink),
                    "duplicate original pointer in feature {index}"
                );
                sinks.push(sink);
            }
        }
        assert_eq!(sinks.len(), port::COUNT + 1); // All 71 slots plus the shared getter shim.
        for index in [1, 2, 3, 7, 17] {
            let specs = specs_for(index).unwrap();
            assert_eq!(specs.len(), 2);
            assert!(matches!(
                specs[0].target,
                HookTargetClass::PlayerBattleStatus
            ));
            assert!(matches!(
                specs[1].target,
                HookTargetClass::PlayerSecondaryStatus
            ));
        }
    }

    #[test]
    fn only_the_won_overlaps_are_excused_from_failing_closed() {
        // Fail-closed is the default: without an active port feature no
        // overlap is excused, and unrelated methods never are.
        use HookTargetClass::*;
        for (target, method) in [
            (PlayerBattleStatus, "get_Stable"),
            (PlayerBattleStatus, "get_Hit"),
            (PlayerBattleStatus, "GetMotionSpeed"),
            (PlayerSecondaryStatus, "GetMotionSpeed"),
            (PlayerBattleStatus, "get_Critical"),
            (PlayerAttackBase, "checkCriticalPercent"),
        ] {
            assert!(
                !port_owns_hook(target, method),
                "{method} excused while port is off"
            );
        }
    }

    #[test]
    fn a_ladder_feature_refuses_to_turn_on_without_a_rung() {
        // The refusal happens before anything is resolved, so it is safe to
        // exercise without a live client. Without it, turning a ladder
        // feature on applied the forced-value slot's placeholder, which is
        // zero for the two damage ladders.
        for index in [14usize, 15, 16, 17] {
            assert_eq!(unsafe { set_feature(index, true) }, -4);
        }
    }

    #[test]
    fn every_ladder_feature_has_a_ladder_to_choose_from() {
        for index in [14usize, 15, 16, 17] {
            let kind = ladder_kind(index).expect("ladder feature");
            let (slot, feature_index, ladder) = ladder_parts(kind).expect("ladder parts");
            assert_eq!(feature_index, index);
            assert!(slot < SLOT_COUNT);
            assert!(!ladder.is_empty());
        }
    }

    #[test]
    fn only_the_ladder_features_use_a_value_ladder() {
        assert_eq!(ladder_kind(14), Some(0));
        assert_eq!(ladder_kind(15), Some(3));
        assert_eq!(ladder_kind(16), Some(1));
        assert_eq!(ladder_kind(17), Some(2));
        for index in [
            0usize, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 18, 19, 20, 22, 23,
        ] {
            assert_eq!(
                ladder_kind(index),
                None,
                "feature {} claimed a ladder",
                index
            );
        }
    }

    #[test]
    fn turning_a_ladder_on_picks_a_rung_and_never_the_placeholder() {
        // Turning on from off re-applies the chosen rung; turning on while it
        // is already on steps up. Either way the value comes from the ladder.
        assert_eq!(ladder_next_rung(10, 0, false), 0);
        assert_eq!(ladder_next_rung(10, 4, false), 4);
        assert_eq!(ladder_next_rung(10, 4, true), 5);
        assert_eq!(ladder_next_rung(10, 9, true), 0);
        assert_eq!(ladder_next_rung(7, 6, true), 0);
        assert_eq!(ladder_next_rung(0, 0, true), 0);
    }

    #[test]
    fn the_ladder_values_are_never_the_placeholder_zero_except_on_purpose() {
        // The element ladder's first rung really is "None"; the damage
        // ladders must never carry a zero, because a zero there would be a
        // mechanic that removes the damage instead of replacing it.
        assert_eq!(ELEMENT_LADDER[0], 0);
        assert!(CRITDMG_LADDER.iter().all(|value| *value > 0));
        assert!(CRITMAGICDMG_LADDER.iter().all(|value| *value > 0));
        assert!(PRORATE_LADDER.iter().all(|value| *value > 0));
    }

    /// Decodes the immediate of the ARM64 `mov w0, #imm` Faunas writes.
    ///
    /// The original stores four bytes in memory order, so the word is read
    /// little-endian. `MOVZ Wd, #imm16, LSL #hw` is the only encoding the
    /// prologue overrides use.
    fn movz_w0(hex: &str) -> i32 {
        let bytes: Vec<u8> = (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect();
        let mut word = [0u8; 4];
        word.copy_from_slice(&bytes);
        let word = u32::from_le_bytes(word);
        assert_eq!(
            word & 0x7F80_0000,
            0x5280_0000,
            "{hex} is not a 32-bit MOVZ"
        );
        let imm16 = (word >> 5) & 0xFFFF;
        let hw = (word >> 21) & 3;
        (imm16 << (hw * 16)) as i32
    }

    #[test]
    fn value_ladders_are_the_immediates_the_original_writes() {
        // The exact byte strings from the original release script.
        let prorate = [
            "C01280D2", "001980D2", "201C80D2", "401F80D2", "602280D2", "802580D2", "C02B80D2",
        ];
        for (rung, bytes) in prorate.iter().enumerate() {
            assert_eq!(movz_w0(bytes), PRORATE_LADDER[rung], "prorate rung {rung}");
        }
        let critdmg = [
            "201C80D2", "401F80D2", "802580D2", "602780D2", "A02880D2", "C02B80D2", "E02E80D2",
            "003280D2", "203580D2", "403880D2",
        ];
        for (rung, bytes) in critdmg.iter().enumerate() {
            assert_eq!(movz_w0(bytes), CRITDMG_LADDER[rung], "critdmg rung {rung}");
        }
        let critmgcdmg = [
            "A00F80D2", "401080D2", "E01080D2", "801180D2", "201280D2", "C01280D2", "601380D2",
            "001480D2", "000280D2", "401580D2",
        ];
        for (rung, bytes) in critmgcdmg.iter().enumerate() {
            assert_eq!(
                movz_w0(bytes),
                CRITMAGICDMG_LADDER[rung],
                "critmgcdmg rung {rung}"
            );
        }
        let element = [
            "00008052", "20008052", "40008052", "60008052", "80008052", "A0008052", "C0008052",
        ];
        for (rung, bytes) in element.iter().enumerate() {
            assert_eq!(movz_w0(bytes), ELEMENT_LADDER[rung], "element rung {rung}");
        }
        assert_eq!(movz_w0("800780D2"), MOTION_VALUE);
        assert_eq!(movz_w0("007D80D2"), STABLE_VALUE);
        assert_eq!(movz_w0("E0FF9F52"), HIT_VALUE);
    }

    #[test]
    fn the_prologue_override_writes_are_what_the_original_writes() {
        // `mov w0, #1 ; ret` is how the original turns a boolean check on.
        assert_eq!(movz_w0("20008052"), 1);
        // `mov w0, #0 ; ret` is how it turns a check off.
        assert_eq!(movz_w0("00008052"), 0);
    }

    #[test]
    fn feature_table_has_unique_ids_and_bits_clear_of_the_core_toggles() {
        let mut seen: Vec<&str> = Vec::new();
        for (index, feature) in FEATURES.iter().enumerate() {
            assert!(
                !seen.contains(&feature.id),
                "duplicate feature {}",
                feature.id
            );
            seen.push(feature.id);
            assert_eq!(feature.bit, bit(index));
            assert!(
                feature.bit >= (1 << BIT_BASE),
                "{} uses a core bit",
                feature.id
            );
            assert!(feature.bit.count_ones() == 1);
        }
        // The core companion features occupy bits 0..=7.
        assert_eq!(protocol::TOGGLE_ANTI_RIGIDITY, 1 << 7);
        assert!(bit(0) > protocol::TOGGLE_ANTI_RIGIDITY);
        assert_eq!(FEATURES.len(), FEATURE_COUNT);
    }

    #[test]
    fn skill_class_lists_match_the_original_and_do_not_overlap() {
        assert_eq!(SKILL_AOE_CLASSES.len(), 16);
        assert_eq!(SKILL_NO_AOE_CLASSES.len(), 2);
        assert_eq!(SKILL_BURST_CLASSES.len(), 14);
        for list in [
            &SKILL_AOE_CLASSES[..],
            &SKILL_NO_AOE_CLASSES[..],
            &SKILL_BURST_CLASSES[..],
        ] {
            let mut sorted = list.to_vec();
            sorted.sort_unstable();
            let unique = sorted.len();
            sorted.dedup();
            assert_eq!(sorted.len(), unique, "a skill class is listed twice");
        }
        for class in SKILL_NO_AOE_CLASSES {
            assert!(!SKILL_AOE_CLASSES.contains(&class));
        }
        // Every burst class is also an AOE class in the original.
        for class in SKILL_BURST_CLASSES {
            assert!(SKILL_AOE_CLASSES.contains(&class));
        }
    }

    #[test]
    fn every_resolvable_feature_has_a_spec_table() {
        for (index, feature) in FEATURES.iter().enumerate().take(bindings().len()) {
            if index == ONE_CLICK_INDEX || !feature.available {
                // The one-click preset is a set of other features, not a
                // mechanic of its own.
                assert!(specs_for(index).is_none());
                continue;
            }
            let specs = specs_for(index).expect("feature has no spec table");
            assert!(!specs.is_empty(), "feature {index} has no hooks");
            assert!(
                specs.len() <= HOOK_CAPACITY,
                "feature {index} needs more hook slots than the table holds"
            );
        }
    }

    #[test]
    fn sustain_slots_carry_verified_targets_not_gate_guesses() {
        // Slots 22/23 used to hold disproven gacha/map gate guesses. They now
        // carry sustain farming with verified PC_v226 targets, so they are
        // available and resolve to exactly one hook each.
        for index in [22, 23] {
            assert!(FEATURES[index].available);
            let specs = specs_for(index).expect("sustain feature has no spec table");
            assert_eq!(specs.len(), 1);
        }
        let cost = mp_cost_specs();
        assert!(matches!(cost[0].target, HookTargetClass::PlayerAttackBase));
        assert_eq!(cost[0].method, "CalcCostMp");
        let delay = skill_delay_specs();
        assert!(matches!(
            delay[0].target,
            HookTargetClass::Named("SkillDelayManager")
        ));
        assert_eq!(delay[0].method, "CheckSkillAvailable");
        // The cost ladder starts at the safest rung and never touches zero.
        assert_eq!(MP_COST_LADDER, [50, 25, 10]);
        assert_eq!(mp_cost_rung_value(), 50);
    }
}
