//! Bounded callback diagnostics. No game fields or callback results are changed.
//! Callbacks only append to memory; command 80 stops/restores and writes the log.

#![allow(function_casts_as_integer)]

use crate::chain::GameChain;
use crate::hook::MethodHookEntry;
use crate::il2cpp::{api, find_class, find_nested_class, get_field, GcScope, MethodSignature};
use crate::win::{
    fn_readable, log_fmt, ptr_ok, GetCurrentProcessId, GetCurrentThreadId, QueryPerformanceCounter,
    QueryPerformanceFrequency, Sleep,
};
use core::ffi::c_void;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;

const CAPACITY: usize = 512;
const NAMES: [&str; 3] = ["OnActionRange", "OnSkillActionStart", "OnSkillActionHit"];
/// Distance source labels. The value is always the game's own
/// `PlayerDistance` cache, never a value this observer computed.
const DISTANCE_UNAVAILABLE: u8 = 0;
const DISTANCE_GAME_CACHE: u8 = 1;
static ACTIVE: AtomicBool = AtomicBool::new(false);
static INSTALLED: AtomicBool = AtomicBool::new(false);
static FLIGHT: AtomicUsize = AtomicUsize::new(0);
static NEXT_CALL: AtomicU64 = AtomicU64::new(0);
static DROPPED: AtomicU64 = AtomicU64::new(0);
/// Operator phase id stamped onto every later sample.
static PHASE: AtomicU32 = AtomicU32::new(0);
static ORIGINALS: [AtomicUsize; 3] = [const { AtomicUsize::new(0) }; 3];
static HOOKS: Mutex<[MethodHookEntry; 3]> = Mutex::new([MethodHookEntry::new(); 3]);
static CONFIG: Mutex<Option<Binding>> = Mutex::new(None);
static EVENTS: Mutex<Buffer> = Mutex::new(Buffer::new());

#[derive(Clone, Copy, Debug, PartialEq)]
struct Sample {
    valid: bool,
    matched_play: bool,
    normal: bool,
    play: i32,
    id_field: i32,
    attack_count: i32,
    range_bits: u32,
    local_mp: i32,
    received_mp: i32,
    /// Operator phase id in force when the sample was taken.
    phase: u32,
    /// Feature toggle mask in force when the sample was taken.
    modes: u32,
    /// Observer callback counter when the sample was taken.
    calls: u64,
    /// Target distance in internal units, from the game's own cache.
    distance: f32,
    /// Label for `distance`; 0 means the reading was unavailable.
    distance_source: u8,
    /// Target distance in HUD metres, `ceil(distance * 0.5)`.
    hud_metres: i32,
}

impl Default for Sample {
    /// The default sample is the "nothing was measured" sample: the distance
    /// is explicitly unavailable rather than a misleading zero.
    fn default() -> Self {
        Self {
            valid: false,
            matched_play: false,
            normal: false,
            play: -1,
            id_field: -1,
            attack_count: -1,
            range_bits: 0,
            local_mp: -1,
            received_mp: -1,
            phase: 0,
            modes: 0,
            calls: 0,
            distance: 0.0,
            distance_source: DISTANCE_UNAVAILABLE,
            hud_metres: -1,
        }
    }
}

#[derive(Clone, Copy, Default)]
struct Event {
    call: u64,
    ticks: i64,
    thread: u32,
    stage: u8,
    exit: bool,
    result: i8,
    battle: usize,
    target: usize,
    target_actor: usize,
    action: usize,
    sample: Sample,
}

struct Buffer {
    entries: [Event; CAPACITY],
    len: usize,
}
impl Buffer {
    const fn new() -> Self {
        const EMPTY: Event = Event {
            call: 0,
            ticks: 0,
            thread: 0,
            stage: 0,
            exit: false,
            result: -1,
            battle: 0,
            target: 0,
            target_actor: 0,
            action: 0,
            sample: Sample {
                valid: false,
                matched_play: false,
                normal: false,
                play: -1,
                id_field: -1,
                attack_count: -1,
                range_bits: 0,
                local_mp: -1,
                received_mp: -1,
                phase: 0,
                modes: 0,
                calls: 0,
                distance: 0.0,
                distance_source: DISTANCE_UNAVAILABLE,
                hud_metres: -1,
            },
        };
        Self {
            entries: [EMPTY; CAPACITY],
            len: 0,
        }
    }
    fn push(&mut self, event: Event) -> bool {
        if self.len == CAPACITY {
            return false;
        }
        self.entries[self.len] = event;
        self.len += 1;
        true
    }
}

