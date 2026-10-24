//! Active IL2CPP feature pipelines, declarative `HookSpec` engine, O(1) combat offset cache,
//! and real-time `SharedTelemetry` publisher (`supported commands`).

#![allow(function_casts_as_integer, dead_code, unused_imports)]

use crate::chain::{is_valid_il2cpp_class, is_valid_il2cpp_obj, read_i32_field, GameChain};
use crate::combat_state::{MpBinding, RangeBinding, RangeSnapshot, EXTENDED_RANGE_WORLD_UNITS};
use crate::critical_path;
use crate::hook::MethodHookEntry;
use crate::il2cpp::{
    api, call_i32_0, call_ptr0, call_void0, call_void_2i32, call_void_bool, call_void_f32,
    call_void_i32, call_void_i32_bool, field_type_matches, find_class, find_nested_class,
    get_field, get_method, get_method_exact, read_inst_ptr, GcScope, MethodSignature,
};
use crate::scoped_callback::ScopedCallback;
use crate::win::{fn_readable, log_fmt, page_write_ok, ptr_ok};
use core::ffi::c_void;
use core::mem::transmute;
use core::ptr;
use core::sync::atomic::{
    AtomicBool, AtomicI32, AtomicU32, AtomicU64, AtomicU8, AtomicUsize, Ordering,
};
use protocol::{
    CRIT_DAMAGE_ADD_100, CRIT_DAMAGE_ADD_1000, CRIT_DAMAGE_ADD_200, CRIT_DAMAGE_ADD_300,
    CRIT_DAMAGE_BONUS_POINTS, CRIT_DAMAGE_MAX_RATIO, MP_AMPR_BONUS_10, MP_AMPR_BONUS_25,
    MP_AMPR_BONUS_50, SKILL_CRIT_CAP_POINTS, TOGGLE_ALWAYS_CRIT, TOGGLE_ANTI_RIGIDITY,
    TOGGLE_INSTANT_ATK, TOGGLE_INVINCIBILITY, TOGGLE_NO_COLLISION, TOGGLE_RANGE_30M,
    TOGGLE_SPEED_BOOST, TOGGLE_UNLIMITED_MP,
};
use std::cell::Cell;
use std::sync::{Mutex, RwLock};

#[cfg(not(test))]
use protocol::telemetry;
#[cfg(test)]
fn telemetry() -> Option<&'static protocol::TelemetryWriter> {
    None
}

// Global atomic state for hooked callbacks
static INVINCIBILITY_ID: AtomicI32 = AtomicI32::new(0);

static SPEED_ACTIVE: AtomicBool = AtomicBool::new(false);
// Values from the supplied v0.96.1 source: getters and stored fields differ.
const SPEED_GETTER_VALUE: f32 = 15.0;
const SPEED_FIELD_VALUE: f32 = 20.0;
static SPEED_FIELDS: Mutex<Option<MovementFields>> = Mutex::new(None);
static NO_COLLISION_FIELDS: Mutex<Option<MovementFields>> = Mutex::new(None);
static SPEED_CALLBACKS: [ScopedCallback; 6] = [const { ScopedCallback::new() }; 6];
static NO_COLLISION_CALLBACKS: [ScopedCallback; 6] = [const { ScopedCallback::new() }; 6];
const NO_COLLISION_VALUES: [bool; 6] = [false, true, false, false, true, false];

unsafe fn forward_move_speed(
    callback: &ScopedCallback,
    this: *mut c_void,
    method: *const c_void,
) -> f32 {
    let address = callback.original.load(Ordering::Acquire);
    if address == 0 {
        return 0.0;
    }
    let native: unsafe extern "C" fn(*mut c_void, *const c_void) -> f32 =
        unsafe { transmute(address) };
    unsafe { native(this, method) }
}

unsafe extern "C" fn local_move_speed<const SLOT: usize>(
    this: *mut c_void,
    method: *const c_void,
) -> f32 {
    let callback = &SPEED_CALLBACKS[SLOT];
    let active = SPEED_ACTIVE.load(Ordering::Acquire);
    if !callback.matches(this, active) {
        return unsafe { forward_move_speed(callback, this, method) };
    }
    note_callback(TOGGLE_SPEED_BOOST);
    SPEED_GETTER_VALUE
}

unsafe extern "C" fn local_speed_dash(this: *mut c_void, method: *const c_void) -> bool {
    let callback = &SPEED_CALLBACKS[5];
    let active = SPEED_ACTIVE.load(Ordering::Acquire);
    if callback.matches(this, active) {
        note_callback(TOGGLE_SPEED_BOOST);
    }
    unsafe { callback.bool_getter(this, method, active, true) }
}

unsafe extern "C" fn local_no_collision<const SLOT: usize>(
    this: *mut c_void,
    method: *const c_void,
) -> bool {
    let callback = &NO_COLLISION_CALLBACKS[SLOT];
    let active = NO_COLLISION_ACTIVE.load(Ordering::Acquire);
    if callback.matches(this, active) {
        note_callback(TOGGLE_NO_COLLISION);
    }
    unsafe { callback.bool_getter(this, method, active, NO_COLLISION_VALUES[SLOT]) }
}

unsafe fn bind_movement_callbacks<const N: usize>(
    callbacks: &[ScopedCallback; N],
    instances: [*mut c_void; N],
    snapshot: &mut MovementFields,
) -> bool {
    for (index, &instance) in instances.iter().enumerate() {
        if instance.is_null()
            || (!instances[..index].contains(&instance)
                && snapshot.roots.try_pin(instance).is_none())
        {
            return false;
        }
    }
    for (callback, instance) in callbacks.iter().zip(instances) {
        if !unsafe { callback.bind(instance) } {
            for callback in callbacks {
                callback.clear();
            }
            return false;
        }
    }
    true
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MovementValue {
    Float(u32),
    Bool(u8),
}

impl MovementValue {
    fn float(value: f32) -> Self {
        Self::Float(value.to_bits())
    }

    fn type_name(self) -> &'static str {
        match self {
            Self::Float(_) => "System.Single",
            Self::Bool(_) => "System.Boolean",
        }
    }

    fn width(self) -> usize {
        match self {
            Self::Float(_) => 4,
            Self::Bool(_) => 1,
        }
    }

    unsafe fn read_like(self, address: usize) -> Option<Self> {
        if !ptr_ok(address, self.width()) || !address.is_multiple_of(self.width()) {
            return None;
        }
        match self {
            Self::Float(_) => {
                let bits = unsafe { (&*(address as *const AtomicU32)).load(Ordering::Acquire) };
                Some(Self::Float(bits))
            }
            Self::Bool(_) => {
                let value = unsafe { (&*(address as *const AtomicU8)).load(Ordering::Acquire) };
                Some(Self::Bool(value))
            }
        }
    }

    unsafe fn replace(self, address: usize, replacement: Self) -> bool {
        if !page_write_ok(address, self.width()) || !address.is_multiple_of(self.width()) {
            return false;
        }
        match (self, replacement) {
            (Self::Float(old), Self::Float(new)) => unsafe {
                (&*(address as *const AtomicU32))
                    .compare_exchange(old, new, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
            },
            (Self::Bool(old), Self::Bool(new)) => unsafe {
                (&*(address as *const AtomicU8))
                    .compare_exchange(old, new, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
            },
            _ => false,
        }
    }
}

struct MovementField {
    object: usize,
    class: usize,
    offset: usize,
    original: MovementValue,
    replacement: MovementValue,
    owned: bool,
}

impl MovementField {
    unsafe fn address(&self) -> Option<usize> {
        if !unsafe { is_valid_il2cpp_obj(self.object as *const c_void) }
            || unsafe { *(self.object as *const usize) } != self.class
        {
            return None;
        }
        self.object.checked_add(self.offset)
    }

    unsafe fn apply(&mut self) -> bool {
        let Some(address) = (unsafe { self.address() }) else {
            return false;
        };
        if self.original == self.replacement {
            return unsafe { self.original.read_like(address) } == Some(self.original);
        }
        if unsafe { self.original.replace(address, self.replacement) } {
            self.owned = true;
            true
        } else {
            false
        }
    }

    unsafe fn restore(&mut self) -> bool {
        if !self.owned {
            return true;
        }
        let Some(address) = (unsafe { self.address() }) else {
            return false;
        };
        let Some(current) = (unsafe { self.replacement.read_like(address) }) else {
            return false;
        };
        if current != self.replacement
            || unsafe { self.replacement.replace(address, self.original) }
        {
            self.owned = false;
            true
        } else {
            // A concurrent native update wins; retry checks it without overwriting it.
            false
        }
    }
}

/// Strong handles keep the exact original instances alive until their edits are retired.
struct MovementFields {
    fields: Vec<MovementField>,
    roots: GcScope,
}

impl MovementFields {
    fn new() -> Self {
        Self {
            fields: Vec::new(),
            roots: GcScope::new(),
        }
    }

    unsafe fn capture(
        &mut self,
        object: *mut c_void,
        class: *mut c_void,
        name: &str,
        replacement: MovementValue,
    ) -> Result<(), &'static str> {
        if !unsafe { is_valid_il2cpp_obj(object) }
            || unsafe { *(object as *const usize) } != class as usize
        {
            return Err("movement owner class mismatch");
        }
        if !self
            .fields
            .iter()
            .any(|field| field.object == object as usize)
        {
            self.roots
                .try_pin(object)
                .ok_or("movement owner cannot be rooted")?;
        }
        let api = api().ok_or("movement API unavailable")?;
        let field = unsafe { get_field(class, name) };
        if !unsafe { field_type_matches(field, replacement.type_name()) } {
            return Err("movement field missing or type mismatch");
        }
        let offset = unsafe { (api.field_get_offset)(field) };
        if !(0x10..=0x4000).contains(&offset) {
            return Err("movement field offset invalid");
        }
        let address = (object as usize)
            .checked_add(offset as usize)
            .ok_or("movement address overflow")?;
        let original = unsafe { replacement.read_like(address) }.ok_or("movement value invalid")?;
        if match original {
            MovementValue::Float(bits) => !f32::from_bits(bits).is_finite(),
            MovementValue::Bool(value) => value > 1,
        } {
            return Err("movement value outside native type");
        }
        self.fields.push(MovementField {
            object: object as usize,
            class: class as usize,
            offset: offset as usize,
            original,
            replacement,
            owned: false,
        });
        Ok(())
    }

    unsafe fn apply(&mut self) -> bool {
        let mut ok = true;
        for field in &mut self.fields {
            ok &= unsafe { field.apply() };
        }
        ok
    }

    unsafe fn capture_fields(
        &mut self,
        object: *mut c_void,
        fields: &[(&str, MovementValue)],
    ) -> Result<(), &'static str> {
        if !unsafe { is_valid_il2cpp_obj(object) } {
            return Err("movement owner unavailable");
        }
        let class = unsafe { *(object as *const *mut c_void) };
        for &(name, value) in fields {
            unsafe { self.capture(object, class, name, value) }?;
        }
        Ok(())
    }

    unsafe fn restore(&mut self) -> bool {
        let mut ok = true;
        for field in self.fields.iter_mut().rev() {
            ok &= unsafe { field.restore() };
        }
        ok
    }
}

struct MovementFeature<'a, const N: usize> {
    fields: &'a Mutex<Option<MovementFields>>,
    hooks: &'a Mutex<[MethodHookEntry; N]>,
    callbacks: &'a [ScopedCallback; N],
    active: &'a AtomicBool,
    bit: u32,
    tag: &'a str,
}

impl<const N: usize> MovementFeature<'_, N> {
    unsafe fn enable(
        &self,
        chain: &GameChain,
        specs: &[HookSpec; N],
        instances: [*mut c_void; N],
        mut snapshot: MovementFields,
    ) -> i32 {
        let Ok(mut fields) = self.fields.lock() else {
            return -8;
        };
        if fields.is_some() || cleanup_pending(self.bit) {
            return -8;
        }
        let Ok(mut hooks) = self.hooks.lock() else {
            return -8;
        };
        if !unsafe { bind_movement_callbacks(self.callbacks, instances, &mut snapshot) } {
            return -8;
        }
        let installed = unsafe { apply_hook_specs(&mut hooks[..], specs, chain, true, self.tag) };
        unsafe { self.finish_activation(snapshot, &mut fields, &mut hooks[..], installed) }
    }

    unsafe fn finish_activation(
        &self,
        mut snapshot: MovementFields,
        fields: &mut Option<MovementFields>,
        hooks: &mut [MethodHookEntry],
        installed: bool,
    ) -> i32 {
        if !installed || !unsafe { snapshot.apply() } {
            self.active.store(false, Ordering::Release);
            for callback in self.callbacks {
                callback.clear();
            }
            let hooks_ok = unsafe { restore_hook_entries(hooks, self.tag) };
            let fields_ok = unsafe { snapshot.restore() };
            let pending = !hooks_ok || !fields_ok;
            if pending {
                *fields = Some(snapshot);
                mark_cleanup_pending(self.bit);
            }
            note_partial(self.bit, pending);
            log_fmt(format_args!(
                "[{}] activation rolled back; complete={}",
                self.tag, !pending
            ));
            return -8;
        }
        let field_count = snapshot.fields.len();
        *fields = Some(snapshot);
        self.active.store(true, Ordering::Release);
        note_partial(self.bit, false);
        if let Some(shm) = telemetry() {
            shm.set_toggle_bit(self.bit, true);
        }
        log_fmt(format_args!(
            "[{}] v0.96.1 movement enabled; {} hooks, {} owned fields",
            self.tag, N, field_count
        ));
        0
    }
}

unsafe fn restore_movement<const N: usize, const C: usize>(
    state: &Mutex<Option<MovementFields>>,
    hooks: &Mutex<[MethodHookEntry; N]>,
    callbacks: &[ScopedCallback; C],
    tag: &str,
) -> bool {
    // Surviving callbacks forward native immediately, including a failed-stop retry.
    for callback in callbacks {
        callback.clear();
    }
    let Ok(mut state) = state.lock() else {
        return false;
    };
    let hooks_ok = hooks
        .lock()
        .map(|mut hooks| unsafe { restore_hook_entries(&mut hooks[..], tag) })
        .unwrap_or(false);
    let fields_ok = state
        .as_mut()
        .map(|state| unsafe { state.restore() })
        .unwrap_or(true);
    if hooks_ok && fields_ok {
        *state = None;
    }
    hooks_ok && fields_ok
}

unsafe fn disable_movement<const N: usize, const C: usize>(
    fields: &Mutex<Option<MovementFields>>,
    hooks: &Mutex<[MethodHookEntry; N]>,
    callbacks: &[ScopedCallback; C],
    active: &AtomicBool,
    bit: u32,
    tag: &str,
) -> i32 {
    active.store(false, Ordering::Release);
    mark_cleanup_pending(bit);
    if !unsafe { restore_movement(fields, hooks, callbacks, tag) } {
        note_partial(bit, true);
        log_fmt(format_args!(
            "[{tag}] restoration incomplete; snapshots retained for retry"
        ));
        return -8;
    }
    LOCAL_CLEANUP_PENDING.fetch_and(!bit, Ordering::Release);
    note_partial(bit, false);
    if let Some(shm) = telemetry() {
        shm.set_toggle_bit(bit, false);
    }
    log_fmt(format_args!(
        "[{tag}] disabled; owned original fields restored"
    ));
    0
}

static NO_COLLISION_ACTIVE: AtomicBool = AtomicBool::new(false);
static RANGE_ACTIVE: AtomicBool = AtomicBool::new(false);
static RANGE_CLEANUP_PENDING: AtomicBool = AtomicBool::new(false);
static INSTANT_ATK_ACTIVE: AtomicBool = AtomicBool::new(false);
static ALWAYS_CRIT_ACTIVE: AtomicBool = AtomicBool::new(false);
static UNLIMITED_MP_ACTIVE: AtomicBool = AtomicBool::new(false);
static MP_CLEANUP_PENDING: AtomicBool = AtomicBool::new(false);
static INVINCIBILITY_ACTIVE: AtomicBool = AtomicBool::new(false);
static CLEAR_DELAY_ACTIVE: AtomicBool = AtomicBool::new(false);

pub(crate) fn note_callback(bit: u32) {
    if let Some(shm) = telemetry() {
        shm.update(|data| {
            if data.active_toggles.load(Ordering::SeqCst) & bit != 0 {
                data.observed_toggles.fetch_or(bit, Ordering::SeqCst);
            }
        });
    }
}

static PARTIAL_FEATURES: AtomicU32 = AtomicU32::new(0);
static LOCAL_CLEANUP_PENDING: AtomicU32 = AtomicU32::new(0);

pub(crate) fn note_partial(bit: u32, partial: bool) {
    if partial {
        PARTIAL_FEATURES.fetch_or(bit, Ordering::Release);
    } else {
        PARTIAL_FEATURES.fetch_and(!bit, Ordering::Release);
    }
    if let Some(shm) = telemetry() {
        shm.update(|data| {
            if partial {
                data.partial_toggles.fetch_or(bit, Ordering::SeqCst);
            } else {
                data.partial_toggles.fetch_and(!bit, Ordering::SeqCst);
            }
        });
    }
}

pub(crate) fn feature_setup_result(bit: u32, enabled: bool) -> i32 {
    if !enabled {
        note_partial(bit, false);
    }
    if enabled && PARTIAL_FEATURES.load(Ordering::Acquire) & bit != 0 {
        log_fmt(format_args!(
            "[setup] partial feature mask={bit}; installed hooks remain active"
        ));
        -11
    } else {
        0
    }
}

// Invincibility: 0..6, timing: 6..11, calculator stats: 11..19.
static LOCAL_CALLBACKS: [ScopedCallback; 19] = [const { ScopedCallback::new() }; 19];

unsafe extern "C" fn local_invincibility_bool<const SLOT: usize>(
    this: *mut c_void,
    method: *const c_void,
) -> bool {
    if LOCAL_CALLBACKS[SLOT].matches(this, INVINCIBILITY_ACTIVE.load(Ordering::Acquire)) {
        note_callback(TOGGLE_INVINCIBILITY);
    }
    unsafe {
        LOCAL_CALLBACKS[SLOT].bool_getter(
            this,
            method,
            INVINCIBILITY_ACTIVE.load(Ordering::Acquire),
            SLOT < 2,
        )
    }
}

unsafe extern "C" fn local_invincibility_damage(
    this: *mut c_void,
    amount: i32,
    flag: bool,
    method: *const c_void,
) {
    let callback = &LOCAL_CALLBACKS[2];
    if callback.matches(this, INVINCIBILITY_ACTIVE.load(Ordering::Acquire)) {
        note_callback(TOGGLE_INVINCIBILITY);
        return;
    }
    let address = callback.original.load(Ordering::Acquire);
    if address != 0 {
        let native: unsafe extern "C" fn(*mut c_void, i32, bool, *const c_void) =
            unsafe { transmute(address) };
        unsafe { native(this, amount, flag, method) };
    }
}

unsafe extern "C" fn local_next_attack<const SLOT: usize>(
    this: *mut c_void,
    aspd: i32,
    method: *const c_void,
) -> f32 {
    let callback = &LOCAL_CALLBACKS[SLOT];
    if callback.matches(this, INSTANT_ATK_ACTIVE.load(Ordering::Acquire)) {
        note_callback(TOGGLE_INSTANT_ATK);
        NEXT_ATTACK_CALLS.fetch_add(1, Ordering::Relaxed);
        return 0.0;
    }
    let address = callback.original.load(Ordering::Acquire);
    if address == 0 {
        return 0.0;
    }
    let native: unsafe extern "C" fn(*mut c_void, i32, *const c_void) -> f32 =
        unsafe { transmute(address) };
    unsafe { native(this, aspd, method) }
}

unsafe extern "C" fn local_motion<const SLOT: usize>(
    this: *mut c_void,
    aspd: i32,
    method: *const c_void,
) -> i32 {
    let callback = &LOCAL_CALLBACKS[SLOT];
    if callback.matches(this, INSTANT_ATK_ACTIVE.load(Ordering::Acquire)) {
        note_callback(TOGGLE_INSTANT_ATK);
        MOTION_SPEED_CALLS.fetch_add(1, Ordering::Relaxed);
        return 60;
    }
    let address = callback.original.load(Ordering::Acquire);
    if address == 0 {
        return 0;
    }
    let native: unsafe extern "C" fn(*mut c_void, i32, *const c_void) -> i32 =
        unsafe { transmute(address) };
    unsafe { native(this, aspd, method) }
}

unsafe extern "C" fn local_stat<const SLOT: usize, const VALUE: i32>(
    this: *mut c_void,
    method: *const c_void,
) -> i32 {
    let enabled = if SLOT == 10 {
        &INSTANT_ATK_ACTIVE
    } else {
        &ALWAYS_CRIT_ACTIVE
    };
    unsafe {
        LOCAL_CALLBACKS[SLOT].i32_getter(this, method, enabled.load(Ordering::Acquire), VALUE)
    }
}

unsafe fn bind_local_callbacks(start: usize, instances: &[*mut c_void]) -> bool {
    for (callback, &instance) in LOCAL_CALLBACKS[start..start + instances.len()]
        .iter()
        .zip(instances)
    {
        if !unsafe { callback.bind(instance) } {
            return false;
        }
    }
    true
}

/// Critical damage request: 0=native, positive=absolute percent, negative=native+N points.
static CUSTOM_CRIT_MULTIPLIER: AtomicI32 = AtomicI32::new(0);

/// Adaptive skill critical damage ladder active toggle.
static ADAPTIVE_LADDER_ACTIVE: AtomicBool = AtomicBool::new(true);

/// Experimental skill-path cap, as a ratio percentage against the native
/// critical damage value. 100 is native and 135 is the configured ceiling;
/// universal server acceptance has not been established. The ladder steps down.
static SKILL_ADAPTIVE_CAP: AtomicI32 = AtomicI32::new(SKILL_CRIT_CAP_POINTS);
/// Consecutive accepted responses since the last rejection.
static SKILL_ACCEPT_STREAK: AtomicI32 = AtomicI32::new(0);
/// The ladder never rises above the configured ceiling.
pub const SKILL_ADAPTIVE_MAX_POINTS: i32 = CRIT_DAMAGE_MAX_RATIO;
/// The ladder never drops below the native value.
pub const SKILL_ADAPTIVE_MIN_RATIO: i32 = 100;
/// One step down when the server rejects a boosted hit.
pub const SKILL_ADAPTIVE_RATIO_STEP: i32 = 15;

fn adaptive_step(cap: i32, accepted: bool, streak: i32) -> (i32, i32) {
    if accepted {
        // A straight accept cannot raise the configured cap.
        (cap, streak.saturating_add(1))
    } else {
        (
            (cap - SKILL_ADAPTIVE_RATIO_STEP).max(SKILL_ADAPTIVE_MIN_RATIO),
            0,
        )
    }
}

pub fn skill_adaptive_cap() -> i32 {
    if !ADAPTIVE_LADDER_ACTIVE.load(Ordering::Acquire) {
        return SKILL_CRIT_CAP_POINTS;
    }
    SKILL_ADAPTIVE_CAP
        .load(Ordering::Relaxed)
        .clamp(SKILL_ADAPTIVE_MIN_RATIO, SKILL_ADAPTIVE_MAX_POINTS)
}

pub(crate) fn set_adaptive_ladder(enabled: bool) {
    let was_enabled = ADAPTIVE_LADDER_ACTIVE.swap(enabled, Ordering::AcqRel);
    if was_enabled && enabled {
        return;
    }
    SKILL_ACCEPT_STREAK.store(0, Ordering::Relaxed);
    SKILL_ADAPTIVE_CAP.store(SKILL_CRIT_CAP_POINTS, Ordering::Relaxed);
    if let Some(shm) = telemetry() {
        shm.update(|data| {
            data.adaptive_enabled
                .store(enabled as u32, Ordering::SeqCst);
            data.skill_cap
                .store(SKILL_CRIT_CAP_POINTS, Ordering::SeqCst);
        });
    }
}

/// A fresh target resets the experimental probe to its configured ceiling.
pub fn note_new_skill_target() {
    if !ADAPTIVE_LADDER_ACTIVE.load(Ordering::Acquire) {
        return;
    }
    SKILL_ACCEPT_STREAK.store(0, Ordering::Relaxed);
    if SKILL_ADAPTIVE_CAP.load(Ordering::Relaxed) < SKILL_ADAPTIVE_MAX_POINTS {
        SKILL_ADAPTIVE_CAP.store(SKILL_ADAPTIVE_MAX_POINTS, Ordering::Relaxed);
        log_fmt(format_args!(
            "[crit_adapt] fresh target: skill cap restored to {}% of native",
            SKILL_ADAPTIVE_MAX_POINTS
        ));
    }
}

/// Accepted response: extend the streak. The cap cannot rise, because the
/// ceiling remains fixed by the implementation.
pub fn note_skill_response_accepted() {
    if !ADAPTIVE_LADDER_ACTIVE.load(Ordering::Acquire) {
        return;
    }
    let cap = SKILL_ADAPTIVE_CAP.load(Ordering::Relaxed);
    let streak = SKILL_ACCEPT_STREAK.load(Ordering::Relaxed);
    let (next_cap, next_streak) = adaptive_step(cap, true, streak);
    SKILL_ACCEPT_STREAK.store(next_streak, Ordering::Relaxed);
    if next_cap != cap {
        SKILL_ADAPTIVE_CAP.store(next_cap, Ordering::Relaxed);
        log_fmt(format_args!(
            "[crit_adapt] straight accepts: skill cap {}% -> {}% of native",
            cap, next_cap
        ));
    }
}

pub(crate) fn note_skill_response_unknown() {
    SKILL_ACCEPT_STREAK.store(0, Ordering::Relaxed);
}

/// Rejected response (AllowDamage=0 on a living mob, or ViolationDamage): step
/// the cap down one rung so the next claim is closer to native.
pub fn note_skill_response_rejected() {
    SKILL_ACCEPT_STREAK.store(0, Ordering::Relaxed);
    let cap = SKILL_ADAPTIVE_CAP.load(Ordering::Relaxed);
    let (next_cap, _) = adaptive_step(cap, false, 0);
    if next_cap != cap {
        SKILL_ADAPTIVE_CAP.store(next_cap, Ordering::Relaxed);
        log_fmt(format_args!(
            "[crit_adapt] server rejected boosted skill: cap {}% -> {}% of native",
            cap, next_cap
        ));
    }
}
/// Requested AMPR bonus in points over the native value: 0 = native recovery.
/// Small calibrated steps only: the server recomputes MP deterministically, so the
/// old flat 2000 reconciled every hit and is retired (live 2026-10-19).
static MP_AMPR_BONUS: AtomicI32 = AtomicI32::new(0);

// Cached original function pointers for forwarding hooks
static ORIG_CALC_BASE_DAMAGE: AtomicUsize = AtomicUsize::new(0);
static ORIG_PAB_CALC_MOB_DMG: AtomicUsize = AtomicUsize::new(0);
static ORIG_NAA_CALC_MOB_DMG: AtomicUsize = AtomicUsize::new(0);
static ORIG_NAA_ACTION_HIT: AtomicUsize = AtomicUsize::new(0);
static ORIG_NAA_DAMAGED: AtomicUsize = AtomicUsize::new(0);
static ORIG_NAA_CALC_BASE_DMG: AtomicUsize = AtomicUsize::new(0);
static ORIG_NAA_CALC_DUAL_SWORD_DMG: AtomicUsize = AtomicUsize::new(0);
pub(crate) static ORIG_PBS_CRIT_DMG: AtomicUsize = AtomicUsize::new(0);
pub(crate) static ORIG_PSS_CRIT_DMG: AtomicUsize = AtomicUsize::new(0);
static ORIG_PBM_OUT_OF_RANGE: AtomicUsize = AtomicUsize::new(0);
static ORIG_CHECK_RANGE_HIT: AtomicUsize = AtomicUsize::new(0);
static ORIG_PENDING_RANGE: AtomicUsize = AtomicUsize::new(0);
static ORIG_NAA_PREPARATION: AtomicUsize = AtomicUsize::new(0);
static ORIG_PBS_AMPR: AtomicUsize = AtomicUsize::new(0);
static ORIG_PSS_AMPR: AtomicUsize = AtomicUsize::new(0);
static ORIG_TPS_AMPR: AtomicUsize = AtomicUsize::new(0);
static ORIG_SUSTAIN_COST: AtomicUsize = AtomicUsize::new(0);
static ORIG_SUSTAIN_COST0: AtomicUsize = AtomicUsize::new(0);
static SUSTAIN_COST_OFFSET: AtomicUsize = AtomicUsize::new(0);
static ORIG_SUSTAIN_DELAY: AtomicUsize = AtomicUsize::new(0);
/// Sustain MP cost percent-of-native (companion-only): 10 = bayar 10% dari native.
static SUSTAIN_MP_COST_PCT: AtomicI32 = AtomicI32::new(10);
static MP_BINDING: RwLock<Option<MpBinding>> = RwLock::new(None);
static RANGE_BINDING: RwLock<Option<RangeBinding>> = RwLock::new(None);
static RANGE_SNAPSHOTS: Mutex<Vec<RangeSnapshot>> = Mutex::new(Vec::new());
static MP_HIT_CALLS: AtomicU64 = AtomicU64::new(0);
static MP_SAMPLES: AtomicU64 = AtomicU64::new(0);
static MP_RECOVERY_CALLS: AtomicU64 = AtomicU64::new(0);
/// Every get_AtkMpRecovery hit, regardless of guard outcome (diagnostic).
static MP_GETTER_CALLS: AtomicU64 = AtomicU64::new(0);
/// Rate-limit cap for [mpdiag] detail lines per process lifetime.
static MP_DIAG_LOGGED: AtomicU64 = AtomicU64::new(0);
/// get_AtkMpRecovery hits on non-player calculator classes (pure observation).
static MP_GETTER_OTHER: AtomicU64 = AtomicU64::new(0);
/// Original getters for the observer slots below; native is always returned.
static ORIG_OBS_AMPR: [AtomicUsize; 3] = [const { AtomicUsize::new(0) }; 3];
static RANGE_PREPARATIONS: AtomicU64 = AtomicU64::new(0);
static RANGE_PENDING_CHECKS: AtomicU64 = AtomicU64::new(0);
static RANGE_PENDING_OVERRIDES: AtomicU64 = AtomicU64::new(0);
static RANGE_ASSIST_OVERRIDES: AtomicU64 = AtomicU64::new(0);
static RANGE_PATCHES: AtomicU64 = AtomicU64::new(0);
static RANGE_HIT_CHECKS: AtomicU64 = AtomicU64::new(0);
static RANGE_HIT_EXTENSIONS: AtomicU64 = AtomicU64::new(0);
static LAST_RECEIVED_MP: AtomicI32 = AtomicI32::new(-1);
static STABILITY_HITS: AtomicU64 = AtomicU64::new(0);
static BASE_DAMAGE_HITS: AtomicU64 = AtomicU64::new(0);
static NORMAL_CALC_HITS: AtomicU64 = AtomicU64::new(0);
static HIT_CHECK_CALLS: AtomicU64 = AtomicU64::new(0);
static NEXT_ATTACK_CALLS: AtomicU64 = AtomicU64::new(0);
static MOTION_SPEED_CALLS: AtomicU64 = AtomicU64::new(0);

