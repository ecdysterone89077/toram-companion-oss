//! Typed Windows callbacks for the Faunas port. A disabled callback always forwards.

use crate::chain::GameChain;
use crate::features::{HookSpec, HookTargetClass};
use crate::il2cpp::{api, field_type_matches, find_class, get_field, GcScope};
use crate::win::{log_fmt, ptr_ok};
use core::ffi::c_void;
use core::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;

pub(crate) const COUNT: usize = 71;
pub(crate) struct Callback {
    pub original: AtomicUsize,
    enabled: AtomicBool,
    feature: AtomicUsize,
    instance: AtomicUsize,
    class: AtomicUsize,
    other_player_offset: AtomicUsize,
    value: AtomicI32,
    calls: AtomicU64,
}

impl Callback {
    const fn new() -> Self {
        Self {
            original: AtomicUsize::new(0),
            enabled: AtomicBool::new(false),
            feature: AtomicUsize::new(usize::MAX),
            instance: AtomicUsize::new(0),
            class: AtomicUsize::new(0),
            other_player_offset: AtomicUsize::new(0),
            value: AtomicI32::new(0),
            calls: AtomicU64::new(0),
        }
    }

    fn matches(&self, this: *mut c_void) -> bool {
        if !self.enabled.load(Ordering::Acquire) || this.is_null() {
            return false;
        }
        let instance = self.instance.load(Ordering::Relaxed);
        let class = self.class.load(Ordering::Relaxed);
        if instance != 0 && instance != this as usize {
            return false;
        }
        if class != 0 && (!ptr_ok(this as usize, 8) || unsafe { *(this as *const usize) } != class)
        {
            return false;
        }
        let offset = self.other_player_offset.load(Ordering::Relaxed);
        if offset != 0 {
            let Some(address) = (this as usize).checked_add(offset) else {
                return false;
            };
            if !ptr_ok(address, 1) || unsafe { *(address as *const u8) } != 0 {
                return false;
            }
        }
        true
    }

    fn observed(&self) {
        self.calls.fetch_add(1, Ordering::Relaxed);
        let feature = self.feature.load(Ordering::Relaxed);
        if feature < 24 {
            crate::features::note_callback(1 << (8 + feature));
        }
        crate::features::bump_hook_telemetry(None);
    }
}

pub(crate) static CALLBACKS: [Callback; COUNT] = [const { Callback::new() }; COUNT];
static ROOTS: [Mutex<Option<GcScope>>; 24] = [const { Mutex::new(None) }; 24];
static CRITDMG_SCOPES: [Callback; 2] = [const { Callback::new() }; 2];

pub(crate) unsafe fn prepare_critdmg(ch: &GameChain) -> bool {
    let Ok(mut saved) = ROOTS[16].lock() else {
        return false;
    };
    let mut roots = GcScope::new();
    for instance in [ch.bstatus, ch.pss] {
        if instance.is_null() || !ptr_ok(instance as usize, 8) || roots.try_pin(instance).is_none()
        {
            return false;
        }
    }
    for (scope, instance) in CRITDMG_SCOPES.iter().zip([ch.bstatus, ch.pss]) {
        scope.instance.store(instance as usize, Ordering::Relaxed);
        scope
            .class
            .store(unsafe { *(instance as *const usize) }, Ordering::Relaxed);
        scope.enabled.store(true, Ordering::Release);
    }
    *saved = Some(roots);
    true
}

pub(crate) fn critdmg_matches(instance: *mut c_void) -> bool {
    CRITDMG_SCOPES.iter().any(|scope| scope.matches(instance))
}

pub(crate) fn set_value(feature: usize, value: i32) {
    for callback in &CALLBACKS {
        if callback.feature.load(Ordering::Acquire) == feature {
            callback.value.store(value, Ordering::Release);
        }
    }
}

pub(crate) fn activate(feature: usize) {
    for callback in &CALLBACKS {
        if callback.feature.load(Ordering::Acquire) == feature {
            callback.enabled.store(true, Ordering::Release);
        }
    }
}

pub(crate) fn deactivate(feature: usize) {
    if feature == 16 {
        for scope in &CRITDMG_SCOPES {
            scope.enabled.store(false, Ordering::Release);
        }
    }
    for callback in &CALLBACKS {
        if callback.feature.load(Ordering::Acquire) == feature {
            callback.enabled.store(false, Ordering::Release);
        }
    }
}

