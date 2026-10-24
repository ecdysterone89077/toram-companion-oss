//! Restrict the secondary-status fallback to one verified normal-attack caller.

use crate::chain::{is_valid_il2cpp_class, is_valid_il2cpp_obj};
use crate::win::{fn_readable, page_exec_ok, ptr_ok};
use core::ffi::c_void;
use core::marker::PhantomData;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::cell::Cell;
use std::rc::Rc;
use std::sync::RwLock;

pub static ORIG_SECONDARY_GETTER: AtomicUsize = AtomicUsize::new(0);
static ENABLED: AtomicBool = AtomicBool::new(false);
static FALLBACKS: AtomicUsize = AtomicUsize::new(0);
static CONFIG: RwLock<Option<Config>> = RwLock::new(None);

thread_local! {
    static NORMAL_ATTACK_DEPTH: Cell<usize> = const { Cell::new(0) };
}

/// This guard must surround only the call to the original normal-attack method.
/// It cannot move to another thread, and nested/unwinding calls restore their depth.
pub struct NormalAttackScope {
    previous: Option<usize>,
    _not_send: PhantomData<Rc<()>>,
}

impl NormalAttackScope {
    pub fn enter() -> Self {
        let previous = NORMAL_ATTACK_DEPTH
            .try_with(|depth| {
                let previous = depth.get();
                previous.checked_add(1).map(|next| {
                    depth.set(next);
                    previous
                })
            })
            .ok()
            .flatten();
        Self {
            previous,
            _not_send: PhantomData,
        }
    }
}

impl Drop for NormalAttackScope {
    fn drop(&mut self) {
        if let Some(previous) = self.previous {
            let _ = NORMAL_ATTACK_DEPTH.try_with(|depth| depth.set(previous));
        }
    }
}

pub(crate) fn in_normal_attack() -> bool {
    NORMAL_ATTACK_DEPTH
        .try_with(|depth| depth.get() != 0)
        .unwrap_or(false)
}

#[derive(Clone, Copy)]
struct Config {
    local_status: usize,
    player_status_class: usize,
    secondary_class: usize,
    battle_class: usize,
    battle_field_offset: usize,
    caller: usize,
    original_getter: usize,
}

const CALLER_OFFSET: usize = 0x27B;
const BATTLE_CAPTURE: &[u8] = &[
    0x48, 0x8b, 0x10, 0x48, 0x8b, 0xc8, 0x4c, 0x8b, 0x82, 0xa8, 0x01, 0x00, 0x00, 0x48, 0x8b, 0x92,
    0xb0, 0x01, 0x00, 0x00, 0x41, 0xff, 0xd0, 0x49, 0x8b, 0x0e, 0x48, 0x8b, 0xd8, 0x48, 0x89, 0x45,
    0x88,
];
const SECONDARY_CAST: &[u8] = &[
    0x48, 0x8b, 0x10, 0x48, 0x8b, 0xc8, 0x4c, 0x8b, 0x82, 0x98, 0x01, 0x00, 0x00, 0x48, 0x8b, 0x92,
    0xa0, 0x01, 0x00, 0x00, 0x41, 0xff, 0xd0, 0x48, 0x8b, 0xd0, 0x48, 0x85, 0xc0, 0x74, 0x34, 0x4c,
    0x8b, 0x05, 0, 0, 0, 0, 0x48, 0x8b, 0x00, 0x41, 0x0f, 0xb6, 0x88, 0x30, 0x01, 0x00, 0x00, 0x38,
    0x88, 0x30, 0x01, 0x00, 0x00, 0x72, 0x12, 0x48, 0x8b, 0x80, 0xc8, 0x00, 0x00, 0x00, 0x4c, 0x39,
    0x44, 0xc8, 0xf8, 0x75, 0x04, 0xb0, 0x01, 0xeb, 0x02, 0x32, 0xc0, 0x84, 0xc0, 0x48, 0x0f, 0x45,
    0xfa,
];
const INTERFACE_FALLBACK: &[u8] = &[
    0x48, 0x85, 0xff, 0x75, 0x1d, 0x48, 0x85, 0xdb, 0x0f, 0x84, 0x1e, 0x1d, 0x00, 0x00, 0x48, 0x8b,
    0x15, 0, 0, 0, 0, 0x8d, 0x4f, 0x1b, 0x4c, 0x8b, 0xc3, 0xe8, 0, 0, 0, 0, 0xeb, 0x39,
];
const HIT_SELECTION: &[u8] = &[
    0x45, 0x33, 0xc9, 0x4d, 0x8b, 0xc4, 0x8b, 0xd0, 0x48, 0x8b, 0xce, 0xe8, 0, 0, 0, 0, 0x84, 0xc0,
    0xb9, 0x01, 0x00, 0x00, 0x00, 0xba, 0x02, 0x00, 0x00, 0x00, 0x0f, 0x45, 0xca, 0x89, 0x4c, 0x24,
    0x74,
];