struct Binding {
    battle: usize,
    battle_class: usize,
    game: usize,
    game_class: usize,
    manager: usize,
    manager_class: usize,
    data_class: usize,
    action_class: usize,
    normal_class: usize,
    current: usize,
    data_action: usize,
    play: usize,
    id: usize,
    count: usize,
    range: usize,
    local_mp: usize,
    received_mp: usize,
    /// `EnemyMobActionManagerBase`, which owns the `PlayerDistance` cache.
    distance_class: usize,
    /// Offset of `<PlayerDistance>k__BackingField`; 0 when unavailable.
    distance: usize,
    _roots: GcScope,
}

unsafe fn read<T: Copy>(object: usize, offset: usize) -> Option<T> {
    let address = object.checked_add(offset)?;
    if !ptr_ok(address, core::mem::size_of::<T>()) {
        return None;
    }
    Some(unsafe { core::ptr::read_unaligned(address as *const T) })
}

unsafe fn exact(object: usize, class: usize) -> bool {
    class != 0 && unsafe { read::<usize>(object, 0) } == Some(class)
}

unsafe fn derives(object: usize, expected: usize) -> bool {
    let Some(parent) = api().and_then(|a| a.class_get_parent) else {
        return false;
    };
    let Some(mut class) = (unsafe { read::<usize>(object, 0) }) else {
        return false;
    };
    for _ in 0..16 {
        if class == expected && expected != 0 {
            return true;
        }
        if !ptr_ok(class, 0x138) {
            return false;
        }
        class = unsafe { parent(class as *mut c_void) } as usize;
    }
    false
}

unsafe fn offset(class: usize, name: &str, kind: u32) -> Result<usize, &'static str> {
    let a = api().ok_or("IL2CPP API unavailable")?;
    if class == 0 {
        log_fmt(format_args!("[observe80] class is null for field {}", name));
        return Err("observer class null");
    }
    let field = unsafe { get_field(class as *mut c_void, name) } as usize;
    if field == 0 {
        log_fmt(format_args!("[observe80] field not found: {}", name));
        return Err("observer field missing");
    }
    let ty = unsafe { read::<usize>(field, 8) }.ok_or("observer field type ptr missing")?;
    let bits = unsafe { read::<u32>(ty, 8) }.ok_or("observer field type missing")?;
    if bits & 0x10 != 0 || (bits >> 16) & 0xff != kind {
        log_fmt(format_args!(
            "[observe80] field type mismatch for {}: expected {}, got {}",
            name,
            kind,
            (bits >> 16) & 0xff
        ));
        return Err("observer field type mismatch");
    }
    let value = unsafe { (a.field_get_offset)(field as *mut c_void) };
    if !(0x10..=0x4000).contains(&value) {
        log_fmt(format_args!(
            "[observe80] field offset invalid for {}: 0x{:x}",
            name, value
        ));
        return Err("observer field offset invalid");
    }
    Ok(value as usize)
}