pub(crate) fn release(feature: usize) {
    deactivate(feature);
    for (slot, callback) in CALLBACKS.iter().enumerate() {
        if callback.feature.load(Ordering::Acquire) == feature {
            log_fmt(format_args!(
                "[faunas] feature={} slot={} executed={}",
                feature,
                slot,
                callback.calls.load(Ordering::Acquire)
            ));
        }
    }
    if let Ok(mut roots) = ROOTS[feature].lock() {
        *roots = None;
    }
}

/// Resolve names and types on this runtime, never Android offsets or saved pointers.
pub(crate) unsafe fn prepare(
    feature: usize,
    specs: &[HookSpec],
    ch: &GameChain,
    value: i32,
) -> bool {
    let Ok(mut saved_roots) = ROOTS[feature].lock() else {
        return false;
    };
    let mut roots = GcScope::new();
    for spec in specs {
        let Some(sink) = spec.orig_sink else {
            return false;
        };
        if feature == 0 && core::ptr::eq(sink, &crate::critical_path::ORIG_SECONDARY_GETTER) {
            if roots.try_pin(ch.status).is_none() {
                return false;
            }
            continue;
        }
        let Some(callback) = CALLBACKS.iter().find(|c| core::ptr::eq(&c.original, sink)) else {
            return false;
        };
        callback.enabled.store(false, Ordering::Release);
        let instance = if feature == 5 {
            ch.status
        } else {
            match spec.target {
                HookTargetClass::PlayerBattleStatus => ch.bstatus,
                HookTargetClass::PlayerSecondaryStatus => ch.pss,
                HookTargetClass::PlayerStatusBase => ch.status,
                HookTargetClass::BattleManagerBase => ch.pbm,
                _ => core::ptr::null_mut(),
            }
        };
        let mut class = 0;
        let mut other_offset = 0;
        let requires_instance = feature == 5
            || matches!(
                spec.target,
                HookTargetClass::PlayerBattleStatus
                    | HookTargetClass::PlayerSecondaryStatus
                    | HookTargetClass::PlayerStatusBase
                    | HookTargetClass::BattleManagerBase
            );
        if requires_instance {
            if instance.is_null()
                || !ptr_ok(instance as usize, 8)
                || roots.try_pin(instance).is_none()
            {
                return false;
            }
            class = unsafe { *(instance as *const usize) };
        }
        if matches!(feature, 0 | 8 | 11..=13 | 22)
            && matches!(
                spec.target,
                HookTargetClass::Named(_)
                    | HookTargetClass::PlayerAttackBase
                    | HookTargetClass::NormalAttackAction
            )
        {
            let name = match spec.target {
                HookTargetClass::Named(name) => name,
                HookTargetClass::PlayerAttackBase => "PlayerAttackBase",
                HookTargetClass::NormalAttackAction => "NormalAttackAction",
                _ => return false,
            };
            let klass = unsafe { find_class(ch.image, ch.domain, "", name) };
            let field = unsafe { get_field(klass, "<IsOtherPlayer>k__BackingField") };
            if !unsafe { field_type_matches(field, "System.Boolean") } {
                return false;
            }
            let Some(api) = api() else {
                return false;
            };
            let offset = unsafe { (api.field_get_offset)(field) };
            if !(0x10..=0x4000).contains(&offset) {
                return false;
            }
            // Base attack methods also run on derived skill classes.
            class = if matches!(spec.target, HookTargetClass::PlayerAttackBase) {
                0
            } else {
                klass as usize
            };
            other_offset = offset as usize;
        }
        callback.feature.store(feature, Ordering::Release);
        callback
            .instance
            .store(instance as usize, Ordering::Relaxed);
        callback.class.store(class, Ordering::Relaxed);
        callback
            .other_player_offset
            .store(other_offset, Ordering::Relaxed);
        callback.value.store(value, Ordering::Release);
        callback.calls.store(0, Ordering::Release);
    }
    *saved_roots = Some(roots);
    true
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct Vector3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

macro_rules! callback {
    ($name:ident, ($($arg:ident: $ty:ty),*), $ret:ty, $value:expr, $fallback:expr) => {
        #[allow(clippy::unused_unit)]
        pub(crate) unsafe extern "C" fn $name<const SLOT: usize>(
            this: *mut c_void, $($arg: $ty,)* method: *const c_void,
        ) -> $ret {
            let callback = &CALLBACKS[SLOT];
            if callback.matches(this) {
                callback.observed();
                return ($value)(callback.value.load(Ordering::Acquire));
            }
            let original = callback.original.load(Ordering::Acquire);
            if original == 0 { return $fallback; }
            let native: unsafe extern "C" fn(*mut c_void, $($ty,)* *const c_void) -> $ret =
                unsafe { core::mem::transmute(original) };
            unsafe { native(this, $($arg,)* method) }
        }
    };
}

callback!(int0, (), i32, |v| v, 0);
callback!(int1, (arg: i32), i32, |v| v, 0);
callback!(bool0, (), bool, |v| v != 0, false);
callback!(crit, (rate: i32, mob: *mut c_void), bool, |v| v != 0, false);
callback!(magic_crit, (status: *mut c_void, mob: *mut c_void), bool, |v| v != 0, false);
callback!(magic_crit_base, (base: i32, status: *mut c_void, mob: *mut c_void), bool, |v| v != 0, false);
callback!(reaction, (guard: i32, evade: i32), i32, |v| v, 0);
callback!(create_action, (pattern: *mut c_void), *mut c_void, |_| core::ptr::null_mut(), core::ptr::null_mut());
callback!(range, (source: *mut c_void, target: *mut c_void, radius: f32), bool, |v| v != 0, false);
callback!(skip, (), (), |_| (), ());
callback!(gauge, (value: i32), (), |_| (), ());
callback!(gauge2, (value: i32, maximum: i32), (), |_| (), ());
callback!(drop_box, (data: *mut c_void), i32, |_| 0, 0);
callback!(enemy_size, (), f32, |_| 62.0, 0.0);
callback!(critical_rate, (), i32, |_| 1000, 0);
callback!(label, (kind: i32, value: i32, position: Vector3), (), |_| (), ());
callback!(resist_label, (flag: bool, value: i32, position: Vector3, scale: f32), (), |_| (), ());

/// Zero skips the Windows chanting state. Preserve native instantaneous
/// skills and side effects, but never turn a positive rate into zero.
pub(crate) unsafe extern "C" fn cast_rate<const SLOT: usize>(
    this: *mut c_void,
    cspd: i32,
    method: *const c_void,
) -> i32 {
    let callback = &CALLBACKS[SLOT];
    let original = callback.original.load(Ordering::Acquire);
    if original == 0 {
        return 100;
    }
    let native: unsafe extern "C" fn(*mut c_void, i32, *const c_void) -> i32 =
        unsafe { core::mem::transmute(original) };
    let rate = unsafe { native(this, cspd, method) };
    if callback.matches(this) {
        callback.observed();
        if rate > 0 {
            return rate.min(callback.value.load(Ordering::Acquire).clamp(1, 100));
        }
    }
    rate
}

/// Sustain farming: shrink a positive MP cost to a percent of native.
///
/// Never zero: zero cost was rejected live with 117 SkillNotCostPayed, so the
/// floor is 1 and native zero-cost skills stay zero. Never raises: the result
/// is at most native, so a high-native character cannot be pushed above its
/// own value. Foreign actors forward native via `matches()`.
pub(crate) unsafe extern "C" fn mp_cost<const SLOT: usize>(
    this: *mut c_void,
    player_action: *mut c_void,
    method: *const c_void,
) -> i32 {
    let callback = &CALLBACKS[SLOT];
    let original = callback.original.load(Ordering::Acquire);
    if original == 0 {
        return 0;
    }
    let native: unsafe extern "C" fn(*mut c_void, *mut c_void, *const c_void) -> i32 =
        unsafe { core::mem::transmute(original) };
    let cost = unsafe { native(this, player_action, method) };
    if callback.matches(this) {
        callback.observed();
        if cost > 0 {
            let pct = callback.value.load(Ordering::Acquire).clamp(1, 100);
            return (cost.saturating_mul(pct) / 100).max(1).min(cost);
        }
    }
    cost
}

/// Sustain farming: experimental reuse-delay override (live-only).
///
/// `SkillDelayManager` is a client-local singleton, so there is no per-actor
/// scoping to preserve: ON returns true, OFF forwards native. The server
/// still gates reuse via `OnSkillDelayStandbyOk(SkillReusePermissioneEvent)`,
/// which this client gate cannot waive — a live smoke decides acceptance.
pub(crate) unsafe extern "C" fn skill_available<const SLOT: usize>(
    this: *mut c_void,
    skill_id: i32,
    method: *const c_void,
) -> bool {
    let callback = &CALLBACKS[SLOT];
    if callback.matches(this) {
        callback.observed();
        return true;
    }
    let original = callback.original.load(Ordering::Acquire);
    if original == 0 {
        return false;
    }
    let native: unsafe extern "C" fn(*mut c_void, i32, *const c_void) -> bool =
        unsafe { core::mem::transmute(original) };
    unsafe { native(this, skill_id, method) }
}

/// Enter the verified caller scope only while the bound local normal attack runs.
pub(crate) unsafe extern "C" fn normal_critical<const SLOT: usize>(
    this: *mut c_void,
    player: *mut c_void,
    mob: *mut c_void,
    method: *const c_void,
) {
    let callback = &CALLBACKS[SLOT];
    let original = callback.original.load(Ordering::Acquire);
    if original == 0 {
        return;
    }
    let native: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *const c_void) =
        unsafe { core::mem::transmute(original) };
    if callback.matches(this) {
        callback.observed();
        let _scope = crate::critical_path::NormalAttackScope::enter();
        unsafe {
            native(this, player, mob, method);
        }
    } else {
        unsafe {
            native(this, player, mob, method);
        }
    }
}