fn pattern_matches(actual: &[u8], expected: &[u8], relocations: &[(usize, usize)]) -> bool {
    actual.len() == expected.len()
        && actual.iter().zip(expected).enumerate().all(|(i, (a, b))| {
            relocations
                .iter()
                .any(|&(start, len)| i >= start && i < start + len)
                || a == b
        })
}

unsafe fn native_pattern(
    base: usize,
    offset: usize,
    expected: &[u8],
    relocations: &[(usize, usize)],
) -> bool {
    let Some(address) = base.checked_add(offset) else {
        return false;
    };
    if !page_exec_ok(address, expected.len()) || !ptr_ok(address, expected.len()) {
        return false;
    }
    let bytes = unsafe { core::slice::from_raw_parts(address as *const u8, expected.len()) };
    pattern_matches(bytes, expected, relocations)
}

unsafe fn rip_target(displacement: usize, next_instruction: usize) -> Option<usize> {
    if !ptr_ok(displacement, 4) {
        return None;
    }
    let delta = unsafe { core::ptr::read_unaligned(displacement as *const i32) } as isize;
    next_instruction.checked_add_signed(delta)
}

/// Validate the installed version's native branch before publishing a disabled configuration.
/// Class identities and battleStatus's field offset must be resolved from current metadata.
pub unsafe fn prepare(
    local_status: *mut c_void,
    player_status_class: *mut c_void,
    secondary_class: *mut c_void,
    battle_class: *mut c_void,
    battle_field_offset: usize,
    original_naa: usize,
    original_secondary_getter: usize,
    interface_class: *mut c_void,
    original_check_critical: usize,
) -> Result<(), &'static str> {
    disable();
    *CONFIG
        .write()
        .map_err(|_| "critical-path configuration lock poisoned")? = None;
    if original_naa.checked_add(0x43A).is_none() {
        return Err("normal-attack address overflow");
    }
    let config = Config {
        local_status: local_status as usize,
        player_status_class: player_status_class as usize,
        secondary_class: secondary_class as usize,
        battle_class: battle_class as usize,
        battle_field_offset,
        caller: original_naa
            .checked_add(CALLER_OFFSET)
            .ok_or("normal-attack address overflow")?,
        original_getter: original_secondary_getter,
    };
    if battle_field_offset != 0x30 || config.battle_class == config.secondary_class {
        return Err("unexpected battle-status layout or class");
    }
    for class in [
        player_status_class,
        secondary_class,
        battle_class,
        interface_class,
    ] {
        if !is_valid_il2cpp_class(class) {
            return Err("invalid status class");
        }
    }
    if !unsafe { is_valid_il2cpp_obj(local_status) }
        || unsafe { *(local_status as *const usize) } != config.player_status_class
    {
        return Err("local status class does not match");
    }
    if !unsafe {
        native_pattern(
            original_secondary_getter,
            0,
            &[0x48, 0x8b, 0x41, 0x28, 0xc3],
            &[],
        )
    } || !unsafe { native_pattern(original_naa, 0x21D, BATTLE_CAPTURE, &[]) }
        || !unsafe { native_pattern(original_naa, 0x264, SECONDARY_CAST, &[(34, 4)]) }
        || !unsafe { native_pattern(original_naa, 0x3BC, INTERFACE_FALLBACK, &[(17, 4), (28, 4)]) }
        || !unsafe { native_pattern(original_naa, 0x417, HIT_SELECTION, &[(12, 4)]) }
    {
        return Err("native critical-selection pattern changed");
    }
    let global = unsafe { rip_target(original_naa + 0x286, original_naa + 0x28A) }
        .ok_or("invalid secondary cast target")?;
    if !ptr_ok(global, 8) || unsafe { *(global as *const usize) } != config.secondary_class {
        return Err("native cast is not the resolved secondary class");
    }
    let interface_global = unsafe { rip_target(original_naa + 0x3CD, original_naa + 0x3D1) }
        .ok_or("invalid status interface target")?;
    if !ptr_ok(interface_global, 8)
        || unsafe { *(interface_global as *const usize) } != interface_class as usize
    {
        return Err("fallback interface is not IPlayerStatusCalculator");
    }
    let helper = unsafe { rip_target(original_naa + 0x3D8, original_naa + 0x3DC) }
        .ok_or("invalid interface helper target")?;
    if !fn_readable(helper) {
        return Err("interface helper is not executable");
    }
    let check_critical = unsafe { rip_target(original_naa + 0x423, original_naa + 0x427) }
        .ok_or("invalid critical check target")?;
    if check_critical != original_check_critical || !fn_readable(check_critical) {
        return Err("critical check target does not match its original method");
    }
    if unsafe { select_fallback(config, config.local_status, config.caller, true, true) }.is_none()
    {
        return Err("local battle-status object is unavailable");
    }
    let mut guard = CONFIG
        .write()
        .map_err(|_| "critical-path configuration lock poisoned")?;
    *guard = Some(config);
    Ok(())
}