impl Binding {
    unsafe fn prepare(ch: &GameChain) -> Result<Self, &'static str> {
        let class = |name: &str| unsafe { find_class(ch.image, ch.domain, "", name) } as usize;
        let game_class = class("PlayerGameStatus");
        let manager_base = class("SkillActionManager");
        let data_class =
            unsafe { find_nested_class(manager_base as *mut c_void, "SkillActionData") } as usize;
        let action_class = class("SkillActionBase");
        let normal_class = class("NormalAttackAction");
        if data_class == 0 {
            log_fmt(format_args!(
                "[observe80] SkillActionData nested class missing"
            ));
            return Err("observer SkillActionData missing");
        }
        let game = unsafe {
            read::<usize>(
                ch.status as usize,
                offset(ch.status_class as usize, "gameStatus", 18)?,
            )
        }
        .ok_or("observer game status unavailable")?;
        let manager = unsafe {
            read::<usize>(
                ch.pbm as usize,
                offset(ch.pbm_class as usize, "skillActManager", 18)?,
            )
        }
        .ok_or("observer skill manager unavailable")?;
        if !unsafe {
            exact(game, game_class)
                && exact(ch.pbm as usize, ch.pbm_class as usize)
                && derives(manager, manager_base)
        } {
            return Err("observer local classes mismatch");
        }
        let mut roots = GcScope::new();
        for object in [ch.pbm as usize, game, manager] {
            roots
                .try_pin(object as *mut c_void)
                .ok_or("observer rooting failed")?;
        }
        // The target distance is read from the game's own `PlayerDistance`
        // cache on `EnemyMobActionManagerBase`. It is a cached value, not a
        // measured one, and the observer never recomputes it.
        let distance_class = class("EnemyMobActionManagerBase");
        let distance = if distance_class == 0 {
            0
        } else {
            unsafe { offset(distance_class, "<PlayerDistance>k__BackingField", 12) }.unwrap_or(0)
        };
        Ok(Self {
            battle: ch.pbm as usize,
            battle_class: ch.pbm_class as usize,
            game,
            game_class,
            manager,
            manager_class: unsafe { read(manager, 0) }.unwrap_or(0),
            data_class,
            action_class,
            normal_class,
            current: unsafe { offset(manager_base, "currentSkillData", 18) }?,
            data_action: unsafe { offset(data_class, "<SkillAction>k__BackingField", 18) }?,
            play: unsafe { offset(data_class, "<PlayId>k__BackingField", 8) }?,
            id: unsafe { offset(action_class, "<Id>k__BackingField", 8) }?,
            count: unsafe { offset(action_class, "_attackCount", 8) }?,
            range: unsafe { offset(action_class, "<ActionRange>k__BackingField", 12) }?,
            local_mp: unsafe { offset(game_class, "localMp", 8) }?,
            received_mp: unsafe { offset(game_class, "serverMp", 8) }?,
            distance_class,
            distance,
            _roots: roots,
        })
    }

    unsafe fn sample(&self, battle: usize, action: usize, target_actor: usize) -> Option<Sample> {
        if battle != self.battle
            || !unsafe {
                exact(battle, self.battle_class)
                    && exact(self.game, self.game_class)
                    && exact(self.manager, self.manager_class)
                    && derives(action, self.action_class)
            }
        {
            return None;
        }
        let mut sample = Sample {
            valid: true,
            play: -1,
            normal: unsafe { exact(action, self.normal_class) },
            id_field: unsafe { read(action, self.id) }?,
            attack_count: unsafe { read(action, self.count) }?,
            range_bits: unsafe { read(action, self.range) }?,
            local_mp: unsafe { read(self.game, self.local_mp) }?,
            received_mp: unsafe { read(self.game, self.received_mp) }?,
            phase: PHASE.load(Ordering::Acquire),
            modes: crate::features::active_toggle_mask(),
            calls: NEXT_CALL.load(Ordering::Relaxed),
            ..Sample::default()
        };
        if self.distance != 0
            && target_actor != 0
            && unsafe { derives(target_actor, self.distance_class) }
        {
            if let Some(value) = unsafe { read::<f32>(target_actor, self.distance) } {
                if value.is_finite() {
                    sample.distance = value;
                    sample.distance_source = DISTANCE_GAME_CACHE;
                    sample.hud_metres = hud_metres(value);
                }
            }
        }
        let data = unsafe { read::<usize>(self.manager, self.current) }?;
        if unsafe { exact(data, self.data_class) }
            && unsafe { read::<usize>(data, self.data_action) } == Some(action)
        {
            sample.play = unsafe { read(data, self.play) }?;
            sample.matched_play = true;
        }
        Some(sample)
    }
}

/// HUD metres for a cached distance: `ceil(distance * 0.5)`.
fn hud_metres(distance: f32) -> i32 {
    (distance * 0.5).ceil() as i32
}

/// Advances the operator phase marker. Every later observation sample carries
/// the new phase id, so one capture can be split into named phases.
pub fn set_phase() -> i32 {
    let phase = PHASE.fetch_add(1, Ordering::AcqRel) + 1;
    log_fmt(format_args!("[observe80] phase marker set to {phase}"));
    0
}

/// Current operator phase id.
pub fn phase() -> u32 {
    PHASE.load(Ordering::Acquire)
}

fn ticks() -> i64 {
    let mut value = 0;
    unsafe {
        QueryPerformanceCounter(&mut value);
    }
    value
}

fn capture(mut event: Event) {
    let Ok(config) = CONFIG.try_lock() else {
        DROPPED.fetch_add(1, Ordering::Relaxed);
        return;
    };
    let Some(binding) = config.as_ref() else {
        return;
    };
    if event.battle != binding.battle {
        return;
    }
    event.sample = unsafe { binding.sample(event.battle, event.action, event.target_actor) }
        .unwrap_or_default();
    drop(config);
    event.ticks = ticks();
    event.thread = unsafe { GetCurrentThreadId() };
    let stored = EVENTS
        .try_lock()
        .map(|mut events| events.push(event))
        .unwrap_or(false);
    if !stored {
        DROPPED.fetch_add(1, Ordering::Relaxed);
    }
}