// These are static managed methods: no hidden `this` precedes `status`.
pub(crate) unsafe extern "C" fn resist_damage<const SLOT: usize>(
    status: *mut c_void,
    power: i32,
    rate: f32,
    method: *const c_void,
) -> i32 {
    let callback = &CALLBACKS[SLOT];
    if callback.matches(status) {
        callback.observed();
        return callback.value.load(Ordering::Acquire);
    }
    let original = callback.original.load(Ordering::Acquire);
    if original == 0 {
        return 0;
    }
    let native: unsafe extern "C" fn(*mut c_void, i32, f32, *const c_void) -> i32 =
        unsafe { core::mem::transmute(original) };
    unsafe { native(status, power, rate, method) }
}

#[cfg(test)]
mod tests {
    use super::*;
    static CAST_ARGS: Mutex<Vec<(usize, i32, usize)>> = Mutex::new(Vec::new());
    unsafe extern "C" fn cast_native(this: *mut c_void, cspd: i32, method: *const c_void) -> i32 {
        CAST_ARGS
            .lock()
            .unwrap()
            .push((this as usize, cspd, method as usize));
        cspd
    }

    #[test]
    fn cast_adapter_preserves_native_zero_arguments_foreign_and_off_calls() {
        let callback = &CALLBACKS[6];
        let mut local = [123usize, 0];
        let mut foreign = [123usize, 0];
        let this = local.as_mut_ptr().cast();
        let other = foreign.as_mut_ptr().cast();
        callback
            .original
            .store(cast_native as *const () as usize, Ordering::Release);
        callback.instance.store(this as usize, Ordering::Relaxed);
        callback.class.store(123, Ordering::Relaxed);
        callback
            .value
            .store(crate::faunas::CAST_RATE_PERCENT, Ordering::Relaxed);
        callback.enabled.store(true, Ordering::Release);
        unsafe {
            assert_eq!(cast_rate::<6>(this, 100, 7usize as *const c_void), 10);
            assert_eq!(cast_rate::<6>(this, 5, 8usize as *const c_void), 5);
            assert_eq!(cast_rate::<6>(this, 0, 9usize as *const c_void), 0);
            assert_eq!(cast_rate::<6>(other, 100, 10usize as *const c_void), 100);
            callback.enabled.store(false, Ordering::Release);
            assert_eq!(cast_rate::<6>(this, 100, 11usize as *const c_void), 100);
        }
        assert_eq!(
            *CAST_ARGS.lock().unwrap(),
            [
                (this as usize, 100, 7),
                (this as usize, 5, 8),
                (this as usize, 0, 9),
                (other as usize, 100, 10),
                (this as usize, 100, 11),
            ]
        );
    }