// O(1) cached offsets for SkillDamageData fields to avoid string reflection during combat hits
static SDD_CLASS_CACHED: AtomicUsize = AtomicUsize::new(0);
static OFF_SDD_HIT_TYPE: AtomicI32 = AtomicI32::new(0);
static OFF_SDD_REACTION_TYPE: AtomicI32 = AtomicI32::new(0);
static OFF_SDD_TOTAL_DMG: AtomicI32 = AtomicI32::new(0);
static OFF_SDD_VIEW_DMG: AtomicI32 = AtomicI32::new(0);
static OFF_SDD_RAW_DMG: AtomicI32 = AtomicI32::new(0);

/// Validate that the cached `SkillDamageData` class pointer is still live across scene/domain transitions;
/// if unmapped or invalid, reset the O(1) offset cache so it re-resolves cleanly on the next hit.
pub fn validate_or_reset_sdd_cache() {
    let cached = SDD_CLASS_CACHED.load(Ordering::Acquire);
    if cached != 0 && !is_valid_il2cpp_class(cached as *const c_void) {
        SDD_CLASS_CACHED.store(0, Ordering::Release);
        OFF_SDD_HIT_TYPE.store(0, Ordering::Relaxed);
        OFF_SDD_REACTION_TYPE.store(0, Ordering::Relaxed);
        OFF_SDD_TOTAL_DMG.store(0, Ordering::Relaxed);
        OFF_SDD_VIEW_DMG.store(0, Ordering::Relaxed);
        OFF_SDD_RAW_DMG.store(0, Ordering::Relaxed);
    }
}

// O(1) cached offsets for DamageData fields to avoid static offset decay on updates
static DD_CLASS_CACHED: AtomicUsize = AtomicUsize::new(0);
static OFF_DD_HIT_TYPE: AtomicI32 = AtomicI32::new(0x1C);
static OFF_DD_IS_SCRATCH: AtomicI32 = AtomicI32::new(0x30);
static OFF_DD_REACTION_TYPE: AtomicI32 = AtomicI32::new(0x34);
static OFF_DD_GUARD_BREAK: AtomicI32 = AtomicI32::new(0x61);
static OFF_DD_AVOID_BREAK: AtomicI32 = AtomicI32::new(0x62);
static OFF_DD_SDD: AtomicI32 = AtomicI32::new(0x38);

pub fn validate_or_reset_dd_cache() {
    let cached = DD_CLASS_CACHED.load(Ordering::Acquire);
    if cached != 0 && !is_valid_il2cpp_class(cached as *const c_void) {
        DD_CLASS_CACHED.store(0, Ordering::Release);
        OFF_DD_HIT_TYPE.store(0x1C, Ordering::Relaxed);
        OFF_DD_IS_SCRATCH.store(0x30, Ordering::Relaxed);
        OFF_DD_REACTION_TYPE.store(0x34, Ordering::Relaxed);
        OFF_DD_GUARD_BREAK.store(0x61, Ordering::Relaxed);
        OFF_DD_AVOID_BREAK.store(0x62, Ordering::Relaxed);
        OFF_DD_SDD.store(0x38, Ordering::Relaxed);
    }
}

// Declarative Hook Tables

#[derive(Clone, Copy)]
pub(crate) enum HookTargetClass {
    PlayerBattleStatus,
    PlayerSecondaryStatus,
    PlayerAttackBase,
    SkillDamageData,
    NormalAttackAction,
    PlayerStatus,
    PlayerStatusBase,
    PlayerActionManager,
    CharacterMove,
    DamageData,
    AttackData,
    MobDamageData,
    SkillActionBase,
    FieldRayPick,
    CharacterActionManagerBase,
    PlayerGameStatus,
    TemporaryPlayerStatus,
    BattleManagerBase,
    PlayerBattleManager,
    /// A class resolved by simple name at install time.
    ///
    /// The Faunas port targets skill action classes and UI managers that have
    /// no fixed slot in this enum. They are resolved per install, so a class
    /// missing from the installed build fails the hook closed instead of
    /// being silently skipped.
    Named(&'static str),
}

#[derive(Clone, Copy)]
pub(crate) struct HookSpec {
    pub(crate) target: HookTargetClass,
    pub(crate) method: &'static str,
    pub(crate) argc: i32,
    pub(crate) detour: usize,
    pub(crate) orig_sink: Option<&'static AtomicUsize>,
}

unsafe impl Sync for HookSpec {}

static INVINCIBILITY_HOOKS: Mutex<[MethodHookEntry; 6]> = Mutex::new([MethodHookEntry::new(); 6]);
static SPEED_HOOKS: Mutex<[MethodHookEntry; 6]> = Mutex::new([MethodHookEntry::new(); 6]);
static NO_COLLISION_HOOKS: Mutex<[MethodHookEntry; 6]> = Mutex::new([MethodHookEntry::new(); 6]);
static INSTANT_ATK_HOOKS: Mutex<[MethodHookEntry; 5]> = Mutex::new([MethodHookEntry::new(); 5]);
static ALWAYS_CRIT_HOOKS: Mutex<[MethodHookEntry; 24]> = Mutex::new([MethodHookEntry::new(); 24]);
/// The two `get_CriticalDmg` declarations, owned jointly.
///
/// Always-critical and the Faunas critical-damage ladder both replace the
/// critical damage value, and a `MethodInfo` has exactly one owner: the hook
/// engine refuses a second detour on a method that is already hooked, which
/// is what rolled the Faunas ladder back every time always-critical was on.
/// The two features therefore share one owner and one detour, and the detour
/// reads whichever channel is active.
static CRIT_DMG_HOOKS: Mutex<[MethodHookEntry; 2]> = Mutex::new([MethodHookEntry::new(); 2]);
/// Bit 0: always-critical. Bit 1: the Faunas critical-damage ladder.
static CRIT_DMG_OWNERS: AtomicU32 = AtomicU32::new(0);
pub(crate) const CRIT_DMG_OWNER_ALWAYS_CRIT: u32 = 1 << 0;
pub(crate) const CRIT_DMG_OWNER_FAUNAS: u32 = 1 << 1;
static UNLIMITED_MP_HOOKS: Mutex<[MethodHookEntry; 3]> = Mutex::new([MethodHookEntry::new(); 3]);
/// Best-effort TemporaryPlayerStatus AMPR hook (PC v226 0x00d94820).
/// Installed after the core PBS/PSS pair; missing class or failed slot only
/// logs and never fails the job, so older builds keep working.
static TPS_MP_HOOKS: Mutex<[MethodHookEntry; 1]> = Mutex::new([MethodHookEntry::new(); 1]);
/// Best-effort observer slots for get_AtkMpRecovery on non-player calculators.
/// Never fails the job: a missing class only logs a skip line.
static MP_AMPR_OBSERVE_HOOKS: Mutex<[MethodHookEntry; 3]> = Mutex::new([MethodHookEntry::new(); 3]);
static CLEAR_DELAY_HOOKS: Mutex<[MethodHookEntry; 3]> = Mutex::new([MethodHookEntry::new(); 3]);
/// Companion sustain (best-effort, tidak menggagalkan job inti bila target absen).
static SUSTAIN_COST_HOOKS: Mutex<[MethodHookEntry; 2]> = Mutex::new([MethodHookEntry::new(); 2]);
static SUSTAIN_DELAY_HOOKS: Mutex<[MethodHookEntry; 1]> = Mutex::new([MethodHookEntry::new(); 1]);
/// Rate-limit cap untuk baris diagnostik sustain per lifetime proses.
static SUSTAIN_DIAG_LOGGED: AtomicU64 = AtomicU64::new(0);
static ATTACK_RANGE_HOOKS: Mutex<[MethodHookEntry; 3]> = Mutex::new([MethodHookEntry::new(); 3]);
static WATCHDOG_HOOKS: Mutex<[MethodHookEntry; 4]> = Mutex::new([MethodHookEntry::new(); 4]);

unsafe extern "C" fn hk_watchdog_suppress(_this: *mut c_void, _method: *const c_void) {
    // Suppress the watchdog check or game-quit command
}

/// Suppress Toram's in-game watchdog (`HackThreadManager`)
/// and crash/quit handlers (`UncheaterHackAppQuit` / `HackApplicationQuit`).
pub unsafe fn install_watchdog_shield() -> bool {
    let mut gc = GcScope::new();
    let mut ch = GameChain::new();
    if !unsafe { ch.resolve(&mut gc) } {
        return false;
    }

    let htm_class = unsafe { find_class(ch.image, ch.domain, "", "HackThreadManager") };
    let apm_class = unsafe {
        find_class(
            ch.image,
            ch.domain,
            "Asobimo.Plugin",
            "AsobimoPluginManager",
        )
    };
    let apb_class =
        unsafe { find_class(ch.image, ch.domain, "Asobimo.Plugin", "AsobimoPluginBase") };

    let Ok(mut hooks) = WATCHDOG_HOOKS.lock() else {
        return false;
    };

    let mut hooked = 0;
    if !htm_class.is_null() {
        if hooks[0].set(
            htm_class,
            "PopHackWindow",
            0,
            hk_watchdog_suppress as usize,
            true,
            "watchdog",
        ) {
            hooked += 1;
        }
        if hooks[1].set(
            htm_class,
            "Update",
            0,
            hk_watchdog_suppress as usize,
            true,
            "watchdog",
        ) {
            hooked += 1;
        }
    }
    if !apm_class.is_null()
        && hooks[2].set(
            apm_class,
            "UncheaterHackAppQuit",
            0,
            hk_watchdog_suppress as usize,
            true,
            "watchdog",
        )
    {
        hooked += 1;
    }
    if !apb_class.is_null()
        && hooks[3].set(
            apb_class,
            "HackApplicationQuit",
            0,
            hk_watchdog_suppress as usize,
            true,
            "watchdog",
        )
    {
        hooked += 1;
    }
    log_fmt(format_args!(
        "[watchdog_shield] installed {hooked}/4 watchdog suppression hooks"
    ));
    hooked > 0
}

#[inline]
pub fn reset_mp_diagnostics() {
    MP_HIT_CALLS.store(0, Ordering::Relaxed);
    MP_GETTER_CALLS.store(0, Ordering::Relaxed);
    MP_GETTER_OTHER.store(0, Ordering::Relaxed);
    MP_RECOVERY_CALLS.store(0, Ordering::Relaxed);
    MP_SAMPLES.store(0, Ordering::Relaxed);
    MP_DIAG_LOGGED.store(0, Ordering::Relaxed);
    if let Some(shm) = telemetry() {
        shm.update(|data| {
            data.observed_toggles
                .fetch_and(!TOGGLE_UNLIMITED_MP, Ordering::SeqCst);
        });
    }
    publish_mp_diagnostics();
}

fn publish_mp_diagnostics() {
    if let Some(shm) = telemetry() {
        shm.update(|data| {
            data.mp_hit_calls
                .store(MP_HIT_CALLS.load(Ordering::Relaxed), Ordering::SeqCst);
            data.mp_getter_calls
                .store(MP_GETTER_CALLS.load(Ordering::Relaxed), Ordering::SeqCst);
        });
    }
}
pub(crate) fn bump_hook_telemetry(damage: Option<i32>) {
    if let Some(shm) = telemetry() {
        shm.update(|data| {
            data.hook_hits.fetch_add(1, Ordering::SeqCst);
            if let Some(dmg) = damage {
                if dmg > 0 {
                    data.last_damage.store(dmg, Ordering::SeqCst);
                }
            }
        });
    }
}

// -----------------------------------------------------------------------------
// Hook Callbacks (x64 C ABI matching IL2CPP generated methods)
// -----------------------------------------------------------------------------

unsafe extern "C" fn hk_get_next_atk_time(
    _this: *mut c_void,
    _aspd: i32,
    _method: *const c_void,
) -> f32 {
    NEXT_ATTACK_CALLS.fetch_add(1, Ordering::Relaxed);
    bump_hook_telemetry(None);
    0.0f32
}

unsafe extern "C" fn hk_get_motion_speed(
    _this: *mut c_void,
    _aspd: i32,
    _method: *const c_void,
) -> i32 {
    MOTION_SPEED_CALLS.fetch_add(1, Ordering::Relaxed);
    60
}

unsafe extern "C" fn hk_get_aspd(_this: *mut c_void, _method: *const c_void) -> i32 {
    10_000
}

unsafe extern "C" fn hk_crit_rate_1000(_this: *mut c_void, _method: *const c_void) -> i32 {
    1_000
}

unsafe extern "C" fn hk_crit_dmg_300(_this: *mut c_void, _method: *const c_void) -> i32 {
    let mult = CUSTOM_CRIT_MULTIPLIER.load(Ordering::Relaxed);
    if mult >= 150 {
        mult
    } else {
        300
    }
}

unsafe extern "C" fn hk_stability_100(_this: *mut c_void, _method: *const c_void) -> i32 {
    100
}

unsafe extern "C" fn hk_stability_factor(
    _this: *mut c_void,
    _attack_type: i32,
    _stable_source: i32,
    _correct_hit: bool,
    _status: *mut c_void,
    _method: *const c_void,
) -> f32 {
    STABILITY_HITS.fetch_add(1, Ordering::Relaxed);
    bump_hook_telemetry(None);
    1.0
}

thread_local! {
    static MP_NATIVE_READ: Cell<bool> = const { Cell::new(false) };
}

/// Reentrancy guard: one calculator can read its native value through another
/// hooked calculator. The inner read must return native so the bonus applies
/// exactly once (live 2026-10-19: without this, 13 -> 23 -> 33 compounded).
struct MpNativeRead<'a> {
    flag: &'a Cell<bool>,
    previous: bool,
}
impl Drop for MpNativeRead<'_> {
    fn drop(&mut self) {
        self.flag.set(self.previous);
    }
}

/// Pure bonus mapping (the compounding site): nested reads return native so the
/// bonus applies once; unboosted reads return native; otherwise saturating add.
fn mp_apply_bonus(native_value: i32, nested: bool, boosted: bool, bonus: i32) -> i32 {
    if nested || !boosted {
        native_value
    } else {
        native_value.saturating_add(bonus)
    }
}

thread_local! {
    static NATIVE_CRITICAL_DAMAGE_READ: Cell<bool> = const { Cell::new(false) };
}

struct NativeCriticalDamageRead<'a> {
    flag: &'a Cell<bool>,
    previous: bool,
}

impl Drop for NativeCriticalDamageRead<'_> {
    fn drop(&mut self) {
        self.flag.set(self.previous);
    }
}

/// Resolves the effective critical damage percentage for one calculation.
///
/// The requested multiplier is bounded by `CRIT_DAMAGE_MAX_RATIO` against the
/// native value, so no configuration can produce a claim the server has
/// already been observed to roll back. The native value is never reduced.
///
/// `skill_ratio` is the adaptive skill-path cap as a ratio percentage; it can
/// only lower the result, never raise it above the ceiling.
fn effective_critical_damage(
    requested: i32,
    native_value: i32,
    skill_ratio: i32,
    normal_attack: bool,
) -> i32 {
    let requested_effective = match requested {
        requested @ (175 | 200 | 250 | 300) => requested,
        CRIT_DAMAGE_ADD_1000 => native_value.saturating_add(CRIT_DAMAGE_BONUS_POINTS),
        CRIT_DAMAGE_ADD_100 => native_value.saturating_add(100),
        CRIT_DAMAGE_ADD_200 => native_value.saturating_add(200),
        CRIT_DAMAGE_ADD_300 => native_value.saturating_add(300),
        _ => native_value,
    };
    let ceiling = native_value
        .saturating_mul(CRIT_DAMAGE_MAX_RATIO)
        .saturating_div(100)
        .max(native_value);
    let bounded = requested_effective.min(ceiling);
    if normal_attack {
        bounded
    } else {
        let skill_cap = native_value
            .saturating_mul(skill_ratio)
            .saturating_div(100)
            .max(native_value);
        bounded.min(skill_cap)
    }
}

unsafe fn critical_damage_value(
    this_ptr: *mut c_void,
    method: *const c_void,
    original: &AtomicUsize,
) -> i32 {
    let address = original.load(Ordering::Acquire);
    if !fn_readable(address) {
        return 150;
    }
    let native: unsafe extern "C" fn(*mut c_void, *const c_void) -> i32 =
        unsafe { transmute(address) };
    NATIVE_CRITICAL_DAMAGE_READ
        .try_with(|flag| {
            let read_scope = NativeCriticalDamageRead {
                flag,
                previous: flag.replace(true),
            };
            let native_value = unsafe { native(this_ptr, method) };
            // Faunas' ladder replaces the method body with an immediate
            // constant, so the value is absolute and every reader sees it,
            // a nested calculator read included. It is deliberately not
            // bounded by the companion's ratio cap: the ladder is an explicit
            // per-rung choice.
            //
            // The native value is still read, under the same guard the rest
            // of this path uses, so the capture can show what the client
            // claimed next to what the server did with it. Without that, a
            // capped claim and a claim that never reached the calculation
            // look the same.
            if let Some(absolute) = crate::faunas::critdmg_forced_value(this_ptr) {
                note_callback(crate::faunas::feature_bit_for_id("critdmg"));
                crate::combat_response::note_critical_damage(absolute, native_value, absolute);
                return absolute;
            }
            // A calculator can obtain its native value through another hooked calculator.
            if read_scope.previous || !always_crit_active() {
                return native_value;
            }
            let requested = CUSTOM_CRIT_MULTIPLIER.load(Ordering::Relaxed);
            // Preserve the requested setting for explicit diagnostics. The skill
            // path has an adaptive ratio cap; normal attacks do not.
            // Arithmetic and callback execution do not establish server acceptance.
            let effective = effective_critical_damage(
                requested,
                native_value,
                skill_adaptive_cap(),
                crate::critical_path::in_normal_attack(),
            );
            crate::combat_response::note_critical_damage(requested, native_value, effective);
            effective
        })
        .unwrap_or_else(|_| {
            let fallback = unsafe { native(this_ptr, method) };
            match crate::faunas::critdmg_forced_value(this_ptr) {
                Some(absolute) => absolute,
                None => fallback,
            }
        })
}

pub(crate) unsafe extern "C" fn hk_pbs_crit_damage(
    this_ptr: *mut c_void,
    method: *const c_void,
) -> i32 {
    unsafe { critical_damage_value(this_ptr, method, &ORIG_PBS_CRIT_DMG) }
}

pub(crate) unsafe extern "C" fn hk_pss_crit_damage(
    this_ptr: *mut c_void,
    method: *const c_void,
) -> i32 {
    unsafe { critical_damage_value(this_ptr, method, &ORIG_PSS_CRIT_DMG) }
}

unsafe extern "C" fn hk_hit_10000(_this: *mut c_void, _method: *const c_void) -> i32 {
    10_000
}

unsafe extern "C" fn hk_true_0arg(_this: *mut c_void, _method: *const c_void) -> bool {
    bump_hook_telemetry(None);
    true
}

unsafe extern "C" fn hk_false_0arg(_this: *mut c_void, _method: *const c_void) -> bool {
    bump_hook_telemetry(None);
    false
}

unsafe extern "C" fn hk_zero_0arg(_this: *mut c_void, _method: *const c_void) -> i32 {
    bump_hook_telemetry(None);
    0
}

unsafe extern "C" fn hk_zero_1arg(
    _this: *mut c_void,
    _a1: *mut c_void,
    _method: *const c_void,
) -> i32 {
    0
}

unsafe extern "C" fn hk_true_1arg(_this: *mut c_void, _a1: i32, _method: *const c_void) -> bool {
    true
}

unsafe extern "C" fn hk_true_2arg(
    _this: *mut c_void,
    _a1: *mut c_void,
    _a2: *mut c_void,
    _method: *const c_void,
) -> bool {
    bump_hook_telemetry(None);
    true
}

unsafe extern "C" fn hk_true_3arg(
    _this: *mut c_void,
    _a1: *mut c_void,
    _a2: *mut c_void,
    _a3: *mut c_void,
    _method: *const c_void,
) -> bool {
    bump_hook_telemetry(None);
    true
}

pub(crate) unsafe extern "C" fn hk_pbm_action_out_of_range(
    this_ptr: *mut c_void,
    target: *mut c_void,
    action: *mut c_void,
    method: *const c_void,
) -> bool {
    let original = ORIG_PBM_OUT_OF_RANGE.load(Ordering::Acquire);
    if fn_readable(original) {
        let forward: unsafe extern "C" fn(
            *mut c_void,
            *mut c_void,
            *mut c_void,
            *const c_void,
        ) -> bool = unsafe { transmute(original) };
        let binding = RANGE_BINDING
            .try_read()
            .ok()
            .and_then(|guard| guard.clone());
        let range = if RANGE_ACTIVE.load(Ordering::Acquire) {
            binding.as_ref().and_then(|binding| unsafe {
                binding.pending_range_override_for_action(this_ptr as usize, action as usize)
            })
        } else {
            None
        };
        if range.is_some() {
            RANGE_ASSIST_OVERRIDES.fetch_add(1, Ordering::Relaxed);
            note_callback(TOGGLE_RANGE_30M);
        }
        let result = unsafe { forward(this_ptr, target, action, method) };
        drop(range);
        return result;
    }
    false
}

/// Original pointers for the assist-move getters.
static RANGE_ASSIST_ORIGINALS: [AtomicUsize; 3] = [const { AtomicUsize::new(0) }; 3];
/// Assist-move queries that matched the local queued action.
static RANGE_ASSIST_QUERIES: AtomicU64 = AtomicU64::new(0);
/// Assist-move queries that were seen but belonged to another actor.
static RANGE_ASSIST_FOREIGN: AtomicU64 = AtomicU64::new(0);

/// Assist-move getter.
///
/// The value is forced to true only for the local battle manager's queued
/// action, so no other actor's action is affected. The native range checks are
/// untouched: the character closes the distance to the target and the attack
/// is then made from a legitimate range.
unsafe extern "C" fn hk_assist_move_bool<const SLOT: usize>(
    this: *mut c_void,
    method: *const c_void,
) -> bool {
    let original = RANGE_ASSIST_ORIGINALS[SLOT].load(Ordering::Acquire);
    if RANGE_ACTIVE.load(Ordering::Acquire) {
        let binding = RANGE_BINDING.read().ok().and_then(|guard| guard.clone());
        if let Some(binding) = binding {
            if unsafe { binding.is_local_pending_action(this as usize) } {
                RANGE_ASSIST_QUERIES.fetch_add(1, Ordering::Relaxed);
                note_callback(TOGGLE_RANGE_30M);
                return true;
            }
            RANGE_ASSIST_FOREIGN.fetch_add(1, Ordering::Relaxed);
        }
    }
    if !fn_readable(original) {
        return false;
    }
    let native: unsafe extern "C" fn(*mut c_void, *const c_void) -> bool =
        unsafe { transmute(original) };
    unsafe { native(this, method) }
}

unsafe extern "C" fn hk_pending_action_range(this: *mut c_void, method: *const c_void) -> bool {
    let address = ORIG_PENDING_RANGE.load(Ordering::Acquire);
    if !fn_readable(address) {
        return false;
    }
    let native: unsafe extern "C" fn(*mut c_void, *const c_void) -> bool =
        unsafe { transmute(address) };
    RANGE_PENDING_CHECKS.fetch_add(1, Ordering::Relaxed);
    let binding = RANGE_BINDING
        .try_read()
        .ok()
        .and_then(|guard| guard.clone());
    let range = if RANGE_ACTIVE.load(Ordering::Acquire) {
        binding
            .as_ref()
            .and_then(|binding| unsafe { binding.pending_range_override(this as usize) })
    } else {
        None
    };
    if range.is_some() {
        RANGE_PENDING_OVERRIDES.fetch_add(1, Ordering::Relaxed);
        note_callback(TOGGLE_RANGE_30M);
    }
    let result = unsafe { native(this, method) };
    drop(range);
    result
}

unsafe extern "C" fn hk_check_range_hit(
    this_ptr: *mut c_void,
    actor_transform: *mut c_void,
    target_transform: *mut c_void,
    size: f32,
    method: *const c_void,
) -> bool {
    let orig = ORIG_CHECK_RANGE_HIT.load(Ordering::Relaxed);
    if fn_readable(orig) {
        RANGE_HIT_CHECKS.fetch_add(1, Ordering::Relaxed);
        let f: unsafe extern "C" fn(
            *mut c_void,
            *mut c_void,
            *mut c_void,
            f32,
            *const c_void,
        ) -> bool = unsafe { transmute(orig) };
        let binding = RANGE_BINDING.read().ok().and_then(|guard| guard.clone());
        let selected = binding
            .as_ref()
            .map(|binding| unsafe {
                binding.selected_target(
                    this_ptr as usize,
                    actor_transform as usize,
                    target_transform as usize,
                )
            })
            .unwrap_or(false);
        let effective_size = if RANGE_ACTIVE.load(Ordering::Relaxed) && selected {
            if size.is_finite() && (0.0..EXTENDED_RANGE_WORLD_UNITS).contains(&size) {
                note_callback(TOGGLE_RANGE_30M);
                RANGE_HIT_EXTENSIONS.fetch_add(1, Ordering::Relaxed);
                EXTENDED_RANGE_WORLD_UNITS
            } else {
                size
            }
        } else {
            size
        };
        return unsafe {
            f(
                this_ptr,
                actor_transform,
                target_transform,
                effective_size,
                method,
            )
        };
    }
    false
}

unsafe extern "C" fn hk_void_2arg(_this: *mut c_void, _a1: i32, _a2: bool, _method: *const c_void) {
}

unsafe extern "C" fn hk_check_hit(
    _this: *mut c_void,
    _status: *mut c_void,
    _need_hit: i32,
    _mp: i32,
    _is_flash: bool,
    correct: *mut bool,
    _method: *const c_void,
) -> bool {
    if !page_write_ok(correct as usize, core::mem::size_of::<bool>()) {
        return false;
    }
    // Native ChackCorrectHit clears this output on a clean hit. True marks
    // the accuracy correction copied to DamageData.IsScratch (graze).
    unsafe {
        ptr::write(correct, false);
    };
    HIT_CHECK_CALLS.fetch_add(1, Ordering::Relaxed);
    note_callback(TOGGLE_ALWAYS_CRIT);
    true
}

unsafe extern "C" fn hk_hit_type_2(_this: *mut c_void, _method: *const c_void) -> i32 {
    2 // HitType::Critical = 2
}

unsafe extern "C" fn hk_reaction_0(_this: *mut c_void, _method: *const c_void) -> i32 {
    0 // HitReactionType::None (no Guard)
}

type FnCalcBaseDamage5 = unsafe extern "C" fn(
    *mut c_void,
    *mut c_void,
    *mut c_void,
    i32,
    i32,
    bool,
    *const c_void,
) -> i32;

pub(crate) unsafe extern "C" fn hk_calc_base_damage(
    this_ptr: *mut c_void,
    player_action: *mut c_void,
    mob_action: *mut c_void,
    attack_type: i32,
    eq_limit: i32,
    is_critical: bool,
    method: *const c_void,
) -> i32 {
    if !unsafe { is_valid_il2cpp_obj(this_ptr) } {
        return 0;
    }
    let orig_addr = ORIG_CALC_BASE_DAMAGE.load(Ordering::Acquire);
    if fn_readable(orig_addr) {
        let orig: FnCalcBaseDamage5 = unsafe { transmute(orig_addr) };
        let dmg = unsafe {
            orig(
                this_ptr,
                player_action,
                mob_action,
                attack_type,
                eq_limit,
                is_critical,
                method,
            )
        };
        bump_hook_telemetry(Some(dmg));
        BASE_DAMAGE_HITS.fetch_add(1, Ordering::Relaxed);
        return dmg;
    }
    bump_hook_telemetry(None);
    0
}

type FnNaaCalcBaseDamage =
    unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void, i32, *const c_void) -> i32;

pub(crate) unsafe extern "C" fn hk_naa_calc_base_damage(
    this_ptr: *mut c_void,
    player_action: *mut c_void,
    mob_action: *mut c_void,
    hit_type: i32,
    method: *const c_void,
) -> i32 {
    if !unsafe { is_valid_il2cpp_obj(this_ptr) } {
        return 0;
    }
    let orig = ORIG_NAA_CALC_BASE_DMG.load(Ordering::Relaxed);
    if fn_readable(orig) {
        let f: FnNaaCalcBaseDamage = unsafe { transmute(orig) };
        let dmg = unsafe { f(this_ptr, player_action, mob_action, hit_type, method) };
        bump_hook_telemetry(Some(dmg));
        return dmg;
    }
    0
}

type FnNaaCalcDualSwordDamage = unsafe extern "C" fn(
    *mut c_void,
    i32,
    *mut c_void,
    *mut c_void,
    bool,
    bool,
    bool,
    *mut bool,
    *const c_void,
) -> i32;

pub(crate) unsafe extern "C" fn hk_naa_calc_dual_sword_damage(
    this_ptr: *mut c_void,
    atk: i32,
    player_act: *mut c_void,
    mob_act: *mut c_void,
    correct_hit: bool,
    critical: bool,
    guard: bool,
    sub_weak: *mut bool,
    method: *const c_void,
) -> i32 {
    if !unsafe { is_valid_il2cpp_obj(this_ptr) } {
        return 0;
    }
    let orig = ORIG_NAA_CALC_DUAL_SWORD_DMG.load(Ordering::Relaxed);
    if fn_readable(orig) {
        let f: FnNaaCalcDualSwordDamage = unsafe { transmute(orig) };
        let dmg = unsafe {
            f(
                this_ptr,
                atk,
                player_act,
                mob_act,
                correct_hit,
                critical,
                guard,
                sub_weak,
                method,
            )
        };
        bump_hook_telemetry(Some(dmg));
        return dmg;
    }
    0
}

