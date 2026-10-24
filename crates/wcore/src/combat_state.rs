//! Metadata-bound local resources and reversible normal-attack preparation.

use crate::chain::{is_valid_il2cpp_obj, GameChain};
use crate::il2cpp::{
    api, call_i32_0, find_class, find_nested_class, get_field, get_method, GcScope,
};
use crate::win::{fn_readable, page_write_ok, ptr_ok};
use core::ffi::c_void;
use core::ptr;
use core::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

static BATTLE_TARGET_FALLBACKS: AtomicUsize = AtomicUsize::new(0);

// The native HUD getter displays ceil(target-adjusted world distance * 0.5).
pub(crate) const EXTENDED_RANGE_WORLD_UNITS: f32 = 30.0 * 2.0;

fn extended_action_range(native: f32) -> Option<f32> {
    (native.is_finite() && (0.0..EXTENDED_RANGE_WORLD_UNITS).contains(&native))
        .then_some(EXTENDED_RANGE_WORLD_UNITS)
}

pub fn battle_target_fallbacks() -> usize {
    BATTLE_TARGET_FALLBACKS.load(Ordering::Relaxed)
}

unsafe fn offset(class: *mut c_void, name: &str) -> Result<usize, &'static str> {
    let api = api().ok_or("IL2CPP API unavailable")?;
    let field = unsafe { get_field(class, name) };
    if field.is_null() {
        return Err("required combat field missing");
    }
    let value = unsafe { (api.field_get_offset)(field) };
    if !(0x10..=0x4000).contains(&value) {
        return Err("invalid combat field offset");
    }
    Ok(value as usize)
}

unsafe fn object_is(object: usize, class: usize) -> bool {
    class != 0
        && unsafe { is_valid_il2cpp_obj(object as *const c_void) }
        && unsafe { *(object as *const usize) } == class
}

unsafe fn read<T: Copy>(object: usize, offset: usize) -> Option<T> {
    let address = object.checked_add(offset)?;
    if !ptr_ok(address, core::mem::size_of::<T>()) {
        return None;
    }
    Some(unsafe { ptr::read_unaligned(address as *const T) })
}