    static NORMAL_CALLS: Mutex<Vec<(bool, usize, usize, usize)>> = Mutex::new(Vec::new());
    unsafe extern "C" fn normal_native(
        _this: *mut c_void,
        player: *mut c_void,
        mob: *mut c_void,
        method: *const c_void,
    ) {
        NORMAL_CALLS.lock().unwrap().push((
            crate::critical_path::in_normal_attack(),
            player as usize,
            mob as usize,
            method as usize,
        ));
    }

    #[test]
    fn normal_adapter_scopes_only_local_calls_and_preserves_arguments() {
        let callback = &CALLBACKS[70];
        callback
            .original
            .store(normal_native as *const () as usize, Ordering::Release);
        callback.class.store(123, Ordering::Relaxed);
        callback.other_player_offset.store(16, Ordering::Relaxed);
        callback.enabled.store(true, Ordering::Release);
        let mut object = [123usize, 0, 0];
        unsafe {
            normal_critical::<70>(
                object.as_mut_ptr().cast(),
                7usize as *mut c_void,
                9usize as *mut c_void,
                11usize as *const c_void,
            );
        }
        assert!(!crate::critical_path::in_normal_attack());
        object[2] = 1;
        unsafe {
            normal_critical::<70>(
                object.as_mut_ptr().cast(),
                8usize as *mut c_void,
                10usize as *mut c_void,
                12usize as *const c_void,
            );
        }
        callback.enabled.store(false, Ordering::Release);
        object[2] = 0;
        unsafe {
            normal_critical::<70>(
                object.as_mut_ptr().cast(),
                9usize as *mut c_void,
                11usize as *mut c_void,
                13usize as *const c_void,
            );
        }
        assert_eq!(
            *NORMAL_CALLS.lock().unwrap(),
            [(true, 7, 9, 11), (false, 8, 10, 12), (false, 9, 11, 13)]
        );
    }