/// Resolve and cache `SkillDamageData` field offsets once per class pointer.
unsafe fn ensure_sdd_offsets(sdd_klass: *mut c_void) {
    if !is_valid_il2cpp_class(sdd_klass) {
        return;
    }
    let k_addr = sdd_klass as usize;
    if SDD_CLASS_CACHED.load(Ordering::Acquire) == k_addr
        && OFF_SDD_HIT_TYPE.load(Ordering::Relaxed) > 0
    {
        return;
    }
    let Some(api) = api() else {
        return;
    };
    let find_first_off = |names: &[&str]| -> i32 {
        for &n in names {
            let f = unsafe { get_field(sdd_klass, n) };
            if !f.is_null() {
                let off = unsafe { (api.field_get_offset)(f) };
                if off > 0 && off < 0x400 {
                    return off;
                }
            }
        }
        0
    };

    OFF_SDD_HIT_TYPE.store(
        find_first_off(&["<HitType>k__BackingField", "hitType", "_hitType"]),
        Ordering::Relaxed,
    );
    OFF_SDD_REACTION_TYPE.store(
        find_first_off(&[
            "<HitReactionType>k__BackingField",
            "hitReactionType",
            "_hitReactionType",
        ]),
        Ordering::Relaxed,
    );
    OFF_SDD_TOTAL_DMG.store(
        find_first_off(&["<TotalDamage>k__BackingField", "totalDamage"]),
        Ordering::Relaxed,
    );
    OFF_SDD_VIEW_DMG.store(
        find_first_off(&["<ViewDamage>k__BackingField", "viewDamage"]),
        Ordering::Relaxed,
    );
    OFF_SDD_RAW_DMG.store(find_first_off(&["_damage", "damage"]), Ordering::Relaxed);
    SDD_CLASS_CACHED.store(k_addr, Ordering::Release);
}

/// Resolve and cache `SkillActionBase/DamageData` field offsets dynamically once per class pointer.
unsafe fn ensure_dd_offsets(dd_klass: *mut c_void) {
    if !is_valid_il2cpp_class(dd_klass) {
        return;
    }
    let k_addr = dd_klass as usize;
    if DD_CLASS_CACHED.load(Ordering::Acquire) == k_addr
        && OFF_DD_HIT_TYPE.load(Ordering::Relaxed) > 0
    {
        return;
    }
    let Some(api) = api() else {
        return;
    };
    let find_first_off = |names: &[&str]| -> i32 {
        for &n in names {
            let f = unsafe { get_field(dd_klass, n) };
            if !f.is_null() {
                let off = unsafe { (api.field_get_offset)(f) };
                if off > 0 && off < 0x400 {
                    return off;
                }
            }
        }
        0
    };

    let off_hit = find_first_off(&["<HitType>k__BackingField", "hitType", "_hitType", "HitType"]);
    OFF_DD_HIT_TYPE.store(if off_hit > 0 { off_hit } else { 0x1C }, Ordering::Relaxed);

    let off_scratch = find_first_off(&[
        "<IsScratch>k__BackingField",
        "isScratch",
        "_isScratch",
        "IsScratch",
    ]);
    OFF_DD_IS_SCRATCH.store(
        if off_scratch > 0 { off_scratch } else { 0x30 },
        Ordering::Relaxed,
    );

    let off_rx = find_first_off(&[
        "<HitReactionType>k__BackingField",
        "hitReactionType",
        "_hitReactionType",
        "HitReactionType",
    ]);
    OFF_DD_REACTION_TYPE.store(if off_rx > 0 { off_rx } else { 0x34 }, Ordering::Relaxed);

    let off_gb = find_first_off(&[
        "<IsGuardBreak>k__BackingField",
        "isGuardBreak",
        "_isGuardBreak",
        "IsGuardBreak",
    ]);
    OFF_DD_GUARD_BREAK.store(if off_gb > 0 { off_gb } else { 0x61 }, Ordering::Relaxed);

    let off_ab = find_first_off(&[
        "<IsAvoidBreak>k__BackingField",
        "isAvoidBreak",
        "_isAvoidBreak",
        "IsAvoidBreak",
    ]);
    OFF_DD_AVOID_BREAK.store(if off_ab > 0 { off_ab } else { 0x62 }, Ordering::Relaxed);

    let off_sdd = find_first_off(&[
        "<CurrentSkillDamage>k__BackingField",
        "currentSkillDamage",
        "_currentSkillDamage",
        "CurrentSkillDamage",
    ]);
    OFF_DD_SDD.store(if off_sdd > 0 { off_sdd } else { 0x38 }, Ordering::Relaxed);

    DD_CLASS_CACHED.store(k_addr, Ordering::Release);
}

pub(crate) unsafe fn stamp_skill_damage_data(sdd: *mut c_void, _mult: i32) {
    if !unsafe { is_valid_il2cpp_obj(sdd) } || !ptr_ok(sdd as usize, 0x60) {
        return;
    }
    let Some(api) = api() else {
        return;
    };
    let Some(obj_get_class) = api.object_get_class else {
        return;
    };
    let sdd_klass = unsafe { obj_get_class(sdd) };
    if !is_valid_il2cpp_class(sdd_klass) {
        return;
    }

    unsafe {
        ensure_sdd_offsets(sdd_klass);
    }

    let base = sdd as usize;

    let ht_off = OFF_SDD_HIT_TYPE.load(Ordering::Relaxed);
    if ht_off > 0 && page_write_ok(base + (ht_off as usize), 4) {
        let addr = (base + (ht_off as usize)) as *mut i32;
        if unsafe { *addr } != 2 {
            unsafe {
                *addr = 2; // HitType::Critical = 2
            }
        }
    }

    let hr_off = OFF_SDD_REACTION_TYPE.load(Ordering::Relaxed);
    if hr_off > 0 && page_write_ok(base + (hr_off as usize), 4) {
        unsafe {
            *((base + (hr_off as usize)) as *mut i32) = 0;
        }
    }

    // Read damage for telemetry reporting without altering memory values
    let raw_off = OFF_SDD_RAW_DMG.load(Ordering::Relaxed);
    if raw_off > 0 && ptr_ok(base + (raw_off as usize), 4) {
        let dmg = unsafe { *((base + (raw_off as usize)) as *const i32) };
        if dmg > 0 {
            bump_hook_telemetry(Some(dmg));
        }
    }
}

/// Stamp `HitType = 2 (Critical)` and `HitReactionType = 0 (None)` on all damage entries of `action`.
pub(crate) unsafe fn force_critical_on_action(action: *mut c_void) {
    if !unsafe { is_valid_il2cpp_obj(action) } || !ptr_ok(action as usize, 0x100) {
        return;
    }
    let Some(api) = api() else {
        return;
    };
    let Some(obj_get_class) = api.object_get_class else {
        return;
    };
    let klass = unsafe { obj_get_class(action) };
    if !is_valid_il2cpp_class(klass) {
        return;
    }

    let mut gc = GcScope::new();
    // targetDamageData is List<SkillActionBase.DamageData> at field "targetDamageData" or offset 0x80
    let list_obj = unsafe { read_inst_ptr(action, klass, "targetDamageData", &mut gc) };
    let list_ptr = if unsafe { is_valid_il2cpp_obj(list_obj) } {
        list_obj
    } else {
        let raw_slot = (action as usize) + 0x80;
        if ptr_ok(raw_slot, 8) {
            unsafe { *(raw_slot as *const *mut c_void) }
        } else {
            ptr::null_mut()
        }
    };

    if unsafe { is_valid_il2cpp_obj(list_ptr) } && ptr_ok(list_ptr as usize, 0x20) {
        let items_ptr = unsafe { *((list_ptr as usize + 0x10) as *const *mut c_void) };
        let size = unsafe { *((list_ptr as usize + 0x18) as *const i32) };
        if size > 0 && size < 64 && unsafe { is_valid_il2cpp_obj(items_ptr) } {
            let arr_addr = items_ptr as usize;
            for i in 0..(size as usize) {
                let elem_slot = arr_addr + 0x20 + i * 8;
                if !ptr_ok(elem_slot, 8) {
                    break;
                }
                let dd = unsafe { *(elem_slot as *const *mut c_void) };
                if !unsafe { is_valid_il2cpp_obj(dd) } || !ptr_ok(dd as usize, 0x70) {
                    continue;
                }
                if let Some(obj_get_class) = api.object_get_class {
                    let dd_klass = unsafe { obj_get_class(dd) };
                    unsafe { ensure_dd_offsets(dd_klass) };
                }

                let dd_base = dd as usize;
                // SkillActionBase.DamageData fields (resolved dynamically via ensure_dd_offsets)
                let off_hit = OFF_DD_HIT_TYPE.load(Ordering::Relaxed) as usize;
                if off_hit > 0 && page_write_ok(dd_base + off_hit, 4) {
                    unsafe {
                        *((dd_base + off_hit) as *mut i32) = 2; // Critical
                    }
                }
                let off_scratch = OFF_DD_IS_SCRATCH.load(Ordering::Relaxed) as usize;
                if off_scratch > 0 && page_write_ok(dd_base + off_scratch, 1) {
                    unsafe {
                        *((dd_base + off_scratch) as *mut u8) = 0; // Clear Graze/IsScratch
                    }
                }
                let off_rx = OFF_DD_REACTION_TYPE.load(Ordering::Relaxed) as usize;
                if off_rx > 0 && page_write_ok(dd_base + off_rx, 4) {
                    unsafe {
                        *((dd_base + off_rx) as *mut i32) = 0;
                    }
                }
                let off_gb = OFF_DD_GUARD_BREAK.load(Ordering::Relaxed) as usize;
                if off_gb > 0 && page_write_ok(dd_base + off_gb, 1) {
                    unsafe {
                        *((dd_base + off_gb) as *mut u8) = 1;
                    }
                }
                let off_ab = OFF_DD_AVOID_BREAK.load(Ordering::Relaxed) as usize;
                if off_ab > 0 && page_write_ok(dd_base + off_ab, 1) {
                    unsafe {
                        *((dd_base + off_ab) as *mut u8) = 1;
                    }
                }

                // CurrentSkillDamage (SkillDamageData)
                let off_sdd = OFF_DD_SDD.load(Ordering::Relaxed) as usize;
                if off_sdd > 0 && ptr_ok(dd_base + off_sdd, 8) {
                    let sdd = unsafe { *((dd_base + off_sdd) as *const *mut c_void) };
                    if unsafe { is_valid_il2cpp_obj(sdd) } {
                        unsafe {
                            stamp_skill_damage_data(sdd, 100);
                        }
                    }
                }
            }
        }
    }
}

type FnCalcPlayerToMobDmg = unsafe extern "C" fn(
    *mut c_void,   // this
    *mut c_void,   // playerAction
    *mut c_void,   // mobAction
    *const c_void, // method
);

pub(crate) unsafe extern "C" fn hk_pab_calc_mob_dmg(
    this_ptr: *mut c_void,
    player_action: *mut c_void,
    mob_action: *mut c_void,
    method: *const c_void,
) {
    if !unsafe { is_valid_il2cpp_obj(this_ptr) } {
        return;
    }
    let orig = ORIG_PAB_CALC_MOB_DMG.load(Ordering::Acquire);
    if fn_readable(orig) {
        let f: FnCalcPlayerToMobDmg = unsafe { transmute(orig) };
        unsafe { f(this_ptr, player_action, mob_action, method) };
    }
}

pub(crate) unsafe extern "C" fn hk_naa_calc_mob_dmg(
    this_ptr: *mut c_void,
    player_action: *mut c_void,
    mob_action: *mut c_void,
    method: *const c_void,
) {
    if !unsafe { is_valid_il2cpp_obj(this_ptr) } {
        return;
    }
    let orig = ORIG_NAA_CALC_MOB_DMG.load(Ordering::Acquire);
    if fn_readable(orig) {
        let _scope = critical_path::NormalAttackScope::enter();
        let f: FnCalcPlayerToMobDmg = unsafe { transmute(orig) };
        unsafe { f(this_ptr, player_action, mob_action, method) };
        NORMAL_CALC_HITS.fetch_add(1, Ordering::Relaxed);
    }
}

type FnActionHit = unsafe extern "C" fn(
    *mut c_void,   // this
    *mut c_void,   // actarAction
    *mut c_void,   // target
    *const c_void, // method
);

pub(crate) unsafe extern "C" fn hk_naa_action_hit(
    this_ptr: *mut c_void,
    actar_action: *mut c_void,
    target: *mut c_void,
    method: *const c_void,
) {
    let original = ORIG_NAA_ACTION_HIT.load(Ordering::Acquire);
    if !fn_readable(original) {
        return;
    }
    let forward: FnActionHit = unsafe { transmute(original) };
    unsafe { forward(this_ptr, actar_action, target, method) };
    if !UNLIMITED_MP_ACTIVE.load(Ordering::Acquire) {
        return;
    }
    MP_HIT_CALLS.fetch_add(1, Ordering::Relaxed);
    publish_mp_diagnostics();
    let binding = MP_BINDING.read().ok().and_then(|guard| *guard);
    // Read-only observation: the old write of localMp to max filled the bar in
    // one hit and reconciled on every server response. Sampling keeps telemetry
    // truthful; accepted gains arrive via serverMp in AttackResponseData.
    if let Some(binding) = binding {
        if let Some(sample) = unsafe { binding.sample(actar_action as usize) } {
            MP_SAMPLES.fetch_add(1, Ordering::Relaxed);
            LAST_RECEIVED_MP.store(sample.received, Ordering::Relaxed);
            if let Some(shm) = telemetry() {
                shm.update(|data| {
                    data.now_mp.store(sample.local, Ordering::SeqCst);
                    data.max_mp.store(sample.maximum, Ordering::SeqCst);
                    data.received_mp.store(sample.received, Ordering::SeqCst);
                    data.mp_sample_tick_ms
                        .store(protocol::monotonic_ms(), Ordering::SeqCst);
                    data.resource_tick_ms
                        .store(protocol::monotonic_ms(), Ordering::SeqCst);
                });
            }
        }
    }
}