struct Flight;
impl Drop for Flight {
    fn drop(&mut self) {
        FLIGHT.fetch_sub(1, Ordering::SeqCst);
    }
}

fn around<T: Copy>(
    mut event: Event,
    forward: impl FnOnce() -> T,
    result: impl FnOnce(T) -> i8,
) -> T {
    FLIGHT.fetch_add(1, Ordering::SeqCst);
    let _flight = Flight;
    let enabled = ACTIVE.load(Ordering::SeqCst);
    if enabled {
        event.call = NEXT_CALL.fetch_add(1, Ordering::Relaxed) + 1;
        capture(event);
    }
    let value = forward();
    if enabled {
        event.exit = true;
        event.result = result(value);
        capture(event);
    }
    value
}

type RangeFn = unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *const c_void) -> bool;
type StartFn =
    unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *mut c_void, *const c_void) -> bool;
type HitFn =
    unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *mut c_void, *const c_void);

unsafe extern "C" fn range(
    b: *mut c_void,
    t: *mut c_void,
    a: *mut c_void,
    m: *const c_void,
) -> bool {
    let f: RangeFn = unsafe { core::mem::transmute(ORIGINALS[0].load(Ordering::Acquire)) };
    around(
        Event {
            stage: 0,
            battle: b as usize,
            target: t as usize,
            action: a as usize,
            result: -1,
            ..Event::default()
        },
        || unsafe { f(b, t, a, m) },
        |v| v as i8,
    )
}
unsafe extern "C" fn start(
    b: *mut c_void,
    t: *mut c_void,
    ta: *mut c_void,
    a: *mut c_void,
    m: *const c_void,
) -> bool {
    let f: StartFn = unsafe { core::mem::transmute(ORIGINALS[1].load(Ordering::Acquire)) };
    around(
        Event {
            stage: 1,
            battle: b as usize,
            target: t as usize,
            target_actor: ta as usize,
            action: a as usize,
            result: -1,
            ..Event::default()
        },
        || unsafe { f(b, t, ta, a, m) },
        |v| v as i8,
    )
}
unsafe extern "C" fn hit(
    b: *mut c_void,
    t: *mut c_void,
    ta: *mut c_void,
    a: *mut c_void,
    m: *const c_void,
) {
    let f: HitFn = unsafe { core::mem::transmute(ORIGINALS[2].load(Ordering::Acquire)) };
    around(
        Event {
            stage: 2,
            battle: b as usize,
            target: t as usize,
            target_actor: ta as usize,
            action: a as usize,
            result: -1,
            ..Event::default()
        },
        || unsafe { f(b, t, ta, a, m) },
        |_| -1,
    )
}

pub fn installed() -> bool {
    INSTALLED.load(Ordering::Acquire)
}

pub unsafe fn stop() -> bool {
    ACTIVE.store(false, Ordering::SeqCst);
    let Ok(mut hooks) = HOOKS.lock() else {
        return false;
    };
    let mut restored = true;
    for (i, hook) in hooks.iter_mut().enumerate() {
        restored &= unsafe {
            hook.set(
                core::ptr::null_mut(),
                NAMES[i],
                if i == 0 { 2 } else { 3 },
                0,
                false,
                "observe80",
            )
        };
    }
    for _ in 0..2000 {
        if FLIGHT.load(Ordering::SeqCst) == 0 {
            break;
        }
        unsafe {
            Sleep(1);
        }
    }
    if !restored || FLIGHT.load(Ordering::SeqCst) != 0 {
        return false;
    }
    let Ok(mut config) = CONFIG.lock() else {
        return false;
    };
    let Ok(mut buffer) = EVENTS.lock() else {
        return false;
    };
    if INSTALLED.swap(false, Ordering::AcqRel) {
        for e in &buffer.entries[..buffer.len] {
            let s = e.sample;
            log_fmt(format_args!(concat!("[combat-observe] {{\"kind\":\"event\",\"pid\":{},\"call\":{},",
                "\"ticks\":{},\"thread\":{},\"stage\":{},\"exit\":{},\"result\":{},",
                "\"battle\":\"0x{:x}\",\"target\":\"0x{:x}\",\"target_actor\":\"0x{:x}\",\"action\":\"0x{:x}\",",
                "\"valid\":{},\"matched_play\":{},\"normal\":{},\"play\":{},\"id_field\":{},",
                "\"attack_count\":{},\"range_bits\":{},\"local_mp\":{},\"received_mp\":{},",
                "\"phase\":{},\"modes\":{},\"calls\":{},",
                "\"distance\":{},\"distance_source\":{},\"hud_metres\":{}}}"),
                unsafe { GetCurrentProcessId() },e.call,e.ticks,e.thread,e.stage,e.exit,e.result,
                e.battle,e.target,e.target_actor,e.action,s.valid,s.matched_play,s.normal,s.play,
                s.id_field,s.attack_count,s.range_bits,s.local_mp,s.received_mp,
                s.phase,s.modes,s.calls,
                s.distance,s.distance_source,s.hud_metres));
        }
        log_fmt(format_args!("[combat-observe] {{\"kind\":\"end\",\"pid\":{},\"events\":{},\"dropped\":{},\"restored\":true,\"phase\":{}}}",
                             unsafe { GetCurrentProcessId() },buffer.len,DROPPED.load(Ordering::Relaxed),phase()));
    }
    *config = None;
    buffer.len = 0;
    true
}

