//! Passive, bounded attack-response and calculation diagnostics (command 82).
//! Original callbacks and decoded response objects are never modified.

#![allow(function_casts_as_integer)]

use crate::chain::GameChain;
use crate::hook::MethodHookEntry;
use crate::il2cpp::{api, find_class, get_field, get_method_exact, GcScope, MethodSignature};
use crate::win::{
    fn_readable, log_fmt, ptr_ok, GetCurrentProcessId, GetCurrentThreadId, QueryPerformanceCounter,
    QueryPerformanceFrequency, Sleep,
};
use core::ffi::c_void;
use core::marker::PhantomData;
use core::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Mutex;

const CAPACITY: usize = 256;
const MAX_MOBS: usize = 8;
static ACTIVE: AtomicBool = AtomicBool::new(false);
static INSTALLED: AtomicBool = AtomicBool::new(false);
static FLIGHT: AtomicUsize = AtomicUsize::new(0);
static DROPPED: AtomicU64 = AtomicU64::new(0);
/// Recently seen skill-target unique ids (ring). A miss means a fresh mob, so
/// the adaptive skill cap re-probes instead of camping the floor.
static SEEN_TARGETS: Mutex<[i32; 32]> = Mutex::new([-1; 32]);
static SEEN_CURSOR: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL: AtomicUsize = AtomicUsize::new(0);
static DEFAULT_ORIGINAL: AtomicUsize = AtomicUsize::new(0);
static HIT_ORIGINAL: AtomicUsize = AtomicUsize::new(0);
static NEXT_CALL: AtomicU64 = AtomicU64::new(0);
const HOOK_NAMES: [(&str, i32); 3] = [
    ("OnActionAttack", 5),
    ("OnActionDefaultResponse", 3),
    ("OnSkillActionHit", 3),
];
static HOOKS: Mutex<[MethodHookEntry; 3]> = Mutex::new([MethodHookEntry::new(); 3]);
static CONFIG: Mutex<Option<Binding>> = Mutex::new(None);
static ACTION_CONFIG: Mutex<Option<ActionBinding>> = Mutex::new(None);
static EVENTS: Mutex<Buffer> = Mutex::new(Buffer::new());
static HP_HISTORY: Mutex<HpHistory> = Mutex::new(HpHistory::new());
static HP_HISTORY_GAP: AtomicI64 = AtomicI64::new(0);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CalculationScope {
    Unknown,
    Normal,
    Skill,
}
impl CalculationScope {
    fn label(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Normal => "normal",
            Self::Skill => "skill",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ActionContext {
    scope: CalculationScope,
    call: u64,
    battle: usize,
    target: usize,
    target_actor: usize,
    action: usize,
    action_class: usize,
}

thread_local! {
    static ACTION_CONTEXT: Cell<Option<ActionContext>> = const { Cell::new(None) };
}

struct ActionScope {
    previous: Option<Option<ActionContext>>,
    _not_send: PhantomData<Rc<()>>,
}
impl ActionScope {
    fn enter(context: Option<ActionContext>) -> Self {
        Self {
            previous: ACTION_CONTEXT.try_with(|slot| slot.replace(context)).ok(),
            _not_send: PhantomData,
        }
    }
}
impl Drop for ActionScope {
    fn drop(&mut self) {
        if let Some(previous) = self.previous {
            let _ = ACTION_CONTEXT.try_with(|slot| slot.set(previous));
        }
    }
}

fn current_context() -> Option<ActionContext> {
    ACTION_CONTEXT.try_with(Cell::get).ok().flatten()
}

fn calculation_scope(normal: bool, context: Option<ActionContext>) -> CalculationScope {
    if normal {
        CalculationScope::Normal
    } else {
        context.map_or(CalculationScope::Unknown, |context| context.scope)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ResourceSample {
    valid: bool,
    local_mp: i32,
    received_mp: i32,
    local_exmp: i32,
    received_exmp: i32,
}
impl ResourceSample {
    const EMPTY: Self = Self {
        valid: false,
        local_mp: -1,
        received_mp: -1,
        local_exmp: -1,
        received_exmp: -1,
    };
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Mob {
    valid: bool,
    mob_id: i32,
    local_id: u8,
    unique_id: i32,
    hp: i32,
    state: i32,
    code: u8,
    allowed: i32,
    violation: i32,
    previous_hp: i32,
    normal_exp: u8,
    physical_exp: u8,
    magic_exp: u8,
}
impl Mob {
    const EMPTY: Self = Self {
        valid: false,
        mob_id: -1,
        local_id: 0,
        unique_id: -1,
        hp: -1,
        state: -1,
        code: 0,
        allowed: -1,
        violation: -1,
        previous_hp: -1,
        normal_exp: 0,
        physical_exp: 0,
        magic_exp: 0,
    };
}

#[derive(Clone, Copy)]
struct HpEntry {
    identity: (i32, u8, i32),
    hp: i32,
    ticks: i64,
}

struct HpHistory {
    entries: [Option<HpEntry>; 128],
    cursor: usize,
    gap_ticks: i64,
}

impl HpHistory {
    const fn new() -> Self {
        Self {
            entries: [None; 128],
            cursor: 0,
            gap_ticks: 0,
        }
    }

    fn observe(&mut self, event: &mut Event) {
        if !event.valid || event.code != 0 {
            return;
        }
        for mob in event.mobs.iter_mut().take(event.captured) {
            mob.previous_hp = -1;
            if event.ticks <= self.gap_ticks {
                continue;
            }
            if !mob.valid || mob.unique_id < 0 || mob.hp < 0 {
                continue;
            }
            let identity = (mob.mob_id, mob.local_id, mob.unique_id);
            let slot = self
                .entries
                .iter()
                .position(|entry| entry.is_some_and(|entry| entry.identity == identity));
            let index = if let Some(index) = slot {
                let prior = self.entries[index].unwrap();
                // QPC ties do not establish callback order. A foreign tie must
                // invalidate the baseline; a local tie cannot earn feedback.
                if event.ticks < prior.ticks || (event.local_response && event.ticks == prior.ticks)
                {
                    continue;
                }
                if event.local_response {
                    mob.previous_hp = prior.hp;
                }
                index
            } else {
                if !event.local_response {
                    continue;
                }
                let index = self.cursor;
                self.cursor = (self.cursor + 1) % self.entries.len();
                index
            };
            self.entries[index] = Some(HpEntry {
                identity,
                // Another actor can change the same mob's HP. Retain the newer
                // timestamp, but require a fresh local baseline before feedback.
                hp: if event.local_response { mob.hp } else { -1 },
                ticks: event.ticks,
            });
        }
    }
}

fn observe_hp(history: &Mutex<HpHistory>, gap: &AtomicI64, event: &mut Event) {
    for mob in event.mobs.iter_mut().take(event.captured) {
        mob.previous_hp = -1;
    }
    if let Ok(mut history) = history.try_lock() {
        let gap_ticks = gap.swap(0, Ordering::AcqRel);
        if gap_ticks > 0 {
            history.gap_ticks = history.gap_ticks.max(gap_ticks);
            for entry in history.entries.iter_mut().flatten() {
                entry.hp = -1;
            }
        }
        history.observe(event);
    } else {
        // A skipped response may have changed any tracked target's HP.
        gap.fetch_max(event.ticks, Ordering::AcqRel);
    }
}

#[derive(Clone, Copy)]
struct Event {
    kind: u8,
    ticks: i64,
    thread: u32,
    request: i32,
    native: i32,
    effective: i32,
    code: i32,
    action_code: i32,
    actor_type: u8,
    actor_id: i32,
    local_response: bool,
    response: usize,
    valid: bool,
    skill: i16,
    damage_id: u8,
    response_local_id: u8,
    response_attack_count: u8,
    result_flags: u8,
    response_mp_valid: bool,
    response_mp: i16,
    response_exmp: i16,
    scope: CalculationScope,
    context: Option<ActionContext>,
    exit: bool,
    resources: ResourceSample,
    mob_count: usize,
    captured: usize,
    mobs: [Mob; MAX_MOBS],
}
impl Event {
    const EMPTY: Self = Self {
        kind: 0,
        ticks: 0,
        thread: 0,
        request: 0,
        native: -1,
        effective: -1,
        code: -1,
        action_code: -1,
        actor_type: 0,
        actor_id: -1,
        local_response: false,
        response: 0,
        valid: false,
        skill: -1,
        damage_id: 0,
        response_local_id: 0,
        response_attack_count: 0,
        result_flags: 0,
        response_mp_valid: false,
        response_mp: -1,
        response_exmp: -1,
        scope: CalculationScope::Unknown,
        context: None,
        exit: false,
        resources: ResourceSample::EMPTY,
        mob_count: 0,
        captured: 0,
        mobs: [Mob::EMPTY; MAX_MOBS],
    };
}

struct Buffer {
    entries: [Event; CAPACITY],
    len: usize,
}
impl Buffer {
    const fn new() -> Self {
        Self {
            entries: [Event::EMPTY; CAPACITY],
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

#[derive(Clone, Copy)]
struct Binding {
    local_actor: Option<(u8, i32)>,
    response_class: usize,
    mob_class: usize,
    player_status_class: usize,
    player_status: usize,
    response_mp: usize,
    response_exmp: usize,
    skill: usize,
    damage_id: usize,
    response_local_id: usize,
    response_attack_count: usize,
    flags: usize,
    mob_list: usize,
    mob_id: usize,
    local_id: usize,
    unique_id: usize,
    hp: usize,
    state: usize,
    code: usize,
    allowed: usize,
    violation: usize,
    normal_exp: usize,
    physical_exp: usize,
    magic_exp: usize,
}

fn is_local_response(local: Option<(u8, i32)>, actor_type: u8, actor_id: i32) -> bool {
    local.is_some_and(|identity| identity.1 > 0 && identity == (actor_type, actor_id))
}

unsafe fn identity_value<T: Copy>(
    ch: &GameChain,
    name: &str,
    return_type: &'static str,
) -> Option<T> {
    let a = api()?;
    let method = unsafe {
        get_method_exact(
            ch.pdm_class,
            name,
            &MethodSignature {
                parameters: &[],
                return_type,
            },
        )
    };
    if method.is_null() || ch.pdm.is_null() {
        return None;
    }
    let mut exception = core::ptr::null_mut();
    let boxed =
        unsafe { (a.runtime_invoke)(method, ch.pdm, core::ptr::null_mut(), &mut exception) };
    if !exception.is_null() || boxed.is_null() {
        return None;
    }
    let value = unsafe { (a.object_unbox)(boxed) };
    unsafe { read(value as usize, 0) }
}

unsafe fn read<T: Copy>(object: usize, offset: usize) -> Option<T> {
    let address = object.checked_add(offset)?;
    if !ptr_ok(address, core::mem::size_of::<T>()) {
        return None;
    }
    Some(unsafe { core::ptr::read_unaligned(address as *const T) })
}

unsafe fn offset(class: usize, name: &str, kind: u32) -> Result<usize, &'static str> {
    let a = api().ok_or("IL2CPP API unavailable")?;
    let field = unsafe { get_field(class as *mut c_void, name) } as usize;
    let ty = unsafe { read::<usize>(field, 8) }.ok_or("response field missing")?;
    let bits = unsafe { read::<u32>(ty, 8) }.ok_or("response field type missing")?;
    if bits & 0x10 != 0 || (bits >> 16) & 0xff != kind {
        log_fmt(format_args!("[response82] field type mismatch: {}", name));
        return Err("response field type mismatch");
    }
    let value = unsafe { (a.field_get_offset)(field as *mut c_void) };
    if !(0x10..=0x4000).contains(&value) {
        return Err("response field offset invalid");
    }
    Ok(value as usize)
}

impl Binding {
    unsafe fn prepare(ch: &GameChain) -> Result<Self, &'static str> {
        let response = unsafe {
            find_class(
                ch.image,
                ch.domain,
                "Toram.Common.Actions",
                "AttackResponseData",
            )
        } as usize;
        let mob = unsafe {
            find_class(
                ch.image,
                ch.domain,
                "Toram.Common.Actions",
                "MobResponseData",
            )
        } as usize;
        let player_status = unsafe {
            find_class(
                ch.image,
                ch.domain,
                "Toram.Common.Actions",
                "PlayerStatusData",
            )
        } as usize;
        if response == 0 || mob == 0 || player_status == 0 {
            return Err("response classes unavailable");
        }
        let actor_type =
            unsafe { identity_value::<u8>(ch, "get_PlayerArchetypeType", "System.Byte") }
                .ok_or("local response actor type unavailable")?;
        let actor_id =
            unsafe { identity_value::<i32>(ch, "get_PlayerArchetypeId", "System.Int32") }
                .filter(|id| *id > 0)
                .ok_or("local response actor id unavailable")?;
        Ok(Self {
            local_actor: Some((actor_type, actor_id)),
            response_class: response,
            mob_class: mob,
            player_status_class: player_status,
            player_status: unsafe { offset(response, "<PlayerStatus>k__BackingField", 18) }?,
            response_mp: unsafe { offset(player_status, "<Mp>k__BackingField", 6) }?,
            response_exmp: unsafe { offset(player_status, "<ExMp>k__BackingField", 6) }?,
            skill: unsafe { offset(response, "<SkillId>k__BackingField", 6) }?,
            damage_id: unsafe { offset(response, "<DamageId>k__BackingField", 5) }?,
            response_local_id: unsafe { offset(response, "<LocalId>k__BackingField", 5) }?,
            response_attack_count: unsafe { offset(response, "<AttackCount>k__BackingField", 5) }?,
            flags: unsafe { offset(response, "<AttackResultFlag>k__BackingField", 5) }?,
            mob_list: unsafe { offset(response, "<MobList>k__BackingField", 29) }?,
            mob_id: unsafe { offset(mob, "<MobId>k__BackingField", 8) }?,
            local_id: unsafe { offset(mob, "<LocalId>k__BackingField", 5) }?,
            unique_id: unsafe { offset(mob, "<UniqueId>k__BackingField", 8) }?,
            hp: unsafe { offset(mob, "<Hp>k__BackingField", 8) }?,
            state: unsafe { offset(mob, "<State>k__BackingField", 8) }?,
            code: unsafe { offset(mob, "<ReturnCode>k__BackingField", 5) }?,
            allowed: unsafe { offset(mob, "<AllowDamage>k__BackingField", 8) }?,
            violation: unsafe { offset(mob, "<ViolationDamage>k__BackingField", 8) }?,
            normal_exp: unsafe { offset(mob, "<NormalDefExp>k__BackingField", 5) }?,
            physical_exp: unsafe { offset(mob, "<PhysicsDefExp>k__BackingField", 5) }?,
            magic_exp: unsafe { offset(mob, "<MagicDefExp>k__BackingField", 5) }?,
        })
    }

    unsafe fn mob(&self, object: usize) -> Option<Mob> {
        if unsafe { read::<usize>(object, 0) } != Some(self.mob_class) {
            return None;
        }
        Some(Mob {
            valid: true,
            mob_id: unsafe { read(object, self.mob_id) }?,
            local_id: unsafe { read(object, self.local_id) }?,
            unique_id: unsafe { read(object, self.unique_id) }?,
            hp: unsafe { read(object, self.hp) }?,
            state: unsafe { read(object, self.state) }?,
            code: unsafe { read(object, self.code) }?,
            allowed: unsafe { read(object, self.allowed) }?,
            violation: unsafe { read(object, self.violation) }?,
            previous_hp: -1,
            normal_exp: unsafe { read(object, self.normal_exp) }?,
            physical_exp: unsafe { read(object, self.physical_exp) }?,
            magic_exp: unsafe { read(object, self.magic_exp) }?,
        })
    }

    unsafe fn response(&self, object: usize, mut event: Event) -> Event {
        event.response = object;
        event.local_response =
            is_local_response(self.local_actor, event.actor_type, event.actor_id);
        if unsafe { read::<usize>(object, 0) } != Some(self.response_class) {
            return event;
        }
        let parsed = (|| -> Option<()> {
            event.skill = unsafe { read(object, self.skill) }?;
            event.damage_id = unsafe { read(object, self.damage_id) }?;
            event.response_local_id = unsafe { read(object, self.response_local_id) }?;
            event.response_attack_count = unsafe { read(object, self.response_attack_count) }?;
            event.result_flags = unsafe { read(object, self.flags) }?;
            // These are signed Int16 fields in PlayerStatusData; resolve offsets
            // from metadata because the saved native build differs from the dump.
            if let Some(status) = unsafe { read::<usize>(object, self.player_status) } {
                if self.player_status_class != 0
                    && unsafe { read::<usize>(status, 0) } == Some(self.player_status_class)
                {
                    if let (Some(mp), Some(exmp)) =
                        (unsafe { read(status, self.response_mp) }, unsafe {
                            read(status, self.response_exmp)
                        })
                    {
                        event.response_mp = mp;
                        event.response_exmp = exmp;
                        event.response_mp_valid = true;
                    }
                }
            }
            let list = unsafe { read::<usize>(object, self.mob_list) }?;
            if list == 0 {
                return Some(());
            }
            let count = unsafe { read::<usize>(list, 0x18) }?;
            if count > 4096 {
                return None;
            }
            event.mob_count = count;
            for i in 0..count.min(MAX_MOBS) {
                let mob = unsafe { read::<usize>(list, 0x20 + i * 8) }?;
                event.mobs[i] = unsafe { self.mob(mob) }.unwrap_or(Mob::EMPTY);
                event.captured += 1;
            }
            Some(())
        })();
        event.valid = parsed.is_some();
        event
    }
}

struct ActionBinding {
    actor: usize,
    actor_class: usize,
    actor_status: usize,
    actor_battle: usize,
    battle: usize,
    battle_class: usize,
    status: usize,
    status_class: usize,
    game: usize,
    game_class: usize,
    status_game: usize,
    action_base: usize,
    normal_class: usize,
    local_mp: usize,
    received_mp: usize,
    local_exmp: usize,
    received_exmp: usize,
    _roots: GcScope,
}

fn classify_action(
    mut class: usize,
    action_base: usize,
    normal_class: usize,
    mut parent: impl FnMut(usize) -> Option<usize>,
) -> CalculationScope {
    let mut normal = false;
    for _ in 0..32 {
        if class == 0 || action_base == 0 || normal_class == 0 {
            break;
        }
        normal |= class == normal_class;
        if class == action_base {
            return if normal {
                CalculationScope::Normal
            } else {
                CalculationScope::Skill
            };
        }
        let Some(next) = parent(class) else {
            break;
        };
        class = next;
    }
    CalculationScope::Unknown
}

unsafe fn exact(object: usize, class: usize) -> bool {
    class != 0 && unsafe { read::<usize>(object, 0) } == Some(class)
}

impl ActionBinding {
    unsafe fn prepare(ch: &GameChain) -> Result<Self, &'static str> {
        let class = |name: &str| unsafe { find_class(ch.image, ch.domain, "", name) } as usize;
        let game_class = class("PlayerGameStatus");
        let action_base = class("SkillActionBase");
        let normal_class = class("NormalAttackAction");
        if game_class == 0 || action_base == 0 || normal_class == 0 {
            return Err("action observation classes unavailable");
        }
        if api().and_then(|a| a.class_get_parent).is_none() {
            return Err("action observation class ancestry unavailable");
        }
        let actor_status = unsafe { offset(ch.pam_class as usize, "_playerStatus", 18) }?;
        let actor_battle = unsafe { offset(ch.pam_class as usize, "_playerBattleManager", 18) }?;
        let status_game = unsafe { offset(ch.status_class as usize, "gameStatus", 18) }?;
        let game = unsafe { read::<usize>(ch.status as usize, status_game) }
            .ok_or("action observation game status unavailable")?;
        let identity_checks = [
            ("pam-exact", unsafe {
                exact(ch.pam as usize, ch.pam_class as usize)
            }),
            (
                "pam-status",
                unsafe { read::<usize>(ch.pam as usize, actor_status) } == Some(ch.status as usize),
            ),
            (
                "pam-battle",
                unsafe { read::<usize>(ch.pam as usize, actor_battle) } == Some(ch.pbm as usize),
            ),
            ("pbm-exact", unsafe {
                exact(ch.pbm as usize, ch.pbm_class as usize)
            }),
            ("status-exact", unsafe {
                exact(ch.status as usize, ch.status_class as usize)
            }),
            ("game-exact", unsafe { exact(game, game_class) }),
        ];
        if identity_checks.iter().any(|(_, passed)| !passed) {
            for (name, passed) in identity_checks.iter() {
                if !passed {
                    log_fmt(format_args!("[response82] identity check failed: {}", name));
                }
            }
            log_fmt(format_args!(
                "[response82] identity detail pam=0x{:x} status-field=0x{:x} status=0x{:x} battle-field=0x{:x} battle=0x{:x} game=0x{:x}",
                ch.pam as usize,
                unsafe { read::<usize>(ch.pam as usize, actor_status) }.unwrap_or(0),
                ch.status as usize,
                unsafe { read::<usize>(ch.pam as usize, actor_battle) }.unwrap_or(0),
                ch.pbm as usize,
                game,
            ));
            return Err("action observation local identity mismatch");
        }
        let mut roots = GcScope::new();
        for object in [ch.pam as usize, ch.pbm as usize, ch.status as usize, game] {
            roots
                .try_pin(object as *mut c_void)
                .ok_or("action observation rooting failed")?;
        }
        Ok(Self {
            actor: ch.pam as usize,
            actor_class: ch.pam_class as usize,
            actor_status,
            actor_battle,
            battle: ch.pbm as usize,
            battle_class: ch.pbm_class as usize,
            status: ch.status as usize,
            status_class: ch.status_class as usize,
            game,
            game_class,
            status_game,
            action_base,
            normal_class,
            local_mp: unsafe { offset(game_class, "localMp", 8) }?,
            received_mp: unsafe { offset(game_class, "serverMp", 8) }?,
            local_exmp: unsafe { offset(game_class, "localExMp", 8) }?,
            received_exmp: unsafe { offset(game_class, "serverExMp", 8) }?,
            _roots: roots,
        })
    }

    unsafe fn resources(&self, battle: usize) -> Option<ResourceSample> {
        if battle != self.battle
            || !unsafe {
                exact(self.actor, self.actor_class)
                    && read::<usize>(self.actor, self.actor_status) == Some(self.status)
                    && read::<usize>(self.actor, self.actor_battle) == Some(self.battle)
                    && exact(battle, self.battle_class)
                    && exact(self.status, self.status_class)
                    && exact(self.game, self.game_class)
            }
            || unsafe { read::<usize>(self.status, self.status_game) } != Some(self.game)
        {
            return None;
        }
        Some(ResourceSample {
            valid: true,
            local_mp: unsafe { read(self.game, self.local_mp) }?,
            received_mp: unsafe { read(self.game, self.received_mp) }?,
            local_exmp: unsafe { read(self.game, self.local_exmp) }?,
            received_exmp: unsafe { read(self.game, self.received_exmp) }?,
        })
    }

    unsafe fn context(
        &self,
        battle: usize,
        target: usize,
        target_actor: usize,
        action: usize,
    ) -> Option<ActionContext> {
        unsafe { self.resources(battle) }?;
        let class = unsafe { read::<usize>(action, 0) }?;
        let parent = api()?.class_get_parent?;
        let scope = classify_action(class, self.action_base, self.normal_class, |class| {
            if !ptr_ok(class, 0x138) {
                return None;
            }
            Some(unsafe { parent(class as *mut c_void) } as usize)
        });
        if scope == CalculationScope::Unknown {
            return None;
        }
        Some(ActionContext {
            scope,
            call: NEXT_CALL.fetch_add(1, Ordering::Relaxed) + 1,
            battle,
            target,
            target_actor,
            action,
            action_class: class,
        })
    }
}

fn local_resources(battle: usize) -> ResourceSample {
    let Ok(config) = ACTION_CONFIG.try_lock() else {
        DROPPED.fetch_add(1, Ordering::Relaxed);
        return ResourceSample::EMPTY;
    };
    config
        .as_ref()
        .and_then(|binding| unsafe { binding.resources(battle) })
        .unwrap_or(ResourceSample::EMPTY)
}

fn around_action<T>(
    context: Option<ActionContext>,
    mut sample: impl FnMut() -> ResourceSample,
    forward: impl FnOnce() -> T,
    mut record: impl FnMut(Event),
) -> T {
    // A foreign nested callback clears its parent's context until it returns.
    let _scope = ActionScope::enter(context);
    let snapshot = |exit, resources| Event {
        context,
        scope: context.map_or(CalculationScope::Unknown, |context| context.scope),
        exit,
        resources,
        valid: true,
        ..event(4)
    };
    if context.is_some() {
        record(snapshot(false, sample()));
    }
    let result = forward();
    if context.is_some() {
        record(snapshot(true, sample()));
    }
    result
}

struct Flight;
impl Flight {
    fn enter() -> Self {
        FLIGHT.fetch_add(1, Ordering::SeqCst);
        Self
    }
}
impl Drop for Flight {
    fn drop(&mut self) {
        FLIGHT.fetch_sub(1, Ordering::SeqCst);
    }
}

fn event(kind: u8) -> Event {
    let mut ticks = 0;
    unsafe {
        QueryPerformanceCounter(&mut ticks);
    }
    Event {
        kind,
        ticks,
        thread: unsafe { GetCurrentThreadId() },
        request: crate::features::critical_damage_request(),
        ..Event::EMPTY
    }
}

fn capture(event: Event) {
    if !ACTIVE.load(Ordering::Acquire) {
        return;
    }
    let recorded = EVENTS
        .try_lock()
        .map(|mut buffer| buffer.push(event))
        .unwrap_or(false);
    if !recorded {
        DROPPED.fetch_add(1, Ordering::Relaxed);
    }
}

/// Positive evidence requires a received HP decrease for every captured target.
/// Zero violation and nonzero AllowDamage alone do not prove damage: the live
/// Hard Hit capture contained both fields with unchanged HP. A decrease is an
/// observation between responses, not proof that the full requested hit landed.
fn response_signal(event: &Event) -> Option<bool> {
    if !event.local_response || !event.valid || event.code != 0 || event.captured == 0 {
        return None;
    }
    let mut decreased = event.captured == event.mob_count && event.captured <= MAX_MOBS;
    for mob in event.mobs.iter().take(event.captured) {
        if !mob.valid {
            decreased = false;
            continue;
        }
        if mob.violation != 0 || (mob.allowed == 0 && mob.hp > 0) {
            return Some(false);
        }
        decreased &= mob.code == 0 && mob.previous_hp > mob.hp && mob.hp >= 0;
    }
    decreased.then_some(true)
}

/// Fresh-target probe: remember unique ids, and when an unseen mob appears let
/// the adaptive skill cap re-probe the fresh envelope (200) instead of camping
/// the rejection floor for the rest of the farm.
fn track_skill_targets(event: &Event) {
    let Ok(mut seen) = SEEN_TARGETS.try_lock() else {
        return;
    };
    for mob in event.mobs[..event.captured].iter() {
        if !mob.valid || mob.unique_id < 0 {
            continue;
        }
        if seen.contains(&mob.unique_id) {
            continue;
        }
        let slot = SEEN_CURSOR.fetch_add(1, Ordering::Relaxed) % seen.len();
        seen[slot] = mob.unique_id;
        crate::features::note_new_skill_target();
    }
}

/// Feed skill observations to the experimental cap; unknowns break its streak.
/// Uses try_lock on the game thread rather than waiting for diagnostic locks.
fn feed_adaptive_skill_cap(event: &Event) {
    if !event.local_response || !event.valid || event.captured == 0 || event.skill <= 0 {
        return;
    }
    let signal = response_signal(event);
    if signal.is_some() {
        track_skill_targets(event);
    }
    apply_skill_feedback(event.skill, signal);
}

pub(crate) fn apply_skill_feedback(skill: i16, signal: Option<bool>) {
    if skill <= 0 {
        return;
    }
    match signal {
        Some(true) => crate::features::note_skill_response_accepted(),
        Some(false) => crate::features::note_skill_response_rejected(),
        None => crate::features::note_skill_response_unknown(),
    }
}

fn publish_response(event: &Event) {
    if !event.local_response {
        return;
    }
    if let Some(shm) = protocol::telemetry() {
        let signal = response_signal(event);
        let last = event.mobs[..event.captured].iter().find(|mob| mob.valid);
        shm.update(|data| {
            match signal {
                Some(true) => {
                    data.responses_accepted.fetch_add(1, Ordering::SeqCst);
                }
                Some(false) => {
                    data.responses_rejected.fetch_add(1, Ordering::SeqCst);
                }
                None => {
                    data.responses_unknown.fetch_add(1, Ordering::SeqCst);
                }
            }
            data.skill_cap
                .store(crate::features::skill_adaptive_cap(), Ordering::SeqCst);
            if let Some(mob) = last {
                data.last_received_hp.store(mob.hp, Ordering::SeqCst);
                data.last_violation.store(mob.violation, Ordering::SeqCst);
            }
        });
    }
}

pub fn reset_metrics() {
    crate::features::reset_mp_diagnostics();
    if let Some(shm) = protocol::telemetry() {
        shm.update(|data| {
            data.responses_accepted.store(0, Ordering::SeqCst);
            data.responses_rejected.store(0, Ordering::SeqCst);
            data.responses_unknown.store(0, Ordering::SeqCst);
            data.last_received_hp.store(-1, Ordering::SeqCst);
            data.last_violation.store(0, Ordering::SeqCst);
        });
    }
}

fn publish_monitor(enabled: bool) {
    if let Some(shm) = protocol::telemetry() {
        shm.update(|data| data.combat_monitor.store(enabled as u32, Ordering::SeqCst));
    }
}

pub unsafe fn ensure_enabled(enabled: bool) -> i32 {
    if !enabled {
        return if unsafe { stop() } { 0 } else { -9 };
    }
    if installed() {
        return if ACTIVE.load(Ordering::Acquire) {
            0
        } else {
            -9
        };
    }
    unsafe { toggle() }
}

pub fn note_critical_damage(request: i32, native: i32, effective: i32) {
    if !ACTIVE.load(Ordering::Acquire) {
        return;
    }
    let _flight = Flight::enter();
    let context = current_context();
    capture(Event {
        request,
        native,
        effective,
        scope: calculation_scope(crate::critical_path::in_normal_attack(), context),
        context,
        valid: true,
        ..event(1)
    });
}

type AttackFn =
    unsafe extern "C" fn(*mut c_void, *mut c_void, i32, u8, i32, *mut c_void, *const c_void);

unsafe extern "C" fn attack(
    this: *mut c_void,
    game: *mut c_void,
    code: i32,
    actor_type: u8,
    actor_id: i32,
    response: *mut c_void,
    method: *const c_void,
) {
    let _flight = Flight::enter();
    let original = ORIGINAL.load(Ordering::Acquire);
    if ACTIVE.load(Ordering::Acquire) {
        let entry = Event {
            code,
            actor_type,
            actor_id,
            ..event(2)
        };
        let binding = CONFIG.try_lock().ok().and_then(|guard| *guard);
        if let Some(binding) = binding {
            let mut parsed = unsafe { binding.response(response as usize, entry) };
            observe_hp(&HP_HISTORY, &HP_HISTORY_GAP, &mut parsed);
            capture(parsed);
            feed_adaptive_skill_cap(&parsed);
            publish_response(&parsed);
        } else {
            HP_HISTORY_GAP.fetch_max(entry.ticks, Ordering::AcqRel);
            DROPPED.fetch_add(1, Ordering::Relaxed);
        }
    }
    if fn_readable(original) {
        let forward: AttackFn = unsafe { core::mem::transmute(original) };
        unsafe {
            forward(this, game, code, actor_type, actor_id, response, method);
        }
    }
}

pub fn installed() -> bool {
    INSTALLED.load(Ordering::Acquire)
}

type DefaultFn = unsafe extern "C" fn(*mut c_void, *mut c_void, i32, i32, *const c_void);

unsafe extern "C" fn default_response(
    this: *mut c_void,
    game: *mut c_void,
    action_code: i32,
    code: i32,
    method: *const c_void,
) {
    let _flight = Flight::enter();
    let original = DEFAULT_ORIGINAL.load(Ordering::Acquire);
    if ACTIVE.load(Ordering::Acquire) {
        capture(Event {
            code,
            action_code,
            valid: true,
            ..event(3)
        });
    }
    if fn_readable(original) {
        let forward: DefaultFn = unsafe { core::mem::transmute(original) };
        unsafe {
            forward(this, game, action_code, code, method);
        }
    }
}

type HitFn =
    unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *mut c_void, *const c_void);

unsafe extern "C" fn action_hit(
    this: *mut c_void,
    target: *mut c_void,
    target_actor: *mut c_void,
    action: *mut c_void,
    method: *const c_void,
) {
    let _flight = Flight::enter();
    let original = HIT_ORIGINAL.load(Ordering::Acquire);
    let context = if ACTIVE.load(Ordering::Acquire) {
        match ACTION_CONFIG.try_lock() {
            Ok(config) => config.as_ref().and_then(|binding| unsafe {
                binding.context(
                    this as usize,
                    target as usize,
                    target_actor as usize,
                    action as usize,
                )
            }),
            Err(_) => {
                DROPPED.fetch_add(1, Ordering::Relaxed);
                None
            }
        }
    } else {
        None
    };
    around_action(
        context,
        || local_resources(this as usize),
        || {
            if fn_readable(original) {
                let forward: HitFn = unsafe { core::mem::transmute(original) };
                unsafe { forward(this, target, target_actor, action, method) };
            }
        },
        capture,
    );
}

pub unsafe fn stop() -> bool {
    ACTIVE.store(false, Ordering::SeqCst);
    crate::features::set_adaptive_ladder(false);
    publish_monitor(false);
    let Ok(mut hooks) = HOOKS.lock() else {
        return false;
    };
    let mut restored = true;
    for (index, hook) in hooks.iter_mut().enumerate().rev() {
        restored &= unsafe {
            hook.set(
                core::ptr::null_mut(),
                HOOK_NAMES[index].0,
                HOOK_NAMES[index].1,
                0,
                false,
                "response82",
            )
        };
    }
    if !restored {
        return false;
    }
    drop(hooks);
    for _ in 0..2000 {
        if FLIGHT.load(Ordering::SeqCst) == 0 {
            break;
        }
        unsafe {
            Sleep(1);
        }
    }
    if FLIGHT.load(Ordering::SeqCst) != 0 {
        return false;
    }
    let Ok(mut config) = CONFIG.lock() else {
        return false;
    };
    let Ok(mut action_config) = ACTION_CONFIG.lock() else {
        return false;
    };
    let Ok(mut buffer) = EVENTS.lock() else {
        return false;
    };
    if INSTALLED.swap(false, Ordering::AcqRel) {
        for (index, e) in buffer.entries[..buffer.len].iter().enumerate() {
            log_fmt(format_args!(concat!("[combat-response] {{\"kind\":\"event\",\"pid\":{},\"event\":{},",
                "\"stage\":{},\"ticks\":{},\"thread\":{},\"request\":{},\"native\":{},\"effective\":{},",
                "\"game_code\":{},\"action_code\":{},\"actor_type\":{},\"actor_id\":{},\"response\":\"0x{:x}\",\"valid\":{},\"local_response\":{},",
                "\"skill\":{},\"damage_id\":{},\"response_local_id\":{},\"response_attack_count\":{},\"result_flags\":{},\"mob_count\":{},\"captured\":{},",
                "\"response_mp_valid\":{},\"response_mp\":{},\"response_exmp\":{},",
                "\"calculation_scope\":\"{}\",\"local_call\":{},\"network_correlated\":false,",
                "\"action_exit\":{},\"battle\":\"0x{:x}\",\"target\":\"0x{:x}\",\"target_actor\":\"0x{:x}\",",
                "\"action\":\"0x{:x}\",\"action_class\":\"0x{:x}\",",
                "\"resources_valid\":{},\"local_mp\":{},\"received_mp\":{},\"local_exmp\":{},\"received_exmp\":{}}}"),
                unsafe { GetCurrentProcessId() }, index, e.kind, e.ticks, e.thread, e.request,
                e.native, e.effective, e.code, e.action_code, e.actor_type, e.actor_id, e.response, e.valid, e.local_response,
                e.skill, e.damage_id, e.response_local_id, e.response_attack_count, e.result_flags, e.mob_count, e.captured,
                e.response_mp_valid, e.response_mp, e.response_exmp, e.scope.label(),
                e.context.map_or(0, |context| context.call), e.exit,
                e.context.map_or(0, |context| context.battle),
                e.context.map_or(0, |context| context.target),
                e.context.map_or(0, |context| context.target_actor),
                e.context.map_or(0, |context| context.action),
                e.context.map_or(0, |context| context.action_class),
                e.resources.valid, e.resources.local_mp, e.resources.received_mp,
                e.resources.local_exmp, e.resources.received_exmp));
            for (mob_index, m) in e.mobs[..e.captured].iter().enumerate() {
                log_fmt(format_args!(
                    concat!(
                        "[combat-response] {{\"kind\":\"mob\",\"event\":{},\"index\":{},",
                        "\"valid\":{},\"mob_id\":{},\"local_id\":{},\"unique_id\":{},\"hp\":{},",
                        "\"state\":{},\"code\":{},\"allowed\":{},\"violation\":{},",
                        "\"previous_hp\":{},\"normal_exp\":{},\"physical_exp\":{},\"magic_exp\":{}}}"
                    ),
                    index,
                    mob_index,
                    m.valid,
                    m.mob_id,
                    m.local_id,
                    m.unique_id,
                    m.hp,
                    m.state,
                    m.code,
                    m.allowed,
                    m.violation,
                    m.previous_hp,
                    m.normal_exp,
                    m.physical_exp,
                    m.magic_exp
                ));
            }
        }
        log_fmt(format_args!("[combat-response] {{\"kind\":\"end\",\"pid\":{},\"events\":{},\"dropped\":{},\"restored\":true}}",
            unsafe { GetCurrentProcessId() }, buffer.len, DROPPED.load(Ordering::Relaxed)));
    }
    *config = None;
    *action_config = None;
    buffer.len = 0;
    true
}

pub unsafe fn toggle() -> i32 {
    if installed() {
        return if unsafe { stop() } { 0 } else { -9 };
    }
    if crate::combat_observe::installed() {
        log_fmt(format_args!(
            "[response82] stop observer80 before installing the shared action-hit observation"
        ));
        return -8;
    }
    let mut ch = GameChain::new();
    let mut gc = GcScope::new();
    if !unsafe { ch.resolve(&mut gc) } {
        return -1;
    }
    let binding = match unsafe { Binding::prepare(&ch) } {
        Ok(binding) => binding,
        Err(reason) => {
            log_fmt(format_args!("[response82] {}", reason));
            return -8;
        }
    };
    let action_binding = match unsafe { ActionBinding::prepare(&ch) } {
        Ok(binding) => binding,
        Err(reason) => {
            log_fmt(format_args!("[response82] {}", reason));
            return -8;
        }
    };
    let listener = unsafe { find_class(ch.image, ch.domain, "", "PhotonListener") };
    if listener.is_null() {
        return -8;
    }
    let Ok(mut config) = CONFIG.lock() else {
        return -8;
    };
    *config = Some(binding);
    drop(config);
    let Ok(mut action_config) = ACTION_CONFIG.lock() else {
        return -8;
    };
    *action_config = Some(action_binding);
    drop(action_config);
    let Ok(mut buffer) = EVENTS.lock() else {
        return -8;
    };
    buffer.len = 0;
    drop(buffer);
    let Ok(mut history) = HP_HISTORY.lock() else {
        return -8;
    };
    *history = HpHistory::new();
    HP_HISTORY_GAP.store(0, Ordering::Release);
    drop(history);
    DROPPED.store(0, Ordering::Relaxed);
    NEXT_CALL.store(0, Ordering::Relaxed);
    let signatures = [
        MethodSignature {
            parameters: &[
                "Toram.Client.Game",
                "Toram.Common.GameReturnCode",
                "System.Byte",
                "System.Int32",
                "Toram.Common.Actions.AttackResponseData",
            ],
            return_type: "System.Void",
        },
        MethodSignature {
            parameters: &[
                "Toram.Client.Game",
                "Toram.Common.Actions.ActionCode",
                "Toram.Common.GameReturnCode",
            ],
            return_type: "System.Void",
        },
        MethodSignature {
            parameters: &[
                "UnityEngine.GameObject",
                "CharacterActionManagerBase",
                "SkillActionBase",
            ],
            return_type: "System.Void",
        },
    ];
    let detours = [
        attack as usize,
        default_response as usize,
        action_hit as usize,
    ];
    let originals = [&ORIGINAL, &DEFAULT_ORIGINAL, &HIT_ORIGINAL];
    let Ok(mut hooks) = HOOKS.lock() else {
        return -8;
    };
    INSTALLED.store(true, Ordering::Release);
    for index in 0..hooks.len() {
        let hook = &mut hooks[index];
        let ok = unsafe {
            hook.set_with_signature(
                if index == 2 { ch.pbm_class } else { listener },
                HOOK_NAMES[index].0,
                HOOK_NAMES[index].1,
                detours[index],
                true,
                "response82",
                Some(&signatures[index]),
                Some(originals[index]),
            )
        };
        if !ok
            || hook.vtable_slot.is_null()
            || !fn_readable(originals[index].load(Ordering::Acquire))
        {
            drop(hooks);
            unsafe {
                stop();
            }
            return -8;
        }
    }
    drop(hooks);
    let mut frequency = 0;
    unsafe {
        QueryPerformanceFrequency(&mut frequency);
    }
    INSTALLED.store(true, Ordering::Release);
    log_fmt(format_args!("[combat-response] {{\"kind\":\"begin\",\"pid\":{},\"version\":3,\"frequency\":{},\"capacity\":{},\"max_mobs\":{},\"network_correlated\":false}}",
        unsafe { GetCurrentProcessId() }, frequency, CAPACITY, MAX_MOBS));
    ACTIVE.store(true, Ordering::SeqCst);
    publish_monitor(true);
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding(response_class: usize, mob_class: usize) -> Binding {
        Binding {
            local_actor: Some((0, 42)),
            response_class,
            mob_class,
            player_status_class: 0,
            player_status: 32,
            response_mp: 16,
            response_exmp: 20,
            skill: 16,
            damage_id: 18,
            response_local_id: 20,
            response_attack_count: 21,
            flags: 19,
            mob_list: 24,
            mob_id: 16,
            local_id: 20,
            unique_id: 24,
            hp: 28,
            state: 32,
            code: 36,
            allowed: 40,
            violation: 44,
            normal_exp: 48,
            physical_exp: 49,
            magic_exp: 50,
        }
    }

    #[test]
    fn response_snapshot_preserves_objects_and_reads_distinct_result_fields() {
        let classes = [1usize, 2];
        let mut mob = [0usize; 8];
        let mut list = [0usize; 5];
        let mut response = [0usize; 5];
        mob[0] = classes.as_ptr() as usize;
        unsafe {
            let bytes = mob.as_mut_ptr().cast::<u8>();
            core::ptr::write_unaligned(bytes.add(16).cast::<i32>(), 42);
            *bytes.add(20) = 7;
            core::ptr::write_unaligned(bytes.add(24).cast::<i32>(), 1234);
            core::ptr::write_unaligned(bytes.add(28).cast::<i32>(), 108);
            core::ptr::write_unaligned(bytes.add(32).cast::<i32>(), 3);
            *bytes.add(36) = 46;
            core::ptr::write_unaligned(bytes.add(40).cast::<i32>(), 200);
            core::ptr::write_unaligned(bytes.add(44).cast::<i32>(), 1158);
            *bytes.add(48) = 50;
            *bytes.add(49) = 250;
            *bytes.add(50) = 150;
        }
        list[3] = 1;
        list[4] = mob.as_ptr() as usize;
        response[0] = unsafe { classes.as_ptr().add(1) } as usize;
        response[2] = (5usize << 24) | (9usize << 16) | 17 | (7usize << 32) | (3usize << 40);
        response[3] = list.as_ptr() as usize;
        let original = (mob, list, response);
        let parsed = unsafe {
            binding(response[0], mob[0]).response(response.as_ptr() as usize, Event::EMPTY)
        };
        assert!(parsed.valid);
        assert_eq!(
            (parsed.skill, parsed.damage_id, parsed.result_flags),
            (17, 9, 5)
        );
        assert_eq!(
            (parsed.response_local_id, parsed.response_attack_count),
            (7, 3)
        );
        assert_eq!((parsed.mob_count, parsed.captured), (1, 1));
        assert_eq!(
            parsed.mobs[0],
            Mob {
                valid: true,
                mob_id: 42,
                local_id: 7,
                unique_id: 1234,
                hp: 108,
                state: 3,
                code: 46,
                allowed: 200,
                violation: 1158,
                previous_hp: -1,
                normal_exp: 50,
                physical_exp: 250,
                magic_exp: 150,
            }
        );
        assert_eq!((mob, list, response), original);
    }

    #[test]
    fn snapshot_rejects_null_wrong_classes_and_impossible_array_lengths() {
        let class = [1usize; 2];
        let b = binding(class.as_ptr() as usize, class.as_ptr() as usize);
        assert!(!unsafe { b.response(0, Event::EMPTY) }.valid);
        let mut response = [0usize; 5];
        assert!(!unsafe { b.response(response.as_ptr() as usize, Event::EMPTY) }.valid);
        response[0] = b.response_class;
        let mut list = [0usize; 4];
        list[3] = 4097;
        response[3] = list.as_ptr() as usize;
        let parsed = unsafe { b.response(response.as_ptr() as usize, Event::EMPTY) };
        assert!(!parsed.valid);
        assert_eq!(parsed.captured, 0);
    }

    #[test]
    fn bounded_buffer_retains_earlier_events_on_overflow() {
        let mut buffer = Buffer::new();
        for i in 0..CAPACITY {
            assert!(buffer.push(Event {
                native: i as i32,
                ..Event::EMPTY
            }));
        }
        assert!(!buffer.push(Event::EMPTY));
        assert_eq!(buffer.entries[0].native, 0);
        assert_eq!(buffer.entries[CAPACITY - 1].native, (CAPACITY - 1) as i32);
    }

    #[test]
    fn response_mp_reads_signed_int16_without_consuming_adjacent_fields() {
        let classes = [1usize, 2];
        let mut b = binding(classes.as_ptr() as usize, 0);
        b.player_status_class = unsafe { classes.as_ptr().add(1) } as usize;
        let mut status = [0xa5a5_a5a5_a5a5_a5a5usize; 4];
        status[0] = b.player_status_class;
        unsafe {
            let bytes = status.as_mut_ptr().cast::<u8>();
            core::ptr::write_unaligned(bytes.add(b.response_mp).cast::<i16>(), 319);
            core::ptr::write_unaligned(bytes.add(b.response_exmp).cast::<i16>(), -7);
        }
        let response = [b.response_class, 0, 0, 0, status.as_ptr() as usize];
        let before = (response, status);
        let parsed = unsafe { b.response(response.as_ptr() as usize, Event::EMPTY) };
        assert!(parsed.valid && parsed.response_mp_valid);
        assert_eq!((parsed.response_mp, parsed.response_exmp), (319, -7));
        assert_eq!((response, status), before);
        status[0] = 0;
        assert_eq!(status[0], 0);
        let parsed = unsafe { b.response(response.as_ptr() as usize, Event::EMPTY) };
        assert!(parsed.valid);
        assert!(!parsed.response_mp_valid);
        assert_eq!((parsed.response_mp, parsed.response_exmp), (-1, -1));
        let mut response = response;
        response[4] = 0;
        assert!(!unsafe { b.response(response.as_ptr() as usize, Event::EMPTY) }.response_mp_valid);
    }

    fn action_context(scope: CalculationScope, call: u64) -> ActionContext {
        ActionContext {
            scope,
            call,
            battle: 0x1110,
            target: 0x2220,
            target_actor: 0x3330,
            action: 0x4440,
            action_class: 0x5550,
        }
    }

    #[test]
    fn calculation_classification_requires_a_verified_action_scope() {
        let parent = |class| match class {
            12 => Some(11),
            11 | 21 => Some(10),
            _ => None,
        };
        assert_eq!(
            classify_action(12, 10, 11, parent),
            CalculationScope::Normal
        );
        assert_eq!(classify_action(21, 10, 11, parent), CalculationScope::Skill);
        assert_eq!(
            classify_action(99, 10, 11, parent),
            CalculationScope::Unknown
        );
        assert_eq!(classify_action(99, 10, 11, Some), CalculationScope::Unknown);
        assert_eq!(calculation_scope(false, None), CalculationScope::Unknown);
        assert_eq!(calculation_scope(true, None), CalculationScope::Normal);
        let skill = Some(action_context(CalculationScope::Skill, 1));
        assert_eq!(calculation_scope(false, skill), CalculationScope::Skill);
        assert_eq!(calculation_scope(true, skill), CalculationScope::Normal);
    }

    #[test]
    fn action_scope_restores_nested_foreign_unwinding_and_thread_local_contexts() {
        assert_eq!(current_context(), None);
        let outer = action_context(CalculationScope::Skill, 1);
        let _outer = ActionScope::enter(Some(outer));
        {
            let _inner = ActionScope::enter(Some(action_context(CalculationScope::Normal, 2)));
            assert_eq!(current_context().unwrap().call, 2);
            {
                let _foreign = ActionScope::enter(None);
                assert_eq!(current_context(), None);
            }
            assert_eq!(current_context().unwrap().call, 2);
        }
        assert_eq!(current_context(), Some(outer));
        let unwound = std::panic::catch_unwind(|| {
            let _inner = ActionScope::enter(None);
            panic!("synthetic observation unwind");
        });
        assert!(unwound.is_err());
        assert_eq!(current_context(), Some(outer));
        assert_eq!(std::thread::spawn(current_context).join().unwrap(), None);
        drop(_outer);
        assert_eq!(current_context(), None);
    }

    #[test]
    fn action_observation_pairs_local_samples_and_forwards_foreign_calls() {
        let context = action_context(CalculationScope::Skill, 7);
        let mp = Cell::new(319);
        let mut recorded = Vec::new();
        let result = around_action(
            Some(context),
            || ResourceSample {
                valid: true,
                local_mp: mp.get(),
                received_mp: 319,
                local_exmp: 2,
                received_exmp: 2,
            },
            || {
                assert_eq!(current_context(), Some(context));
                mp.set(219);
                47
            },
            |event| recorded.push(event),
        );
        assert_eq!(result, 47);
        assert_eq!(current_context(), None);
        assert_eq!(recorded.len(), 2);
        assert!(!recorded[0].exit && recorded[1].exit);
        assert_eq!(recorded[0].context, recorded[1].context);
        assert_eq!(recorded[0].resources.local_mp, 319);
        assert_eq!(recorded[1].resources.local_mp, 219);
        assert_eq!(recorded[1].damage_id, 0);
        assert_eq!(recorded[1].skill, -1);
        let _outer = ActionScope::enter(Some(context));
        assert_eq!(
            around_action(
                None,
                || panic!("foreign callback must not sample local resources"),
                || {
                    assert_eq!(current_context(), None);
                    83
                },
                |_| panic!("foreign callback must not record a local action"),
            ),
            83
        );
        assert_eq!(current_context(), Some(context));
    }

    #[test]
    fn action_resources_reject_foreign_battle_and_replaced_game_status() {
        let mut battle = [11usize, 0];
        let foreign = [11usize, 0];
        let mut game = [13usize, 0, 0, 0];
        unsafe {
            let bytes = game.as_mut_ptr().cast::<u8>();
            for (offset, value) in [(16, 100i32), (20, 90), (24, 4), (28, 3)] {
                core::ptr::write_unaligned(bytes.add(offset).cast::<i32>(), value);
            }
        }
        let mut status = [12usize, 0, game.as_ptr() as usize];
        let mut actor = [
            14usize,
            0,
            status.as_ptr() as usize,
            battle.as_ptr() as usize,
        ];
        let binding = ActionBinding {
            actor: actor.as_ptr() as usize,
            actor_class: 14,
            actor_status: 16,
            actor_battle: 24,
            battle: battle.as_ptr() as usize,
            battle_class: 11,
            status: status.as_ptr() as usize,
            status_class: 12,
            game: game.as_ptr() as usize,
            game_class: 13,
            status_game: 16,
            action_base: 10,
            normal_class: 11,
            local_mp: 16,
            received_mp: 20,
            local_exmp: 24,
            received_exmp: 28,
            _roots: GcScope::new(),
        };
        let before = (battle, status, game);
        assert_eq!(
            unsafe { binding.resources(binding.battle) }.unwrap(),
            ResourceSample {
                valid: true,
                local_mp: 100,
                received_mp: 90,
                local_exmp: 4,
                received_exmp: 3,
            }
        );
        assert_eq!((battle, status, game), before);
        assert!(unsafe { binding.resources(foreign.as_ptr() as usize) }.is_none());
        assert!(unsafe { binding.context(foreign.as_ptr() as usize, 0, 0, 0) }.is_none());
        let replacement_status = status;
        actor[2] = replacement_status.as_ptr() as usize;
        assert!(unsafe { binding.resources(binding.battle) }.is_none());
        actor[2] = status.as_ptr() as usize;
        actor[3] = foreign.as_ptr() as usize;
        assert!(unsafe { binding.resources(binding.battle) }.is_none());
        actor[3] = battle.as_ptr() as usize;
        assert_eq!(actor[2], status.as_ptr() as usize);
        assert_eq!(actor[3], battle.as_ptr() as usize);
        status[2] = 0;
        assert!(unsafe { binding.resources(binding.battle) }.is_none());
        status[2] = game.as_ptr() as usize;
        battle[0] = 22;
        assert_eq!(status[2], game.as_ptr() as usize);
        assert_eq!(battle[0], 22);
        assert!(unsafe { binding.resources(binding.battle) }.is_none());
    }

    #[test]
    fn callback_forwards_every_argument_when_capture_is_off() {
        unsafe extern "C" fn original(
            this: *mut c_void,
            game: *mut c_void,
            code: i32,
            actor_type: u8,
            actor_id: i32,
            response: *mut c_void,
            method: *const c_void,
        ) {
            unsafe {
                *(this as *mut [usize; 6]) = [
                    game as usize,
                    code as u32 as usize,
                    actor_type as usize,
                    actor_id as u32 as usize,
                    response as usize,
                    method as usize,
                ];
            }
        }
        let mut args = [0usize; 6];
        assert!(!ACTIVE.load(Ordering::Acquire));
        ORIGINAL.store(original as usize, Ordering::Release);
        unsafe {
            attack(
                args.as_mut_ptr().cast(),
                0x12340 as *mut c_void,
                -7,
                255,
                -31,
                0x56780 as *mut c_void,
                0x9abc0 as *const c_void,
            );
        }
        assert_eq!(
            args,
            [
                0x12340,
                (-7i32) as u32 as usize,
                255,
                (-31i32) as u32 as usize,
                0x56780,
                0x9abc0
            ]
        );
        ORIGINAL.store(0, Ordering::Release);
        unsafe extern "C" fn default_original(
            this: *mut c_void,
            game: *mut c_void,
            action_code: i32,
            code: i32,
            method: *const c_void,
        ) {
            unsafe {
                *(this as *mut [usize; 4]) = [
                    game as usize,
                    action_code as u32 as usize,
                    code as u32 as usize,
                    method as usize,
                ];
            }
        }
        let mut default_args = [0usize; 4];
        DEFAULT_ORIGINAL.store(default_original as usize, Ordering::Release);
        unsafe {
            default_response(
                default_args.as_mut_ptr().cast(),
                0x12340 as *mut c_void,
                17,
                -31,
                0x9abc0 as *const c_void,
            );
        }
        assert_eq!(
            default_args,
            [0x12340, 17, (-31i32) as u32 as usize, 0x9abc0]
        );
        DEFAULT_ORIGINAL.store(0, Ordering::Release);
        unsafe extern "C" fn hit_original(
            this: *mut c_void,
            target: *mut c_void,
            target_actor: *mut c_void,
            action: *mut c_void,
            method: *const c_void,
        ) {
            assert_eq!(current_context(), None);
            unsafe {
                *(this as *mut [usize; 4]) = [
                    target as usize,
                    target_actor as usize,
                    action as usize,
                    method as usize,
                ];
            }
        }
        HIT_ORIGINAL.store(hit_original as usize, Ordering::Release);
        let _outer = ActionScope::enter(Some(action_context(CalculationScope::Skill, 99)));
        let mut hit_args = [0usize; 4];
        unsafe {
            action_hit(
                hit_args.as_mut_ptr().cast(),
                0x12340 as *mut c_void,
                0x56780 as *mut c_void,
                0x98760 as *mut c_void,
                0x9abc0 as *const c_void,
            );
        }
        assert_eq!(hit_args, [0x12340, 0x56780, 0x98760, 0x9abc0]);
        assert_eq!(current_context().unwrap().call, 99);
        HIT_ORIGINAL.store(0, Ordering::Release);
        assert_eq!(FLIGHT.load(Ordering::SeqCst), 0);
    }

    fn signal_event(mobs: [Mob; MAX_MOBS], captured: usize) -> Event {
        Event {
            local_response: true,
            valid: true,
            code: 0,
            mob_count: captured,
            captured,
            mobs,
            ..Event::EMPTY
        }
    }

    fn live_mob(unique_id: i32, hp: i32, allowed: i32, violation: i32) -> Mob {
        Mob {
            valid: true,
            mob_id: 7,
            local_id: 1,
            unique_id,
            hp,
            state: 1,
            code: 0,
            allowed,
            violation,
            ..Mob::EMPTY
        }
    }

    fn empty_mobs() -> [Mob; MAX_MOBS] {
        [Mob::EMPTY; MAX_MOBS]
    }

    #[test]
    fn response_signal_requires_hp_evidence() {
        let mut mobs = empty_mobs();
        mobs[0] = live_mob(1, 500, 1, 0);
        mobs[1] = live_mob(2, 300, 5, 0);
        assert_eq!(response_signal(&signal_event(mobs, 2)), None);
        mobs[0].previous_hp = 600;
        mobs[1].previous_hp = 400;
        assert_eq!(response_signal(&signal_event(mobs, 2)), Some(true));
        // Refused on a living mob.
        let mut mobs = empty_mobs();
        mobs[0] = live_mob(1, 500, 0, 0);
        assert_eq!(response_signal(&signal_event(mobs, 1)), Some(false));
        // Violation flag.
        let mut mobs = empty_mobs();
        mobs[0] = live_mob(1, 500, 1, 1158);
        assert_eq!(response_signal(&signal_event(mobs, 1)), Some(false));
        // A dead snapshot without a baseline is not a confirmed new kill.
        let mut mobs = empty_mobs();
        mobs[0] = live_mob(1, 0, 0, 0);
        assert_eq!(response_signal(&signal_event(mobs, 1)), None);
        mobs[0].previous_hp = 5;
        assert_eq!(response_signal(&signal_event(mobs, 1)), Some(true));
        // Nothing readable: no signal.
        assert_eq!(response_signal(&Event::EMPTY), None);
        assert_eq!(response_signal(&signal_event(empty_mobs(), 0)), None);
    }

    #[test]
    fn foreign_responses_invalidate_shared_target_without_feeding_feedback() {
        assert!(is_local_response(Some((0, 42)), 0, 42));
        assert!(!is_local_response(Some((0, 42)), 9, 42));
        assert!(!is_local_response(Some((0, 42)), 0, 43));
        assert!(!is_local_response(None, 0, 42));
        assert!(!is_local_response(Some((0, 0)), 0, 0));
        let mut history = HpHistory::new();
        let mut mobs = empty_mobs();
        mobs[0] = live_mob(1, 500, 1000, 0);
        let mut local = signal_event(mobs, 1);
        local.ticks = 1;
        history.observe(&mut local);
        let mut foreign = local;
        foreign.local_response = false;
        foreign.ticks = 2;
        foreign.mobs[0].hp = 300;
        foreign.mobs[0].violation = 1158;
        history.observe(&mut foreign);
        assert_eq!(foreign.mobs[0].previous_hp, -1);
        assert_eq!(response_signal(&foreign), None);
        local.ticks = 3;
        local.mobs[0].hp = 400;
        history.observe(&mut local);
        assert_eq!(local.mobs[0].previous_hp, -1);
        assert_eq!(response_signal(&local), None);
        local.ticks = 4;
        local.mobs[0].hp = 350;
        history.observe(&mut local);
        assert_eq!(local.mobs[0].previous_hp, 400);
        assert_eq!(response_signal(&local), Some(true));
    }

    #[test]
    fn foreign_history_invalidation_is_target_scoped_and_timestamp_ordered() {
        let mut history = HpHistory::new();
        let mut mobs = empty_mobs();
        mobs[0] = live_mob(1, 500, 1000, 0);
        let mut local = signal_event(mobs, 1);
        local.ticks = 10;
        history.observe(&mut local);

        let mut stale = local;
        stale.local_response = false;
        stale.ticks = 9;
        stale.mobs[0].hp = 300;
        history.observe(&mut stale);
        let mut other = local;
        other.local_response = false;
        other.ticks = 11;
        other.mobs[0].unique_id = 2;
        history.observe(&mut other);
        local.ticks = 12;
        local.mobs[0].hp = 400;
        history.observe(&mut local);
        assert_eq!(local.mobs[0].previous_hp, 500);
        assert_eq!(response_signal(&local), Some(true));

        let mut foreign = local;
        foreign.local_response = false;
        foreign.ticks = 12;
        foreign.mobs[0].hp = 200;
        history.observe(&mut foreign);
        assert_eq!(foreign.mobs[0].previous_hp, -1);
        local.ticks = 13;
        local.mobs[0].hp = 300;
        history.observe(&mut local);
        assert_eq!(local.mobs[0].previous_hp, -1);
        assert_eq!(response_signal(&local), None);
        local.ticks = 14;
        local.mobs[0].hp = 250;
        history.observe(&mut local);
        assert_eq!(response_signal(&local), Some(true));

        foreign.ticks = 16;
        foreign.mobs[0].hp = 100;
        history.observe(&mut foreign);
        local.ticks = 15;
        local.mobs[0].hp = 200;
        history.observe(&mut local);
        assert_eq!(local.mobs[0].previous_hp, -1);
        assert_eq!(response_signal(&local), None);
        local.ticks = 17;
        local.mobs[0].hp = 150;
        history.observe(&mut local);
        assert_eq!(local.mobs[0].previous_hp, -1);
        assert_eq!(response_signal(&local), None);
    }

    #[test]
    fn missed_response_history_requires_a_fresh_baseline() {
        let history = Mutex::new(HpHistory::new());
        let gap = AtomicI64::new(0);
        let mut mobs = empty_mobs();
        mobs[0] = live_mob(1, 500, 1000, 0);
        let mut local = signal_event(mobs, 1);
        local.ticks = 10;
        observe_hp(&history, &gap, &mut local);
        let mut foreign = local;
        foreign.local_response = false;
        foreign.ticks = 11;
        foreign.mobs[0].hp = 300;
        let guard = history.lock().unwrap();
        observe_hp(&history, &gap, &mut foreign);
        assert_eq!(gap.load(Ordering::Acquire), 11);
        assert_eq!(response_signal(&foreign), None);
        drop(guard);

        for ticks in [9, 10, 11] {
            local.ticks = ticks;
            local.mobs[0].hp = 900;
            observe_hp(&history, &gap, &mut local);
            assert_eq!(local.mobs[0].previous_hp, -1);
            assert_eq!(response_signal(&local), None);
        }
        local.ticks = 12;
        local.mobs[0].hp = 400;
        observe_hp(&history, &gap, &mut local);
        assert_eq!(gap.load(Ordering::Acquire), 0);
        assert_eq!(local.mobs[0].previous_hp, -1);
        assert_eq!(response_signal(&local), None);
        local.ticks = 13;
        local.mobs[0].hp = 350;
        observe_hp(&history, &gap, &mut local);
        assert_eq!(response_signal(&local), Some(true));
    }

    #[test]
    fn hard_hit_capture_does_not_reward_unchanged_hp() {
        let mut history = HpHistory::new();
        let samples = [
            (40427, None),
            (40236, Some(true)),
            (40236, None),
            (40007, Some(true)),
            (40007, None),
            (38441, Some(true)),
            (38174, Some(true)),
        ];
        for (i, (hp, expected)) in samples.into_iter().enumerate() {
            let mut mobs = empty_mobs();
            mobs[0] = live_mob(316059, hp, 42720, 0);
            let mut event = signal_event(mobs, 1);
            event.ticks = i as i64 + 1;
            history.observe(&mut event);
            assert_eq!(response_signal(&event), expected, "sample {i}");
        }
    }

    #[test]
    fn history_rejects_stale_and_incomplete_evidence() {
        let mut history = HpHistory::new();
        let mut mobs = empty_mobs();
        mobs[0] = live_mob(1, 500, 1000, 0);
        let mut baseline = signal_event(mobs, 1);
        baseline.ticks = 10;
        history.observe(&mut baseline);
        mobs[0].hp = 400;
        let mut stale = signal_event(mobs, 1);
        stale.ticks = 9;
        history.observe(&mut stale);
        assert_eq!(response_signal(&stale), None);
        let mut duplicate = signal_event(mobs, 1);
        duplicate.ticks = 10;
        history.observe(&mut duplicate);
        assert_eq!(response_signal(&duplicate), None);
        let mut next = signal_event(mobs, 1);
        next.ticks = 11;
        history.observe(&mut next);
        assert_eq!(next.mobs[0].previous_hp, 500);
        assert_eq!(response_signal(&next), Some(true));
        next.mob_count = 2;
        assert_eq!(response_signal(&next), None);
        next.mob_count = 1;
        next.mobs[0].code = 46;
        assert_eq!(response_signal(&next), None);
        next.mobs[0].code = 0;
        next.mobs[0].previous_hp = 300;
        assert_eq!(response_signal(&next), None);
        let mut fresh = signal_event(mobs, 1);
        fresh.mobs[0].unique_id = 2;
        fresh.ticks = 12;
        history.observe(&mut fresh);
        assert_eq!(response_signal(&fresh), None);
    }
}