unsafe fn write<T: Copy>(object: usize, offset: usize, value: T) -> bool {
    let Some(address) = object.checked_add(offset) else {
        return false;
    };
    if !page_write_ok(address, core::mem::size_of::<T>()) {
        return false;
    }
    unsafe { ptr::write_unaligned(address as *mut T, value) };
    true
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MpSample {
    pub local: i32,
    pub received: i32,
    pub maximum: i32,
}

#[derive(Clone, Copy)]
pub struct MpBinding {
    action_class: usize,
    status_class: usize,
    game_class: usize,
    action_status: usize,
    status_game: usize,
    local_mp: usize,
    received_mp: usize,
    maximum: i32,
    local_status: usize,
    calculators: [(usize, usize); 2],
    tps_class: usize,
    tps_base: usize,
}

impl MpBinding {
    pub unsafe fn prepare(chain: &GameChain) -> Result<Self, &'static str> {
        let game_class = unsafe { find_class(chain.image, chain.domain, "", "PlayerGameStatus") };
        let maximum = unsafe { call_i32_0(chain.bstatus_class, chain.bstatus, "get_MaxMp") }
            .filter(|value| (1..=2000).contains(value))
            .ok_or("native maximum MP unavailable")?;
        // TemporaryPlayerStatus is best-effort: the wrapper exists in PC v226
        // (baseStatus at +0x18, cached AMPR at +0x30) but must not fail the
        // whole job on builds where it is absent.
        let tps_class =
            unsafe { find_class(chain.image, chain.domain, "", "TemporaryPlayerStatus") } as usize;
        let tps_base = if tps_class != 0 {
            unsafe { offset(tps_class as *mut c_void, "baseStatus") }.unwrap_or(0)
        } else {
            0
        };
        let binding = Self {
            action_class: chain.pam_class as usize,
            status_class: chain.status_class as usize,
            game_class: game_class as usize,
            action_status: unsafe { offset(chain.pam_class, "_playerStatus") }?,
            status_game: unsafe { offset(chain.status_class, "gameStatus") }?,
            local_mp: unsafe { offset(game_class, "localMp") }?,
            received_mp: unsafe { offset(game_class, "serverMp") }?,
            maximum,
            local_status: chain.status as usize,
            calculators: [
                (chain.bstatus_class as usize, unsafe {
                    offset(chain.bstatus_class, "playerStatus")
                }?),
                (chain.pss_class as usize, unsafe {
                    offset(chain.pss_class, "playerStatus")
                }?),
            ],
            tps_class,
            tps_base,
        };
        if unsafe { binding.sample(chain.pam as usize) }.is_none() {
            return Err("local resource object chain unavailable");
        }
        Ok(binding)
    }

    unsafe fn game(&self, actor: usize) -> Option<usize> {
        if !unsafe { object_is(actor, self.action_class) } {
            return None;
        }
        let status = unsafe { read::<usize>(actor, self.action_status) }?;
        if status != self.local_status || !unsafe { object_is(status, self.status_class) } {
            return None;
        }
        let game = unsafe { read::<usize>(status, self.status_game) }?;
        unsafe { object_is(game, self.game_class) }.then_some(game)
    }

    pub unsafe fn owns_calculator(&self, calculator: usize) -> bool {
        if self.calculators.iter().any(|&(class, field)| {
            (unsafe { object_is(calculator, class) })
                && unsafe { read::<usize>(calculator, field) } == Some(self.local_status)
        }) {
            return true;
        }
        // TemporaryPlayerStatus wrapper: owns when its baseStatus points at
        // one of our owned PBS/PSS calculators. Missing TPS metadata (0,0)
        // never owns.
        if self.tps_class != 0
            && self.tps_base != 0
            && unsafe { object_is(calculator, self.tps_class) }
        {
            if let Some(base) = unsafe { read::<usize>(calculator, self.tps_base) } {
                return self.calculators.iter().any(|&(class, field)| {
                    (unsafe { object_is(base, class) })
                        && unsafe { read::<usize>(base, field) } == Some(self.local_status)
                });
            }
        }
        false
    }

    pub unsafe fn sample(&self, actor: usize) -> Option<MpSample> {
        let game = unsafe { self.game(actor) }?;
        Some(MpSample {
            local: unsafe { read(game, self.local_mp) }?,
            received: unsafe { read(game, self.received_mp) }?,
            maximum: self.maximum,
        })
    }
}

#[derive(Clone)]
pub struct RangeBinding {
    action_class: usize,
    actor_class: usize,
    range: usize,
    enabled: usize,
    rate: usize,
    flags: usize,
    weapon: usize,
    sub_weapon: usize,
    take: usize,
    set_take: usize,
    set_take_method: usize,
    barrier: usize,
    local_actor: usize,
    actor_transform: usize,
    actor_target_manager: usize,
    target_manager_class: usize,
    selected_action: usize,
    actor_battle: usize,
    battle_class: usize,
    local_battle: usize,
    pending_action: usize,
    battle_active: usize,
    battle_target: usize,
    target_data_class: usize,
    target_data_action: usize,
    character_class: usize,
    attack_class: usize,
    _roots: Arc<GcScope>,
}

pub struct RangeSnapshot {
    pub action: usize,
    range: f32,
    patched_range: Option<f32>,
    enabled: u8,
    rate: f32,
    flags: i32,
    take: usize,
    patched_take: usize,
    patched_flags: i32,
    _roots: GcScope,
}

/// Only the scoped native query or movement callback sees this override.
pub struct PendingRangeOverride {
    action: usize,
    offset: usize,
    original: f32,
    patched: f32,
    _roots: Arc<GcScope>,
    _same_thread: core::marker::PhantomData<std::rc::Rc<()>>,
}