    static STATIC_ARGS: Mutex<Vec<(usize, i32, u32, usize)>> = Mutex::new(Vec::new());
    unsafe extern "C" fn static_native(
        status: *mut c_void,
        power: i32,
        rate: f32,
        method: *const c_void,
    ) -> i32 {
        STATIC_ARGS
            .lock()
            .unwrap()
            .push((status as usize, power, rate.to_bits(), method as usize));
        417
    }

    #[test]
    fn static_forwarding_keeps_float_position_without_hidden_this() {
        let c = &CALLBACKS[9];
        let mut object = [321usize, 0];
        let status = object.as_mut_ptr().cast();
        c.original
            .store(static_native as *const () as usize, Ordering::Release);
        c.instance.store(status as usize, Ordering::Relaxed);
        c.value.store(0, Ordering::Relaxed);
        c.enabled.store(true, Ordering::Release);
        unsafe {
            assert_eq!(
                resist_damage::<9>(status, 95, 1.25, 28usize as *const c_void),
                0
            );
            c.enabled.store(false, Ordering::Release);
            assert_eq!(
                resist_damage::<9>(status, 96, 1.5, 29usize as *const c_void),
                417
            );
        }
        assert_eq!(
            *STATIC_ARGS.lock().unwrap(),
            [(status as usize, 96, 1.5f32.to_bits(), 29)]
        );
    }

    #[test]
    fn enemy_size_uses_float_return_register() {
        let c = &CALLBACKS[67];
        c.enabled.store(true, Ordering::Release);
        let mut object = [321usize, 0];
        assert_eq!(
            unsafe { enemy_size::<67>(object.as_mut_ptr().cast(), core::ptr::null()) },
            62.0
        );
        c.enabled.store(false, Ordering::Release);
    }

    static ARGUMENTS: Mutex<Vec<(usize, i32, usize, usize)>> = Mutex::new(Vec::new());
    unsafe extern "C" fn original(
        this: *mut c_void,
        rate: i32,
        mob: *mut c_void,
        method: *const c_void,
    ) -> bool {
        ARGUMENTS
            .lock()
            .unwrap()
            .push((this as usize, rate, mob as usize, method as usize));
        false
    }