pub unsafe fn toggle() -> i32 {
    if installed() {
        return if unsafe { stop() } { 0 } else { -9 };
    }
    let mut ch = GameChain::new();
    let mut gc = GcScope::new();
    if !unsafe { ch.resolve(&mut gc) } {
        return -1;
    }
    let binding = match unsafe { Binding::prepare(&ch) } {
        Ok(value) => value,
        Err(reason) => {
            log_fmt(format_args!("[observe80] {}", reason));
            return -8;
        }
    };
    let Ok(mut config) = CONFIG.lock() else {
        return -8;
    };
    *config = Some(binding);
    drop(config);
    let Ok(mut buffer) = EVENTS.lock() else {
        return -8;
    };
    buffer.len = 0;
    drop(buffer);
    DROPPED.store(0, Ordering::Relaxed);
    NEXT_CALL.store(0, Ordering::Relaxed);
    let Ok(mut hooks) = HOOKS.lock() else {
        return -8;
    };
    INSTALLED.store(true, Ordering::Release);
    let detours = [range as usize, start as usize, hit as usize];
    for i in 0..3 {
        let signature = MethodSignature {
            parameters: if i == 0 {
                &["UnityEngine.GameObject", "SkillActionBase"]
            } else {
                &[
                    "UnityEngine.GameObject",
                    "CharacterActionManagerBase",
                    "SkillActionBase",
                ]
            },
            return_type: if i == 2 {
                "System.Void"
            } else {
                "System.Boolean"
            },
        };
        let ok = unsafe {
            hooks[i].set_with_signature(
                ch.pbm_class,
                NAMES[i],
                if i == 0 { 2 } else { 3 },
                detours[i],
                true,
                "observe80",
                Some(&signature),
                Some(&ORIGINALS[i]),
            )
        };
        if !ok
            || hooks[i].vtable_slot.is_null()
            || !fn_readable(ORIGINALS[i].load(Ordering::Acquire))
        {
            drop(hooks);
            unsafe {
                stop();
            }
            return -8;
        }
    }
    let mut frequency = 0;
    unsafe {
        QueryPerformanceFrequency(&mut frequency);
    }
    log_fmt(format_args!("[combat-observe] {{\"kind\":\"begin\",\"version\":1,\"pid\":{},\"ticks\":{},\"frequency\":{},\"capacity\":{}}}",
                         unsafe { GetCurrentProcessId() },ticks(),frequency,CAPACITY));
    log_fmt(format_args!(
        "[combat-observe] {{\"kind\":\"configuration\",\"toggles\":{},\"crit_request\":{},\"skill_cap\":{},\"mp_bonus\":{},\"response_monitor\":{}}}",
        crate::features::active_toggle_mask(),
        crate::features::critical_damage_request(),
        crate::features::skill_adaptive_cap(),
        crate::features::mp_ampr_bonus_request(),
        crate::combat_response::installed()
    ));
    ACTIVE.store(true, Ordering::SeqCst);
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    static CALLBACK_TEST_LOCK: Mutex<()> = Mutex::new(());
    unsafe extern "C" fn range_original(
        b: *mut c_void,
        t: *mut c_void,
        a: *mut c_void,
        m: *const c_void,
    ) -> bool {
        let output = unsafe { &mut *(b as *mut [usize; 5]) };
        *output = [b as usize, t as usize, 0, a as usize, m as usize];
        false
    }
    unsafe extern "C" fn start_original(
        b: *mut c_void,
        t: *mut c_void,
        ta: *mut c_void,
        a: *mut c_void,
        m: *const c_void,
    ) -> bool {
        let output = unsafe { &mut *(b as *mut [usize; 5]) };
        *output = [b as usize, t as usize, ta as usize, a as usize, m as usize];
        true
    }
    unsafe extern "C" fn hit_original(
        b: *mut c_void,
        t: *mut c_void,
        ta: *mut c_void,
        a: *mut c_void,
        m: *const c_void,
    ) {
        let output = unsafe { &mut *(b as *mut [usize; 5]) };
        *output = [b as usize, t as usize, ta as usize, a as usize, m as usize];
    }

    #[test]
    fn observation_sample_carries_phase_modes_and_a_labelled_distance() {
        let _guard = CALLBACK_TEST_LOCK.lock().unwrap();
        // The phase marker advances monotonically and is readable.
        let before = phase();
        assert_eq!(set_phase(), 0);
        assert_eq!(phase(), before + 1);
        assert_eq!(set_phase(), 0);
        assert_eq!(phase(), before + 2);

        // A sample that never resolved a distance reports it as unavailable
        // rather than as a zero distance.
        let sample = Sample::default();
        assert_eq!(sample.distance_source, DISTANCE_UNAVAILABLE);
        assert_eq!(sample.hud_metres, -1);
        assert_eq!(sample.distance, 0.0);
        assert_eq!(sample.phase, 0);
        assert_eq!(sample.modes, 0);
        assert_eq!(sample.calls, 0);

        // HUD metres use the game's own conversion, ceil(distance * 0.5).
        assert_eq!(hud_metres(0.0), 0);
        assert_eq!(hud_metres(1.0), 1);
        assert_eq!(hud_metres(6.0), 3);
        assert_eq!(hud_metres(6.1), 4);
        assert_eq!(hud_metres(11.0), 6);
    }

    #[test]
    fn callback_wrappers_preserve_every_argument_and_native_return() {
        let _guard = CALLBACK_TEST_LOCK.lock().unwrap();
        ORIGINALS[0].store(range_original as usize, Ordering::Release);
        ORIGINALS[1].store(start_original as usize, Ordering::Release);
        ORIGINALS[2].store(hit_original as usize, Ordering::Release);
        let mut output = [0usize; 5];
        let b = output.as_mut_ptr().cast();
        let t = 0x1111usize as *mut c_void;
        let ta = 0x2222usize as *mut c_void;
        let a = 0x3333usize as *mut c_void;
        let m = 0x4444usize as *const c_void;
        assert!(!unsafe { range(b, t, a, m) });
        assert_eq!(output, [b as usize, t as usize, 0, a as usize, m as usize]);
        assert!(unsafe { start(b, t, ta, a, m) });
        assert_eq!(
            output,
            [b as usize, t as usize, ta as usize, a as usize, m as usize]
        );
        output.fill(0);
        unsafe {
            hit(b, t, ta, a, m);
        }
        assert_eq!(
            output,
            [b as usize, t as usize, ta as usize, a as usize, m as usize]
        );
    }
    #[test]
    fn overflow_preserves_earlier_events() {
        let mut buffer = Buffer::new();
        for call in 1..=CAPACITY as u64 {
            assert!(buffer.push(Event {
                call,
                ..Event::default()
            }));
        }
        assert!(!buffer.push(Event {
            call: 999,
            ..Event::default()
        }));
        assert_eq!(
            (buffer.entries[0].call, buffer.entries[CAPACITY - 1].call),
            (1, CAPACITY as u64)
        );
    }
    #[test]
    fn forwarding_preserves_false_true_and_void_results() {
        let _guard = CALLBACK_TEST_LOCK.lock().unwrap();
        let mut calls = 0;
        for expected in [false, true] {
            let actual = around(
                Event::default(),
                || {
                    calls += 1;
                    expected
                },
                |v| v as i8,
            );
            assert_eq!(actual, expected);
        }
        around(
            Event::default(),
            || {
                calls += 1;
            },
            |_| -1,
        );
        assert_eq!(calls, 3);
        assert_eq!(FLIGHT.load(Ordering::SeqCst), 0);
    }
}