impl Drop for PendingRangeOverride {
    fn drop(&mut self) {
        if unsafe { read::<f32>(self.action, self.offset) } == Some(self.patched) {
            unsafe { write(self.action, self.offset, self.original) };
        }
    }
}

impl RangeBinding {
    pub unsafe fn prepare(chain: &GameChain) -> Result<Self, &'static str> {
        let class = unsafe { find_class(chain.image, chain.domain, "", "NormalAttackAction") };
        if !unsafe { object_is(chain.pam as usize, chain.pam_class as usize) } {
            return Err("local actor is unavailable");
        }
        let targets = unsafe { find_class(chain.image, chain.domain, "", "TargetManager") };
        let character =
            unsafe { find_class(chain.image, chain.domain, "", "CharacterActionManagerBase") };
        let attack = unsafe { find_class(chain.image, chain.domain, "", "PlayerAttackBase") };
        let battle_base = unsafe { find_class(chain.image, chain.domain, "", "BattleManagerBase") };
        let target_data = unsafe { find_nested_class(battle_base, "TargetData") };
        if target_data.is_null() {
            return Err("BattleManagerBase.TargetData nested class missing");
        }
        let method = unsafe { get_method(class, "SetPowerWaveTake", 2) };
        if !ptr_ok(method as usize, 8) {
            return Err("native projectile preparation missing");
        }
        let set_take = unsafe { *(method as *const usize) };
        let barrier = api()
            .and_then(|api| api.gc_wbarrier_set_field)
            .map(|function| function as usize)
            .ok_or("managed reference write barrier unavailable")?;
        if !fn_readable(set_take) {
            return Err("native projectile preparation unreadable");
        }
        if api().and_then(|api| api.gchandle_new).is_none()
            || api().and_then(|api| api.gchandle_free).is_none()
        {
            return Err("managed object rooting unavailable");
        }
        let mut roots = GcScope::new();
        roots
            .try_pin(chain.pam)
            .ok_or("local actor rooting failed")?;
        roots
            .try_pin(chain.pbm)
            .ok_or("local battle rooting failed")?;
        Ok(Self {
            action_class: class as usize,
            actor_class: chain.pam_class as usize,
            range: unsafe { offset(class, "<ActionRange>k__BackingField") }?,
            enabled: unsafe { offset(class, "powerWaveEnable") }?,
            rate: unsafe { offset(class, "powerWaveRate") }?,
            flags: unsafe { offset(class, "<SkillIndividualFlag>k__BackingField") }?,
            weapon: unsafe { offset(class, "<WeaponType>k__BackingField") }?,
            sub_weapon: unsafe { offset(class, "<SubWeaponType>k__BackingField") }?,
            take: unsafe { offset(class, "<CurrentTake>k__BackingField") }?,
            set_take,
            set_take_method: method as usize,
            barrier,
            local_actor: chain.pam as usize,
            actor_transform: unsafe { offset(character, "charaTransform") }?,
            actor_target_manager: unsafe { offset(chain.pam_class, "targetManager") }?,
            target_manager_class: targets as usize,
            selected_action: unsafe { offset(targets, "actionManager") }?,
            actor_battle: unsafe { offset(chain.pam_class, "_playerBattleManager") }?,
            battle_class: chain.pbm_class as usize,
            local_battle: chain.pbm as usize,
            pending_action: unsafe { offset(chain.pbm_class, "nextAction") }?,
            battle_active: unsafe { offset(chain.pbm_class, "<IsBattleActive>k__BackingField") }?,
            battle_target: unsafe { offset(chain.pbm_class, "mainTargetData") }?,
            target_data_class: target_data as usize,
            target_data_action: unsafe { offset(target_data, "<ActionManager>k__BackingField") }?,
            character_class: character as usize,
            attack_class: attack as usize,
            _roots: Arc::new(roots),
        })
    }

    /// CheckActionRange runs before attack preparation and reads nextAction
    /// directly. Preserve its native target/geometry checks and widen only the
    /// queued normal attack belonging to this exact local battle manager.
    ///
    /// The widening is disabled. The server validates the distance of a damage
    /// claim: an out-of-range basic attack is dropped, and an out-of-range
    /// skill is treated as an invalid claim and sanctioned. Inflating the
    /// client's range therefore buys nothing and can kill the character. The
    /// feature now enables assist-move only, so the character closes to a
    /// legitimate distance and the claim lands on its own terms.
    pub unsafe fn pending_range_override(&self, battle: usize) -> Option<PendingRangeOverride> {
        let _ = battle;
        None
    }

    /// Out-of-range callbacks read the action again to choose the assist radius.
    /// A stale callback must not alter a different queued action.
    pub unsafe fn pending_range_override_for_action(
        &self,
        battle: usize,
        action: usize,
    ) -> Option<PendingRangeOverride> {
        let _ = (battle, action);
        None
    }

    /// True when the given action is the local battle manager's queued action.
    ///
    /// This is the scoping rule for the assist-move getters: only the player's
    /// own queued action is affected, never another actor's.
    pub unsafe fn is_local_pending_action(&self, action: usize) -> bool {
        if action == 0 || !unsafe { object_is(self.local_battle, self.battle_class) } {
            return false;
        }
        unsafe { read::<usize>(self.local_battle, self.pending_action) == Some(action) }
    }

    pub unsafe fn selected_target(
        &self,
        action: usize,
        actor_transform: usize,
        target_transform: usize,
    ) -> bool {
        unsafe fn derives(object: usize, expected: usize) -> bool {
            if !unsafe { is_valid_il2cpp_obj(object as *const c_void) } {
                return false;
            }
            let mut class = unsafe { *(object as *const *mut c_void) };
            for _ in 0..16 {
                if class as usize == expected {
                    return true;
                }
                let Some(parent) = api().and_then(|api| api.class_get_parent) else {
                    return false;
                };
                class = unsafe { parent(class) };
                if class.is_null() {
                    return false;
                }
            }
            false
        }
        if !unsafe { derives(action, self.attack_class) }
            || !unsafe { object_is(self.local_actor, self.actor_class) }
            || unsafe { read::<usize>(self.local_actor, self.actor_transform) }
                != Some(actor_transform)
        {
            return false;
        }
        let Some(targets) = (unsafe { read::<usize>(self.local_actor, self.actor_target_manager) })
        else {
            return false;
        };
        let selected = if targets == 0 {
            0
        } else {
            if !unsafe { object_is(targets, self.target_manager_class) } {
                return false;
            }
            let Some(selected) = (unsafe { read::<usize>(targets, self.selected_action) }) else {
                return false;
            };
            selected
        };
        let target = if selected != 0 {
            selected
        } else {
            // TargetManager clears its selection during an ongoing attack. Use
            // only the active battle's target, never a retained inactive target.
            let Some(battle) = (unsafe { read::<usize>(self.local_actor, self.actor_battle) })
            else {
                return false;
            };
            if !unsafe { object_is(battle, self.battle_class) }
                || unsafe { read::<u8>(battle, self.battle_active) } != Some(1)
            {
                return false;
            }
            let Some(data) = (unsafe { read::<usize>(battle, self.battle_target) }) else {
                return false;
            };
            if !unsafe { object_is(data, self.target_data_class) } {
                return false;
            }
            let Some(target) = (unsafe { read::<usize>(data, self.target_data_action) }) else {
                return false;
            };
            target
        };
        let matched = target_transform != 0
            && unsafe { derives(target, self.character_class) }
            && unsafe { read::<usize>(target, self.actor_transform) } == Some(target_transform);
        if matched && selected == 0 {
            BATTLE_TARGET_FALLBACKS.fetch_add(1, Ordering::Relaxed);
        }
        matched
    }

    /// Runs after native ActionPreparation, on that same game thread. Its real
    /// take builder is used because PowerWaveCheck is inlined in the current build.
    pub unsafe fn apply(&self, action: usize, actor: usize) -> Option<RangeSnapshot> {
        if !unsafe { object_is(action, self.action_class) }
            || !unsafe { object_is(actor, self.actor_class) }
            || actor != self.local_actor
        {
            return None;
        }
        let mut roots = GcScope::new();
        roots.try_pin(action as *mut c_void)?;
        let old_take = unsafe { read::<usize>(action, self.take) }?;
        roots.try_pin(old_take as *mut c_void)?;
        let flags = unsafe { read::<i32>(action, self.flags) }?;
        let weapon = unsafe { read::<i32>(action, self.weapon) }?;
        let sub_weapon = unsafe { read::<i32>(action, self.sub_weapon) }?;
        let range = unsafe { read::<f32>(action, self.range) }?;
        if !range.is_finite() {
            return None;
        }
        let mut saved = RangeSnapshot {
            action,
            range,
            patched_range: extended_action_range(range),
            enabled: unsafe { read(action, self.enabled) }?,
            rate: unsafe { read(action, self.rate) }?,
            flags,
            take: old_take,
            patched_take: old_take,
            patched_flags: flags | 512,
            _roots: roots,
        };
        for (offset, size) in [
            (self.range, 4),
            (self.enabled, 1),
            (self.rate, 4),
            (self.flags, 4),
            (self.take, 8),
        ] {
            if !page_write_ok(action.checked_add(offset)?, size) {
                return None;
            }
        }
        unsafe {
            if let Some(range) = saved.patched_range {
                write(action, self.range, range);
            }
            write(action, self.enabled, 1u8);
            write(action, self.rate, 1.0f32);
            write(action, self.flags, saved.patched_flags);
        }
        let set_take: unsafe extern "C" fn(*mut c_void, i32, i32, *const c_void) =
            unsafe { core::mem::transmute(self.set_take) };
        unsafe {
            set_take(
                action as *mut c_void,
                weapon,
                sub_weapon,
                self.set_take_method as *const c_void,
            )
        };
        saved.patched_take = unsafe { read(action, self.take) }.unwrap_or(old_take);
        Some(saved)
    }

    pub unsafe fn restore(&self, saved: &RangeSnapshot) -> bool {
        let action = saved.action;
        if !unsafe { object_is(action, self.action_class) } {
            return false;
        }
        // A later native preparation may already have replaced these values.
        let mut ok = true;
        if let Some(range) = saved.patched_range {
            if unsafe { read::<f32>(action, self.range) } == Some(range) {
                ok &= unsafe { write(action, self.range, saved.range) };
            }
        }
        if unsafe { read::<u8>(action, self.enabled) } == Some(1) {
            ok &= unsafe { write(action, self.enabled, saved.enabled) };
        }
        if unsafe { read::<f32>(action, self.rate) } == Some(1.0) {
            ok &= unsafe { write(action, self.rate, saved.rate) };
        }
        if let Some(flags) = unsafe { read::<i32>(action, self.flags) } {
            ok &= unsafe { write(action, self.flags, (flags & !512) | (saved.flags & 512)) };
        }
        if unsafe { read::<usize>(action, self.take) } == Some(saved.patched_take) {
            if page_write_ok(action + self.take, 8) && fn_readable(self.barrier) {
                let barrier: unsafe extern "C" fn(*mut c_void, *mut *mut c_void, *mut c_void) =
                    unsafe { core::mem::transmute(self.barrier) };
                unsafe {
                    barrier(
                        action as *mut c_void,
                        (action + self.take) as *mut *mut c_void,
                        saved.take as *mut c_void,
                    )
                };
            } else {
                ok = false;
            }
        }
        ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    unsafe extern "C" fn test_barrier(
        _object: *mut c_void,
        field: *mut *mut c_void,
        value: *mut c_void,
    ) {
        unsafe { *field = value };
    }

    fn range_fixture(class: usize) -> RangeBinding {
        RangeBinding {
            action_class: class,
            actor_class: class,
            range: 16,
            rate: 20,
            flags: 24,
            enabled: 28,
            weapon: 32,
            sub_weapon: 36,
            take: 40,
            set_take: 0,
            set_take_method: 0,
            barrier: test_barrier as *const () as usize,
            local_actor: 0,
            actor_transform: 48,
            actor_target_manager: 56,
            target_manager_class: class,
            selected_action: 16,
            actor_battle: 64,
            battle_class: class,
            local_battle: 0,
            pending_action: 32,
            battle_active: 16,
            battle_target: 24,
            target_data_class: class,
            target_data_action: 16,
            character_class: class,
            attack_class: class,
            _roots: Arc::new(GcScope::new()),
        }
    }

    #[test]
    fn pending_range_never_inflates_the_queued_action() {
        // The server validates the distance of a damage claim. Inflating the
        // client's range produced claims it dropped for a basic attack and
        // sanctioned for a skill, so the widening is retired: the queued
        // action's range field is never written.
        let class = [1usize; 16];
        let mut action = [0usize; 16];
        let mut battle = [0usize; 16];
        action[0] = class.as_ptr() as usize;
        battle[0] = class.as_ptr() as usize;
        battle[4] = action.as_ptr() as usize;
        let object = action.as_ptr() as usize;
        let manager = battle.as_ptr() as usize;
        let mut binding = range_fixture(class.as_ptr() as usize);
        binding.local_battle = manager;
        for original in [2.0f32, 40.0, 60.0, f32::NAN, f32::INFINITY] {
            unsafe { write(object, binding.range, original) };
            assert!(unsafe { binding.pending_range_override(manager) }.is_none());
            assert!(
                unsafe { binding.pending_range_override_for_action(manager, object) }.is_none()
            );
            assert_eq!(
                unsafe { read::<f32>(object, binding.range) }
                    .unwrap()
                    .to_bits(),
                original.to_bits(),
                "the queued action's range must never be rewritten"
            );
        }
    }

    #[test]
    fn assist_scope_is_the_local_queued_action_only() {
        let class = [1usize; 16];
        let mut action = [0usize; 16];
        let mut other_action = [0usize; 16];
        let mut battle = [0usize; 16];
        for object in [&mut action, &mut other_action, &mut battle] {
            object[0] = class.as_ptr() as usize;
        }
        battle[4] = action.as_ptr() as usize;
        let object = action.as_ptr() as usize;
        let other = other_action.as_ptr() as usize;
        let manager = battle.as_ptr() as usize;
        let mut binding = range_fixture(class.as_ptr() as usize);
        binding.local_battle = manager;
        assert!(unsafe { binding.is_local_pending_action(object) });
        assert!(!unsafe { binding.is_local_pending_action(other) });
        assert!(!unsafe { binding.is_local_pending_action(0) });
        // A different battle manager never scopes to this action.
        binding.local_battle = other;
        assert!(!unsafe { binding.is_local_pending_action(object) });
    }

    #[test]
    fn range_restore_preserves_later_native_flags_and_take_changes() {
        let class = [1usize; 16];
        let mut action = [0usize; 16];
        action[0] = class.as_ptr() as usize;
        let object = action.as_mut_ptr() as usize;
        let binding = range_fixture(class.as_ptr() as usize);
        let saved = RangeSnapshot {
            action: object,
            range: 2.5,
            patched_range: Some(EXTENDED_RANGE_WORLD_UNITS),
            rate: 0.4,
            enabled: 0,
            flags: 8,
            take: 0x1111,
            patched_take: 0x2222,
            patched_flags: 520,
            _roots: GcScope::new(),
        };
        unsafe {
            write(object, 16, 60.0f32);
            write(object, 20, 1.0f32);
            write(object, 24, 520i32 | 16);
            write(object, 28, 1u8);
            write(object, 40, 0x2222usize);
            assert!(binding.restore(&saved));
            assert_eq!(read::<f32>(object, 16), Some(2.5));
            assert_eq!(read::<f32>(object, 20), Some(0.4));
            assert_eq!(read::<i32>(object, 24), Some(8 | 16));
            assert_eq!(read::<u8>(object, 28), Some(0));
            assert_eq!(read::<usize>(object, 40), Some(0x1111));
            write(object, 40, 0x3333usize);
            assert!(binding.restore(&saved));
            assert_eq!(read::<usize>(object, 40), Some(0x3333));
        }
    }

    #[test]
    fn range_restore_only_owns_the_range_value_it_extended() {
        let class = [1usize; 16];
        let mut action = [0usize; 16];
        action[0] = class.as_ptr() as usize;
        let object = action.as_ptr() as usize;
        let binding = range_fixture(class.as_ptr() as usize);
        let mut saved = RangeSnapshot {
            action: object,
            range: 4.0,
            patched_range: extended_action_range(4.0),
            enabled: 0,
            rate: 0.0,
            flags: 0,
            take: 0,
            patched_take: 0,
            patched_flags: 512,
            _roots: GcScope::new(),
        };
        for native_range in [8.0f32, -1.0, 80.0] {
            unsafe {
                write(object, binding.range, native_range);
                assert!(binding.restore(&saved));
                assert_eq!(read::<f32>(object, binding.range), Some(native_range));
            }
        }
        for original in [-1.0f32, 60.0, 80.0] {
            saved.range = original;
            saved.patched_range = extended_action_range(original);
            assert!(saved.patched_range.is_none());
            unsafe {
                // A native write to 60 is not owned when no extension occurred.
                write(object, binding.range, 60.0f32);
                assert!(binding.restore(&saved));
                assert_eq!(read::<f32>(object, binding.range), Some(60.0));
            }
        }
    }

    #[test]
    fn extended_hit_geometry_is_limited_to_the_selected_target_and_local_actor() {
        let class = [1usize; 16];
        let class_address = class.as_ptr() as usize;
        let mut action = [0usize; 16];
        let mut actor = [0usize; 16];
        let mut target_manager = [0usize; 16];
        let mut target = [0usize; 16];
        for object in [&mut action, &mut actor, &mut target_manager, &mut target] {
            object[0] = class_address;
        }
        actor[6] = 0x1234;
        actor[7] = target_manager.as_ptr() as usize;
        target_manager[2] = target.as_ptr() as usize;
        target[6] = 0x5678;
        std::hint::black_box(&target_manager);
        let mut binding = range_fixture(class_address);
        binding.local_actor = actor.as_ptr() as usize;
        let action = action.as_ptr() as usize;
        assert!(unsafe { binding.selected_target(action, 0x1234, 0x5678) });
        assert!(!unsafe { binding.selected_target(action, 0x1234, 0x9999) });
        assert!(!unsafe { binding.selected_target(action, 0x9999, 0x5678) });
        target[0] = 0;
        std::hint::black_box(&target);
        assert!(!unsafe { binding.selected_target(action, 0x1234, 0x5678) });
    }

    #[test]
    fn range_uses_only_the_active_battle_target_when_selection_clears() {
        let class = [1usize; 16];
        let class_address = class.as_ptr() as usize;
        let mut action = [0usize; 16];
        let mut actor = [0usize; 16];
        let mut targets = [0usize; 16];
        let mut battle = [0usize; 16];
        let mut data = [0usize; 16];
        let mut target = [0usize; 16];
        let mut other = [0usize; 16];
        for object in [
            &mut action,
            &mut actor,
            &mut targets,
            &mut battle,
            &mut data,
            &mut target,
            &mut other,
        ] {
            object[0] = class_address;
        }
        actor[6] = 0x1234;
        actor[7] = targets.as_ptr() as usize;
        actor[8] = battle.as_ptr() as usize;
        battle[2] = 1;
        battle[3] = data.as_ptr() as usize;
        data[2] = target.as_ptr() as usize;
        target[6] = 0x5678;
        other[6] = 0x9999;
        std::hint::black_box((&target, &other));
        let mut binding = range_fixture(class_address);
        binding.local_actor = actor.as_ptr() as usize;
        let action = action.as_ptr() as usize;
        assert!(unsafe { binding.selected_target(action, 0x1234, 0x5678) });
        assert!(!unsafe { binding.selected_target(action, 0x1234, 0x9999) });
        assert!(!unsafe { binding.selected_target(action, 0x9999, 0x5678) });
        battle[2] = 0;
        std::hint::black_box(&battle);
        assert!(!unsafe { binding.selected_target(action, 0x1234, 0x5678) });
        battle[2] = 1;
        targets[2] = other.as_ptr() as usize;
        std::hint::black_box((&battle, &targets));
        assert!(!unsafe { binding.selected_target(action, 0x1234, 0x5678) });
        assert!(unsafe { binding.selected_target(action, 0x1234, 0x9999) });
        targets[2] = 0;
        data[0] = 0;
        std::hint::black_box((&data, &targets));
        assert!(!unsafe { binding.selected_target(action, 0x1234, 0x5678) });
        data[0] = class_address;
        actor[7] = 0;
        std::hint::black_box((&data, &actor));
        assert!(unsafe { binding.selected_target(action, 0x1234, 0x5678) });
    }

    #[test]
    fn range_preparation_rejects_another_actor_with_the_same_runtime_class() {
        let class = [1usize; 16];
        let class_address = class.as_ptr() as usize;
        let mut action = [0usize; 16];
        let mut local_actor = [0usize; 16];
        let mut other_actor = [0usize; 16];
        for object in [&mut action, &mut local_actor, &mut other_actor] {
            object[0] = class_address;
        }
        let mut binding = range_fixture(class_address);
        binding.local_actor = local_actor.as_ptr() as usize;
        assert!(
            unsafe { binding.apply(action.as_ptr() as usize, other_actor.as_ptr() as usize) }
                .is_none()
        );
        assert_eq!(action[2..], [0; 14]);
    }

    #[test]
    fn mp_sample_reads_truthfully_without_writing_local_prediction() {
        let class = [1usize; 16];
        let mut game = [0usize; 16];
        let mut status = [0usize; 16];
        let mut actor = [0usize; 16];
        for object in [&mut game, &mut status, &mut actor] {
            object[0] = class.as_ptr() as usize;
        }
        game[2] = 0x11223344;
        game[3] = 37;
        game[4] = 14;
        status[2] = game.as_mut_ptr() as usize;
        actor[2] = status.as_mut_ptr() as usize;
        let binding = MpBinding {
            action_class: class.as_ptr() as usize,
            status_class: class.as_ptr() as usize,
            game_class: class.as_ptr() as usize,
            action_status: 16,
            status_game: 16,
            received_mp: 24,
            local_mp: 32,
            maximum: 400,
            local_status: status.as_ptr() as usize,
            calculators: [(class.as_ptr() as usize, 16); 2],
            tps_class: 0,
            tps_base: 0,
        };
        assert_eq!(
            unsafe { binding.sample(actor.as_mut_ptr() as usize) },
            Some(MpSample {
                local: 14,
                received: 37,
                maximum: 400
            })
        );
        assert_eq!(game[2], 0x11223344);
        assert_eq!(game[3], 37);
        actor[0] = 0;
        game[4] = 2;
        assert!(unsafe { binding.sample(actor.as_mut_ptr() as usize) }.is_none());
        assert_eq!(game[4], 2);
    }
}