unsafe extern "C" fn hk_naa_preparation(
    this: *mut c_void,
    actor: *mut c_void,
    target: *mut c_void,
    method: *const c_void,
) {
    let binding = RANGE_BINDING.read().ok().and_then(|guard| guard.clone());
    let mut restored = true;
    if let Some(ref binding) = binding {
        if let Ok(mut snapshots) = RANGE_SNAPSHOTS.lock() {
            if let Some(index) = snapshots
                .iter()
                .position(|saved| saved.action == this as usize)
            {
                if unsafe { binding.restore(&snapshots[index]) } {
                    snapshots.remove(index);
                } else {
                    restored = false;
                }
            }
        }
    }
    let original = ORIG_NAA_PREPARATION.load(Ordering::Acquire);
    if !fn_readable(original) {
        return;
    }
    let forward: FnActionHit = unsafe { transmute(original) };
    unsafe { forward(this, actor, target, method) };
    RANGE_PREPARATIONS.fetch_add(1, Ordering::Relaxed);
    if !restored || !RANGE_ACTIVE.load(Ordering::Acquire) {
        return;
    }
    if let (Some(binding), Ok(mut snapshots)) = (binding, RANGE_SNAPSHOTS.lock()) {
        if RANGE_ACTIVE.load(Ordering::Acquire) && snapshots.len() < 64 {
            if let Some(snapshot) = unsafe { binding.apply(this as usize, actor as usize) } {
                snapshots.push(snapshot);
                RANGE_PATCHES.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

unsafe fn restore_range_state() -> bool {
    let binding = RANGE_BINDING.read().ok().and_then(|guard| guard.clone());
    let Ok(mut snapshots) = RANGE_SNAPSHOTS.lock() else {
        return false;
    };
    let Some(binding) = binding else {
        return snapshots.is_empty();
    };
    snapshots.retain(|saved| !unsafe { binding.restore(saved) });
    snapshots.is_empty()
}

// Unlimited MP hooks
unsafe extern "C" fn hk_mp_2000(_this: *mut c_void, _method: *const c_void) -> i32 {
    2_000
}

/// Native AMPR plus the selected `MP_AMPR_BONUS`, when this getter is called
/// for the local owned calculator. Getter activation does not establish that
/// combat uses it or that any extra recovery is accepted.
/// Returns the untouched native value whenever the engine is off, the binding
/// is missing, or the calculator belongs to another actor.
unsafe fn mp_recovery_value(
    this: *mut c_void,
    method: *const c_void,
    original: &AtomicUsize,
) -> i32 {
    let address = original.load(Ordering::Acquire);
    if !fn_readable(address) {
        return 0;
    }
    let forward: unsafe extern "C" fn(*mut c_void, *const c_void) -> i32 =
        unsafe { transmute(address) };
    MP_NATIVE_READ
        .try_with(|flag| {
            let read_scope = MpNativeRead {
                flag,
                previous: flag.replace(true),
            };
            let nested = read_scope.previous;
            let native_value = unsafe { forward(this, method) };
            MP_GETTER_CALLS.fetch_add(1, Ordering::Relaxed);
            publish_mp_diagnostics();
            let binding = MP_BINDING.read().ok().and_then(|guard| *guard);
            let owned = binding
                .map(|binding| unsafe { binding.owns_calculator(this as usize) })
                .unwrap_or(false);
            let bonus = MP_AMPR_BONUS.load(Ordering::Relaxed);
            let boosted = UNLIMITED_MP_ACTIVE.load(Ordering::Acquire) && owned;
            let effective = mp_apply_bonus(native_value, nested, boosted, bonus);
            if boosted && !nested {
                note_callback(TOGGLE_UNLIMITED_MP);
                MP_RECOVERY_CALLS.fetch_add(1, Ordering::Relaxed);
            }
            // Rate-limited ground truth: guard outcome, native scale, nesting, result.
            if MP_DIAG_LOGGED.load(Ordering::Relaxed) < 24 {
                MP_DIAG_LOGGED.fetch_add(1, Ordering::Relaxed);
                log_fmt(format_args!(
                    "[mpdiag] owned={} nested={} native={} bonus={} effective={}",
                    owned as u8, nested as u8, native_value, bonus, effective
                ));
            }
            effective
        })
        .unwrap_or_else(|_| unsafe { forward(this, method) })
}

pub(crate) fn mp_ampr_bonus_request() -> i32 {
    MP_AMPR_BONUS.load(Ordering::Relaxed)
}
unsafe extern "C" fn hk_pbs_mp_recovery(this: *mut c_void, method: *const c_void) -> i32 {
    unsafe { mp_recovery_value(this, method, &ORIG_PBS_AMPR) }
}

unsafe extern "C" fn hk_pss_mp_recovery(this: *mut c_void, method: *const c_void) -> i32 {
    unsafe { mp_recovery_value(this, method, &ORIG_PSS_AMPR) }
}

unsafe extern "C" fn hk_tps_mp_recovery(this: *mut c_void, method: *const c_void) -> i32 {
    unsafe { mp_recovery_value(this, method, &ORIG_TPS_AMPR) }
}

#[inline]
fn sustain_cost_value(cost: i32, pct: i32) -> i32 {
    if cost <= 0 {
        return cost;
    }
    let pct = pct.clamp(1, 100);
    (cost.saturating_mul(pct) / 100).max(1).min(cost)
}

unsafe extern "C" fn hk_sustain_mp_cost(
    this: *mut c_void,
    player_action: *mut c_void,
    method: *const c_void,
) -> i32 {
    let address = ORIG_SUSTAIN_COST.load(Ordering::Acquire);
    if !fn_readable(address) {
        return 0;
    }
    let native: unsafe extern "C" fn(*mut c_void, *mut c_void, *const c_void) -> i32 =
        unsafe { transmute(address) };
    let cost = unsafe { native(this, player_action, method) };
    if !UNLIMITED_MP_ACTIVE.load(Ordering::Acquire) || cost <= 0 || this.is_null() {
        return cost;
    }
    let offset = SUSTAIN_COST_OFFSET.load(Ordering::Relaxed);
    if offset == 0 {
        return cost;
    }
    let addr = (this as usize).wrapping_add(offset);
    let foreign = ptr_ok(addr, 1) && unsafe { *(addr as *const u8) } != 0;
    if foreign {
        return cost;
    }
    let effective = sustain_cost_value(cost, SUSTAIN_MP_COST_PCT.load(Ordering::Relaxed));
    if SUSTAIN_DIAG_LOGGED.load(Ordering::Relaxed) < 24 {
        SUSTAIN_DIAG_LOGGED.fetch_add(1, Ordering::Relaxed);
        log_fmt(format_args!(
            "[sustain-cost] native={} effective={} foreign=0 offset=0x{:X}",
            cost, effective, offset
        ));
    }
    effective
}

unsafe extern "C" fn hk_sustain_mp_cost0(this: *mut c_void, method: *const c_void) -> i32 {
    let address = ORIG_SUSTAIN_COST0.load(Ordering::Acquire);
    if !fn_readable(address) {
        return 0;
    }
    let native: unsafe extern "C" fn(*mut c_void, *const c_void) -> i32 =
        unsafe { transmute(address) };
    let cost = unsafe { native(this, method) };
    if !UNLIMITED_MP_ACTIVE.load(Ordering::Acquire) || cost <= 0 || this.is_null() {
        return cost;
    }
    let effective = sustain_cost_value(cost, SUSTAIN_MP_COST_PCT.load(Ordering::Relaxed));
    if SUSTAIN_DIAG_LOGGED.load(Ordering::Relaxed) < 24 {
        SUSTAIN_DIAG_LOGGED.fetch_add(1, Ordering::Relaxed);
        log_fmt(format_args!(
            "[sustain-cost0] native={} effective={}",
            cost, effective
        ));
    }
    effective
}

unsafe extern "C" fn hk_sustain_skill_available(
    this: *mut c_void,
    skill_id: i32,
    method: *const c_void,
) -> bool {
    if CLEAR_DELAY_ACTIVE.load(Ordering::Acquire) && !this.is_null() {
        if SUSTAIN_DIAG_LOGGED.load(Ordering::Relaxed) < 24 {
            SUSTAIN_DIAG_LOGGED.fetch_add(1, Ordering::Relaxed);
            log_fmt(format_args!(
                "[sustain-delay] skill={} override=1",
                skill_id
            ));
        }
        return true;
    }
    let address = ORIG_SUSTAIN_DELAY.load(Ordering::Acquire);
    if !fn_readable(address) {
        return false;
    }
    let native: unsafe extern "C" fn(*mut c_void, i32, *const c_void) -> bool =
        unsafe { transmute(address) };
    unsafe { native(this, skill_id, method) }
}

#[cfg(test)]
mod sustain_tests {
    use super::sustain_cost_value;

    #[test]
    fn sustain_cost_shrinks_positive_floors_at_one_and_preserves_zero() {
        assert_eq!(sustain_cost_value(100, 10), 10);
        assert_eq!(sustain_cost_value(101, 10), 10);
        assert_eq!(sustain_cost_value(1, 10), 1);
        assert_eq!(sustain_cost_value(0, 10), 0);
        assert_eq!(sustain_cost_value(-5, 10), -5);
    }

    #[test]
    fn sustain_cost_never_exceeds_native_and_clamps_pct() {
        assert_eq!(sustain_cost_value(100, 0), 1);
        assert_eq!(sustain_cost_value(100, 200), 100);
        assert_eq!(sustain_cost_value(50, 50), 25);
    }
}

/// Non-player calculators exposing get_AtkMpRecovery (dump.cs ground truth).
/// Pure observation: native is always returned, behavior never changes.
const MP_OBSERVE_CLASSES: [&str; 3] = [
    "MercenaryBattleStatus",
    "PetStatus",
    "MobaPlayerSecondaryStatus",
];

unsafe fn mp_observe_ampr(slot: usize, this_ptr: *mut c_void, method: *const c_void) -> i32 {
    let address = ORIG_OBS_AMPR
        .get(slot)
        .map(|slot| slot.load(Ordering::Acquire))
        .unwrap_or(0);
    if !fn_readable(address) {
        return 0;
    }
    let forward: unsafe extern "C" fn(*mut c_void, *const c_void) -> i32 =
        unsafe { transmute(address) };
    let native_value = unsafe { forward(this_ptr, method) };
    MP_GETTER_OTHER.fetch_add(1, Ordering::Relaxed);
    if MP_DIAG_LOGGED.load(Ordering::Relaxed) < 24 {
        MP_DIAG_LOGGED.fetch_add(1, Ordering::Relaxed);
        log_fmt(format_args!(
            "[mpdiag] observe={} native={}",
            MP_OBSERVE_CLASSES.get(slot).copied().unwrap_or("?"),
            native_value
        ));
    }
    native_value
}

unsafe extern "C" fn hk_obs_ampr_0(this_ptr: *mut c_void, method: *const c_void) -> i32 {
    unsafe { mp_observe_ampr(0, this_ptr, method) }
}

unsafe extern "C" fn hk_obs_ampr_1(this_ptr: *mut c_void, method: *const c_void) -> i32 {
    unsafe { mp_observe_ampr(1, this_ptr, method) }
}

unsafe extern "C" fn hk_obs_ampr_2(this_ptr: *mut c_void, method: *const c_void) -> i32 {
    unsafe { mp_observe_ampr(2, this_ptr, method) }
}

/// Best-effort observer install/remove for the non-player AMPR calculators.
/// Missing classes and failed slots only log; the job result never depends on them.
unsafe fn mp_observe_apply(ch: &GameChain, enabling: bool) {
    let Ok(mut hooks) = MP_AMPR_OBSERVE_HOOKS.lock() else {
        return;
    };
    if !enabling {
        let mut restored = true;
        for hook in hooks.iter_mut().rev() {
            restored &= unsafe { hook.set(ptr::null_mut(), "restore", 0, 0, false, "mpobs") };
        }
        log_fmt(format_args!("[mpobs] restore complete={}", restored));
        return;
    }
    let signature = MethodSignature {
        parameters: &[],
        return_type: "System.Int32",
    };
    let detours = [
        hk_obs_ampr_0 as usize,
        hk_obs_ampr_1 as usize,
        hk_obs_ampr_2 as usize,
    ];
    for (index, hook) in hooks.iter_mut().enumerate() {
        let class_name = MP_OBSERVE_CLASSES[index];
        let klass = unsafe { find_class(ch.image, ch.domain, "", class_name) };
        if klass.is_null() {
            log_fmt(format_args!("[mpobs] class unavailable: {}", class_name));
            continue;
        }
        let ok = unsafe {
            hook.set_with_signature(
                klass,
                "get_AtkMpRecovery",
                0,
                detours[index],
                true,
                "mpobs",
                Some(&signature),
                Some(&ORIG_OBS_AMPR[index]),
            )
        };
        log_fmt(format_args!(
            "[mpobs] {} get_AtkMpRecovery installed={}",
            class_name, ok
        ));
    }
}
unsafe extern "C" fn hk_no_cost_true(_this: *mut c_void, _method: *const c_void) -> bool {
    true
}

unsafe extern "C" fn hk_calc_cost_mp_0(
    _this: *mut c_void,
    _a1: *mut c_void,
    _a2: i32,
    _method: *const c_void,
) -> i32 {
    0
}

unsafe extern "C" fn hk_pss_mp_1arg(_this: *mut c_void, _a1: i32, _method: *const c_void) -> i32 {
    2_000
}

unsafe extern "C" fn hk_pss_mp_2arg(
    _this: *mut c_void,
    _a1: i32,
    _a2: *mut c_void,
    _method: *const c_void,
) -> i32 {
    2_000
}

unsafe extern "C" fn hk_action_range_30(_this: *mut c_void, _method: *const c_void) -> f32 {
    EXTENDED_RANGE_WORLD_UNITS
}

unsafe extern "C" fn hk_void_1arg(_this: *mut c_void, _a1: *mut c_void, _method: *const c_void) {}

// -----------------------------------------------------------------------------
// Declarative Hook Specifications
// -----------------------------------------------------------------------------

#[inline]
pub(crate) fn is_core_hook_method(method: &str) -> bool {
    matches!(
        method,
        "CalcStable"
            | "CheckHit"
            | "get_SecondaryStatus"
            | "get_CriticalDmg"
            | "get_IsInvincibility"
            | "ActionPreparation"
            | "ActionHit"
            | "CheckActionRange"
            | "OnActionOutOfRange"
            | "CheckRangeHit"
            | "NextRangeHit"
            | "calcPlayerToMobDamage"
            | "GetNextAtkTime"
            | "GetMotionSpeed"
            | "get_AtkMpRecovery"
            // Faunas port targets. Every ported feature is one mechanic, so a
            // half-installed feature is worse than a refused one: a missing
            // hook has to roll the whole feature back instead of leaving a
            // mechanic half live.
            | "checkCriticalPercent"
            | "CheckMagicCritical"
            | "GetSkillDelayRate1"
            | "get_Stable"
            | "get_Hit"
            | "get_CriticalMagicDmg"
            | "get_ExpDefNormal"
            | "get_ExpDefSkill"
            | "get_ExpDefMagic"
            | "GetEquipElement"
            | "CalcPowerResistDamage"
            | "CalcMagicResistDamage"
            | "checkMobReaction"
            | "get_SystemInvincible"
            | "CreateMobAction"
            | "CalcCostMp"
            | "CheckSkillAvailable"
            | "get_Range"
            | "get_IsBattleEnd"
            | "SetBonusGauge"
            | "get_IsBeginner"
            | "get_IsTapLock"
            | "AbnormalPopLabelPlayerToEnemy"
            | "AbnormalResistPopUpLabel"
            | "Update"
    )
}

pub(crate) unsafe fn apply_hook_specs(
    entries: &mut [MethodHookEntry],
    specs: &[HookSpec],
    ch: &GameChain,
    enabling: bool,
    tag: &str,
) -> bool {
    if !enabling {
        return unsafe { restore_hook_entries(entries, tag) };
    }
    if specs.len() > entries.len() {
        log_fmt(format_args!("[{}] insufficient hook storage", tag));
        return false;
    }
    let pbs_class = ch.bstatus_class;
    let pss_class = unsafe { find_class(ch.image, ch.domain, "", "PlayerSecondaryStatus") };
    let pab_class = unsafe { find_class(ch.image, ch.domain, "", "PlayerAttackBase") };
    let sdd_class = unsafe { find_class(ch.image, ch.domain, "", "SkillDamageData") };
    let naa_class = unsafe { find_class(ch.image, ch.domain, "", "NormalAttackAction") };
    let ps_class = ch.status_class;
    let psb_class = unsafe { find_class(ch.image, ch.domain, "", "PlayerStatusBase") };
    let pam_class = if !ch.pam_class.is_null() {
        ch.pam_class
    } else {
        unsafe { find_class(ch.image, ch.domain, "", "PlayerActionManager") }
    };
    let cm_class = if !ch.cm_class.is_null() {
        ch.cm_class
    } else {
        unsafe { find_class(ch.image, ch.domain, "", "CharacterMove") }
    };
    let sab_class = unsafe { find_class(ch.image, ch.domain, "", "SkillActionBase") };
    let dd_class = {
        let direct = unsafe { find_class(ch.image, ch.domain, "", "SkillActionBase/DamageData") };
        if !direct.is_null() {
            direct
        } else if !sab_class.is_null() {
            unsafe { find_nested_class(sab_class, "DamageData") }
        } else {
            unsafe { find_class(ch.image, ch.domain, "", "DamageData") }
        }
    };
    let ad_class = {
        let direct =
            unsafe { find_class(ch.image, ch.domain, "Toram.Common.Actions", "AttackData") };
        if !direct.is_null() {
            direct
        } else {
            unsafe { find_class(ch.image, ch.domain, "", "AttackData") }
        }
    };
    let mdd_class = {
        let direct =
            unsafe { find_class(ch.image, ch.domain, "Toram.Common.Actions", "MobDamageData") };
        if !direct.is_null() {
            direct
        } else {
            unsafe { find_class(ch.image, ch.domain, "", "MobDamageData") }
        }
    };
    let frp_class = unsafe { find_class(ch.image, ch.domain, "", "FieldRayPick") };
    let camb_class = unsafe { find_class(ch.image, ch.domain, "", "CharacterActionManagerBase") };
    let pgs_class = unsafe { find_class(ch.image, ch.domain, "", "PlayerGameStatus") };
    let tps_class = unsafe { find_class(ch.image, ch.domain, "", "TemporaryPlayerStatus") };
    let bmb_class = unsafe { find_class(ch.image, ch.domain, "", "BattleManagerBase") };
    let pbm_class = if !ch.pbm_class.is_null() {
        ch.pbm_class
    } else {
        unsafe { find_class(ch.image, ch.domain, "", "PlayerBattleManager") }
    };

    let mut success = true;
    let mut installed_count = 0usize;
    for (entry, spec) in entries.iter_mut().zip(specs.iter()) {
        let is_core = is_core_hook_method(spec.method)
            || matches!(tag, "speed68" | "nocol69")
            || crate::faunas::feature_bit_for_id(tag) != 0;
        let klass = match spec.target {
            // These declarations are inherited. Gameplay dispatches through the
            // concrete player/ordinary-mob classes, whose vtables are separate.
            HookTargetClass::BattleManagerBase if tag == "range" => pbm_class,
            HookTargetClass::Named("EnemyMobActionManagerBase")
                if tag == "range" && spec.method == "get_Size" =>
            unsafe { find_class(ch.image, ch.domain, "", "MobActionManager") },
            HookTargetClass::PlayerBattleStatus => pbs_class,
            HookTargetClass::PlayerSecondaryStatus => pss_class,
            HookTargetClass::PlayerAttackBase => pab_class,
            HookTargetClass::SkillDamageData => sdd_class,
            HookTargetClass::NormalAttackAction => naa_class,
            HookTargetClass::PlayerStatus => ps_class,
            HookTargetClass::PlayerStatusBase => psb_class,
            HookTargetClass::PlayerActionManager => pam_class,
            HookTargetClass::CharacterMove => cm_class,
            HookTargetClass::DamageData => dd_class,
            HookTargetClass::AttackData => ad_class,
            HookTargetClass::MobDamageData => mdd_class,
            HookTargetClass::SkillActionBase => sab_class,
            HookTargetClass::FieldRayPick => frp_class,
            HookTargetClass::CharacterActionManagerBase => camb_class,
            HookTargetClass::PlayerGameStatus => pgs_class,
            HookTargetClass::TemporaryPlayerStatus => tps_class,
            HookTargetClass::BattleManagerBase => bmb_class,
            HookTargetClass::PlayerBattleManager => pbm_class,
            HookTargetClass::Named(name) => unsafe { find_class(ch.image, ch.domain, "", name) },
        };
        let signature = match spec.method {
            "get_MoveSpeed" | "get_DefaultMoveSpeed" if spec.argc == 0 => Some(MethodSignature {
                parameters: &[],
                return_type: "System.Single",
            }),
            "get_GmMoveSpeedDush"
            | "get_EnableWallHitCheck"
            | "get_EnableAllFloorMove"
            | "get_EnableWallHit"
            | "get_IsGravity"
            | "get_IgnoreHeight"
            | "get_MoveHeightRangeCheck"
                if spec.argc == 0 =>
            {
                Some(MethodSignature {
                    parameters: &[],
                    return_type: "System.Boolean",
                })
            }
            "ActionPreparation" | "ActionHit" if spec.argc == 2 => Some(MethodSignature {
                parameters: &["CharacterActionManagerBase", "UnityEngine.GameObject"],
                return_type: "System.Void",
            }),
            "OnActionOutOfRange" if spec.argc == 2 => Some(MethodSignature {
                parameters: &["UnityEngine.GameObject", "SkillActionBase"],
                return_type: "System.Boolean",
            }),
            "CheckRangeHit" if spec.argc == 3 => Some(MethodSignature {
                parameters: &[
                    "UnityEngine.Transform",
                    "UnityEngine.Transform",
                    "System.Single",
                ],
                return_type: "System.Boolean",
            }),
            "DamageHp" if spec.argc == 2 => Some(MethodSignature {
                parameters: &["System.Int32", "System.Boolean"],
                return_type: "System.Void",
            }),
            "get_AtkMpRecovery" if spec.argc == 0 => Some(MethodSignature {
                parameters: &[],
                return_type: "System.Int32",
            }),
            "CalcCostMp" if spec.argc == 1 => Some(MethodSignature {
                parameters: &["PlayerActionManagerBase"],
                return_type: "System.Int32",
            }),
            "CalcCostMp" if spec.argc == 0 => Some(MethodSignature {
                parameters: &[],
                return_type: "System.Int32",
            }),
            "CheckSkillAvailable" if spec.argc == 1 => Some(MethodSignature {
                parameters: &["SkillId"],
                return_type: "System.Boolean",
            }),
            "get_SecondaryStatus" if spec.argc == 0 => Some(MethodSignature {
                parameters: &[],
                return_type: "IPlayerStatusCalculator",
            }),
            "GetNextAtkTime" if spec.argc == 1 => Some(MethodSignature {
                parameters: &["System.Int32"],
                return_type: "System.Single",
            }),
            "GetMotionSpeed" if spec.argc == 1 => Some(MethodSignature {
                parameters: &["System.Int32"],
                return_type: "System.Int32",
            }),
            "CheckHit" if spec.argc == 5 => Some(MethodSignature {
                parameters: &[
                    "PlayerStatusBase",
                    "System.Int32",
                    "System.Int32",
                    "System.Boolean",
                    "System.Boolean&",
                ],
                return_type: "System.Boolean",
            }),
            "calcBaseDamage" if spec.argc == 5 => Some(MethodSignature {
                parameters: &[
                    "PlayerActionManagerBase",
                    "MobActionManagerBase",
                    "SkillAttackType",
                    "SkillEqLimitFlag",
                    "System.Boolean",
                ],
                return_type: "System.Int32",
            }),
            "CalcStable" if spec.argc == 4 => Some(MethodSignature {
                parameters: &[
                    "SkillAttackType",
                    "System.Int32",
                    "System.Boolean",
                    "PlayerStatusBase",
                ],
                return_type: "System.Single",
            }),
            "get_Critical" | "get_CriticalDmg" => Some(MethodSignature {
                parameters: &[],
                return_type: "System.Int32",
            }),
            "calcPlayerToMobDamage" => Some(MethodSignature {
                parameters: &["PlayerActionManagerBase", "MobActionManagerBase"],
                return_type: "System.Void",
            }),
            "get_ActionRange" if spec.argc == 0 => Some(MethodSignature {
                parameters: &[],
                return_type: "System.Single",
            }),
            "get_IsShukuchiAssistMove" if spec.argc == 0 => Some(MethodSignature {
                parameters: &[],
                return_type: "System.Boolean",
            }),
            "get_IsActiveAssistMove" if spec.argc == 0 => Some(MethodSignature {
                parameters: &[],
                return_type: "System.Boolean",
            }),
            "get_IsMoveAssistContinue" if spec.argc == 0 => Some(MethodSignature {
                parameters: &[],
                return_type: "System.Boolean",
            }),
            "CheckActionRange" if spec.argc == 0 => Some(MethodSignature {
                parameters: &[],
                return_type: "System.Boolean",
            }),
            "RecalcCostMp" if spec.argc == 1 => Some(MethodSignature {
                parameters: &["PlayerActionManagerBase"],
                return_type: "System.Void",
            }),
            // ---- Faunas port targets -------------------------------
            "checkCriticalPercent" if spec.argc == 2 => Some(MethodSignature {
                parameters: &["System.Int32", "MobActionManagerBase"],
                return_type: "System.Boolean",
            }),
            "CheckMagicCritical" if spec.argc == 2 => Some(MethodSignature {
                parameters: &["PlayerStatusBase", "MobActionManagerBase"],
                return_type: "System.Boolean",
            }),
            "CheckMagicCritical" if spec.argc == 3 => Some(MethodSignature {
                parameters: &["System.Int32", "PlayerStatusBase", "MobActionManagerBase"],
                return_type: "System.Boolean",
            }),
            "GetMotionSpeed" if spec.argc == 1 => Some(MethodSignature {
                parameters: &["System.Int32"],
                return_type: "System.Int32",
            }),
            "GetSkillDelayRate1" if spec.argc == 1 => Some(MethodSignature {
                parameters: &["System.Int32"],
                return_type: "System.Int32",
            }),
            "get_Stable" if spec.argc == 0 => Some(MethodSignature {
                parameters: &[],
                return_type: "System.Int32",
            }),
            "get_Hit" if spec.argc == 0 => Some(MethodSignature {
                parameters: &[],
                return_type: "System.Int32",
            }),
            "get_CriticalMagicDmg" if spec.argc == 0 => Some(MethodSignature {
                parameters: &[],
                return_type: "System.Int32",
            }),
            "get_ExpDefNormal" | "get_ExpDefSkill" | "get_ExpDefMagic" if spec.argc == 0 => {
                Some(MethodSignature {
                    parameters: &[],
                    return_type: "System.Int32",
                })
            }
            "GetEquipElement" if spec.argc == 0 => Some(MethodSignature {
                parameters: &[],
                return_type: "ElementType",
            }),
            "CalcPowerResistDamage" | "CalcMagicResistDamage" if spec.argc == 3 => {
                Some(MethodSignature {
                    parameters: &["PlayerStatusBase", "System.Int32", "System.Single"],
                    return_type: "System.Int32",
                })
            }
            "checkMobReaction" if spec.argc == 2 => Some(MethodSignature {
                parameters: &["System.Int32", "System.Int32"],
                return_type: "SkillHitReactionType",
            }),
            "get_SystemInvincible" if spec.argc == 0 => Some(MethodSignature {
                parameters: &[],
                return_type: "System.Boolean",
            }),
            "CreateMobAction" if spec.argc == 1 => Some(MethodSignature {
                parameters: &["MobActionPattern"],
                return_type: "MobAttackBase",
            }),
            "get_Range" if spec.argc == 0 => Some(MethodSignature {
                parameters: &[],
                return_type: "System.Int32",
            }),
            "NextRangeHit" if spec.argc == 0 => Some(MethodSignature {
                parameters: &[],
                return_type: "System.Void",
            }),
            "AbnormalPopLabelPlayerToEnemy" if spec.argc == 3 => Some(MethodSignature {
                parameters: &["AbnormalType", "System.Int32", "UnityEngine.Vector3"],
                return_type: "System.Void",
            }),
            "AbnormalResistPopUpLabel" if spec.argc == 4 => Some(MethodSignature {
                parameters: &[
                    "System.Boolean",
                    "System.Int32",
                    "UnityEngine.Vector3",
                    "System.Single",
                ],
                return_type: "System.Void",
            }),
            "get_IsBattleEnd" if spec.argc == 0 => Some(MethodSignature {
                parameters: &[],
                return_type: "System.Boolean",
            }),
            "get_IsBeginner" | "get_IsTapLock" if spec.argc == 0 => Some(MethodSignature {
                parameters: &[],
                return_type: "System.Boolean",
            }),
            "SetBonusGauge" if spec.argc == 1 => Some(MethodSignature {
                parameters: &["System.Int32"],
                return_type: "System.Void",
            }),
            "SetBonusGauge" if spec.argc == 2 => Some(MethodSignature {
                parameters: &["System.Int32", "System.Int32"],
                return_type: "System.Void",
            }),
            "PlayDropBox" if spec.argc == 1 => Some(MethodSignature {
                parameters: &["EffectPlayer.DropData"],
                return_type: "System.Int32",
            }),
            "get_Size" if spec.argc == 0 => Some(MethodSignature {
                parameters: &[],
                return_type: "System.Single",
            }),
            "Update" if spec.argc == 0 => Some(MethodSignature {
                parameters: &[],
                return_type: "System.Void",
            }),
            _ => None,
        };
        let virtual_only = (tag == "range" && spec.method == "get_Size")
            || matches!(
                spec.method,
                "CheckHit"
                    | "CalcStable"
                    | "ActionHit"
                    | "ActionPreparation"
                    | "CheckActionRange"
                    | "OnActionOutOfRange"
                    | "get_IsMoveAssistContinue"
                    | "CheckRangeHit"
                    | "NextRangeHit"
            );
        if crate::faunas::feature_bit_for_id(tag) != 0 {
            let abi_matches = signature
                .as_ref()
                .and_then(|signature| {
                    let method = unsafe { get_method_exact(klass, spec.method, signature) };
                    let flags = api()?.method_get_flags?;
                    if method.is_null() {
                        return None;
                    }
                    let mut impl_flags = 0;
                    let is_static = unsafe { flags(method, &mut impl_flags) } & 0x10 != 0;
                    Some(
                        is_static
                            == matches!(
                                spec.method,
                                "CalcPowerResistDamage" | "CalcMagicResistDamage"
                            ),
                    )
                })
                .unwrap_or(false);
            if !abi_matches {
                log_fmt(format_args!("[{tag}] method ABI mismatch: {}", spec.method));
                success = false;
                break;
            }
        }
        let installed = if virtual_only {
            signature
                .as_ref()
                .map(|signature| unsafe {
                    entry.set_vtable_with_signature(
                        klass,
                        spec.method,
                        signature,
                        spec.detour,
                        tag,
                        spec.orig_sink,
                    )
                })
                .unwrap_or(false)
        } else {
            unsafe {
                entry.set_with_signature(
                    klass,
                    spec.method,
                    spec.argc,
                    spec.detour,
                    true,
                    tag,
                    signature.as_ref(),
                    spec.orig_sink,
                )
            }
        };
        if klass.is_null() || !installed {
            // A refusal on a designed overlap is excused into skip + partial:
            // either side owning the method means the mechanic lives. Anything
            // else refused keeps failing closed.
            if is_core && !overlap_excused(spec.target, spec.method) {
                log_fmt(format_args!(
                    "[{}] critical hook failed: {}({})",
                    tag, spec.method, spec.argc
                ));
                success = false;
                break;
            } else {
                log_fmt(format_args!(
                    "[{}] optional auxiliary hook skipped: {}({})",
                    tag, spec.method, spec.argc
                ));
                continue;
            }
        }
        if matches!(
            spec.method,
            "CalcStable"
                | "CheckHit"
                | "get_SecondaryStatus"
                | "ActionPreparation"
                | "ActionHit"
                | "CheckActionRange"
                | "OnActionOutOfRange"
        ) && entry.vtable_slot.is_null()
        {
            log_fmt(format_args!(
                "[{}] {} has no reachable derived vtable slot",
                tag, spec.method
            ));
            success = false;
            break;
        }
        installed_count += 1;
    }
    if success && installed_count == 0 && !specs.is_empty() {
        log_fmt(format_args!(
            "[{}] zero hooks installed out of {} specs",
            tag,
            specs.len()
        ));
        success = false;
    }
    let bit = match tag {
        "inv32" => TOGGLE_INVINCIBILITY,
        "speed68" => TOGGLE_SPEED_BOOST,
        "nocol69" => TOGGLE_NO_COLLISION,
        "rigidity70" => TOGGLE_ANTI_RIGIDITY,
        "range71" => TOGGLE_RANGE_30M,
        "inst72" => TOGGLE_INSTANT_ATK,
        "crit73" => TOGGLE_ALWAYS_CRIT,
        "mp74" => TOGGLE_UNLIMITED_MP,
        _ => crate::faunas::feature_bit_for_id(tag),
    };
    if !success {
        let restored = unsafe { restore_hook_entries(entries, tag) };
        log_fmt(format_args!("[{}] rollback complete={}", tag, restored));
        if !restored {
            match bit {
                TOGGLE_RANGE_30M => RANGE_CLEANUP_PENDING.store(true, Ordering::Release),
                TOGGLE_UNLIMITED_MP => MP_CLEANUP_PENDING.store(true, Ordering::Release),
                _ if bit
                    & (TOGGLE_INVINCIBILITY
                        | TOGGLE_INSTANT_ATK
                        | TOGGLE_ALWAYS_CRIT
                        | TOGGLE_ANTI_RIGIDITY)
                    != 0 =>
                {
                    LOCAL_CLEANUP_PENDING.fetch_or(bit, Ordering::Release);
                }
                _ if bit != 0 => {
                    LOCAL_CLEANUP_PENDING.fetch_or(bit, Ordering::Release);
                }
                _ => {}
            }
            note_partial(bit, true);
        }
    }
    if bit != 0 && success {
        note_partial(bit, installed_count != specs.len());
    }
    success
}

unsafe fn restore_hook_entries(entries: &mut [MethodHookEntry], tag: &str) -> bool {
    let mut restored = true;
    for entry in entries.iter_mut().rev() {
        restored &= unsafe { entry.set(ptr::null_mut(), "restore", 0, 0, false, tag) };
    }
    restored
}

unsafe fn restore_hook_table<const N: usize>(
    table: &Mutex<[MethodHookEntry; N]>,
    tag: &str,
) -> bool {
    table
        .lock()
        .map(|mut entries| unsafe { restore_hook_entries(&mut entries[..], tag) })
        .unwrap_or(false)
}

pub(crate) unsafe fn disable_local_feature<const N: usize>(
    hooks: &Mutex<[MethodHookEntry; N]>,
    active: &AtomicBool,
    callbacks: &[ScopedCallback],
    bit: u32,
    tag: &str,
) -> i32 {
    LOCAL_CLEANUP_PENDING.fetch_or(bit, Ordering::Release);
    active.store(false, Ordering::Release);
    for callback in callbacks {
        callback.clear();
    }
    let restored = unsafe { restore_hook_table(hooks, tag) };
    note_partial(bit, !restored);
    if !restored {
        log_fmt(format_args!(
            "[{tag}] restoration incomplete; retry OFF using the saved hooks"
        ));
        return -8;
    }
    LOCAL_CLEANUP_PENDING.fetch_and(!bit, Ordering::Release);
    if let Some(shm) = telemetry() {
        shm.set_toggle_bit(bit, false);
    }
    0
}

fn finish_cleanup(pending: &AtomicBool, bit: u32, restored: bool, tag: &str) -> i32 {
    pending.store(!restored, Ordering::Release);
    note_partial(bit, !restored);
    if restored {
        if let Some(shm) = telemetry() {
            shm.set_toggle_bit(bit, false);
        }
        log_fmt(format_args!("[{tag}] disabled; owned state restored"));
        0
    } else {
        log_fmt(format_args!(
            "[{tag}] restoration incomplete; callbacks inactive, saved state retained for retry"
        ));
        -8
    }
}

unsafe fn disable_attack_range() -> i32 {
    RANGE_CLEANUP_PENDING.store(true, Ordering::Release);
    RANGE_ACTIVE.store(false, Ordering::Release);
    let fields_ok = unsafe { restore_range_state() };
    let hooks_ok = unsafe { restore_hook_table(&ATTACK_RANGE_HOOKS, "range71") };
    let restored = fields_ok
        && hooks_ok
        && RANGE_BINDING
            .write()
            .map(|mut binding| *binding = None)
            .is_ok();
    finish_cleanup(
        &RANGE_CLEANUP_PENDING,
        TOGGLE_RANGE_30M,
        restored,
        "range71",
    )
}

unsafe fn disable_unlimited_mp() -> i32 {
    MP_CLEANUP_PENDING.store(true, Ordering::Release);
    UNLIMITED_MP_ACTIVE.store(false, Ordering::Release);
    let hooks_ok = unsafe { restore_hook_table(&UNLIMITED_MP_HOOKS, "mp74") };
    let tps_ok = unsafe { restore_hook_table(&TPS_MP_HOOKS, "mp74tps") };
    let observers_ok = unsafe { restore_hook_table(&MP_AMPR_OBSERVE_HOOKS, "mpobs") };
    let sustain_ok = unsafe { restore_hook_table(&SUSTAIN_COST_HOOKS, "mp74cost") };
    let restored = hooks_ok
        && tps_ok
        && observers_ok
        && sustain_ok
        && MP_BINDING
            .write()
            .map(|mut binding| *binding = None)
            .is_ok();
    finish_cleanup(&MP_CLEANUP_PENDING, TOGGLE_UNLIMITED_MP, restored, "mp74")
}

pub(crate) fn cleanup_pending(bit: u32) -> bool {
    match bit {
        TOGGLE_RANGE_30M => RANGE_CLEANUP_PENDING.load(Ordering::Acquire),
        TOGGLE_UNLIMITED_MP => MP_CLEANUP_PENDING.load(Ordering::Acquire),
        _ => LOCAL_CLEANUP_PENDING.load(Ordering::Acquire) & bit != 0,
    }
}

/// Records that a feature still owns something it has to restore, so the next
/// OFF retries the cleanup instead of reporting a clean state.
pub(crate) fn mark_cleanup_pending(bit: u32) {
    LOCAL_CLEANUP_PENDING.fetch_or(bit, Ordering::Release);
}

/// Restore owned callback pointers and movement/range field edits before stopping the worker.
pub unsafe fn restore_all_hooks() -> bool {
    critical_path::disable();
    let faunas_restored = crate::faunas::disable_all();
    let range_restored = unsafe { disable_attack_range() } == 0;
    let mp_restored = unsafe { disable_unlimited_mp() } == 0;
    let movement_restored = (unsafe {
        disable_movement(
            &SPEED_FIELDS,
            &SPEED_HOOKS,
            &SPEED_CALLBACKS,
            &SPEED_ACTIVE,
            TOGGLE_SPEED_BOOST,
            "shutdown",
        )
    } == 0)
        & (unsafe {
            disable_movement(
                &NO_COLLISION_FIELDS,
                &NO_COLLISION_HOOKS,
                &NO_COLLISION_CALLBACKS,
                &NO_COLLISION_ACTIVE,
                TOGGLE_NO_COLLISION,
                "shutdown",
            )
        } == 0);
    fn restore<const N: usize>(table: &Mutex<[MethodHookEntry; N]>) -> bool {
        match table.lock() {
            Ok(mut entries) => unsafe { restore_hook_entries(&mut entries[..], "shutdown") },
            Err(_) => false,
        }
    }
    let mut ok = faunas_restored && movement_restored && range_restored && mp_restored;
    ok &= unsafe { crate::combat_response::stop() };
    ok &= unsafe { crate::combat_observe::stop() };
    ok &= unsafe {
        disable_local_feature(
            &ALWAYS_CRIT_HOOKS,
            &ALWAYS_CRIT_ACTIVE,
            &LOCAL_CALLBACKS[11..],
            TOGGLE_ALWAYS_CRIT,
            "shutdown",
        )
    } == 0;
    // The shared critical-damage hooks outlive either feature on its own, so
    // the shutdown path clears every owner and restores them.
    CRIT_DMG_OWNERS.store(0, Ordering::Release);
    ok &= restore(&CRIT_DMG_HOOKS);
    ok &= unsafe {
        disable_local_feature(
            &INSTANT_ATK_HOOKS,
            &INSTANT_ATK_ACTIVE,
            &LOCAL_CALLBACKS[6..11],
            TOGGLE_INSTANT_ATK,
            "shutdown",
        )
    } == 0;
    ok &= unsafe {
        disable_local_feature(
            &CLEAR_DELAY_HOOKS,
            &CLEAR_DELAY_ACTIVE,
            &[],
            TOGGLE_ANTI_RIGIDITY,
            "shutdown",
        )
    } == 0;
    ok &= restore(&SUSTAIN_DELAY_HOOKS);
    ok &= unsafe {
        disable_local_feature(
            &INVINCIBILITY_HOOKS,
            &INVINCIBILITY_ACTIVE,
            &LOCAL_CALLBACKS[..6],
            TOGGLE_INVINCIBILITY,
            "shutdown",
        )
    } == 0;
    ok &= restore(&WATCHDOG_HOOKS);
    if ok {
        PARTIAL_FEATURES.store(0, Ordering::Release);
        for callback in &LOCAL_CALLBACKS {
            callback.clear();
        }
        for state in [
            &ALWAYS_CRIT_ACTIVE,
            &INSTANT_ATK_ACTIVE,
            &UNLIMITED_MP_ACTIVE,
            &CLEAR_DELAY_ACTIVE,
            &NO_COLLISION_ACTIVE,
            &SPEED_ACTIVE,
            &INVINCIBILITY_ACTIVE,
            &RANGE_ACTIVE,
        ] {
            state.store(false, Ordering::Release);
        }
        if let Some(shm) = telemetry() {
            shm.update(|data| {
                data.active_toggles.store(0, Ordering::SeqCst);
                data.observed_toggles.store(0, Ordering::SeqCst);
                data.partial_toggles.store(0, Ordering::SeqCst);
            });
        }
    }
    log_fmt(format_args!(
        "[shutdown] owned hook restoration complete={}",
        ok
    ));
    ok
}

// -----------------------------------------------------------------------------
// Helper to write f32/i32/u8 fields on an IL2CPP object
// -----------------------------------------------------------------------------

unsafe fn write_f32_field(obj: *mut c_void, klass: *mut c_void, name: &str, val: f32) -> bool {
    let Some(api) = api() else {
        return false;
    };
    let f = unsafe { get_field(klass, name) };
    if f.is_null() {
        return false;
    }
    let off = unsafe { (api.field_get_offset)(f) };
    if off <= 0 || off > 0x4000 {
        return false;
    }
    let addr = (obj as usize) + (off as usize);
    if !page_write_ok(addr, 4) {
        return false;
    }
    unsafe {
        *(addr as *mut f32) = val;
    }
    true
}

unsafe fn read_f32_field(obj: *mut c_void, klass: *mut c_void, name: &str) -> Option<f32> {
    let api = api()?;
    let f = unsafe { get_field(klass, name) };
    if f.is_null() {
        return None;
    }
    let off = unsafe { (api.field_get_offset)(f) };
    if !(1..=0x4000).contains(&off) {
        return None;
    }
    let addr = (obj as usize) + (off as usize);
    if !ptr_ok(addr, 4) {
        return None;
    }
    Some(unsafe { *(addr as *const f32) })
}

unsafe fn write_i32_field(obj: *mut c_void, klass: *mut c_void, name: &str, val: i32) -> bool {
    let Some(api) = api() else {
        return false;
    };
    let f = unsafe { get_field(klass, name) };
    if f.is_null() {
        return false;
    }
    let off = unsafe { (api.field_get_offset)(f) };
    if off <= 0 || off > 0x4000 {
        return false;
    }
    let addr = (obj as usize) + (off as usize);
    if !page_write_ok(addr, 4) {
        return false;
    }
    unsafe {
        *(addr as *mut i32) = val;
    }
    true
}

unsafe fn write_u8_field(obj: *mut c_void, klass: *mut c_void, name: &str, val: u8) -> bool {
    let Some(api) = api() else {
        return false;
    };
    let f = unsafe { get_field(klass, name) };
    if f.is_null() {
        return false;
    }
    let off = unsafe { (api.field_get_offset)(f) };
    if off <= 0 || off > 0x4000 {
        return false;
    }
    let addr = (obj as usize) + (off as usize);
    if !page_write_ok(addr, 1) {
        return false;
    }
    unsafe {
        *(addr as *mut u8) = val;
    }
    true
}

// -----------------------------------------------------------------------------
// Individual Feature Commands
// -----------------------------------------------------------------------------

/// `cmd 1`: Probe & log primary/battle status values and publish to `SharedTelemetry`.
pub unsafe fn job_probe() -> i32 {
    log_fmt(format_args!(
        "[combat] stable_calls={} base_damage_calls={} normal_calculations={} hit_checks={} next_attack_calls={} motion_speed_calls={} critical_fallbacks={}",
        STABILITY_HITS.load(Ordering::Relaxed),
        BASE_DAMAGE_HITS.load(Ordering::Relaxed),
        NORMAL_CALC_HITS.load(Ordering::Relaxed),
        HIT_CHECK_CALLS.load(Ordering::Relaxed),
        NEXT_ATTACK_CALLS.load(Ordering::Relaxed),
        MOTION_SPEED_CALLS.load(Ordering::Relaxed),
        critical_path::fallback_count()
    ));
    log_fmt(format_args!(
        "[combat-v95.1] mp_hit_calls={} mp_samples={} mp_recovery_calls={} mp_getter_calls={} mp_getter_other={} last_received_mp={} range_preparations={} range_patches={} range_hit_checks={} range_hit_extensions={} battle_target_fallbacks={}",
        MP_HIT_CALLS.load(Ordering::Relaxed), MP_SAMPLES.load(Ordering::Relaxed),
        MP_RECOVERY_CALLS.load(Ordering::Relaxed), MP_GETTER_CALLS.load(Ordering::Relaxed), MP_GETTER_OTHER.load(Ordering::Relaxed), LAST_RECEIVED_MP.load(Ordering::Relaxed),
        RANGE_PREPARATIONS.load(Ordering::Relaxed), RANGE_PATCHES.load(Ordering::Relaxed),
        RANGE_HIT_CHECKS.load(Ordering::Relaxed), RANGE_HIT_EXTENSIONS.load(Ordering::Relaxed),
        crate::combat_state::battle_target_fallbacks()
    ));
    log_fmt(format_args!(
        "[range-entry] pending_checks={} pending_overrides={} assist_overrides={}",
        RANGE_PENDING_CHECKS.load(Ordering::Relaxed),
        RANGE_PENDING_OVERRIDES.load(Ordering::Relaxed),
        RANGE_ASSIST_OVERRIDES.load(Ordering::Relaxed)
    ));
    validate_or_reset_sdd_cache();
    validate_or_reset_dd_cache();
    let mut gc = GcScope::new();
    let mut ch = GameChain::new();
    if !unsafe { ch.resolve(&mut gc) } {
        log_fmt(format_args!("[probe] chain resolve failed"));
        return -1;
    }

    if !ch.status.is_null() && !ch.status_class.is_null() {
        let primary =
            unsafe { read_inst_ptr(ch.status, ch.status_class, "primaryStatus", &mut gc) };
        let primary_class = if is_valid_il2cpp_obj(primary) {
            unsafe { *(primary as *const *mut c_void) }
        } else {
            ptr::null_mut()
        };
        let names = ["str", "int", "vit", "agi", "dex", "crt"];
        let values =
            names.map(|name| unsafe { read_i32_field(primary, primary_class, name) }.unwrap_or(-1));
        if let Some(shm) = telemetry() {
            shm.update(|data| {
                for (slot, value) in data.stats.iter().zip(values) {
                    slot.store(value, Ordering::SeqCst);
                }
            });
        }
        log_fmt(format_args!(
            "[probe] PlayerStatus={:p} primary={:p} stats={:?}",
            ch.status, primary, values
        ));
    }

    if !ch.bstatus.is_null() && !ch.bstatus_class.is_null() {
        let game = unsafe { read_inst_ptr(ch.status, ch.status_class, "gameStatus", &mut gc) };
        let game_class = if unsafe { is_valid_il2cpp_obj(game) } {
            unsafe { *(game as *const *mut c_void) }
        } else {
            ptr::null_mut()
        };
        let hp = unsafe { read_i32_field(game, game_class, "localHp") }.unwrap_or(-1);
        let max_hp = unsafe { call_i32_0(ch.bstatus_class, ch.bstatus, "get_MaxHp") }.unwrap_or(-1);
        let mp = unsafe { read_i32_field(game, game_class, "localMp") }.unwrap_or(-1);
        let received_mp = unsafe { read_i32_field(game, game_class, "serverMp") }.unwrap_or(-1);
        LAST_RECEIVED_MP.store(received_mp, Ordering::Relaxed);
        let max_mp = unsafe { call_i32_0(ch.bstatus_class, ch.bstatus, "get_MaxMp") }.unwrap_or(-1);
        if let Some(shm) = telemetry() {
            shm.update(|data| {
                data.now_hp.store(hp, Ordering::SeqCst);
                data.max_hp.store(max_hp, Ordering::SeqCst);
                data.now_mp.store(mp, Ordering::SeqCst);
                data.max_mp.store(max_mp, Ordering::SeqCst);
                data.received_mp.store(received_mp, Ordering::SeqCst);
                data.mp_sample_tick_ms
                    .store(protocol::monotonic_ms(), Ordering::SeqCst);
                data.resource_tick_ms
                    .store(protocol::monotonic_ms(), Ordering::SeqCst);
            });
        }
        log_fmt(format_args!(
            "[probe] PlayerBattleStatus={:p} HP={}/{} MP={}/{} received_mp={}",
            ch.bstatus, hp, max_hp, mp, max_mp, received_mp
        ));
    }

    0
}

/// `cmd 2`: Restore local HP/MP prediction without changing received server state.
pub unsafe fn job_heal() -> i32 {
    let mut gc = GcScope::new();
    let mut ch = GameChain::new();
    if !unsafe { ch.resolve(&mut gc) } {
        return -1;
    }
    let max_hp = unsafe { call_i32_0(ch.bstatus_class, ch.bstatus, "get_MaxHp") }.unwrap_or(0);
    let max_mp = unsafe { call_i32_0(ch.bstatus_class, ch.bstatus, "get_MaxMp") }.unwrap_or(0);
    let game = unsafe { read_inst_ptr(ch.status, ch.status_class, "gameStatus", &mut gc) };
    if !unsafe { is_valid_il2cpp_obj(game) } || max_hp <= 0 || max_mp <= 0 {
        return -8;
    }
    let class = unsafe { *(game as *const *mut c_void) };
    let hp_ok = unsafe { write_i32_field(game, class, "localHp", max_hp) };
    let mp_ok = unsafe { write_i32_field(game, class, "localMp", max_mp) };
    if !hp_ok || !mp_ok {
        return -8;
    }
    if let Some(shm) = telemetry() {
        shm.update(|data| {
            data.now_hp.store(max_hp, Ordering::SeqCst);
            data.max_hp.store(max_hp, Ordering::SeqCst);
            data.now_mp.store(max_mp, Ordering::SeqCst);
            data.max_mp.store(max_mp, Ordering::SeqCst);
            data.resource_tick_ms
                .store(protocol::monotonic_ms(), Ordering::SeqCst);
        });
    }
    log_fmt(format_args!(
        "[heal] local HP={} MP={}; received state preserved",
        max_hp, max_mp
    ));
    0
}

/// `cmd 3` / `cmd 32`: Toggle local-player invincibility without resource writes.
pub unsafe fn job_invincibility_toggle() -> i32 {
    if INVINCIBILITY_ACTIVE.load(Ordering::Acquire) || cleanup_pending(TOGGLE_INVINCIBILITY) {
        return unsafe {
            disable_local_feature(
                &INVINCIBILITY_HOOKS,
                &INVINCIBILITY_ACTIVE,
                &LOCAL_CALLBACKS[..6],
                TOGGLE_INVINCIBILITY,
                "inv32",
            )
        };
    }
    let mut gc = GcScope::new();
    let mut ch = GameChain::new();
    if !unsafe { ch.resolve(&mut gc) } {
        log_fmt(format_args!("[inv32] GameChain resolve failed"));
        return -1;
    }

    let specs = [
        HookSpec {
            target: HookTargetClass::PlayerStatus,
            method: "get_IsInvincibility",
            argc: 0,
            detour: local_invincibility_bool::<0> as usize,
            orig_sink: Some(&LOCAL_CALLBACKS[0].original),
        },
        HookSpec {
            target: HookTargetClass::PlayerStatusBase,
            method: "get_IsInvincibility",
            argc: 0,
            detour: local_invincibility_bool::<1> as usize,
            orig_sink: Some(&LOCAL_CALLBACKS[1].original),
        },
        HookSpec {
            target: HookTargetClass::PlayerStatusBase,
            method: "DamageHp",
            argc: 2,
            detour: local_invincibility_damage as usize,
            orig_sink: Some(&LOCAL_CALLBACKS[2].original),
        },
        HookSpec {
            target: HookTargetClass::PlayerStatus,
            method: "get_IsDead",
            argc: 0,
            detour: local_invincibility_bool::<3> as usize,
            orig_sink: Some(&LOCAL_CALLBACKS[3].original),
        },
        HookSpec {
            target: HookTargetClass::PlayerStatus,
            method: "get_IsLocalDead",
            argc: 0,
            detour: local_invincibility_bool::<4> as usize,
            orig_sink: Some(&LOCAL_CALLBACKS[4].original),
        },
        HookSpec {
            target: HookTargetClass::PlayerStatus,
            method: "get_IsActDead",
            argc: 0,
            detour: local_invincibility_bool::<5> as usize,
            orig_sink: Some(&LOCAL_CALLBACKS[5].original),
        },
    ];

    let enabling = !INVINCIBILITY_ACTIVE.load(Ordering::Relaxed);
    if enabling && !unsafe { bind_local_callbacks(0, &[ch.status; 6]) } {
        return -8;
    }
    if let Ok(mut hooks) = INVINCIBILITY_HOOKS.lock() {
        unsafe {
            if !apply_hook_specs(&mut hooks[..specs.len()], &specs, &ch, enabling, "inv32") {
                return -8;
            }
        }
    } else {
        return -8;
    }

    INVINCIBILITY_ACTIVE.store(enabling, Ordering::Relaxed);
    if let Some(shm) = telemetry() {
        shm.set_toggle_bit(TOGGLE_INVINCIBILITY, enabling);
    }
    log_fmt(format_args!(
        "[inv32] Local invincibility {} (other instances forward native; resources unchanged)",
        if enabling { "ENABLED" } else { "DISABLED" }
    ));
    feature_setup_result(TOGGLE_INVINCIBILITY, enabling)
}

const SPEED_CM_FIELDS: [(&str, MovementValue); 2] = [
    (
        "moveSpeed",
        MovementValue::Float(SPEED_FIELD_VALUE.to_bits()),
    ),
    (
        "defaultMoveSpeed",
        MovementValue::Float(SPEED_FIELD_VALUE.to_bits()),
    ),
];
const SPEED_PAM_FIELDS: [(&str, MovementValue); 1] = [(
    "<DefaultMoveSpeed>k__BackingField",
    MovementValue::Float(SPEED_FIELD_VALUE.to_bits()),
)];
const NO_COLLISION_CM_FIELDS: [(&str, MovementValue); 4] = [
    ("isGravity", MovementValue::Bool(0)),
    ("gravity", MovementValue::Float(0.0f32.to_bits())),
    ("ignoreHeight", MovementValue::Bool(1)),
    (
        "<MoveHeightRangeCheck>k__BackingField",
        MovementValue::Bool(0),
    ),
];
const NO_COLLISION_FRP_FIELDS: [(&str, MovementValue); 6] = [
    (
        "<EnableWallHitCheck>k__BackingField",
        MovementValue::Bool(0),
    ),
    (
        "<EnableAllFloorMove>k__BackingField",
        MovementValue::Bool(1),
    ),
    (
        "<EnableFloorHitCheck>k__BackingField",
        MovementValue::Bool(0),
    ),
    (
        "<EnableOnGroundMove>k__BackingField",
        MovementValue::Bool(0),
    ),
    ("rad", MovementValue::Float(0.001f32.to_bits())),
    ("height", MovementValue::Float(0.001f32.to_bits())),
];

fn speed_hook_specs() -> [HookSpec; 6] {
    [
        HookSpec {
            target: HookTargetClass::PlayerActionManager,
            method: "get_MoveSpeed",
            argc: 0,
            detour: local_move_speed::<0> as usize,
            orig_sink: Some(&SPEED_CALLBACKS[0].original),
        },
        HookSpec {
            target: HookTargetClass::CharacterActionManagerBase,
            method: "get_MoveSpeed",
            argc: 0,
            detour: local_move_speed::<1> as usize,
            orig_sink: Some(&SPEED_CALLBACKS[1].original),
        },
        HookSpec {
            target: HookTargetClass::CharacterActionManagerBase,
            method: "get_DefaultMoveSpeed",
            argc: 0,
            detour: local_move_speed::<2> as usize,
            orig_sink: Some(&SPEED_CALLBACKS[2].original),
        },
        HookSpec {
            target: HookTargetClass::CharacterMove,
            method: "get_MoveSpeed",
            argc: 0,
            detour: local_move_speed::<3> as usize,
            orig_sink: Some(&SPEED_CALLBACKS[3].original),
        },
        HookSpec {
            target: HookTargetClass::CharacterMove,
            method: "get_DefaultMoveSpeed",
            argc: 0,
            detour: local_move_speed::<4> as usize,
            orig_sink: Some(&SPEED_CALLBACKS[4].original),
        },
        HookSpec {
            target: HookTargetClass::PlayerActionManager,
            method: "get_GmMoveSpeedDush",
            argc: 0,
            detour: local_speed_dash as usize,
            orig_sink: Some(&SPEED_CALLBACKS[5].original),
        },
    ]
}

fn no_collision_hook_specs() -> [HookSpec; 6] {
    [
        HookSpec {
            target: HookTargetClass::FieldRayPick,
            method: "get_EnableWallHitCheck",
            argc: 0,
            detour: local_no_collision::<0> as usize,
            orig_sink: Some(&NO_COLLISION_CALLBACKS[0].original),
        },
        HookSpec {
            target: HookTargetClass::FieldRayPick,
            method: "get_EnableAllFloorMove",
            argc: 0,
            detour: local_no_collision::<1> as usize,
            orig_sink: Some(&NO_COLLISION_CALLBACKS[1].original),
        },
        HookSpec {
            target: HookTargetClass::CharacterMove,
            method: "get_EnableWallHit",
            argc: 0,
            detour: local_no_collision::<2> as usize,
            orig_sink: Some(&NO_COLLISION_CALLBACKS[2].original),
        },
        HookSpec {
            target: HookTargetClass::CharacterMove,
            method: "get_IsGravity",
            argc: 0,
            detour: local_no_collision::<3> as usize,
            orig_sink: Some(&NO_COLLISION_CALLBACKS[3].original),
        },
        HookSpec {
            target: HookTargetClass::CharacterMove,
            method: "get_IgnoreHeight",
            argc: 0,
            detour: local_no_collision::<4> as usize,
            orig_sink: Some(&NO_COLLISION_CALLBACKS[4].original),
        },
        HookSpec {
            target: HookTargetClass::CharacterMove,
            method: "get_MoveHeightRangeCheck",
            argc: 0,
            detour: local_no_collision::<5> as usize,
            orig_sink: Some(&NO_COLLISION_CALLBACKS[5].original),
        },
    ]
}

/// `cmd 68`: Restore v0.96.1 speed, with exact owned-state restoration on OFF.
pub unsafe fn job_speed_boost() -> i32 {
    if SPEED_ACTIVE.load(Ordering::Acquire) || cleanup_pending(TOGGLE_SPEED_BOOST) {
        return unsafe {
            disable_movement(
                &SPEED_FIELDS,
                &SPEED_HOOKS,
                &SPEED_CALLBACKS,
                &SPEED_ACTIVE,
                TOGGLE_SPEED_BOOST,
                "speed68",
            )
        };
    }
    let mut gc = GcScope::new();
    let mut ch = GameChain::new();
    if !unsafe { ch.resolve(&mut gc) } {
        return -1;
    }
    let mut snapshot = MovementFields::new();
    let captured = unsafe {
        snapshot
            .capture_fields(ch.cm, &SPEED_CM_FIELDS)
            .and_then(|()| snapshot.capture_fields(ch.pam, &SPEED_PAM_FIELDS))
    };
    if let Err(reason) = captured {
        log_fmt(format_args!("[speed68] field capture rejected: {reason}"));
        return -8;
    }
    let feature = MovementFeature {
        fields: &SPEED_FIELDS,
        hooks: &SPEED_HOOKS,
        callbacks: &SPEED_CALLBACKS,
        active: &SPEED_ACTIVE,
        bit: TOGGLE_SPEED_BOOST,
        tag: "speed68",
    };
    unsafe {
        feature.enable(
            &ch,
            &speed_hook_specs(),
            [ch.pam, ch.pam, ch.pam, ch.cm, ch.cm, ch.pam],
            snapshot,
        )
    }
}

/// `cmd 69`: Restore v0.96.1 wall/gravity override and the native collision fields on OFF.
pub unsafe fn job_no_collision() -> i32 {
    if NO_COLLISION_ACTIVE.load(Ordering::Acquire) || cleanup_pending(TOGGLE_NO_COLLISION) {
        return unsafe {
            disable_movement(
                &NO_COLLISION_FIELDS,
                &NO_COLLISION_HOOKS,
                &NO_COLLISION_CALLBACKS,
                &NO_COLLISION_ACTIVE,
                TOGGLE_NO_COLLISION,
                "nocol69",
            )
        };
    }
    let mut gc = GcScope::new();
    let mut ch = GameChain::new();
    if !unsafe { ch.resolve(&mut gc) } {
        return -1;
    }
    let frp = unsafe { read_inst_ptr(ch.cm, ch.cm_class, "fieldRay", &mut gc) };
    let mut snapshot = MovementFields::new();
    let captured = unsafe {
        snapshot
            .capture_fields(ch.cm, &NO_COLLISION_CM_FIELDS)
            .and_then(|()| snapshot.capture_fields(frp, &NO_COLLISION_FRP_FIELDS))
    };
    if let Err(reason) = captured {
        log_fmt(format_args!("[nocol69] field capture rejected: {reason}"));
        return -8;
    }
    let feature = MovementFeature {
        fields: &NO_COLLISION_FIELDS,
        hooks: &NO_COLLISION_HOOKS,
        callbacks: &NO_COLLISION_CALLBACKS,
        active: &NO_COLLISION_ACTIVE,
        bit: TOGGLE_NO_COLLISION,
        tag: "nocol69",
    };
    unsafe {
        feature.enable(
            &ch,
            &no_collision_hook_specs(),
            [frp, frp, ch.cm, ch.cm, ch.cm, ch.cm],
            snapshot,
        )
    }
}

/// `cmd 70`: Anti-Rigidity & Clear Delay (`NormalAttackAction.get_IsHitRigidity` false + clear timers).
pub unsafe fn job_clear_delay() -> i32 {
    if CLEAR_DELAY_ACTIVE.load(Ordering::Acquire) || cleanup_pending(TOGGLE_ANTI_RIGIDITY) {
        let sustain_ok = unsafe { restore_hook_table(&SUSTAIN_DELAY_HOOKS, "rigidity70delay") };
        let core = unsafe {
            disable_local_feature(
                &CLEAR_DELAY_HOOKS,
                &CLEAR_DELAY_ACTIVE,
                &[],
                TOGGLE_ANTI_RIGIDITY,
                "rigidity70",
            )
        };
        if !sustain_ok {
            return -8;
        }
        return core;
    }
    let mut gc = GcScope::new();
    let mut ch = GameChain::new();
    if !unsafe { ch.resolve(&mut gc) } {
        return -1;
    }

    let specs = [
        HookSpec {
            target: HookTargetClass::NormalAttackAction,
            method: "get_IsHitRigidity",
            argc: 0,
            detour: hk_false_0arg as usize,
            orig_sink: None,
        },
        HookSpec {
            target: HookTargetClass::BattleManagerBase,
            method: "get_IsDelay",
            argc: 0,
            detour: hk_false_0arg as usize,
            orig_sink: None,
        },
        HookSpec {
            target: HookTargetClass::BattleManagerBase,
            method: "get_IsRigidity",
            argc: 0,
            detour: hk_false_0arg as usize,
            orig_sink: None,
        },
    ];

    let mut fields_ok = true;
    let enabling = !CLEAR_DELAY_ACTIVE.load(Ordering::Relaxed);
    if let Ok(mut hooks) = CLEAR_DELAY_HOOKS.lock() {
        unsafe {
            if !apply_hook_specs(&mut hooks[..], &specs, &ch, enabling, "rigidity70") {
                return -8;
            }
        }
    } else {
        return -8;
    }

    if enabling && !ch.pbm.is_null() && !ch.pbm_class.is_null() {
        unsafe {
            fields_ok &= call_void0(ch.pbm_class, ch.pbm, "ClearDelay");
            fields_ok &= call_void0(ch.pbm_class, ch.pbm, "ClearRigidity");
            fields_ok &= write_f32_field(ch.pbm, ch.pbm_class, "attackDelay", 0.0);
            fields_ok &= write_f32_field(ch.pbm, ch.pbm_class, "partsAttackDelay", 0.0);
            fields_ok &= write_f32_field(ch.pbm, ch.pbm_class, "guardNextAttackDelay", 0.0);
            fields_ok &= write_u8_field(ch.pbm, ch.pbm_class, "<IsDelay>k__BackingField", 0);
            fields_ok &= write_u8_field(ch.pbm, ch.pbm_class, "<IsRigidity>k__BackingField", 0);
            fields_ok &= write_u8_field(
                ch.pbm,
                ch.pbm_class,
                "<IsBattleActionLock>k__BackingField",
                0,
            );
        }
    }
    // Companion sustain: skill reuse override (best-effort, server tetap mengunci via standby).
    {
        let delay_spec = HookSpec {
            target: HookTargetClass::Named("SkillDelayManager"),
            method: "CheckSkillAvailable",
            argc: 1,
            detour: hk_sustain_skill_available as usize,
            orig_sink: Some(&ORIG_SUSTAIN_DELAY),
        };
        if let Ok(mut sustain_hooks) = SUSTAIN_DELAY_HOOKS.lock() {
            unsafe {
                let ok = apply_hook_specs(
                    &mut sustain_hooks[..],
                    &[delay_spec],
                    &ch,
                    enabling,
                    "rigidity70delay",
                );
                log_fmt(format_args!(
                    "[rigidity70delay] CheckSkillAvailable sustain {}",
                    if ok {
                        "ENABLED full-override"
                    } else {
                        "SKIPPED target absen"
                    }
                ));
            }
        }
    }

    CLEAR_DELAY_ACTIVE.store(enabling, Ordering::Relaxed);
    if enabling && !fields_ok {
        note_partial(TOGGLE_ANTI_RIGIDITY, true);
    }
    if let Some(shm) = telemetry() {
        shm.set_toggle_bit(TOGGLE_ANTI_RIGIDITY, enabling);
    }
    log_fmt(format_args!(
        "[rigidity70] Anti-Rigidity & Clear Delay {}",
        if enabling { "ENABLED" } else { "DISABLED" }
    ));
    feature_setup_result(TOGGLE_ANTI_RIGIDITY, enabling)
}

/// `cmd 71`: Toggle local queued-action assist movement.
pub unsafe fn job_attack_range() -> i32 {
    if RANGE_ACTIVE.load(Ordering::Acquire) || RANGE_CLEANUP_PENDING.load(Ordering::Acquire) {
        return unsafe { disable_attack_range() };
    }
    let mut gc = GcScope::new();
    let mut ch = GameChain::new();
    if !unsafe { ch.resolve(&mut gc) } {
        return -1;
    }
    let enabling = true;
    if enabling {
        let binding = match unsafe { RangeBinding::prepare(&ch) } {
            Ok(binding) => binding,
            Err(reason) => {
                log_fmt(format_args!("[range71] preparation rejected: {}", reason));
                return -8;
            }
        };
        let Ok(mut config) = RANGE_BINDING.write() else {
            return -8;
        };
        *config = Some(binding);
    }

    let specs = range_hook_specs();

    if let Ok(mut hooks) = ATTACK_RANGE_HOOKS.lock() {
        unsafe {
            if !apply_hook_specs(&mut hooks[..specs.len()], &specs, &ch, enabling, "range71") {
                return -8;
            }
        }
    } else {
        return -8;
    }

    RANGE_ACTIVE.store(enabling, Ordering::Relaxed);
    if let Some(shm) = telemetry() {
        shm.set_toggle_bit(TOGGLE_RANGE_30M, enabling);
    }
    log_fmt(format_args!(
        "[range71] Engage {} (assist-move only; native range checks kept, no out-of-range claims)",
        if enabling { "ENABLED" } else { "DISABLED" }
    ));
    feature_setup_result(TOGGLE_RANGE_30M, enabling)
}

/// The hooks the range feature installs.
///
/// Assist-move only; native range, hit-size and action-range checks remain.
/// These hooks request movement toward the target. They do not establish
/// remote acceptance or the cause of any historical disconnect.
fn range_hook_specs() -> [HookSpec; 3] {
    [
        HookSpec {
            target: HookTargetClass::PlayerAttackBase,
            method: "get_IsShukuchiAssistMove",
            argc: 0,
            detour: hk_assist_move_bool::<0> as usize,
            orig_sink: Some(&RANGE_ASSIST_ORIGINALS[0]),
        },
        HookSpec {
            target: HookTargetClass::PlayerAttackBase,
            method: "get_IsActiveAssistMove",
            argc: 0,
            detour: hk_assist_move_bool::<1> as usize,
            orig_sink: Some(&RANGE_ASSIST_ORIGINALS[1]),
        },
        HookSpec {
            target: HookTargetClass::NormalAttackAction,
            method: "get_IsMoveAssistContinue",
            argc: 0,
            detour: hk_assist_move_bool::<2> as usize,
            orig_sink: Some(&RANGE_ASSIST_ORIGINALS[2]),
        },
    ]
}

/// `cmd 72`: Toggle Instant Attack / Zero Attack Delay.
pub unsafe fn job_instant_attack() -> i32 {
    if INSTANT_ATK_ACTIVE.load(Ordering::Acquire) || cleanup_pending(TOGGLE_INSTANT_ATK) {
        return unsafe {
            disable_local_feature(
                &INSTANT_ATK_HOOKS,
                &INSTANT_ATK_ACTIVE,
                &LOCAL_CALLBACKS[6..11],
                TOGGLE_INSTANT_ATK,
                "inst72",
            )
        };
    }
    let mut gc = GcScope::new();
    let mut ch = GameChain::new();
    if !unsafe { ch.resolve(&mut gc) } {
        return -1;
    }

    let specs = [
        HookSpec {
            target: HookTargetClass::PlayerSecondaryStatus,
            method: "GetNextAtkTime",
            argc: 1,
            detour: local_next_attack::<6> as usize,
            orig_sink: Some(&LOCAL_CALLBACKS[6].original),
        },
        HookSpec {
            target: HookTargetClass::PlayerSecondaryStatus,
            method: "GetMotionSpeed",
            argc: 1,
            detour: local_motion::<7> as usize,
            orig_sink: Some(&LOCAL_CALLBACKS[7].original),
        },
        HookSpec {
            target: HookTargetClass::PlayerBattleStatus,
            method: "GetNextAtkTime",
            argc: 1,
            detour: local_next_attack::<8> as usize,
            orig_sink: Some(&LOCAL_CALLBACKS[8].original),
        },
        HookSpec {
            target: HookTargetClass::PlayerBattleStatus,
            method: "GetMotionSpeed",
            argc: 1,
            detour: local_motion::<9> as usize,
            orig_sink: Some(&LOCAL_CALLBACKS[9].original),
        },
        HookSpec {
            target: HookTargetClass::PlayerBattleStatus,
            method: "get_Aspd",
            argc: 0,
            detour: local_stat::<10, 10000> as usize,
            orig_sink: Some(&LOCAL_CALLBACKS[10].original),
        },
    ];

    let enabling = !INSTANT_ATK_ACTIVE.load(Ordering::Relaxed);
    if enabling
        && !unsafe {
            bind_local_callbacks(6, &[ch.pss, ch.pss, ch.bstatus, ch.bstatus, ch.bstatus])
        }
    {
        return -8;
    }
    if let Ok(mut hooks) = INSTANT_ATK_HOOKS.lock() {
        unsafe {
            if !apply_hook_specs(&mut hooks[..], &specs, &ch, enabling, "inst72") {
                return -8;
            }
        }
    } else {
        return -8;
    }

    INSTANT_ATK_ACTIVE.store(enabling, Ordering::Relaxed);
    if let Some(shm) = telemetry() {
        shm.set_toggle_bit(TOGGLE_INSTANT_ATK, enabling);
    }
    log_fmt(format_args!(
        "[inst72] Instant Attack {}",
        if enabling { "ENABLED" } else { "DISABLED" }
    ));
    feature_setup_result(TOGGLE_INSTANT_ATK, enabling)
}

pub(crate) unsafe fn prepare_normal_critical_path(ch: &GameChain) -> Result<(), &'static str> {
    let api = api().ok_or("IL2CPP API unavailable")?;
    let naa = unsafe { find_class(ch.image, ch.domain, "", "NormalAttackAction") };
    let secondary = unsafe { find_class(ch.image, ch.domain, "", "PlayerSecondaryStatus") };
    let attack_base = unsafe { find_class(ch.image, ch.domain, "", "PlayerAttackBase") };
    let calculator = unsafe { find_class(ch.image, ch.domain, "", "IPlayerStatusCalculator") };
    let normal_method = unsafe {
        get_method_exact(
            naa,
            "calcPlayerToMobDamage",
            &MethodSignature {
                parameters: &["PlayerActionManagerBase", "MobActionManagerBase"],
                return_type: "System.Void",
            },
        )
    };
    let getter_method = unsafe {
        get_method_exact(
            ch.status_class,
            "get_SecondaryStatus",
            &MethodSignature {
                parameters: &[],
                return_type: "IPlayerStatusCalculator",
            },
        )
    };
    let critical_method = unsafe {
        get_method_exact(
            attack_base,
            "checkCriticalPercent",
            &MethodSignature {
                parameters: &["System.Int32", "MobActionManagerBase"],
                return_type: "System.Boolean",
            },
        )
    };
    if !ptr_ok(normal_method as usize, core::mem::size_of::<usize>())
        || !ptr_ok(getter_method as usize, core::mem::size_of::<usize>())
        || !ptr_ok(critical_method as usize, core::mem::size_of::<usize>())
    {
        return Err("normal attack or secondary getter signature unavailable");
    }
    let field = unsafe { get_field(ch.status_class, "battleStatus") };
    if field.is_null() {
        return Err("battleStatus field unavailable");
    }
    let offset = unsafe { (api.field_get_offset)(field) };
    if !(0x10..=0x400).contains(&offset) {
        return Err("battleStatus field offset invalid");
    }
    unsafe {
        critical_path::prepare(
            ch.status,
            ch.status_class,
            secondary,
            ch.bstatus_class,
            offset as usize,
            *(normal_method as *const usize),
            *(getter_method as *const usize),
            calculator,
            *(critical_method as *const usize),
        )
    }
}

/// The shared `get_CriticalDmg` replacements.
///
/// The port owns the declaration; this is the single owner both features use.
fn crit_dmg_specs() -> [HookSpec; 2] {
    crate::faunas::critdmg_specs()
}

/// The owner bookkeeping for the shared critical-damage hooks.
///
/// The hooks are installed by the first owner and restored by the last one to
/// let go, so always-critical and the Faunas ladder can be turned on and off
/// in either order without either one losing its hooks. Returns the new owner
/// set and whether the hooks have to be installed or restored.
fn crit_dmg_owner_step(previous: u32, owner: u32, acquire: bool) -> (u32, bool) {
    if acquire {
        (previous | owner, previous == 0)
    } else {
        let next = previous & !owner;
        (next, previous != 0 && next == 0)
    }
}

/// Takes a share of the shared critical-damage hooks.
///
/// The first owner installs them; a later owner only records its claim and
/// reuses the hooks that are already there. The hooks stay installed until
/// the last owner releases them, so always-critical and the Faunas ladder can
/// be turned on and off in either order without either one losing its hooks.
pub(crate) unsafe fn crit_dmg_acquire(owner: u32, ch: &GameChain) -> bool {
    let previous = CRIT_DMG_OWNERS.fetch_or(owner, Ordering::AcqRel);
    let (_, install) = crit_dmg_owner_step(previous, owner, true);
    if !install {
        return true;
    }
    let specs = crit_dmg_specs();
    let Ok(mut hooks) = CRIT_DMG_HOOKS.lock() else {
        CRIT_DMG_OWNERS.fetch_and(!owner, Ordering::AcqRel);
        return false;
    };
    if !unsafe { apply_hook_specs(&mut hooks[..specs.len()], &specs, ch, true, "critdmg") } {
        // An incomplete rollback still owns native pointers. Preserve the owner
        // so the subsequent OFF retries restoring this shared table.
        if !hooks.iter().any(|entry| entry.active) {
            CRIT_DMG_OWNERS.fetch_and(!owner, Ordering::AcqRel);
        }
        return false;
    }
    true
}

/// Releases a share of the shared critical-damage hooks.
///
/// The hooks are only restored once the last owner lets go.
pub(crate) unsafe fn crit_dmg_release(owner: u32) -> bool {
    let previous = CRIT_DMG_OWNERS.fetch_and(!owner, Ordering::AcqRel);
    let (_, restore) = crit_dmg_owner_step(previous, owner, false);
    if !restore {
        return true;
    }
    let restored = unsafe { restore_hook_table(&CRIT_DMG_HOOKS, "critdmg") };
    if !restored {
        CRIT_DMG_OWNERS.fetch_or(owner, Ordering::AcqRel);
    }
    restored
}

/// Positions of the always-critical specs the Faunas port wins outright.
///
/// `PlayerBattleStatus.get_Stable` is index 1 and `PlayerBattleStatus.get_Hit`
/// is index 3 in the enable-time spec table below. The port's `stable`
/// (1000) and `mob_no_flee` (65535) values replace always-critical's sidecar
/// values (100 / 10000) whenever the port's features are on.
const ALWAYS_CRIT_PBS_STABLE: usize = 1;
const ALWAYS_CRIT_PBS_HIT: usize = 3;

/// The always-critical specs that collide with the Faunas port.
///
/// A `MethodInfo` has exactly one owner, so these two specs yield while the
/// port's `stable`/`mob_no_flee` features are on: they are restored before
/// the port installs and reinstalled after it lets go. This is the single
/// source of truth for both the enable table and the reclaim path.
fn always_crit_conflict_specs() -> [HookSpec; 4] {
    [
        HookSpec {
            target: HookTargetClass::PlayerBattleStatus,
            method: "get_Stable",
            argc: 0,
            detour: local_stat::<12, 100> as usize,
            orig_sink: Some(&LOCAL_CALLBACKS[12].original),
        },
        HookSpec {
            target: HookTargetClass::PlayerBattleStatus,
            method: "get_Hit",
            argc: 0,
            detour: local_stat::<14, 10000> as usize,
            orig_sink: Some(&LOCAL_CALLBACKS[14].original),
        },
        HookSpec {
            target: HookTargetClass::PlayerSecondaryStatus,
            method: "get_Stable",
            argc: 0,
            detour: local_stat::<16, 100> as usize,
            orig_sink: Some(&LOCAL_CALLBACKS[16].original),
        },
        HookSpec {
            target: HookTargetClass::PlayerSecondaryStatus,
            method: "get_Hit",
            argc: 0,
            detour: local_stat::<18, 10000> as usize,
            orig_sink: Some(&LOCAL_CALLBACKS[18].original),
        },
    ]
}

/// Whether always-critical is currently holding its hooks.
pub(crate) fn always_crit_active() -> bool {
    ALWAYS_CRIT_ACTIVE.load(Ordering::Acquire)
}

/// Yields the port-won methods to the Faunas port.
///
/// Restores always-critical's `get_Stable`/`get_Hit` entries so the port can
/// install its own values. Entries the port already owns are already
/// yielded. Returns false when a live entry cannot be restored.
pub(crate) unsafe fn always_crit_yield_stable_hit() -> bool {
    if !always_crit_active() {
        return true;
    }
    let Ok(mut hooks) = ALWAYS_CRIT_HOOKS.lock() else {
        return false;
    };
    for index in [ALWAYS_CRIT_PBS_STABLE, ALWAYS_CRIT_PBS_HIT, 5, 7] {
        let Some(entry) = hooks.get_mut(index) else {
            return false;
        };
        if !unsafe { entry.set(ptr::null_mut(), "restore", 0, 0, false, "crit73") } {
            return false;
        }
    }
    log_fmt(format_args!(
        "[crit73] yielded get_Stable/get_Hit to the Faunas port"
    ));
    true
}

/// Reinstalls the yielded methods after the Faunas port lets go.
///
/// Called when the port's `stable`/`mob_no_flee` turns off while
/// always-critical is still on. Entries the port never took are left alone.
pub(crate) unsafe fn always_crit_reclaim_stable_hit(ch: &GameChain) -> bool {
    if !always_crit_active() {
        return true;
    }
    if ch.bstatus_class.is_null() {
        return false;
    }
    let specs = always_crit_conflict_specs();
    let signature = MethodSignature {
        parameters: &[],
        return_type: "System.Int32",
    };
    let Ok(mut hooks) = ALWAYS_CRIT_HOOKS.lock() else {
        return false;
    };
    for (index, spec) in [
        (ALWAYS_CRIT_PBS_STABLE, &specs[0]),
        (ALWAYS_CRIT_PBS_HIT, &specs[1]),
        (5, &specs[2]),
        (7, &specs[3]),
    ] {
        let Some(entry) = hooks.get_mut(index) else {
            return false;
        };
        if entry.active || crate::faunas::port_owns_hook(spec.target, spec.method) {
            continue;
        }
        let klass = if matches!(spec.target, HookTargetClass::PlayerSecondaryStatus) {
            ch.pss_class
        } else {
            ch.bstatus_class
        };
        if !unsafe {
            entry.set_with_signature(
                klass,
                spec.method,
                spec.argc,
                spec.detour,
                true,
                "crit73",
                Some(&signature),
                spec.orig_sink,
            )
        } {
            return false;
        }
    }
    log_fmt(format_args!(
        "[crit73] reclaimed get_Stable/get_Hit after the Faunas port"
    ));
    true
}

/// Whether an active companion feature already serves a hook the port wants.
///
/// Mirror of the port-side excuse: `GetMotionSpeed` is forced to 60 by
/// instant-attack too, so the port turning on second skips instead of
/// failing closed. Same value, same effect, no fight.
pub(crate) fn companion_serves_hook(target: HookTargetClass, method: &str) -> bool {
    match (target, method) {
        (
            HookTargetClass::PlayerBattleStatus | HookTargetClass::PlayerSecondaryStatus,
            "GetMotionSpeed",
        ) => INSTANT_ATK_ACTIVE.load(Ordering::Acquire),
        _ => false,
    }
}

/// Whether a refused core hook is a designed overlap, not a broken build.
///
/// Either side owning the method excuses the other into skip + partial;
/// anything else refused keeps failing closed.
fn overlap_excused(target: HookTargetClass, method: &str) -> bool {
    crate::faunas::port_owns_hook(target, method) || companion_serves_hook(target, method)
}

/// `cmd 73`: Toggle calculation-stage critical settings and normal-attack stability/hit.
pub unsafe fn job_always_crit() -> i32 {
    if crate::faunas::critical_active() && !ALWAYS_CRIT_ACTIVE.load(Ordering::Acquire) {
        log_fmt(format_args!(
            "[crit73] disable Faunas critical before selecting the companion critical path"
        ));
        return -8;
    }
    if ALWAYS_CRIT_ACTIVE.load(Ordering::Acquire) || cleanup_pending(TOGGLE_ALWAYS_CRIT) {
        critical_path::disable();
        let result = unsafe {
            disable_local_feature(
                &ALWAYS_CRIT_HOOKS,
                &ALWAYS_CRIT_ACTIVE,
                &LOCAL_CALLBACKS[11..],
                TOGGLE_ALWAYS_CRIT,
                "crit73",
            )
        };
        if result != 0 {
            // The feature is still partly live, so its share of the shared
            // critical-damage hooks has to stay with it.
            return result;
        }
        if !unsafe { crit_dmg_release(CRIT_DMG_OWNER_ALWAYS_CRIT) } {
            LOCAL_CLEANUP_PENDING.fetch_or(TOGGLE_ALWAYS_CRIT, Ordering::Release);
            note_partial(TOGGLE_ALWAYS_CRIT, true);
            return -8;
        }
        return 0;
    }
    validate_or_reset_sdd_cache();
    validate_or_reset_dd_cache();
    let mut gc = GcScope::new();
    let mut ch = GameChain::new();
    if !unsafe { ch.resolve(&mut gc) } {
        return -1;
    }

    let specs = [
        // PlayerBattleStatus (4)
        HookSpec {
            target: HookTargetClass::PlayerBattleStatus,
            method: "get_Critical",
            argc: 0,
            detour: local_stat::<11, 1000> as usize,
            orig_sink: Some(&LOCAL_CALLBACKS[11].original),
        },
        // The port's `stable` value wins while its feature is on; the
        // enable table, yield and reclaim share this spec. See
        // `always_crit_conflict_specs`.
        always_crit_conflict_specs()[0],
        HookSpec {
            target: HookTargetClass::PlayerBattleStatus,
            method: "get_SubStable",
            argc: 0,
            detour: local_stat::<13, 100> as usize,
            orig_sink: Some(&LOCAL_CALLBACKS[13].original),
        },
        // The port's `mob_no_flee` value wins while its feature is on; see
        // `always_crit_conflict_specs`.
        always_crit_conflict_specs()[1],
        // PlayerSecondaryStatus (4)
        HookSpec {
            target: HookTargetClass::PlayerSecondaryStatus,
            method: "get_Critical",
            argc: 0,
            detour: local_stat::<15, 1000> as usize,
            orig_sink: Some(&LOCAL_CALLBACKS[15].original),
        },
        always_crit_conflict_specs()[2],
        HookSpec {
            target: HookTargetClass::PlayerSecondaryStatus,
            method: "get_SubStable",
            argc: 0,
            detour: local_stat::<17, 100> as usize,
            orig_sink: Some(&LOCAL_CALLBACKS[17].original),
        },
        always_crit_conflict_specs()[3],
        // PlayerAttackBase - Skills (5)
        HookSpec {
            target: HookTargetClass::PlayerAttackBase,
            method: "checkCriticalPercent",
            argc: 2,
            detour: hk_true_2arg as usize,
            orig_sink: None,
        },
        HookSpec {
            target: HookTargetClass::PlayerAttackBase,
            method: "CheckMagicCritical",
            argc: 2,
            detour: hk_true_2arg as usize,
            orig_sink: None,
        },
        HookSpec {
            target: HookTargetClass::PlayerAttackBase,
            method: "CheckMagicCritical",
            argc: 3,
            detour: hk_true_3arg as usize,
            orig_sink: None,
        },
        HookSpec {
            target: HookTargetClass::NormalAttackAction,
            method: "CheckHit",
            argc: 5,
            detour: hk_check_hit as usize,
            orig_sink: None,
        },
        HookSpec {
            target: HookTargetClass::PlayerAttackBase,
            method: "calcBaseDamage",
            argc: 5,
            detour: hk_calc_base_damage as usize,
            orig_sink: Some(&ORIG_CALC_BASE_DAMAGE),
        },
        HookSpec {
            target: HookTargetClass::PlayerAttackBase,
            method: "calcPlayerToMobDamage",
            argc: 2,
            detour: hk_pab_calc_mob_dmg as usize,
            orig_sink: Some(&ORIG_PAB_CALC_MOB_DMG),
        },
        // Basic attacks use inherited CalcStable through their own vtable.
        HookSpec {
            target: HookTargetClass::NormalAttackAction,
            method: "calcPlayerToMobDamage",
            argc: 2,
            detour: hk_naa_calc_mob_dmg as usize,
            orig_sink: Some(&ORIG_NAA_CALC_MOB_DMG),
        },
        HookSpec {
            target: HookTargetClass::NormalAttackAction,
            method: "CalcStable",
            argc: 4,
            detour: hk_stability_factor as usize,
            orig_sink: None,
        },
        // The two `get_CriticalDmg` declarations live in CRIT_DMG_HOOKS, which
        // always-critical and the Faunas critical-damage ladder share.
        HookSpec {
            target: HookTargetClass::PlayerStatus,
            method: "get_SecondaryStatus",
            argc: 0,
            detour: critical_path::secondary_getter_shim as usize,
            orig_sink: Some(&critical_path::ORIG_SECONDARY_GETTER),
        },
    ];

    let enabling = !ALWAYS_CRIT_ACTIVE.load(Ordering::Relaxed);
    if enabling
        && !unsafe {
            bind_local_callbacks(
                11,
                &[
                    ch.bstatus, ch.bstatus, ch.bstatus, ch.bstatus, ch.pss, ch.pss, ch.pss, ch.pss,
                ],
            )
        }
    {
        return -8;
    }
    if enabling {
        if let Err(reason) = unsafe { prepare_normal_critical_path(&ch) } {
            log_fmt(format_args!(
                "[crit73] native critical path rejected: {}",
                reason
            ));
            return -8;
        }
    } else {
        critical_path::disable();
    }
    if let Ok(mut hooks) = ALWAYS_CRIT_HOOKS.lock() {
        unsafe {
            if !apply_hook_specs(&mut hooks[..specs.len()], &specs, &ch, enabling, "crit73") {
                critical_path::disable();
                return -8;
            }
            if enabling && !crit_dmg_acquire(CRIT_DMG_OWNER_ALWAYS_CRIT, &ch) {
                critical_path::disable();
                let restored = restore_hook_entries(&mut hooks[..specs.len()], "crit73");
                if !restored {
                    LOCAL_CLEANUP_PENDING.fetch_or(TOGGLE_ALWAYS_CRIT, Ordering::Release);
                    note_partial(TOGGLE_ALWAYS_CRIT, true);
                }
                log_fmt(format_args!(
                    "[crit73] shared critical damage hooks failed; rollback={}",
                    restored
                ));
                return -8;
            }
            if enabling && !critical_path::enable() {
                critical_path::disable();
                let restored = restore_hook_entries(&mut hooks[..specs.len()], "crit73");
                if !restored {
                    LOCAL_CLEANUP_PENDING.fetch_or(TOGGLE_ALWAYS_CRIT, Ordering::Release);
                    note_partial(TOGGLE_ALWAYS_CRIT, true);
                }
                log_fmt(format_args!(
                    "[crit73] critical path activation failed; rollback={}",
                    restored
                ));
                return -8;
            }
        }
    } else {
        return -8;
    }

    ALWAYS_CRIT_ACTIVE.store(enabling, Ordering::Relaxed);
    if let Some(shm) = telemetry() {
        shm.set_toggle_bit(TOGGLE_ALWAYS_CRIT, enabling);
    }
    log_fmt(format_args!(
        "[crit73] Critical settings + normal-attack stability/hit {} (scoped normal critical path; native resistance check retained)",
        if enabling { "ENABLED" } else { "DISABLED" }
    ));
    feature_setup_result(TOGGLE_ALWAYS_CRIT, enabling)
}

/// `cmd 74`: Toggle experimental AMPR getters and read-only hit sampling.
pub unsafe fn job_unlimited_mp() -> i32 {
    if UNLIMITED_MP_ACTIVE.load(Ordering::Acquire) || MP_CLEANUP_PENDING.load(Ordering::Acquire) {
        return unsafe { disable_unlimited_mp() };
    }
    let mut gc = GcScope::new();
    let mut ch = GameChain::new();
    if !unsafe { ch.resolve(&mut gc) } {
        return -1;
    }

    let specs = [
        HookSpec {
            target: HookTargetClass::NormalAttackAction,
            method: "ActionHit",
            argc: 2,
            detour: hk_naa_action_hit as usize,
            orig_sink: Some(&ORIG_NAA_ACTION_HIT),
        },
        HookSpec {
            target: HookTargetClass::PlayerBattleStatus,
            method: "get_AtkMpRecovery",
            argc: 0,
            detour: hk_pbs_mp_recovery as usize,
            orig_sink: Some(&ORIG_PBS_AMPR),
        },
        HookSpec {
            target: HookTargetClass::PlayerSecondaryStatus,
            method: "get_AtkMpRecovery",
            argc: 0,
            detour: hk_pss_mp_recovery as usize,
            orig_sink: Some(&ORIG_PSS_AMPR),
        },
    ];

    let enabling = true;
    if enabling {
        reset_mp_diagnostics();
        let binding = match unsafe { MpBinding::prepare(&ch) } {
            Ok(binding) => binding,
            Err(reason) => {
                log_fmt(format_args!("[mp74] resource binding rejected: {}", reason));
                return -8;
            }
        };
        // True native scale, read BEFORE hooks install so no selected bonus and
        // no nesting can tint it (live 2026-10-19: post-hook readout showed 33/23).
        let native_battle =
            unsafe { call_i32_0(ch.bstatus_class, ch.bstatus, "get_AtkMpRecovery") };
        let native_secondary = unsafe { call_i32_0(ch.pss_class, ch.pss, "get_AtkMpRecovery") };
        log_fmt(format_args!(
            "[mp74] native AMPR battle={:?} secondary={:?} selected-bonus={}",
            native_battle,
            native_secondary,
            MP_AMPR_BONUS.load(Ordering::Relaxed)
        ));
        let Ok(mut config) = MP_BINDING.write() else {
            return -8;
        };
        *config = Some(binding);
    }
    if let Ok(mut hooks) = UNLIMITED_MP_HOOKS.lock() {
        unsafe {
            if !apply_hook_specs(&mut hooks[..], &specs, &ch, enabling, "mp74") {
                return -8;
            }
        }
    } else {
        return -8;
    }
    // Best-effort TPS wrapper hook: proves whether combat queries the
    // TemporaryPlayerStatus calculator. Never fails the job.
    if let Ok(mut tps_hooks) = TPS_MP_HOOKS.lock() {
        if enabling {
            let tps_spec = HookSpec {
                target: HookTargetClass::TemporaryPlayerStatus,
                method: "get_AtkMpRecovery",
                argc: 0,
                detour: hk_tps_mp_recovery as usize,
                orig_sink: Some(&ORIG_TPS_AMPR),
            };
            unsafe {
                // Reuse the shared installer but ignore its core-fail result:
                // a missing TPS class must not roll back the PBS/PSS pair.
                let _ = apply_hook_specs(&mut tps_hooks[..], &[tps_spec], &ch, true, "mp74tps");
            }
            log_fmt(format_args!(
                "[mp74tps] TemporaryPlayerStatus get_AtkMpRecovery best-effort install attempted"
            ));
        } else {
            unsafe {
                let _ = restore_hook_entries(&mut tps_hooks[..], "mp74tps");
            }
        }
    }
    // Companion sustain: MP cost 10% of native (best-effort, tidak menggagalkan job).
    {
        let offset = (|| {
            let klass = unsafe { find_class(ch.image, ch.domain, "", "PlayerAttackBase") };
            if klass.is_null() {
                return 0usize;
            }
            let field = unsafe { get_field(klass, "<IsOtherPlayer>k__BackingField") };
            if !unsafe { field_type_matches(field, "System.Boolean") } {
                return 0usize;
            }
            let Some(api) = api() else {
                return 0usize;
            };
            let off = unsafe { (api.field_get_offset)(field) } as usize;
            if !(0x10..=0x4000).contains(&off) {
                return 0usize;
            }
            off
        })();
        SUSTAIN_COST_OFFSET.store(offset, Ordering::Relaxed);
        let cost_spec = HookSpec {
            target: HookTargetClass::PlayerAttackBase,
            method: "CalcCostMp",
            argc: 1,
            detour: hk_sustain_mp_cost as usize,
            orig_sink: Some(&ORIG_SUSTAIN_COST),
        };
        let cost0_spec = HookSpec {
            target: HookTargetClass::SkillActionBase,
            method: "CalcCostMp",
            argc: 0,
            detour: hk_sustain_mp_cost0 as usize,
            orig_sink: Some(&ORIG_SUSTAIN_COST0),
        };
        if let Ok(mut sustain_hooks) = SUSTAIN_COST_HOOKS.lock() {
            unsafe {
                let ok = apply_hook_specs(
                    &mut sustain_hooks[..],
                    &[cost_spec, cost0_spec],
                    &ch,
                    true,
                    "mp74cost",
                );
                log_fmt(format_args!(
                    "[mp74cost] CalcCostMp sustain {} (offset=0x{:X})",
                    if ok {
                        "ENABLED at 10% dual-path"
                    } else {
                        "SKIPPED target absen"
                    },
                    offset
                ));
            }
        }
    }

    UNLIMITED_MP_ACTIVE.store(enabling, Ordering::Relaxed);
    if let Some(shm) = telemetry() {
        shm.set_toggle_bit(TOGGLE_UNLIMITED_MP, enabling);
    }
    log_fmt(format_args!(
        "[mp74] Experimental MP hooks {} (activation is not evidence of added recovery)",
        if enabling { "ENABLED" } else { "DISABLED" }
    ));
    feature_setup_result(TOGGLE_UNLIMITED_MP, enabling)
}

/// `cmd 75..79, 81, 83..85`: absolute percent, native, or experimental additive points.
pub fn job_set_crit_multiplier(cmd: i32) -> i32 {
    let pct = match cmd {
        75 => 175,
        76 => 200,
        77 => 250,
        78 => 300,
        79 => 0,
        81 => CRIT_DAMAGE_ADD_1000,
        83 => CRIT_DAMAGE_ADD_100,
        84 => CRIT_DAMAGE_ADD_200,
        85 => CRIT_DAMAGE_ADD_300,
        _ => return -4,
    };
    CUSTOM_CRIT_MULTIPLIER.store(pct, Ordering::Relaxed);
    if let Some(shm) = telemetry() {
        shm.update(|data| {
            data.crit_multiplier_pct.store(pct, Ordering::SeqCst);
        });
    }
    log_fmt(format_args!(
        "[crit_mult] cmd={} -> requested={} (0=native, negative=native+N points; +1000 legacy unsafe; applies while crit mode is on)",
        cmd, pct
    ));
    0
}
pub(crate) fn critical_damage_request() -> i32 {
    CUSTOM_CRIT_MULTIPLIER.load(Ordering::Relaxed)
}

/// `cmd 86..88`: select the AMPR getter bonus in points (10/25/50).
/// 0 (initial) returns native recovery. Combat use and remote acceptance
/// require separate observation.
pub fn job_set_mp_bonus(cmd: i32) -> i32 {
    let bonus = match cmd {
        91 => 0,
        86 => MP_AMPR_BONUS_10,
        87 => MP_AMPR_BONUS_25,
        88 => MP_AMPR_BONUS_50,
        _ => return -4,
    };
    MP_AMPR_BONUS.store(bonus, Ordering::Relaxed);
    if let Some(shm) = telemetry() {
        shm.update(|data| {
            data.mp_ampr_bonus.store(bonus, Ordering::SeqCst);
        });
    }
    log_fmt(format_args!(
        "[mp_bonus] cmd={} -> bonus={} AMPR points over native (0=native; applies while MP engine is on)",
        cmd, bonus
    ));
    0
}

pub fn active_toggle_mask() -> u32 {
    [
        &INVINCIBILITY_ACTIVE,
        &SPEED_ACTIVE,
        &NO_COLLISION_ACTIVE,
        &RANGE_ACTIVE,
        &INSTANT_ATK_ACTIVE,
        &ALWAYS_CRIT_ACTIVE,
        &UNLIMITED_MP_ACTIVE,
        &CLEAR_DELAY_ACTIVE,
    ]
    .iter()
    .enumerate()
    .fold(0, |mask, (index, flag)| {
        mask | if flag.load(Ordering::Acquire) {
            1 << index
        } else {
            0
        }
    })
}

/// Dispatch an in-game command `cmd` (`supported IDs`) on the attached IL2CPP worker thread.
fn observation_command_blocked(cmd: i32, observing: bool, responses: bool) -> bool {
    if observing {
        // The phase marker is the one mutating-looking command that must stay
        // reachable while a capture is running: it is how a capture is split
        // into operator-named phases.
        !matches!(cmd, 1 | 80 | 93 | 116)
    } else {
        cmd == 80 && responses
    }
}

pub unsafe fn dispatch_game_command(cmd: i32) -> i32 {
    if !protocol::CommandId::is_valid_range(cmd) {
        return -4;
    }
    if protocol::CommandId::is_blocked(cmd) {
        return -7;
    }
    if protocol::CommandId::unavailable_reason(cmd).is_some() {
        return -10;
    }
    // Both observers use OnSkillActionHit. Never stack or independently restore
    // their callbacks; an action capture also freezes mutating commands.
    if observation_command_blocked(
        cmd,
        crate::combat_observe::installed(),
        crate::combat_response::installed(),
    ) {
        log_fmt(format_args!(
            "[observe80] command blocked: stop the action capture before changing settings; stop response feedback before starting an action capture"
        ));
        return -9;
    }
    if let Some((toggle, bit, desired)) = protocol::CommandId::decode_toggle_command(cmd) {
        // An incomplete OFF must be retried even though its callbacks are inactive.
        if cleanup_pending(bit) {
            return if desired {
                -8
            } else {
                unsafe { dispatch_game_command(toggle) }
            };
        }
        return if (active_toggle_mask() & bit != 0) == desired {
            feature_setup_result(bit, desired)
        } else {
            unsafe { dispatch_game_command(toggle) }
        };
    }
    match cmd {
        1 => unsafe { job_probe() },
        2 => unsafe { job_heal() },
        3 | 32 => unsafe { job_invincibility_toggle() },
        21 => unsafe { job_probe() },
        68 => unsafe { job_speed_boost() },
        69 => unsafe { job_no_collision() },
        70 => unsafe { job_clear_delay() },
        71 => unsafe { job_attack_range() },
        72 => unsafe { job_instant_attack() },
        73 => unsafe { job_always_crit() },
        74 => unsafe { job_unlimited_mp() },
        75..=79 | 81 | 83..=85 => job_set_crit_multiplier(cmd),
        86..=88 | 91 => job_set_mp_bonus(cmd),
        89 => unsafe { crate::combat_response::ensure_enabled(true) },
        90 => unsafe { crate::combat_response::ensure_enabled(false) },
        92 => {
            crate::combat_response::reset_metrics();
            0
        }
        93 => {
            if !crate::combat_observe::installed() || unsafe { crate::combat_observe::stop() } {
                0
            } else {
                -9
            }
        }
        80 => unsafe { crate::combat_observe::toggle() },
        82 => unsafe { crate::combat_response::toggle() },
        116 => crate::combat_observe::set_phase(),
        200..=247 | 300..=306 | 310..=320 | 330..=340 | 350..=356 => {
            log_fmt(format_args!(
                "[companion-final] faunas band disabled; cmd={} unavailable",
                cmd
            ));
            -10
        }
        94 => {
            let status = unsafe { crate::combat_response::ensure_enabled(true) };
            if status != 0 {
                return status;
            }
            set_adaptive_ladder(true);
            log_fmt(format_args!(
                "[adaptive_ladder] feedback-driven skill cap enabled"
            ));
            0
        }
        95 => {
            set_adaptive_ladder(false);
            log_fmt(format_args!(
                "[adaptive_ladder] disabled; skill cap reset to {}% of native",
                SKILL_CRIT_CAP_POINTS
            ));
            0
        }
        _ => {
            log_fmt(format_args!("[dispatch] unsupported cmd={}", cmd));
            -4
        }
    }
}

#[cfg(test)]
mod combat_tests {
    #[repr(C)]
    struct MovementFixture {
        class: usize,
        monitor: usize,
        speed: u32,
        flag: u8,
        untouched: [u8; 3],
    }

    fn movement_fixture(class: &[usize; 8], speed: f32, flag: u8) -> MovementFixture {
        MovementFixture {
            class: class.as_ptr() as usize,
            monitor: 0,
            speed: speed.to_bits(),
            flag,
            untouched: [0xa5; 3],
        }
    }

    fn movement_edit(object: &mut MovementFixture, is_float: bool) -> MovementField {
        MovementField {
            object: object as *mut _ as usize,
            class: object.class,
            offset: if is_float { 16 } else { 20 },
            original: if is_float {
                MovementValue::Float(object.speed)
            } else {
                MovementValue::Bool(object.flag)
            },
            replacement: if is_float {
                MovementValue::float(20.0)
            } else {
                MovementValue::Bool(0)
            },
            owned: false,
        }
    }

    #[test]
    fn shared_critical_damage_hooks_are_owned_by_both_features() {
        // The first owner installs the hooks, a later owner only claims them,
        // and the last one to let go restores them.
        let (owners, install) = crit_dmg_owner_step(0, CRIT_DMG_OWNER_ALWAYS_CRIT, true);
        assert_eq!(owners, CRIT_DMG_OWNER_ALWAYS_CRIT);
        assert!(install);

        let (owners, install) = crit_dmg_owner_step(owners, CRIT_DMG_OWNER_FAUNAS, true);
        assert_eq!(owners, CRIT_DMG_OWNER_ALWAYS_CRIT | CRIT_DMG_OWNER_FAUNAS);
        assert!(!install);

        // Releasing one owner leaves the hooks in place for the other.
        let (owners, restore) = crit_dmg_owner_step(owners, CRIT_DMG_OWNER_FAUNAS, false);
        assert_eq!(owners, CRIT_DMG_OWNER_ALWAYS_CRIT);
        assert!(!restore);

        // Releasing the last owner restores them.
        let (owners, restore) = crit_dmg_owner_step(owners, CRIT_DMG_OWNER_ALWAYS_CRIT, false);
        assert_eq!(owners, 0);
        assert!(restore);
    }

    #[test]
    fn shared_critical_damage_failed_restore_keeps_owner_until_retry() {
        let _guard = CALLBACK_TEST_LOCK.lock().unwrap();
        CRIT_DMG_OWNERS.store(CRIT_DMG_OWNER_FAUNAS, Ordering::Release);
        {
            let mut hooks = CRIT_DMG_HOOKS.lock().unwrap();
            hooks[0] = MethodHookEntry::new();
            hooks[0].active = true;
        }
        assert!(!unsafe { crit_dmg_release(CRIT_DMG_OWNER_FAUNAS) });
        assert_eq!(
            CRIT_DMG_OWNERS.load(Ordering::Acquire),
            CRIT_DMG_OWNER_FAUNAS
        );
        let mut pointers = [111usize, 222];
        {
            let mut hooks = CRIT_DMG_HOOKS.lock().unwrap();
            hooks[0].method = pointers.as_mut_ptr();
            hooks[0].orig_fn0 = 333;
            hooks[0].orig_fn1 = 444;
        }
        assert!(unsafe { crit_dmg_release(CRIT_DMG_OWNER_FAUNAS) });
        assert_eq!(pointers, [333, 444]);
        assert_eq!(CRIT_DMG_OWNERS.load(Ordering::Acquire), 0);
        CRIT_DMG_HOOKS.lock().unwrap()[0] = MethodHookEntry::new();
    }

    #[test]
    fn the_faunas_critical_damage_ladder_shares_the_always_critical_detours() {
        // The ladder replaces the same two declarations always-critical
        // replaces. A MethodInfo has one owner, so a second detour on either
        // of them would be refused and the feature would roll back.
        let specs = crate::faunas::critdmg_specs();
        assert_eq!(specs.len(), 2);
        assert_eq!(specs[0].method, "get_CriticalDmg");
        assert_eq!(specs[1].method, "get_CriticalDmg");
        assert_eq!(specs[0].detour, hk_pbs_crit_damage as usize);
        assert_eq!(specs[1].detour, hk_pss_crit_damage as usize);
    }

    #[test]
    fn the_faunas_port_wins_stable_and_hit_over_always_critical() {
        // The port's `stable` (1000) and `mob_no_flee` (65535) replace
        // always-critical's sidecar values (100 / 10000). These specs are the
        // single source of truth for the enable table, yield and reclaim, so
        // pin their identity: target, counts, detours and sinks.
        let specs = always_crit_conflict_specs();
        assert_eq!(specs.len(), 4);
        assert_eq!(specs[2].detour, local_stat::<16, 100> as usize);
        assert_eq!(specs[3].detour, local_stat::<18, 10000> as usize);
        assert_eq!(specs[0].method, "get_Stable");
        assert_eq!(specs[0].argc, 0);
        assert_eq!(specs[0].detour, local_stat::<12, 100> as usize);
        assert!(specs[0].orig_sink.is_some());
        assert_eq!(specs[1].method, "get_Hit");
        assert_eq!(specs[1].argc, 0);
        assert_eq!(specs[1].detour, local_stat::<14, 10000> as usize);
        assert!(specs[1].orig_sink.is_some());
        // And they sit at the table positions yield/reclaim address.
        assert_eq!(ALWAYS_CRIT_PBS_STABLE, 1);
        assert_eq!(ALWAYS_CRIT_PBS_HIT, 3);
    }

    #[test]
    fn designed_overlaps_are_excused_only_while_owned() {
        // In tests no feature is active, so every overlap stays unexcused:
        // refusals keep failing closed by default.
        use HookTargetClass::*;
        for (target, method) in [
            (PlayerBattleStatus, "get_Stable"),
            (PlayerBattleStatus, "get_Hit"),
            (PlayerBattleStatus, "GetMotionSpeed"),
            (PlayerSecondaryStatus, "GetMotionSpeed"),
        ] {
            assert!(
                !overlap_excused(target, method),
                "{method} excused with nothing active"
            );
        }
        assert!(!overlap_excused(PlayerBattleStatus, "get_Critical"));
    }

    #[test]
    fn constant_detours_report_without_changing_values() {
        // Reporting rides along in every shared constant detour; without a
        // live worker it is a no-op, so the constants must read back
        // exactly. Null pointers are safe: no detour dereferences them.
        unsafe {
            assert!(hk_true_0arg(core::ptr::null_mut(), core::ptr::null()));
            assert!(!hk_false_0arg(core::ptr::null_mut(), core::ptr::null()));
            assert_eq!(hk_zero_0arg(core::ptr::null_mut(), core::ptr::null()), 0);
            assert!(hk_true_2arg(
                core::ptr::null_mut(),
                core::ptr::null_mut(),
                core::ptr::null_mut(),
                core::ptr::null()
            ));
            assert!(hk_true_3arg(
                core::ptr::null_mut(),
                core::ptr::null_mut(),
                core::ptr::null_mut(),
                core::ptr::null_mut(),
                core::ptr::null()
            ));
        }
    }

    #[test]
    fn movement_restores_exact_nondefault_values_and_boolean_width() {
        let class = [0usize; 8];
        let mut object = movement_fixture(&class, 7.25, 1);
        let mut speed = movement_edit(&mut object, true);
        let mut flag = movement_edit(&mut object, false);
        unsafe {
            assert!(speed.apply());
            assert!(flag.apply());
            assert_eq!(f32::from_bits(object.speed), 20.0);
            assert_eq!(object.flag, 0);
            assert!(speed.restore());
            assert!(flag.restore());
            assert!(speed.restore());
        }
        assert_eq!(f32::from_bits(object.speed), 7.25);
        assert_eq!(object.flag, 1);
        assert_eq!(object.untouched, [0xa5; 3]);
    }

    #[test]
    fn movement_restore_preserves_native_updates_and_never_touches_replacement_actor() {
        let class = [0usize; 8];
        let mut original = movement_fixture(&class, 6.0, 1);
        let replacement = movement_fixture(&class, 3.0, 1);
        let mut edit = movement_edit(&mut original, true);
        unsafe {
            assert!(edit.apply());
            original.speed = f32::NAN.to_bits();
            assert!(edit.restore());
        }
        assert!(f32::from_bits(original.speed).is_nan());
        assert!(!edit.owned);
        assert_eq!(f32::from_bits(replacement.speed), 3.0);
    }

    #[test]
    fn movement_changed_class_keeps_snapshot_for_retry_without_writing() {
        let class = [0usize; 8];
        let other_class = [0usize; 8];
        let mut object = movement_fixture(&class, 8.0, 1);
        let mut edit = movement_edit(&mut object, true);
        unsafe {
            assert!(edit.apply());
            object.class = other_class.as_ptr() as usize;
            assert!(!edit.restore());
            assert!(edit.owned);
            assert_eq!(f32::from_bits(object.speed), 20.0);
            object.class = class.as_ptr() as usize;
            assert!(edit.restore());
        }
        assert_eq!(f32::from_bits(object.speed), 8.0);
    }

    #[test]
    fn movement_partial_apply_restores_only_successfully_owned_fields() {
        let class = [0usize; 8];
        let mut object = movement_fixture(&class, 20.0, 1);
        let mut state = MovementFields::new();
        state.fields.push(movement_edit(&mut object, true));
        state.fields.push(movement_edit(&mut object, false));
        object.flag = 0;
        unsafe {
            assert!(!state.apply());
            assert!(state.fields.iter().all(|field| !field.owned));
            object.speed = 9.0f32.to_bits();
            assert!(state.restore());
        }
        assert_eq!(f32::from_bits(object.speed), 9.0);
        assert_eq!(object.flag, 0);
    }

    #[test]
    fn movement_apply_detects_raced_snapshot_and_restore_retains_native_bits() {
        let class = [0usize; 8];
        let mut object = movement_fixture(&class, -0.0, 1);
        let mut state = MovementFields::new();
        state.fields.push(movement_edit(&mut object, true));
        state.fields.push(movement_edit(&mut object, false));
        object.flag = 0;
        unsafe {
            assert!(!state.apply());
            assert!(state.fields[0].owned);
            assert!(!state.fields[1].owned);
            assert!(state.restore());
        }
        assert_eq!(object.speed, (-0.0f32).to_bits());
        assert_eq!(object.flag, 0);
    }

    #[test]
    fn movement_callbacks_scope_speed_and_flags_and_forward_original_arguments() {
        let _guard = CALLBACK_TEST_LOCK.lock().unwrap();
        unsafe extern "C" fn native_speed(this: *mut c_void, method: *const c_void) -> f32 {
            assert_eq!(method as usize, 456);
            f32::from_bits(unsafe { (*(this as *const MovementFixture)).speed })
        }
        unsafe extern "C" fn native_bool(this: *mut c_void, method: *const c_void) -> bool {
            assert_eq!(method as usize, 456);
            unsafe { (*(this as *const MovementFixture)).flag != 0 }
        }
        let class = [0usize; 8];
        let mut local = movement_fixture(&class, 6.5, 0);
        let mut other = movement_fixture(&class, 8.75, 0);
        let local_ptr = (&mut local as *mut MovementFixture).cast();
        let other_ptr = (&mut other as *mut MovementFixture).cast();
        let method = 456usize as *const c_void;
        let prior_speed = SPEED_ACTIVE.swap(true, Ordering::AcqRel);
        let prior_no_collision = NO_COLLISION_ACTIVE.swap(true, Ordering::AcqRel);
        let speed_callbacks: [(
            usize,
            unsafe extern "C" fn(*mut c_void, *const c_void) -> f32,
        ); 5] = [
            (0, local_move_speed::<0>),
            (1, local_move_speed::<1>),
            (2, local_move_speed::<2>),
            (3, local_move_speed::<3>),
            (4, local_move_speed::<4>),
        ];
        let expected = SPEED_GETTER_VALUE;
        for (slot, call) in speed_callbacks {
            let callback = &SPEED_CALLBACKS[slot];
            let previous = callback
                .original
                .swap(native_speed as usize, Ordering::AcqRel);
            unsafe {
                assert!(callback.bind(local_ptr));
                assert_eq!(call(local_ptr, method), expected);
                assert_eq!(call(other_ptr, method), 8.75);
                SPEED_ACTIVE.store(false, Ordering::Release);
                assert_eq!(call(local_ptr, method), 6.5);
                SPEED_ACTIVE.store(true, Ordering::Release);
                callback.clear();
                assert_eq!(call(local_ptr, method), 6.5);
            }
            callback.original.store(previous, Ordering::Release);
        }
        let calls: [unsafe extern "C" fn(*mut c_void, *const c_void) -> bool; 6] = [
            local_no_collision::<0>,
            local_no_collision::<1>,
            local_no_collision::<2>,
            local_no_collision::<3>,
            local_no_collision::<4>,
            local_no_collision::<5>,
        ];
        for (slot, call) in calls.into_iter().enumerate() {
            let native_value = !NO_COLLISION_VALUES[slot];
            local.flag = native_value as u8;
            other.flag = native_value as u8;
            let callback = &NO_COLLISION_CALLBACKS[slot];
            let previous = callback
                .original
                .swap(native_bool as usize, Ordering::AcqRel);
            unsafe {
                assert!(callback.bind(local_ptr));
                assert_eq!(call(local_ptr, method), NO_COLLISION_VALUES[slot]);
                assert_eq!(call(other_ptr, method), native_value);
                NO_COLLISION_ACTIVE.store(false, Ordering::Release);
                assert_eq!(call(local_ptr, method), native_value);
                NO_COLLISION_ACTIVE.store(true, Ordering::Release);
                callback.clear();
                assert_eq!(call(local_ptr, method), native_value);
            }
            assert_eq!(local.flag, native_value as u8);
            assert_eq!(other.flag, native_value as u8);
            callback.original.store(previous, Ordering::Release);
        }
        SPEED_ACTIVE.store(prior_speed, Ordering::Release);
        NO_COLLISION_ACTIVE.store(prior_no_collision, Ordering::Release);
    }

    #[test]
    fn github_speed_and_dash_override_only_the_bound_actor() {
        let _guard = CALLBACK_TEST_LOCK.lock().unwrap();
        static CALLS: AtomicUsize = AtomicUsize::new(0);
        unsafe extern "C" fn native_speed(this: *mut c_void, method: *const c_void) -> f32 {
            assert_eq!(method as usize, 456);
            CALLS.fetch_add(1, Ordering::Relaxed);
            f32::from_bits(unsafe { (*(this as *const MovementFixture)).speed })
        }
        unsafe extern "C" fn native_dash(this: *mut c_void, method: *const c_void) -> bool {
            assert_eq!(method as usize, 456);
            unsafe { (*(this as *const MovementFixture)).flag != 0 }
        }
        let class = [0usize; 8];
        let mut local = movement_fixture(&class, SPEED_FIELD_VALUE, 0);
        let mut other = movement_fixture(&class, 4.0, 0);
        let local_ptr = (&mut local as *mut MovementFixture).cast();
        let other_ptr = (&mut other as *mut MovementFixture).cast();
        let method = 456usize as *const c_void;
        let prior = SPEED_ACTIVE.swap(true, Ordering::AcqRel);
        let original = SPEED_CALLBACKS[0]
            .original
            .swap(native_speed as usize, Ordering::AcqRel);
        let dash_original = SPEED_CALLBACKS[5]
            .original
            .swap(native_dash as usize, Ordering::AcqRel);
        CALLS.store(0, Ordering::Relaxed);
        unsafe {
            assert!(SPEED_CALLBACKS[0].bind(local_ptr));
            assert!(SPEED_CALLBACKS[5].bind(local_ptr));
            assert_eq!(local_move_speed::<0>(local_ptr, method), 15.0);
            assert_eq!(
                CALLS.load(Ordering::Relaxed),
                0,
                "active local speed must replace the native body"
            );
            assert!(local_speed_dash(local_ptr, method));
            assert_eq!(local_move_speed::<0>(other_ptr, method), 4.0);
            assert!(!local_speed_dash(other_ptr, method));
            SPEED_ACTIVE.store(false, Ordering::Release);
            assert_eq!(local_move_speed::<0>(local_ptr, method), SPEED_FIELD_VALUE);
            assert!(!local_speed_dash(local_ptr, method));
            assert_eq!(CALLS.load(Ordering::Relaxed), 2);
        }
        SPEED_CALLBACKS[0].clear();
        SPEED_CALLBACKS[5].clear();
        SPEED_CALLBACKS[0]
            .original
            .store(original, Ordering::Release);
        SPEED_CALLBACKS[5]
            .original
            .store(dash_original, Ordering::Release);
        SPEED_ACTIVE.store(prior, Ordering::Release);
    }

    #[test]
    fn failed_movement_activation_rolls_back_fields_and_retries_owned_hooks() {
        let _guard = CALLBACK_TEST_LOCK.lock().unwrap();
        let class = [0usize; 8];
        let mut object = movement_fixture(&class, 7.25, 1);
        let fields = Mutex::new(None);
        let hooks = Mutex::new([cleanup_hook_fixture(ptr::null_mut())]);
        let callbacks = [ScopedCallback::new()];
        let active = AtomicBool::new(false);
        let feature = MovementFeature {
            fields: &fields,
            hooks: &hooks,
            callbacks: &callbacks,
            active: &active,
            bit: TOGGLE_SPEED_BOOST,
            tag: "test",
        };
        let mut snapshot = MovementFields::new();
        snapshot.fields.push(movement_edit(&mut object, true));
        snapshot.fields.push(movement_edit(&mut object, false));
        object.flag = 0; // The native value raced after capture: the second edit must fail.
        unsafe {
            assert!(callbacks[0].bind((&mut object as *mut MovementFixture).cast()));
            assert_eq!(
                feature.finish_activation(
                    snapshot,
                    &mut fields.lock().unwrap(),
                    &mut hooks.lock().unwrap()[..],
                    true
                ),
                -8
            );
        }
        assert!(!active.load(Ordering::Acquire));
        assert_eq!(object.speed, 7.25f32.to_bits());
        assert_eq!(object.flag, 0);
        assert!(cleanup_pending(TOGGLE_SPEED_BOOST));
        assert!(fields.lock().unwrap().is_some());
        assert!(!callbacks[0].matches((&mut object as *mut MovementFixture).cast(), true));
        let on = protocol::CommandId::set_toggle_command(TOGGLE_SPEED_BOOST, true).unwrap();
        assert_eq!(unsafe { dispatch_game_command(on) }, -8);
        let mut method = [0x9999usize, 0x9999];
        hooks.lock().unwrap()[0].method = method.as_mut_ptr();
        assert_eq!(
            unsafe {
                disable_movement(
                    &fields,
                    &hooks,
                    &callbacks,
                    &active,
                    TOGGLE_SPEED_BOOST,
                    "test",
                )
            },
            0
        );
        assert_eq!(method, [0x1111, 0x2222]);
        assert!(fields.lock().unwrap().is_none());
        assert!(!cleanup_pending(TOGGLE_SPEED_BOOST));
    }

    #[test]
    fn the_range_feature_never_installs_a_hook_that_widens_a_range_check() {
        // The server validates the distance of a damage claim. An
        // out-of-range basic attack is dropped and an out-of-range skill is
        // sanctioned, so the feature must not widen any range, hit size or
        // action range. Only the assist-move getters may be hooked.
        let forbidden = [
            "CheckActionRange",
            "OnActionOutOfRange",
            "CheckRangeHit",
            "ActionPreparation",
            "get_ActionRange",
            "CheckHit",
        ];
        for spec in range_hook_specs() {
            assert!(
                !forbidden.contains(&spec.method),
                "the range feature must not hook {}",
                spec.method
            );
        }
        let methods: Vec<&str> = range_hook_specs().iter().map(|spec| spec.method).collect();
        assert_eq!(
            methods,
            vec![
                "get_IsShukuchiAssistMove",
                "get_IsActiveAssistMove",
                "get_IsMoveAssistContinue"
            ]
        );
    }

    #[test]
    fn movement_disable_clears_callbacks_and_releases_completed_snapshot() {
        let class = [0usize; 8];
        let mut object = movement_fixture(&class, 3.5, 1);
        let mut snapshot = MovementFields::new();
        snapshot.fields.push(movement_edit(&mut object, true));
        let callbacks: [ScopedCallback; 6] = [const { ScopedCallback::new() }; 6];
        unsafe {
            assert!(snapshot.apply());
            assert!(callbacks[0].bind((&mut object as *mut MovementFixture).cast()));
        }
        let state = Mutex::new(Some(snapshot));
        let hooks = Mutex::new([MethodHookEntry::new(); 6]);
        unsafe { assert!(restore_movement(&state, &hooks, &callbacks, "test")) };
        assert!(state.lock().unwrap().is_none());
        assert!(!callbacks[0].matches((&mut object as *mut MovementFixture).cast(), true));
        assert_eq!(object.speed, 3.5f32.to_bits());
    }

    #[test]
    fn scoped_invincibility_and_timing_callbacks_preserve_foreign_actor_arguments() {
        unsafe extern "C" fn native_bool(this: *mut c_void, method: *const c_void) -> bool {
            assert!(!this.is_null());
            assert_eq!(method as usize, 321);
            false
        }
        unsafe extern "C" fn native_next(
            this: *mut c_void,
            aspd: i32,
            method: *const c_void,
        ) -> f32 {
            assert!(!this.is_null());
            assert_eq!(aspd, 707);
            assert_eq!(method as usize, 321);
            1.75
        }
        unsafe extern "C" fn native_damage(
            this: *mut c_void,
            amount: i32,
            flag: bool,
            method: *const c_void,
        ) {
            assert!(!this.is_null());
            assert_eq!(amount, 47);
            assert!(flag);
            assert_eq!(method as usize, 321);
            unsafe {
                *((this as *mut usize).add(1)) += 1;
            }
        }
        let _guard = CALLBACK_TEST_LOCK.lock().unwrap();
        let mut local = [0x1111usize, 0];
        let mut other = [0x1111usize, 0];
        let local_ptr = local.as_mut_ptr().cast();
        let other_ptr = other.as_mut_ptr().cast();
        let method = 321usize as *const c_void;
        for slot in [0, 2, 6] {
            unsafe {
                assert!(LOCAL_CALLBACKS[slot].bind(local_ptr));
            }
        }
        LOCAL_CALLBACKS[0]
            .original
            .store(native_bool as usize, Ordering::Release);
        LOCAL_CALLBACKS[2]
            .original
            .store(native_damage as usize, Ordering::Release);
        LOCAL_CALLBACKS[6]
            .original
            .store(native_next as usize, Ordering::Release);
        INVINCIBILITY_ACTIVE.store(true, Ordering::Release);
        INSTANT_ATK_ACTIVE.store(true, Ordering::Release);
        unsafe {
            assert!(local_invincibility_bool::<0>(local_ptr, method));
            assert!(!local_invincibility_bool::<0>(other_ptr, method));
            local_invincibility_damage(local_ptr, 47, true, method);
            local_invincibility_damage(other_ptr, 47, true, method);
            assert_eq!(local[1], 0);
            assert_eq!(other[1], 1);
            assert_eq!(local_next_attack::<6>(local_ptr, 707, method), 0.0);
            assert_eq!(local_next_attack::<6>(other_ptr, 707, method), 1.75);
            INVINCIBILITY_ACTIVE.store(false, Ordering::Release);
            INSTANT_ATK_ACTIVE.store(false, Ordering::Release);
            assert!(!local_invincibility_bool::<0>(local_ptr, method));
            assert_eq!(local_next_attack::<6>(local_ptr, 707, method), 1.75);
        }
        for slot in [0, 2, 6] {
            LOCAL_CALLBACKS[slot].clear();
            LOCAL_CALLBACKS[slot].original.store(0, Ordering::Release);
        }
    }
    use super::*;

    static CALLBACK_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn cleanup_hook_fixture(method: *mut usize) -> MethodHookEntry {
        let mut hook = MethodHookEntry::new();
        hook.method = method;
        hook.orig_fn0 = 0x1111;
        hook.orig_fn1 = 0x2222;
        hook.active = true;
        hook
    }

    #[test]
    fn range_and_mp_off_do_not_require_a_resolved_player() {
        let _guard = CALLBACK_TEST_LOCK.lock().unwrap();
        for (active, bit) in [
            (&RANGE_ACTIVE, TOGGLE_RANGE_30M),
            (&UNLIMITED_MP_ACTIVE, TOGGLE_UNLIMITED_MP),
        ] {
            active.store(true, Ordering::Release);
            let off = protocol::CommandId::set_toggle_command(bit, false).unwrap();
            assert_eq!(unsafe { dispatch_game_command(off) }, 0);
            assert_eq!(unsafe { dispatch_game_command(off) }, 0);
            assert!(!active.load(Ordering::Acquire));
            assert!(!cleanup_pending(bit));
            assert_eq!(PARTIAL_FEATURES.load(Ordering::Acquire) & bit, 0);
        }
    }

    #[test]
    fn range_and_mp_failed_activation_keep_rollback_retryable() {
        let _guard = CALLBACK_TEST_LOCK.lock().unwrap();
        let specs = [HookSpec {
            target: HookTargetClass::PlayerBattleStatus,
            method: "get_AtkMpRecovery",
            argc: 0,
            detour: hk_pbs_mp_recovery as usize,
            orig_sink: None,
        }];
        let mut outcomes = Vec::new();
        for (bit, tag) in [(TOGGLE_RANGE_30M, "range71"), (TOGGLE_UNLIMITED_MP, "mp74")] {
            let activation = {
                let mut hooks = if bit == TOGGLE_RANGE_30M {
                    ATTACK_RANGE_HOOKS.lock().unwrap()
                } else {
                    UNLIMITED_MP_HOOKS.lock().unwrap()
                };
                hooks[0] = cleanup_hook_fixture(ptr::null_mut());
                unsafe { apply_hook_specs(&mut hooks[..], &specs, &GameChain::new(), true, tag) }
            };
            let pending = cleanup_pending(bit);
            let partial = PARTIAL_FEATURES.load(Ordering::Acquire) & bit != 0;
            let on = protocol::CommandId::set_toggle_command(bit, true).unwrap();
            let off = protocol::CommandId::set_toggle_command(bit, false).unwrap();
            let on_result = unsafe { dispatch_game_command(on) };
            let failed_off = unsafe { dispatch_game_command(off) };
            let mut method = [0x3333usize, 0x4444];
            {
                let mut hooks = if bit == TOGGLE_RANGE_30M {
                    ATTACK_RANGE_HOOKS.lock().unwrap()
                } else {
                    UNLIMITED_MP_HOOKS.lock().unwrap()
                };
                hooks[0].method = method.as_mut_ptr();
            }
            let retried_off = unsafe { dispatch_game_command(off) };
            let restored = method == [0x1111, 0x2222] && !cleanup_pending(bit);
            // Release fixture pointers even when running against the pre-fix path.
            let cleanup = unsafe {
                if bit == TOGGLE_RANGE_30M {
                    disable_attack_range()
                } else {
                    disable_unlimited_mp()
                }
            };
            assert_eq!(cleanup, 0);
            outcomes.push((
                activation,
                pending,
                partial,
                on_result,
                failed_off,
                retried_off,
                restored,
            ));
        }
        for outcome in outcomes {
            assert_eq!(outcome, (false, true, true, -8, -8, 0, true));
        }
    }

    #[test]
    fn local_feature_off_does_not_require_a_resolved_player() {
        let _guard = CALLBACK_TEST_LOCK.lock().unwrap();
        for (active, bit) in [
            (&INVINCIBILITY_ACTIVE, TOGGLE_INVINCIBILITY),
            (&INSTANT_ATK_ACTIVE, TOGGLE_INSTANT_ATK),
            (&ALWAYS_CRIT_ACTIVE, TOGGLE_ALWAYS_CRIT),
            (&CLEAR_DELAY_ACTIVE, TOGGLE_ANTI_RIGIDITY),
        ] {
            active.store(true, Ordering::Release);
            let off = protocol::CommandId::set_toggle_command(bit, false).unwrap();
            let status = unsafe { dispatch_game_command(off) };
            let still_active = active.swap(false, Ordering::AcqRel);
            assert_eq!(status, 0, "OFF failed for feature {bit}");
            assert!(!still_active);
            assert_eq!(unsafe { dispatch_game_command(off) }, 0);
            assert!(!cleanup_pending(bit));
        }
    }

    #[test]
    fn local_failed_off_deactivates_callbacks_and_retries_without_player_lookup() {
        let _guard = CALLBACK_TEST_LOCK.lock().unwrap();
        INVINCIBILITY_HOOKS.lock().unwrap()[0] = cleanup_hook_fixture(ptr::null_mut());
        INVINCIBILITY_ACTIVE.store(true, Ordering::Release);
        let off = protocol::CommandId::set_toggle_command(TOGGLE_INVINCIBILITY, false).unwrap();
        let on = protocol::CommandId::set_toggle_command(TOGGLE_INVINCIBILITY, true).unwrap();
        assert_eq!(unsafe { dispatch_game_command(off) }, -8);
        assert!(!INVINCIBILITY_ACTIVE.load(Ordering::Acquire));
        assert!(cleanup_pending(TOGGLE_INVINCIBILITY));
        assert_eq!(unsafe { dispatch_game_command(on) }, -8);
        let mut method = [0x9999usize; 2];
        INVINCIBILITY_HOOKS.lock().unwrap()[0].method = method.as_mut_ptr();
        assert_eq!(unsafe { dispatch_game_command(off) }, 0);
        assert_eq!(method, [0x1111, 0x2222]);
        assert!(!cleanup_pending(TOGGLE_INVINCIBILITY));
        assert_eq!(
            PARTIAL_FEATURES.load(Ordering::Acquire) & TOGGLE_INVINCIBILITY,
            0
        );
        INVINCIBILITY_HOOKS.lock().unwrap()[0] = MethodHookEntry::new();
    }

    #[test]
    fn range_failed_off_stays_inactive_and_retries_the_saved_hook() {
        let _guard = CALLBACK_TEST_LOCK.lock().unwrap();
        ATTACK_RANGE_HOOKS.lock().unwrap()[0] = cleanup_hook_fixture(ptr::null_mut());
        RANGE_ACTIVE.store(true, Ordering::Release);
        let off = protocol::CommandId::set_toggle_command(TOGGLE_RANGE_30M, false).unwrap();
        let on = protocol::CommandId::set_toggle_command(TOGGLE_RANGE_30M, true).unwrap();
        assert_eq!(unsafe { dispatch_game_command(off) }, -8);
        assert!(!RANGE_ACTIVE.load(Ordering::Acquire));
        assert!(cleanup_pending(TOGGLE_RANGE_30M));
        assert_ne!(
            PARTIAL_FEATURES.load(Ordering::Acquire) & TOGGLE_RANGE_30M,
            0
        );
        assert!(ATTACK_RANGE_HOOKS.lock().unwrap()[0].active);
        assert_eq!(unsafe { dispatch_game_command(on) }, -8);
        assert_eq!(unsafe { dispatch_game_command(71) }, -8);
        let mut method = [0x3333usize, 0x4444];
        ATTACK_RANGE_HOOKS.lock().unwrap()[0].method = method.as_mut_ptr();
        assert_eq!(unsafe { dispatch_game_command(off) }, 0);
        assert_eq!(method, [0x1111, 0x2222]);
        assert!(!ATTACK_RANGE_HOOKS.lock().unwrap()[0].active);
        assert!(!cleanup_pending(TOGGLE_RANGE_30M));
        assert_eq!(
            PARTIAL_FEATURES.load(Ordering::Acquire) & TOGGLE_RANGE_30M,
            0
        );
        ATTACK_RANGE_HOOKS.lock().unwrap()[0] = MethodHookEntry::new();
    }

    #[test]
    fn mp_off_restores_observers_and_retries_incomplete_cleanup() {
        let _guard = CALLBACK_TEST_LOCK.lock().unwrap();
        let mut main_method = [0x3333usize, 0x4444];
        UNLIMITED_MP_HOOKS.lock().unwrap()[0] = cleanup_hook_fixture(main_method.as_mut_ptr());
        MP_AMPR_OBSERVE_HOOKS.lock().unwrap()[0] = cleanup_hook_fixture(ptr::null_mut());
        UNLIMITED_MP_ACTIVE.store(true, Ordering::Release);
        let off = protocol::CommandId::set_toggle_command(TOGGLE_UNLIMITED_MP, false).unwrap();
        let on = protocol::CommandId::set_toggle_command(TOGGLE_UNLIMITED_MP, true).unwrap();
        assert_eq!(unsafe { dispatch_game_command(off) }, -8);
        assert_eq!(main_method, [0x1111, 0x2222]);
        assert!(!UNLIMITED_MP_HOOKS.lock().unwrap()[0].active);
        assert!(MP_AMPR_OBSERVE_HOOKS.lock().unwrap()[0].active);
        assert!(!UNLIMITED_MP_ACTIVE.load(Ordering::Acquire));
        assert!(cleanup_pending(TOGGLE_UNLIMITED_MP));
        assert_ne!(
            PARTIAL_FEATURES.load(Ordering::Acquire) & TOGGLE_UNLIMITED_MP,
            0
        );
        assert_eq!(unsafe { dispatch_game_command(on) }, -8);
        assert_eq!(unsafe { dispatch_game_command(off) }, -8);
        let mut observer_method = [0x5555usize, 0x6666];
        MP_AMPR_OBSERVE_HOOKS.lock().unwrap()[0].method = observer_method.as_mut_ptr();
        assert_eq!(unsafe { dispatch_game_command(off) }, 0);
        assert_eq!(observer_method, [0x1111, 0x2222]);
        assert!(!MP_AMPR_OBSERVE_HOOKS.lock().unwrap()[0].active);
        assert!(!cleanup_pending(TOGGLE_UNLIMITED_MP));
        assert_eq!(
            PARTIAL_FEATURES.load(Ordering::Acquire) & TOGGLE_UNLIMITED_MP,
            0
        );
        UNLIMITED_MP_HOOKS.lock().unwrap()[0] = MethodHookEntry::new();
        MP_AMPR_OBSERVE_HOOKS.lock().unwrap()[0] = MethodHookEntry::new();
    }

    #[test]
    fn combined_observation_allows_fixed_modes_but_freezes_mutating_commands() {
        assert!(!observation_command_blocked(80, false, false));
        assert!(observation_command_blocked(80, false, true));
        for cmd in [1, 80, 93] {
            assert!(!observation_command_blocked(cmd, true, false));
            assert!(!observation_command_blocked(cmd, true, true));
        }
        for cmd in [2, 32, 71, 73, 75, 82, 86, 89, 90, 92, 94, 95, 101] {
            assert!(observation_command_blocked(cmd, true, false));
        }
    }

    unsafe extern "C" fn pending_range_original(this: *mut c_void, method: *const c_void) -> bool {
        let output = unsafe { &mut *(this as *mut [usize; 2]) };
        output[0] = method as usize;
        output[1] != 0
    }

    #[test]
    fn pending_range_preserves_native_arguments_and_result_without_a_binding() {
        let _guard = CALLBACK_TEST_LOCK.lock().unwrap();
        let previous = ORIG_PENDING_RANGE.swap(pending_range_original as usize, Ordering::AcqRel);
        let enabled = RANGE_ACTIVE.swap(false, Ordering::AcqRel);
        let mut output = [0usize; 2];
        for result in [0, 1] {
            output[1] = result;
            let actual = unsafe {
                hk_pending_action_range(output.as_mut_ptr().cast(), 0x1234usize as *const c_void)
            };
            assert_eq!(actual, result != 0);
            assert_eq!(output[0], 0x1234);
        }
        RANGE_ACTIVE.store(enabled, Ordering::Release);
        ORIG_PENDING_RANGE.store(previous, Ordering::Release);
    }

    unsafe extern "C" fn original_base(
        this: *mut c_void,
        player: *mut c_void,
        mob: *mut c_void,
        attack: i32,
        equipment: i32,
        critical: bool,
        method: *const c_void,
    ) -> i32 {
        let output = unsafe { &mut *(player as *mut [usize; 6]) };
        *output = [
            this as usize,
            mob as usize,
            attack as usize,
            equipment as u32 as usize,
            critical as usize,
            method as usize,
        ];
        if critical {
            660
        } else {
            80
        }
    }

    unsafe extern "C" fn original_normal(
        this: *mut c_void,
        player: *mut c_void,
        _mob: *mut c_void,
        _method: *const c_void,
    ) {
        let calls = unsafe { &mut *(player as *mut usize) };
        *calls += 1;
        // Model native hit fields and damage as a coherent record.
        let data = unsafe { &mut *(this as *mut [usize; 32]) };
        data[4] = 1;
        data[5] = 2;
        data[6] = 80;
        data[7] = 80;
    }

    unsafe extern "C" fn original_critical(_this: *mut c_void, _method: *const c_void) -> i32 {
        212
    }

    #[test]
    fn base_damage_preserves_native_critical_choice_and_full_abi() {
        let _guard = CALLBACK_TEST_LOCK.lock().unwrap();
        let class = [0xABCDusize; 16];
        let mut action = [0usize; 32];
        action[0] = class.as_ptr() as usize;
        let mut args = [0usize; 6];
        ORIG_CALC_BASE_DAMAGE.store(original_base as usize, Ordering::Release);
        for critical in [false, true] {
            let damage = unsafe {
                hk_calc_base_damage(
                    action.as_mut_ptr().cast(),
                    args.as_mut_ptr().cast(),
                    0x12345678usize as *mut c_void,
                    3,
                    -17,
                    critical,
                    0xABCDEFusize as *const c_void,
                )
            };
            assert_eq!(damage, if critical { 660 } else { 80 });
            assert_eq!(
                args,
                [
                    action.as_ptr() as usize,
                    0x12345678,
                    3,
                    (-17i32) as u32 as usize,
                    critical as usize,
                    0xABCDEF
                ]
            );
        }
        ORIG_CALC_BASE_DAMAGE.store(0, Ordering::Release);
    }

    #[test]
    fn normal_calculation_does_not_relabel_or_rescale_completed_damage() {
        let _guard = CALLBACK_TEST_LOCK.lock().unwrap();
        let class = [0xABCDusize; 16];
        let mut action = [0usize; 32];
        action[0] = class.as_ptr() as usize;
        let mut calls = 0usize;
        ORIG_NAA_CALC_MOB_DMG.store(original_normal as usize, Ordering::Release);
        for _ in 0..3 {
            unsafe {
                hk_naa_calc_mob_dmg(
                    action.as_mut_ptr().cast(),
                    (&mut calls as *mut usize).cast(),
                    ptr::null_mut(),
                    ptr::null(),
                )
            };
            assert_eq!(&action[4..8], &[1, 2, 80, 80]);
        }
        assert_eq!(calls, 3);
        ORIG_NAA_CALC_MOB_DMG.store(0, Ordering::Release);
    }

    #[test]
    fn mp_hit_forwards_native_damage_with_crit_off_and_respects_its_own_toggle() {
        let _guard = CALLBACK_TEST_LOCK.lock().unwrap();
        let mut action = [0usize; 32];
        let mut calls = 0usize;
        let hits = MP_HIT_CALLS.load(Ordering::Relaxed);
        let samples = MP_SAMPLES.load(Ordering::Relaxed);
        ORIG_NAA_ACTION_HIT.store(original_normal as *const () as usize, Ordering::Release);
        *MP_BINDING.write().unwrap() = None;
        ALWAYS_CRIT_ACTIVE.store(false, Ordering::Release);
        for enabled in [false, true, false] {
            UNLIMITED_MP_ACTIVE.store(enabled, Ordering::Release);
            unsafe {
                hk_naa_action_hit(
                    action.as_mut_ptr().cast(),
                    (&mut calls as *mut usize).cast(),
                    ptr::null_mut(),
                    ptr::null(),
                )
            };
            assert_eq!(&action[4..8], &[1, 2, 80, 80]);
        }
        assert_eq!(calls, 3);
        assert_eq!(MP_HIT_CALLS.load(Ordering::Relaxed), hits + 1);
        assert_eq!(MP_SAMPLES.load(Ordering::Relaxed), samples);
        ORIG_NAA_ACTION_HIT.store(0, Ordering::Release);
    }

    #[test]
    fn mp_bonus_selects_small_calibrated_steps_and_stays_native_without_binding() {
        unsafe extern "C" fn native_ampr(_this: *mut c_void, _method: *const c_void) -> i32 {
            12
        }

        let _guard = CALLBACK_TEST_LOCK.lock().unwrap();
        ORIG_PBS_AMPR.store(native_ampr as usize, Ordering::Release);
        let getters = MP_GETTER_CALLS.load(Ordering::Relaxed);
        *MP_BINDING.write().unwrap() = None;
        for (cmd, bonus) in [(86, 10), (87, 25), (88, 50)] {
            assert_eq!(job_set_mp_bonus(cmd), 0);
            assert_eq!(mp_ampr_bonus_request(), bonus);
            UNLIMITED_MP_ACTIVE.store(true, Ordering::Release);
            // No binding: the owned-calculator guard fails closed, native passes through.
            assert_eq!(
                unsafe { hk_pbs_mp_recovery(ptr::null_mut(), ptr::null()) },
                12
            );
        }
        assert_eq!(job_set_mp_bonus(89), -4);
        assert_eq!(job_set_mp_bonus(74), -4);
        assert_eq!(unsafe { dispatch_game_command(87) }, 0);
        assert_eq!(mp_ampr_bonus_request(), MP_AMPR_BONUS_25);
        UNLIMITED_MP_ACTIVE.store(false, Ordering::Release);
        assert_eq!(
            unsafe { hk_pbs_mp_recovery(ptr::null_mut(), ptr::null()) },
            12
        );
        assert_eq!(MP_GETTER_CALLS.load(Ordering::Relaxed), getters + 4);
        ORIG_PBS_AMPR.store(0, Ordering::Release);
    }
    #[test]
    fn mp_bonus_mapping_applies_once_and_nested_reads_stay_native() {
        // The compounding site, pinned: 13 -> 23 exactly once, never 13 -> 23 -> 33.
        assert_eq!(mp_apply_bonus(13, false, true, 10), 23);
        assert_eq!(mp_apply_bonus(13, false, true, 25), 38);
        assert_eq!(mp_apply_bonus(13, false, true, 50), 63);
        assert_eq!(mp_apply_bonus(23, true, true, 10), 23);
        assert_eq!(mp_apply_bonus(13, false, false, 50), 13);
        assert_eq!(mp_apply_bonus(13, true, false, 50), 13);
        assert_eq!(mp_apply_bonus(i32::MAX, true, true, 50), i32::MAX);
    }

    #[test]
    fn tps_mp_recovery_forwards_native_and_counts_without_binding() {
        unsafe extern "C" fn native_tps(_this: *mut c_void, _method: *const c_void) -> i32 {
            17
        }

        let _guard = CALLBACK_TEST_LOCK.lock().unwrap();
        ORIG_TPS_AMPR.store(native_tps as usize, Ordering::Release);
        let getters = MP_GETTER_CALLS.load(Ordering::Relaxed);
        *MP_BINDING.write().unwrap() = None;
        UNLIMITED_MP_ACTIVE.store(true, Ordering::Release);
        // No binding: owned guard fails closed, native passes through.
        assert_eq!(
            unsafe { hk_tps_mp_recovery(ptr::null_mut(), ptr::null()) },
            17
        );
        assert_eq!(MP_GETTER_CALLS.load(Ordering::Relaxed), getters + 1);
        UNLIMITED_MP_ACTIVE.store(false, Ordering::Release);
        ORIG_TPS_AMPR.store(0, Ordering::Release);
        assert_eq!(
            unsafe { hk_tps_mp_recovery(ptr::null_mut(), ptr::null()) },
            0
        );
    }

    #[test]
    fn mp_observe_forwards_native_and_counts_without_changing_behavior() {
        unsafe extern "C" fn native_obs(_this: *mut c_void, _method: *const c_void) -> i32 {
            42
        }

        let _guard = CALLBACK_TEST_LOCK.lock().unwrap();
        ORIG_OBS_AMPR[1].store(native_obs as usize, Ordering::Release);
        let before = MP_GETTER_OTHER.load(Ordering::Relaxed);
        assert_eq!(unsafe { hk_obs_ampr_1(ptr::null_mut(), ptr::null()) }, 42);
        assert_eq!(MP_GETTER_OTHER.load(Ordering::Relaxed), before + 1);
        ORIG_OBS_AMPR[1].store(0, Ordering::Release);
        assert_eq!(unsafe { hk_obs_ampr_1(ptr::null_mut(), ptr::null()) }, 0);
        assert_eq!(MP_OBSERVE_CLASSES.len(), 3);
        assert!(MP_OBSERVE_CLASSES.iter().all(|name| !name.is_empty()));
        assert!(!MP_OBSERVE_CLASSES.contains(&"PlayerBattleStatus"));
        assert!(!MP_OBSERVE_CLASSES.contains(&"PlayerSecondaryStatus"));
    }

    #[test]
    fn range_preparation_forwards_when_disabled_or_binding_is_unavailable() {
        let _guard = CALLBACK_TEST_LOCK.lock().unwrap();
        let mut action = [0usize; 32];
        let mut calls = 0usize;
        ORIG_NAA_PREPARATION.store(original_normal as *const () as usize, Ordering::Release);
        *RANGE_BINDING.write().unwrap() = None;
        for enabled in [false, true, false] {
            RANGE_ACTIVE.store(enabled, Ordering::Release);
            unsafe {
                hk_naa_preparation(
                    action.as_mut_ptr().cast(),
                    (&mut calls as *mut usize).cast(),
                    ptr::null_mut(),
                    ptr::null(),
                )
            };
            assert_eq!(&action[4..8], &[1, 2, 80, 80]);
        }
        assert_eq!(calls, 3);
        ORIG_NAA_PREPARATION.store(0, Ordering::Release);
    }

    #[test]
    fn accepted_hit_clears_graze_output_without_touching_adjacent_bytes() {
        let mut bytes = [0xA5u8, 1, 0x5A];
        let correct = unsafe { bytes.as_mut_ptr().add(1).cast::<bool>() };
        let accepted = unsafe {
            hk_check_hit(
                ptr::null_mut(),
                ptr::null_mut(),
                200,
                300,
                false,
                correct,
                ptr::null(),
            )
        };
        assert!(accepted);
        assert_eq!(bytes, [0xA5, 0, 0x5A]);
        assert!(!unsafe {
            hk_check_hit(
                ptr::null_mut(),
                ptr::null_mut(),
                200,
                300,
                false,
                ptr::null_mut(),
                ptr::null(),
            )
        });
    }

    #[test]
    fn stability_factor_uses_fractional_units_for_all_input_rolls() {
        for stability in [0, 40, 60, 100] {
            for correct in [false, true] {
                let factor = unsafe {
                    hk_stability_factor(
                        ptr::null_mut(),
                        3,
                        stability,
                        correct,
                        ptr::null_mut(),
                        ptr::null(),
                    )
                };
                assert_eq!(factor, 1.0);
                assert_eq!((80.0 * factor) as i32, 80);
            }
        }
    }

    #[test]
    fn multiplier_reset_preserves_native_value_and_changes_do_not_compound() {
        let _guard = CALLBACK_TEST_LOCK.lock().unwrap();
        let previous_active = ALWAYS_CRIT_ACTIVE.swap(true, Ordering::AcqRel);
        let _basic_scope = crate::critical_path::NormalAttackScope::enter();
        ORIG_PBS_CRIT_DMG.store(original_critical as usize, Ordering::Release);
        // The native value in this fixture is 212, so the ratio ceiling is
        // 212 * 135 / 100 = 286. Requests above it are bounded; the native
        // value itself is never reduced.
        for (cmd, expected) in [
            (79, 212),
            (75, 175),
            (76, 200),
            (77, 250),
            (78, 286),
            (81, 286),
            (81, 286),
            (79, 212),
        ] {
            assert_eq!(job_set_crit_multiplier(cmd), 0);
            for _ in 0..3 {
                assert_eq!(
                    unsafe { hk_pbs_crit_damage(ptr::null_mut(), ptr::null()) },
                    expected
                );
            }
        }
        ORIG_PBS_CRIT_DMG.store(0, Ordering::Release);
        ALWAYS_CRIT_ACTIVE.store(previous_active, Ordering::Release);
    }

    #[test]
    fn critical_damage_never_exceeds_the_ratio_ceiling_and_never_drops_below_native() {
        for native in [1, 100, 150, 212, 350, 1000, i32::MAX - 500] {
            let ceiling = native
                .saturating_mul(CRIT_DAMAGE_MAX_RATIO)
                .saturating_div(100)
                .max(native);
            // The native value is never reduced by the ceiling.
            assert_eq!(effective_critical_damage(0, native, 135, true), native);
            assert_eq!(effective_critical_damage(0, native, 135, false), native);
            for requested in [
                0,
                175,
                200,
                250,
                300,
                CRIT_DAMAGE_ADD_100,
                CRIT_DAMAGE_ADD_200,
                CRIT_DAMAGE_ADD_300,
                CRIT_DAMAGE_ADD_1000,
            ] {
                for normal_attack in [true, false] {
                    let effective = effective_critical_damage(
                        requested,
                        native,
                        SKILL_CRIT_CAP_POINTS,
                        normal_attack,
                    );
                    assert!(
                        effective <= ceiling,
                        "effective {effective} exceeded the ceiling {ceiling}"
                    );
                }
                // An additive request is an increase, so the bound may only
                // reduce it toward the ceiling, never below native.
                if requested < 0 {
                    let effective =
                        effective_critical_damage(requested, native, SKILL_CRIT_CAP_POINTS, true);
                    assert!(
                        effective >= native,
                        "additive request fell below native {native}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_ratio_ceiling_preserves_the_observed_accepted_values() {
        // The 2026-10-20 comparison accepted an absolute 200% at a native 150
        // and rolled back 250% and above. The ceiling must keep the accepted
        // value intact and bound the rejected ones.
        let native = 150;
        assert_eq!(effective_critical_damage(0, native, 135, true), 150);
        assert_eq!(effective_critical_damage(175, native, 135, true), 175);
        assert_eq!(effective_critical_damage(200, native, 135, true), 200);
        assert!(effective_critical_damage(250, native, 135, true) < 250);
        assert!(effective_critical_damage(300, native, 135, true) < 300);
        assert!(effective_critical_damage(CRIT_DAMAGE_ADD_100, native, 135, true) < 250);
        assert!(effective_critical_damage(CRIT_DAMAGE_ADD_1000, native, 135, true) < 1_150);
    }

    #[test]
    fn additive_critical_damage_reads_current_native_value_for_both_calculators() {
        unsafe extern "C" fn native_from_field(this: *mut c_void, _method: *const c_void) -> i32 {
            unsafe { *this.cast::<i32>() }
        }

        let _guard = CALLBACK_TEST_LOCK.lock().unwrap();
        let previous_active = ALWAYS_CRIT_ACTIVE.swap(true, Ordering::AcqRel);
        let _basic_scope = crate::critical_path::NormalAttackScope::enter();
        ORIG_PBS_CRIT_DMG.store(native_from_field as usize, Ordering::Release);
        ORIG_PSS_CRIT_DMG.store(native_from_field as usize, Ordering::Release);
        assert_eq!(unsafe { dispatch_game_command(81) }, 0);
        for initial in [150, 212, 350, i32::MAX - 500] {
            let mut native = initial;
            let this = (&mut native as *mut i32).cast();
            let ceiling = initial
                .saturating_mul(CRIT_DAMAGE_MAX_RATIO)
                .saturating_div(100)
                .max(initial);
            let expected = initial
                .saturating_add(CRIT_DAMAGE_BONUS_POINTS)
                .min(ceiling);
            for _ in 0..3 {
                assert_eq!(unsafe { hk_pbs_crit_damage(this, ptr::null()) }, expected);
                assert_eq!(unsafe { hk_pss_crit_damage(this, ptr::null()) }, expected);
                assert_eq!(native, initial);
            }
            assert_eq!(job_set_crit_multiplier(79), 0);
            assert_eq!(unsafe { hk_pbs_crit_damage(this, ptr::null()) }, initial);
            assert_eq!(unsafe { hk_pss_crit_damage(this, ptr::null()) }, initial);
            assert_eq!(job_set_crit_multiplier(81), 0);
        }
        assert_eq!(job_set_crit_multiplier(79), 0);
        ORIG_PBS_CRIT_DMG.store(0, Ordering::Release);
        ORIG_PSS_CRIT_DMG.store(0, Ordering::Release);
        ALWAYS_CRIT_ACTIVE.store(previous_active, Ordering::Release);
    }

    #[test]
    fn safe_additive_ladder_is_capped_at_the_measured_ceiling_without_double_apply() {
        unsafe extern "C" fn native_from_field(this: *mut c_void, _method: *const c_void) -> i32 {
            unsafe { *this.cast::<i32>() }
        }
        let _guard = CALLBACK_TEST_LOCK.lock().unwrap();
        let previous_active = ALWAYS_CRIT_ACTIVE.swap(true, Ordering::AcqRel);
        let _basic_scope = crate::critical_path::NormalAttackScope::enter();
        ORIG_PBS_CRIT_DMG.store(native_from_field as usize, Ordering::Release);
        ORIG_PSS_CRIT_DMG.store(native_from_field as usize, Ordering::Release);
        // The adaptive cap is shared state; pin it so the expectation does not
        // depend on which test ran before.
        SKILL_ADAPTIVE_CAP.store(SKILL_CRIT_CAP_POINTS, Ordering::Relaxed);
        // The native value is 150, so the ratio ceiling is 150 * 135 / 100 =
        // 202. Every additive request above that is bounded to it.
        for (cmd, bonus) in [(83, 100), (84, 200), (85, 300)] {
            assert_eq!(job_set_crit_multiplier(cmd), 0);
            assert_eq!(critical_damage_request(), -bonus);
            let mut native = 150;
            let this = (&mut native as *mut i32).cast();
            assert_eq!(unsafe { hk_pbs_crit_damage(this, ptr::null()) }, 202);
            assert_eq!(unsafe { hk_pss_crit_damage(this, ptr::null()) }, 202);
            assert_eq!(native, 150);
        }
        assert_eq!(unsafe { dispatch_game_command(84) }, 0);
        assert_eq!(critical_damage_request(), CRIT_DAMAGE_ADD_200);
        assert_eq!(job_set_crit_multiplier(79), 0);
        assert_eq!(critical_damage_request(), 0);
        drop(_basic_scope);
        // Skill path (outside the normal-attack scope): the same ceiling
        // applies through the adaptive ratio cap. This checks the
        // calculation, not acceptance.
        for (cmd, _bonus) in [(83, 100), (84, 200), (85, 300), (81, 1_000)] {
            assert_eq!(job_set_crit_multiplier(cmd), 0);
            let mut native = 150;
            let this = (&mut native as *mut i32).cast();
            assert_eq!(unsafe { hk_pbs_crit_damage(this, ptr::null()) }, 202);
            assert_eq!(unsafe { hk_pss_crit_damage(this, ptr::null()) }, 202);
            assert_eq!(native, 150);
        }
        // The ceiling scales with the current native value (212 * 135 / 100 =
        // 286).
        assert_eq!(job_set_crit_multiplier(84), 0);
        let mut native = 212;
        let this = (&mut native as *mut i32).cast();
        assert_eq!(unsafe { hk_pbs_crit_damage(this, ptr::null()) }, 286);
        assert_eq!(native, 212);
        // Absolute presets are bounded the same way (300 -> 202), while the
        // observed accepted value 200 is left intact.
        assert_eq!(job_set_crit_multiplier(78), 0);
        let mut native = 150;
        let this = (&mut native as *mut i32).cast();
        assert_eq!(unsafe { hk_pbs_crit_damage(this, ptr::null()) }, 202);
        assert_eq!(job_set_crit_multiplier(76), 0);
        assert_eq!(unsafe { hk_pbs_crit_damage(this, ptr::null()) }, 200);
        assert_eq!(job_set_crit_multiplier(79), 0);
        assert_eq!(unsafe { hk_pbs_crit_damage(this, ptr::null()) }, 150);
        SKILL_ADAPTIVE_CAP.store(SKILL_CRIT_CAP_POINTS, Ordering::Relaxed);
        ORIG_PBS_CRIT_DMG.store(0, Ordering::Release);
        ORIG_PSS_CRIT_DMG.store(0, Ordering::Release);
        ALWAYS_CRIT_ACTIVE.store(previous_active, Ordering::Release);
    }

    #[test]
    fn nested_calculator_reads_apply_additive_damage_once_and_restore_native() {
        unsafe extern "C" fn native_secondary(this: *mut c_void, _method: *const c_void) -> i32 {
            unsafe { *this.cast::<i32>() }
        }
        unsafe extern "C" fn native_battle(this: *mut c_void, method: *const c_void) -> i32 {
            unsafe { hk_pss_crit_damage(this, method) }.saturating_add(12)
        }

        let _guard = CALLBACK_TEST_LOCK.lock().unwrap();
        let previous_active = ALWAYS_CRIT_ACTIVE.swap(true, Ordering::AcqRel);
        let _basic_scope = crate::critical_path::NormalAttackScope::enter();
        ORIG_PBS_CRIT_DMG.store(native_battle as usize, Ordering::Release);
        ORIG_PSS_CRIT_DMG.store(native_secondary as usize, Ordering::Release);
        let mut native = 200;
        let this = (&mut native as *mut i32).cast();
        for (cmd, battle, secondary) in [
            (81, 286, 270),
            (81, 286, 270),
            (76, 200, 200),
            (79, 212, 200),
            (81, 286, 270),
            (79, 212, 200),
        ] {
            assert_eq!(job_set_crit_multiplier(cmd), 0);
            for _ in 0..3 {
                assert_eq!(unsafe { hk_pbs_crit_damage(this, ptr::null()) }, battle);
                assert_eq!(unsafe { hk_pss_crit_damage(this, ptr::null()) }, secondary);
                assert_eq!(native, 200);
            }
        }
        ORIG_PBS_CRIT_DMG.store(0, Ordering::Release);
        ORIG_PSS_CRIT_DMG.store(0, Ordering::Release);
        ALWAYS_CRIT_ACTIVE.store(previous_active, Ordering::Release);
    }

    #[test]
    fn test_core_hook_classification_and_damage_offsets() {
        assert!(is_core_hook_method("CalcStable"));
        assert!(is_core_hook_method("CheckHit"));
        assert!(is_core_hook_method("get_SecondaryStatus"));
        assert!(is_core_hook_method("get_CriticalDmg"));
        assert!(is_core_hook_method("get_IsInvincibility"));

        assert!(is_core_hook_method("GetNextAtkTime"));
        assert!(is_core_hook_method("GetMotionSpeed"));
        assert!(!is_core_hook_method("get_MaxMp"));
        assert!(is_core_hook_method("calcPlayerToMobDamage"));
        assert!(!is_core_hook_method("get_FieldRay"));
        assert!(!is_core_hook_method("LineCast"));
        assert!(!is_core_hook_method("get_ActionRange"));
        assert!(is_core_hook_method("CheckActionRange"));
        assert!(is_core_hook_method("OnActionOutOfRange"));
        assert!(is_core_hook_method("CheckRangeHit"));
        assert!(!is_core_hook_method("PowerWaveCheck"));
        assert!(!is_core_hook_method("DamageHp"));
        assert!(is_core_hook_method("ActionHit"));
        assert_eq!(
            unsafe { hk_action_range_30(ptr::null_mut(), ptr::null()) },
            EXTENDED_RANGE_WORLD_UNITS
        );
        assert!(!unsafe {
            hk_pbm_action_out_of_range(
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null(),
            )
        });
        assert!(unsafe {
            hk_true_3arg(
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null(),
            )
        });
        assert!(unsafe {
            hk_true_2arg(
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null(),
            )
        });
        assert!(!unsafe { hk_false_0arg(ptr::null_mut(), ptr::null()) });

        unsafe {
            hk_void_2arg(ptr::null_mut(), 50, false, ptr::null());
        }

        assert!(!unsafe {
            hk_check_range_hit(
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                5.0,
                ptr::null(),
            )
        });

        unsafe {
            ensure_dd_offsets(ptr::null_mut());
        }
    }

    #[test]
    fn adaptive_skill_cap_breathes_with_live_feedback() {
        let _guard = CALLBACK_TEST_LOCK.lock().unwrap();
        SKILL_ADAPTIVE_CAP.store(SKILL_CRIT_CAP_POINTS, Ordering::Relaxed);
        SKILL_ACCEPT_STREAK.store(0, Ordering::Relaxed);
        assert_eq!(skill_adaptive_cap(), 135);
        // A fresh target restores the measured ceiling.
        note_new_skill_target();
        assert_eq!(skill_adaptive_cap(), 135);
        // One rejection steps the cap down one rung.
        note_skill_response_rejected();
        assert_eq!(skill_adaptive_cap(), 120);
        // Accepts never raise the cap: the ceiling is a measured boundary.
        for _ in 0..24 {
            note_skill_response_accepted();
        }
        assert_eq!(skill_adaptive_cap(), 120);
        // Further rejections step down to the native floor and stop there.
        note_skill_response_rejected();
        assert_eq!(skill_adaptive_cap(), 105);
        note_skill_response_rejected();
        assert_eq!(skill_adaptive_cap(), 100);
        note_skill_response_rejected();
        assert_eq!(skill_adaptive_cap(), 100);
        // Pure step function pins the edges.
        assert_eq!(adaptive_step(135, true, 3), (135, 4));
        assert_eq!(adaptive_step(135, true, 100), (135, 101));
        assert_eq!(adaptive_step(135, false, 5), (120, 0));
        assert_eq!(adaptive_step(100, false, 0), (100, 0));
        // Leave the shared default exactly as found for the ladder test.
        SKILL_ADAPTIVE_CAP.store(SKILL_CRIT_CAP_POINTS, Ordering::Relaxed);
        SKILL_ACCEPT_STREAK.store(0, Ordering::Relaxed);
    }

    #[test]
    fn basic_attack_feedback_does_not_promote_the_skill_cap_and_state_sets_are_idempotent() {
        let _guard = CALLBACK_TEST_LOCK.lock().unwrap();
        SKILL_ADAPTIVE_CAP.store(135, Ordering::Relaxed);
        SKILL_ACCEPT_STREAK.store(7, Ordering::Relaxed);
        crate::combat_response::apply_skill_feedback(0, Some(true));
        assert_eq!(skill_adaptive_cap(), 135);
        assert_eq!(SKILL_ACCEPT_STREAK.load(Ordering::Relaxed), 7);
        crate::combat_response::apply_skill_feedback(1, Some(true));
        assert_eq!(skill_adaptive_cap(), 135);
        crate::combat_response::apply_skill_feedback(1, Some(false));
        assert_eq!(skill_adaptive_cap(), 120);
        let before = active_toggle_mask();
        for index in 0..8 {
            let bit = 1 << index;
            let cmd = protocol::CommandId::set_toggle_command(bit, before & bit != 0).unwrap();
            assert_eq!(unsafe { dispatch_game_command(cmd) }, 0);
            assert_eq!(unsafe { dispatch_game_command(cmd) }, 0);
        }
        assert_eq!(active_toggle_mask(), before);
        SKILL_ACCEPT_STREAK.store(0, Ordering::Relaxed);
        SKILL_ADAPTIVE_CAP.store(SKILL_CRIT_CAP_POINTS, Ordering::Relaxed);
    }

    #[test]
    fn disabled_ladder_ignores_feedback_and_reenable_preserves_progress() {
        let _guard = CALLBACK_TEST_LOCK.lock().unwrap();
        assert_eq!(unsafe { dispatch_game_command(95) }, 0);
        note_new_skill_target();
        for _ in 0..12 {
            note_skill_response_accepted();
        }
        assert_eq!(skill_adaptive_cap(), 135);
        set_adaptive_ladder(true);
        for _ in 0..7 {
            note_skill_response_accepted();
        }
        set_adaptive_ladder(true);
        note_skill_response_accepted();
        assert_eq!(skill_adaptive_cap(), 135);
        assert_eq!(unsafe { dispatch_game_command(95) }, 0);
        assert_eq!(skill_adaptive_cap(), 135);
        set_adaptive_ladder(true);
        SKILL_ADAPTIVE_CAP.store(SKILL_CRIT_CAP_POINTS, Ordering::Relaxed);
    }

    #[test]
    fn unknown_skill_damage_breaks_the_promotion_streak() {
        let _guard = CALLBACK_TEST_LOCK.lock().unwrap();
        set_adaptive_ladder(false);
        set_adaptive_ladder(true);
        for _ in 0..7 {
            crate::combat_response::apply_skill_feedback(33, Some(true));
        }
        crate::combat_response::apply_skill_feedback(33, None);
        crate::combat_response::apply_skill_feedback(33, Some(true));
        assert_eq!(skill_adaptive_cap(), 135);
        assert_eq!(SKILL_ACCEPT_STREAK.load(Ordering::Relaxed), 1);
        set_adaptive_ladder(false);
        set_adaptive_ladder(true);
        SKILL_ADAPTIVE_CAP.store(SKILL_CRIT_CAP_POINTS, Ordering::Relaxed);
    }

    #[test]
    fn unsupported_dispatch_never_reports_success_and_partial_setup_is_an_error() {
        let _guard = CALLBACK_TEST_LOCK.lock().unwrap();
        for cmd in [4, 10, 31, 45, 67, 117] {
            assert_eq!(unsafe { dispatch_game_command(cmd) }, -4);
        }
        for cmd in 96..=99 {
            assert_eq!(unsafe { dispatch_game_command(cmd) }, -10);
        }
        // The phase marker is a supported command and always succeeds.
        let before = crate::combat_observe::phase();
        assert_eq!(unsafe { dispatch_game_command(116) }, 0);
        assert_eq!(crate::combat_observe::phase(), before + 1);
        assert_eq!(crate::combat_observe::set_phase(), 0);
        assert_eq!(crate::combat_observe::phase(), before + 2);
        assert_eq!(unsafe { dispatch_game_command(44) }, -7);
        note_partial(TOGGLE_SPEED_BOOST, true);
        assert_eq!(feature_setup_result(TOGGLE_SPEED_BOOST, true), -11);
        assert_eq!(feature_setup_result(TOGGLE_SPEED_BOOST, false), 0);
        assert_eq!(
            PARTIAL_FEATURES.load(Ordering::Acquire) & TOGGLE_SPEED_BOOST,
            0
        );
    }
}