    #[test]
    fn typed_forwarding_preserves_arguments_and_scopes_local_calls() {
        let c = &CALLBACKS[0];
        let mut local = [123usize, 0];
        let mut other = [123usize, 0];
        let this = local.as_mut_ptr().cast();
        c.original
            .store(original as *const () as usize, Ordering::Release);
        c.instance.store(this as usize, Ordering::Relaxed);
        c.class.store(123, Ordering::Relaxed);
        c.value.store(1, Ordering::Relaxed);
        c.feature.store(0, Ordering::Relaxed);
        c.enabled.store(true, Ordering::Release);
        unsafe {
            assert!(crit::<0>(
                this,
                42,
                17usize as *mut c_void,
                29usize as *const c_void
            ));
            assert!(!crit::<0>(
                other.as_mut_ptr().cast(),
                43,
                18usize as *mut c_void,
                30usize as *const c_void
            ));
            c.enabled.store(false, Ordering::Release);
            assert!(!crit::<0>(
                this,
                44,
                19usize as *mut c_void,
                31usize as *const c_void
            ));
        }
        let calls = ARGUMENTS.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].1, 43);
        assert_eq!(calls[1], (this as usize, 44, 19, 31));
        assert_eq!(c.calls.load(Ordering::Acquire), 1);
        c.feature.store(usize::MAX, Ordering::Release);
    }

    #[test]
    fn skill_scope_rejects_other_players_and_changed_classes() {
        let c = Callback::new();
        let mut object = [123usize, 0, 0];
        let this = object.as_mut_ptr().cast();
        c.enabled.store(true, Ordering::Release);
        c.class.store(123, Ordering::Relaxed);
        c.other_player_offset.store(16, Ordering::Relaxed);
        assert!(c.matches(this));
        object[2] = 1;
        assert_eq!(object[2], 1);
        assert!(!c.matches(this));
        object[2] = 0;
        object[0] = 456;
        assert_eq!(object[0], 456);
        assert!(!c.matches(this));
        assert!(!c.matches(core::ptr::null_mut()));
    }

    #[test]
    fn mp_cost_adapter_shrinks_positive_preserves_zero_and_foreign() {
        let c = &CALLBACKS[64];
        static NATIVE_COST: AtomicI32 = AtomicI32::new(0);
        unsafe extern "C" fn cost_native(
            _this: *mut c_void,
            _player: *mut c_void,
            _method: *const c_void,
        ) -> i32 {
            NATIVE_COST.load(Ordering::Acquire)
        }
        let mut local = [777usize, 0];
        let mut foreign = [777usize, 0];
        let this = local.as_mut_ptr().cast();
        let other = foreign.as_mut_ptr().cast();
        c.original
            .store(cost_native as *const () as usize, Ordering::Release);
        c.instance.store(this as usize, Ordering::Relaxed);
        c.class.store(777, Ordering::Relaxed);
        // No other-player offset on this unit scope: instance+class gate only.
        c.other_player_offset.store(0, Ordering::Relaxed);
        c.value.store(10, Ordering::Relaxed);
        c.enabled.store(true, Ordering::Release);
        unsafe {
            NATIVE_COST.store(200, Ordering::Release);
            assert_eq!(
                mp_cost::<64>(this, core::ptr::null_mut(), core::ptr::null()),
                20
            );
            NATIVE_COST.store(5, Ordering::Release);
            assert_eq!(
                mp_cost::<64>(this, core::ptr::null_mut(), core::ptr::null()),
                1
            );
            NATIVE_COST.store(0, Ordering::Release);
            assert_eq!(
                mp_cost::<64>(this, core::ptr::null_mut(), core::ptr::null()),
                0
            );
            NATIVE_COST.store(200, Ordering::Release);
            assert_eq!(
                mp_cost::<64>(other, core::ptr::null_mut(), core::ptr::null()),
                200
            );
            c.enabled.store(false, Ordering::Release);
            assert_eq!(
                mp_cost::<64>(this, core::ptr::null_mut(), core::ptr::null()),
                200
            );
        }
        c.instance.store(0, Ordering::Relaxed);
        c.class.store(0, Ordering::Relaxed);
    }

    #[test]
    fn skill_available_forces_true_only_while_on() {
        let c = &CALLBACKS[65];
        static NATIVE_AVAIL: AtomicBool = AtomicBool::new(false);
        unsafe extern "C" fn avail_native(
            _this: *mut c_void,
            _skill: i32,
            _method: *const c_void,
        ) -> bool {
            NATIVE_AVAIL.load(Ordering::Acquire)
        }
        let mut singleton = [0usize];
        let this = singleton.as_mut_ptr().cast();
        c.original
            .store(avail_native as *const () as usize, Ordering::Release);
        c.instance.store(0, Ordering::Relaxed);
        c.class.store(0, Ordering::Relaxed);
        c.other_player_offset.store(0, Ordering::Relaxed);
        c.enabled.store(true, Ordering::Release);
        unsafe {
            NATIVE_AVAIL.store(false, Ordering::Release);
            assert!(skill_available::<65>(this, 7, core::ptr::null()));
            c.enabled.store(false, Ordering::Release);
            assert!(!skill_available::<65>(this, 7, core::ptr::null()));
            NATIVE_AVAIL.store(true, Ordering::Release);
            assert!(skill_available::<65>(this, 7, core::ptr::null()));
        }
    }
}