/// Call only after the complete hook transaction has succeeded.
pub fn enable() -> bool {
    disable();
    let config = CONFIG.read().ok().and_then(|guard| *guard);
    let Some(config) = config else { return false };
    if ORIG_SECONDARY_GETTER.load(Ordering::Acquire) != config.original_getter
        || !fn_readable(config.original_getter)
    {
        return false;
    }
    ENABLED.store(true, Ordering::Release);
    true
}

/// Disable before restoring any hook; keep the original pointer for in-flight calls.
pub fn disable() {
    ENABLED.store(false, Ordering::Release);
}

pub fn fallback_count() -> usize {
    FALLBACKS.load(Ordering::Relaxed)
}

unsafe fn select_fallback(
    config: Config,
    this: usize,
    caller: usize,
    enabled: bool,
    scoped: bool,
) -> Option<usize> {
    if !enabled || !scoped || this != config.local_status || caller != config.caller {
        return None;
    }
    if !unsafe { is_valid_il2cpp_obj(this as *const c_void) }
        || unsafe { *(this as *const usize) } != config.player_status_class
    {
        return None;
    }
    let field = this.checked_add(config.battle_field_offset)?;
    if !ptr_ok(field, core::mem::size_of::<usize>()) {
        return None;
    }
    let battle = unsafe { *(field as *const usize) };
    if !unsafe { is_valid_il2cpp_obj(battle as *const c_void) }
        || unsafe { *(battle as *const usize) } != config.battle_class
        || config.battle_class == config.secondary_class
    {
        return None;
    }
    Some(battle)
}

type Getter = unsafe extern "C" fn(*mut c_void, *const c_void) -> *mut c_void;

unsafe extern "C" fn secondary_getter_callback(
    this: *mut c_void,
    method: *const c_void,
    caller: usize,
) -> *mut c_void {
    let original = ORIG_SECONDARY_GETTER.load(Ordering::Acquire);
    if ENABLED.load(Ordering::Acquire) && in_normal_attack() {
        let config = CONFIG.try_read().ok().and_then(|guard| *guard);
        if let Some(config) = config {
            if original == config.original_getter {
                if let Some(battle) = unsafe {
                    select_fallback(
                        config,
                        this as usize,
                        caller,
                        ENABLED.load(Ordering::Acquire),
                        true,
                    )
                } {
                    FALLBACKS.fetch_add(1, Ordering::Relaxed);
                    return battle as *mut c_void;
                }
            }
        }
    }
    if original == secondary_getter_shim as *const () as usize || !fn_readable(original) {
        return core::ptr::null_mut();
    }
    let forward: Getter = unsafe { core::mem::transmute(original) };
    unsafe { forward(this, method) }
}

// Win64: RCX=this, RDX=MethodInfo. Capture the caller before any Rust prologue;
// the tail jump preserves the original return address and shadow-space contract.
#[unsafe(naked)]
pub unsafe extern "C" fn secondary_getter_shim(
    _this: *mut c_void,
    _method: *const c_void,
) -> *mut c_void {
    core::arch::naked_asm!(
        "mov r8, [rsp]",
        "jmp {}",
        sym secondary_getter_callback,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::sync::Mutex;

    static STATE_TEST_LOCK: Mutex<()> = Mutex::new(());

    struct RestoreState {
        config: Option<Config>,
        original: usize,
        enabled: bool,
    }

    impl RestoreState {
        fn save() -> Self {
            Self {
                config: *CONFIG.read().unwrap(),
                original: ORIG_SECONDARY_GETTER.load(Ordering::Acquire),
                enabled: ENABLED.load(Ordering::Acquire),
            }
        }
    }

    impl Drop for RestoreState {
        fn drop(&mut self) {
            disable();
            *CONFIG.write().unwrap() = self.config;
            ORIG_SECONDARY_GETTER.store(self.original, Ordering::Release);
            ENABLED.store(self.enabled, Ordering::Release);
        }
    }

    struct Fixture {
        player_class: Box<[usize; 8]>,
        secondary_class: Box<[usize; 8]>,
        battle_class: Box<[usize; 8]>,
        status: Box<[usize; 8]>,
        battle: Box<[usize; 2]>,
    }

    impl Fixture {
        fn new() -> Self {
            let mut fixture = Self {
                player_class: Box::new([0; 8]),
                secondary_class: Box::new([0; 8]),
                battle_class: Box::new([0; 8]),
                status: Box::new([0; 8]),
                battle: Box::new([0; 2]),
            };
            fixture.status[0] = fixture.player_class.as_ptr() as usize;
            fixture.battle[0] = fixture.battle_class.as_ptr() as usize;
            fixture.status[6] = fixture.battle.as_ptr() as usize;
            fixture
        }
        fn config(&self) -> Config {
            Config {
                local_status: self.status.as_ptr() as usize,
                player_status_class: self.player_class.as_ptr() as usize,
                secondary_class: self.secondary_class.as_ptr() as usize,
                battle_class: self.battle_class.as_ptr() as usize,
                battle_field_offset: 0x30,
                caller: 0x12345678,
                original_getter: 0,
            }
        }
    }

    #[test]
    fn fallback_requires_exact_scope_caller_instance_and_enabled_state() {
        let fixture = Fixture::new();
        let config = fixture.config();
        for (this, caller, enabled, scoped) in [
            (config.local_status, config.caller + 1, true, true),
            (config.local_status + 8, config.caller, true, true),
            (config.local_status, config.caller, false, true),
            (config.local_status, config.caller, true, false),
        ] {
            assert_eq!(
                unsafe { select_fallback(config, this, caller, enabled, scoped) },
                None
            );
        }
        assert_eq!(
            unsafe { select_fallback(config, config.local_status, config.caller, true, true) },
            Some(fixture.battle.as_ptr() as usize)
        );
    }

    #[test]
    fn fallback_reads_existing_battle_object_without_mutating_status() {
        let fixture = Fixture::new();
        let config = fixture.config();
        let status_before = *fixture.status;
        let battle_before = *fixture.battle;
        let _scope = NormalAttackScope::enter();
        assert_eq!(
            unsafe {
                select_fallback(
                    config,
                    config.local_status,
                    config.caller,
                    true,
                    in_normal_attack(),
                )
            },
            Some(fixture.battle.as_ptr() as usize)
        );
        assert_eq!(*fixture.status, status_before);
        assert_eq!(*fixture.battle, battle_before);
    }

    #[test]
    fn invalid_or_changed_status_objects_forward() {
        let mut fixture = Fixture::new();
        let config = fixture.config();
        fixture.status[0] = fixture.secondary_class.as_ptr() as usize;
        assert_eq!(
            unsafe { select_fallback(config, config.local_status, config.caller, true, true) },
            None
        );
        fixture.status[0] = config.player_status_class;
        fixture.battle[0] = config.secondary_class;
        assert_eq!(
            unsafe { select_fallback(config, config.local_status, config.caller, true, true) },
            None
        );
        fixture.status[6] = 0;
        assert_eq!(
            unsafe { select_fallback(config, config.local_status, config.caller, true, true) },
            None
        );
        fixture.status[6] = 0x1234;
        assert_eq!(
            unsafe { select_fallback(config, config.local_status, config.caller, true, true) },
            None
        );
    }

    #[test]
    fn nested_and_unwinding_scopes_restore_previous_state() {
        assert!(!in_normal_attack());
        {
            let _outer = NormalAttackScope::enter();
            assert!(in_normal_attack());
            {
                let _inner = NormalAttackScope::enter();
                assert!(in_normal_attack());
            }
            assert!(in_normal_attack());
            assert!(catch_unwind(AssertUnwindSafe(|| {
                let _inner = NormalAttackScope::enter();
                panic!("test unwind");
            }))
            .is_err());
            assert!(in_normal_attack());
        }
        assert!(!in_normal_attack());
        assert!(catch_unwind(AssertUnwindSafe(|| {
            let _outer = NormalAttackScope::enter();
            panic!("test unwind");
        }))
        .is_err());
        assert!(!in_normal_attack());
    }

    #[test]
    fn normal_attack_scope_is_thread_local() {
        let _scope = NormalAttackScope::enter();
        assert!(in_normal_attack());
        std::thread::spawn(|| {
            assert!(!in_normal_attack());
            {
                let _scope = NormalAttackScope::enter();
                assert!(in_normal_attack());
            }
            assert!(!in_normal_attack());
        })
        .join()
        .unwrap();
        assert!(in_normal_attack());
    }

    #[test]
    fn native_patterns_allow_only_the_documented_relocations() {
        assert_eq!(SECONDARY_CAST.len(), 83);
        assert_eq!(INTERFACE_FALLBACK.len(), 34);
        assert_eq!(HIT_SELECTION.len(), 35);
        let mut relocated = SECONDARY_CAST.to_vec();
        relocated[34..38].copy_from_slice(&[1, 2, 3, 4]);
        assert!(pattern_matches(&relocated, SECONDARY_CAST, &[(34, 4)]));
        relocated[20] ^= 1;
        assert!(!pattern_matches(&relocated, SECONDARY_CAST, &[(34, 4)]));
        assert!(!pattern_matches(
            &relocated[..82],
            SECONDARY_CAST,
            &[(34, 4)]
        ));
    }

    #[test]
    fn shim_forwards_original_arguments_outside_scope() {
        let _lock = STATE_TEST_LOCK.lock().unwrap();
        let _restore = RestoreState::save();
        unsafe extern "C" fn original(this: *mut c_void, method: *const c_void) -> *mut c_void {
            if this as usize == 0x12340 && method as usize == 0x56780 {
                this
            } else {
                core::ptr::null_mut()
            }
        }
        disable();
        ORIG_SECONDARY_GETTER.store(original as *const () as usize, Ordering::Release);
        let result =
            unsafe { secondary_getter_shim(0x12340 as *mut c_void, 0x56780 as *const c_void) };
        assert_eq!(result as usize, 0x12340);
    }

    #[test]
    fn callback_forwards_all_rejected_contexts_and_only_counts_valid_fallback() {
        let _lock = STATE_TEST_LOCK.lock().unwrap();
        let _restore = RestoreState::save();
        thread_local! { static CALLS: Cell<usize> = const { Cell::new(0) }; }
        unsafe extern "C" fn original(this: *mut c_void, method: *const c_void) -> *mut c_void {
            CALLS.with(|calls| calls.set(calls.get() + 1));
            if method as usize == 0x56780 {
                this
            } else {
                core::ptr::null_mut()
            }
        }
        let fixture = Fixture::new();
        let mut config = fixture.config();
        config.original_getter = original as *const () as usize;
        *CONFIG.write().unwrap() = Some(config);
        ORIG_SECONDARY_GETTER.store(config.original_getter, Ordering::Release);
        assert!(enable());
        let this = config.local_status as *mut c_void;
        let method = 0x56780 as *const c_void;
        let before_count = fallback_count();
        assert_eq!(
            unsafe { secondary_getter_callback(this, method, config.caller) },
            this
        );
        {
            let _scope = NormalAttackScope::enter();
            assert_eq!(
                unsafe { secondary_getter_callback(this, method, config.caller + 1) },
                this
            );
            let other = (config.local_status + 8) as *mut c_void;
            assert_eq!(
                unsafe { secondary_getter_callback(other, method, config.caller) },
                other
            );
            assert_eq!(
                unsafe { secondary_getter_callback(this, method, config.caller) },
                fixture.battle.as_ptr() as *mut c_void
            );
            assert_eq!(fallback_count(), before_count + 1);
            disable();
            assert_eq!(
                unsafe { secondary_getter_callback(this, method, config.caller) },
                this
            );
        }
        assert_eq!(CALLS.with(Cell::get), 4);
        assert_eq!(fallback_count(), before_count + 1);
    }

    #[test]
    fn failed_prepare_discards_previous_configuration_and_cannot_reenable() {
        let _lock = STATE_TEST_LOCK.lock().unwrap();
        let _restore = RestoreState::save();
        let fixture = Fixture::new();
        *CONFIG.write().unwrap() = Some(fixture.config());
        ENABLED.store(true, Ordering::Release);
        let error = unsafe {
            prepare(
                core::ptr::null_mut(),
                core::ptr::null_mut(),
                core::ptr::null_mut(),
                core::ptr::null_mut(),
                0,
                0,
                0,
                core::ptr::null_mut(),
                0,
            )
        };
        assert!(error.is_err());
        assert!(CONFIG.read().unwrap().is_none());
        assert!(!ENABLED.load(Ordering::Acquire));
        assert!(!enable());
    }
}
