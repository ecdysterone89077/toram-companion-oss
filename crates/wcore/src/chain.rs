//! Game object chain resolution (`PlayerManager` -> `PlayManager` -> `PlayerBattleStatus` / `PlayerBattleManager`).

use crate::il2cpp::{
    api, call_ptr0 as raw_call_ptr0, cstr_to_str, find_class, find_image, get_field, get_method,
    read_inst_ptr as raw_read_inst_ptr, read_static_ptr as raw_read_static_ptr, GcScope,
};
use crate::win::{log_fmt, ptr_ok};
use core::ffi::c_void;
use core::ptr;

unsafe fn validated_object(obj: *mut c_void) -> *mut c_void {
    if unsafe { is_valid_il2cpp_obj(obj) } {
        obj
    } else {
        ptr::null_mut()
    }
}

unsafe fn read_inst_ptr(
    instance: *mut c_void,
    klass: *mut c_void,
    name: &str,
    gc: &mut GcScope,
) -> *mut c_void {
    if !unsafe { is_valid_il2cpp_obj(instance) } || !is_valid_il2cpp_class(klass) {
        return ptr::null_mut();
    }
    unsafe { validated_object(raw_read_inst_ptr(instance, klass, name, gc)) }
}

unsafe fn read_static_ptr(klass: *mut c_void, name: &str, gc: &mut GcScope) -> *mut c_void {
    if !is_valid_il2cpp_class(klass) {
        return ptr::null_mut();
    }
    unsafe { validated_object(raw_read_static_ptr(klass, name, gc)) }
}

unsafe fn call_ptr0(
    klass: *mut c_void,
    instance: *mut c_void,
    name: &str,
    gc: &mut GcScope,
) -> *mut c_void {
    if !is_valid_il2cpp_class(klass)
        || (!instance.is_null() && !unsafe { is_valid_il2cpp_obj(instance) })
    {
        return ptr::null_mut();
    }
    unsafe { validated_object(raw_call_ptr0(klass, instance, name, gc)) }
}

pub struct GameChain {
    pub domain: *mut c_void,
    pub image: *mut c_void,
    pub player_mgr_class: *mut c_void,
    pub play_mgr_class: *mut c_void,
    pub status_class: *mut c_void,
    pub bstatus_class: *mut c_void,
    pub pbm_class: *mut c_void,
    pub pss_class: *mut c_void,
    pub pdm_class: *mut c_void,
    pub cm_class: *mut c_void,
    pub pam_class: *mut c_void,
    pub player_mgr: *mut c_void,
    pub play_mgr: *mut c_void,
    pub status: *mut c_void,
    pub bstatus: *mut c_void,
    pub pbm: *mut c_void,
    pub pss: *mut c_void,
    pub pdm: *mut c_void,
    pub cm: *mut c_void,
    pub pam: *mut c_void,
}

impl Default for GameChain {
    fn default() -> Self {
        Self::new()
    }
}

impl GameChain {
    pub const fn new() -> Self {
        Self {
            domain: ptr::null_mut(),
            image: ptr::null_mut(),
            player_mgr_class: ptr::null_mut(),
            play_mgr_class: ptr::null_mut(),
            status_class: ptr::null_mut(),
            bstatus_class: ptr::null_mut(),
            pbm_class: ptr::null_mut(),
            pss_class: ptr::null_mut(),
            pdm_class: ptr::null_mut(),
            cm_class: ptr::null_mut(),
            pam_class: ptr::null_mut(),
            player_mgr: ptr::null_mut(),
            play_mgr: ptr::null_mut(),
            status: ptr::null_mut(),
            bstatus: ptr::null_mut(),
            pbm: ptr::null_mut(),
            pss: ptr::null_mut(),
            pdm: ptr::null_mut(),
            cm: ptr::null_mut(),
            pam: ptr::null_mut(),
        }
    }

    /// Prefer the live local player's references over a graph-wide manager hunt.
    /// These fields were independently checked in the current runtime metadata.
    unsafe fn resolve_local_player(&mut self, gc: &mut GcScope) -> bool {
        self.pdm_class = unsafe { find_class(self.image, self.domain, "", "PlayerDataManager") };
        self.pam_class = unsafe { find_class(self.image, self.domain, "", "PlayerActionManager") };
        self.cm_class = unsafe { find_class(self.image, self.domain, "", "CharacterMove") };
        self.pdm =
            unsafe { call_ptr0(self.pdm_class, ptr::null_mut(), "GetPlayerDataManager", gc) };
        if !unsafe { is_valid_il2cpp_obj(self.pdm) } {
            return false;
        }
        let player = unsafe { read_inst_ptr(self.pdm, self.pdm_class, "activePlayer", gc) };
        if !unsafe { is_valid_il2cpp_obj(player) } {
            return false;
        }
        let player_class = unsafe { *(player as *const *mut c_void) };
        self.status =
            unsafe { read_inst_ptr(player, player_class, "<PlayerStatus>k__BackingField", gc) };
        self.pam = unsafe {
            read_inst_ptr(
                player,
                player_class,
                "<actionManagerBase>k__BackingField",
                gc,
            )
        };
        self.cm =
            unsafe { read_inst_ptr(player, player_class, "<CharacterMove>k__BackingField", gc) };
        if !unsafe { is_valid_il2cpp_obj(self.status) } || !unsafe { is_valid_il2cpp_obj(self.pam) }
        {
            return false;
        }
        self.bstatus = unsafe { read_inst_ptr(self.status, self.status_class, "battleStatus", gc) };
        self.pss = unsafe { read_inst_ptr(self.status, self.status_class, "secondaryStatus", gc) };
        self.pbm = unsafe { read_inst_ptr(self.pam, self.pam_class, "_playerBattleManager", gc) };
        (unsafe { is_valid_il2cpp_obj(self.bstatus) })
            && unsafe { is_valid_il2cpp_obj(self.pss) }
            && unsafe { is_valid_il2cpp_obj(self.pbm) }
    }

    /// Resolve a singleton instance via common static fields/getters or `UnityEngine.Object::FindObjectOfType`.
    pub unsafe fn resolve_singleton(&self, klass: *mut c_void, gc: &mut GcScope) -> *mut c_void {
        if !is_valid_il2cpp_class(klass) {
            return ptr::null_mut();
        }
        let Some(api) = api() else {
            return ptr::null_mut();
        };

        for field_name in [
            "instance",
            "_instance",
            "Instance",
            "s_Instance",
            "m_Instance",
            "<Instance>k__BackingField",
        ] {
            let v = unsafe { read_static_ptr(klass, field_name, gc) };
            if unsafe { is_valid_il2cpp_obj(v) } {
                return v;
            }
        }

        let mut cur = klass;
        let mut depth = 0;
        while is_valid_il2cpp_class(cur) && depth < 6 {
            for getter in ["get_Instance", "get_instance", "GetInstance"] {
                let v = unsafe { call_ptr0(cur, ptr::null_mut(), getter, gc) };
                if unsafe { is_valid_il2cpp_obj(v) } {
                    return v;
                }
            }
            cur = match api.class_get_parent {
                Some(parent_fn) => unsafe { parent_fn(cur) },
                None => ptr::null_mut(),
            };
            depth += 1;
        }

        // Fallback: UnityEngine.Object.FindObjectOfType(System.Type)
        let uobj = unsafe { find_class(ptr::null_mut(), self.domain, "UnityEngine", "Object") };
        let stype = unsafe { find_class(ptr::null_mut(), self.domain, "System", "Type") };
        if !uobj.is_null() && !stype.is_null() {
            if let (Some(cn_fn), Some(ns_fn), Some(str_new)) =
                (api.class_get_name, api.class_get_namespace, api.string_new)
            {
                let cname = unsafe { cstr_to_str(cn_fn(klass)) };
                let cns = unsafe { cstr_to_str(ns_fn(klass)) };
                if !cname.is_empty() {
                    let full = if !cns.is_empty() {
                        format!("{}.{}\0", cns, cname)
                    } else {
                        format!("{}, Assembly-CSharp\0", cname)
                    };
                    let m_get_type = unsafe { get_method(stype, "GetType", 1) };
                    let m_find = unsafe { get_method(uobj, "FindObjectOfType", 1) };
                    if !m_get_type.is_null() && !m_find.is_null() {
                        let il_str = unsafe { str_new(full.as_ptr() as *const i8) };
                        if !il_str.is_null() {
                            let mut a1: [*mut c_void; 1] = [il_str];
                            let mut exc: *mut c_void = ptr::null_mut();
                            let t_obj = unsafe {
                                (api.runtime_invoke)(
                                    m_get_type,
                                    ptr::null_mut(),
                                    a1.as_mut_ptr(),
                                    &mut exc,
                                )
                            };
                            if exc.is_null() && unsafe { is_valid_il2cpp_obj(t_obj) } {
                                let mut a2: [*mut c_void; 1] = [t_obj];
                                let mut exc2: *mut c_void = ptr::null_mut();
                                let found = unsafe {
                                    (api.runtime_invoke)(
                                        m_find,
                                        ptr::null_mut(),
                                        a2.as_mut_ptr(),
                                        &mut exc2,
                                    )
                                };
                                if exc2.is_null() && unsafe { is_valid_il2cpp_obj(found) } {
                                    return gc.pin(found);
                                }
                            }
                        }
                    }
                }
            }
        }

        ptr::null_mut()
    }

    /// Breadth-first scan across static manager fields and reference fields to find
    /// `PlayManager`, `PlayerBattleStatus`, `PlayerStatus`, `PlayerBattleManager`, and `PlayerSecondaryStatus`.
    unsafe fn hunt_managers(&mut self, gc: &mut GcScope) {
        let Some(api) = api() else {
            return;
        };
        let (
            Some(class_get_fields),
            Some(field_get_name),
            Some(object_get_class),
            Some(class_get_name),
            Some(class_is_valuetype),
        ) = (
            api.class_get_fields,
            api.field_get_name,
            api.object_get_class,
            api.class_get_name,
            api.class_is_valuetype,
        )
        else {
            return;
        };

        let mut q: Vec<*mut c_void> = Vec::with_capacity(512);
        let push = |obj: *mut c_void, gc: &mut GcScope, q: &mut Vec<*mut c_void>| {
            if q.len() >= 512 || !unsafe { is_valid_il2cpp_obj(obj) } {
                return;
            }
            if !q.contains(&obj) {
                q.push(gc.pin(obj));
            }
        };

        push(self.player_mgr, gc, &mut q);
        push(self.play_mgr, gc, &mut q);

        for root_name in [
            "PlayManager",
            "PlayerManager",
            "GameManager",
            "BattleManager",
            "PlayerBattleManager",
            "UIManager",
            "MapManager",
            "StageManager",
            "PartyManager",
            "CharacterManager",
            "StatusManager",
            "DataManager",
            "NetworkManager",
            "SceneManager",
            "AppManager",
            "MainManager",
            "SingletonMonoBehaviour",
            "SystemManager",
            "GlobalData",
            "UserDataManager",
            "SkillManager",
            "ItemManager",
            "EquipManager",
            "ActorManager",
            "ObjectManager",
            "FieldManager",
        ] {
            let k = unsafe { find_class(self.image, self.domain, "", root_name) };
            if k.is_null() {
                continue;
            }
            let s = unsafe { self.resolve_singleton(k, gc) };
            push(s, gc, &mut q);

            let mut f_iter: *mut c_void = ptr::null_mut();
            loop {
                let f = unsafe { class_get_fields(k, &mut f_iter) };
                if f.is_null() {
                    break;
                }
                let off = unsafe { (api.field_get_offset)(f) };
                if off <= 4 {
                    let mut sv: *mut c_void = ptr::null_mut();
                    unsafe {
                        (api.field_static_get_value)(f, (&mut sv as *mut *mut c_void).cast());
                    }
                    if !sv.is_null() && (sv as usize) > 0x10000 && ptr_ok(sv as usize, 24) {
                        let ck = unsafe { *(sv as *const *mut c_void) };
                        if !ck.is_null() && ptr_ok(ck as usize, 64) {
                            push(sv, gc, &mut q);
                        }
                    }
                }
            }
        }

        let mut qi = 0usize;
        while qi < q.len()
            && (self.bstatus.is_null() || self.status.is_null() || self.pbm.is_null())
        {
            let obj = q[qi];
            qi += 1;
            if !unsafe { is_valid_il2cpp_obj(obj) } {
                continue;
            }
            let mut k = unsafe { object_get_class(obj) };
            if k.is_null() || !ptr_ok(k as usize, 32) {
                continue;
            }
            let cn = unsafe { cstr_to_str(class_get_name(k)) };
            if cn == "PlayManager" && self.play_mgr.is_null() {
                self.play_mgr = obj;
                log_fmt(format_args!("[hunt] found PlayManager={:p}", obj));
            } else if cn == "PlayerBattleStatus" && self.bstatus.is_null() {
                self.bstatus = obj;
                log_fmt(format_args!("[hunt] found PlayerBattleStatus={:p}", obj));
            } else if cn == "PlayerStatus" && self.status.is_null() {
                self.status = obj;
                log_fmt(format_args!("[hunt] found PlayerStatus={:p}", obj));
            } else if cn == "PlayerBattleManager" && self.pbm.is_null() {
                self.pbm = obj;
                log_fmt(format_args!("[hunt] found PlayerBattleManager={:p}", obj));
            } else if cn == "PlayerSecondaryStatus" && self.pss.is_null() {
                self.pss = obj;
                log_fmt(format_args!("[hunt] found PlayerSecondaryStatus={:p}", obj));
            }

            if qi > 80 {
                continue;
            }

            let mut depth = 0;
            while !k.is_null() && depth < 4 {
                let mut f_iter: *mut c_void = ptr::null_mut();
                loop {
                    let f = unsafe { class_get_fields(k, &mut f_iter) };
                    if f.is_null() {
                        break;
                    }
                    let off = unsafe { (api.field_get_offset)(f) };
                    if !(0x10..=0x800).contains(&off) {
                        continue;
                    }
                    let fn_str = unsafe { cstr_to_str(field_get_name(f)) };
                    if fn_str.is_empty() {
                        continue;
                    }
                    let addr = (obj as usize) + (off as usize);
                    if !ptr_ok(addr, 8) {
                        continue;
                    }
                    let child = unsafe { *(addr as *const *mut c_void) };
                    if !unsafe { is_valid_il2cpp_obj(child) } {
                        continue;
                    }
                    let ck = unsafe { *(child as *const *mut c_void) };
                    if ck.is_null()
                        || (ck as usize) < 0x10000
                        || ((ck as usize) & 7) != 0
                        || !ptr_ok(ck as usize, 64)
                    {
                        continue;
                    }
                    if unsafe { class_is_valuetype(ck) } {
                        continue;
                    }
                    let ccn = unsafe { cstr_to_str(class_get_name(ck)) };
                    if ccn.is_empty() || !ccn.as_bytes()[0].is_ascii_alphabetic() {
                        continue;
                    }
                    if ccn.contains("Player")
                        || ccn.contains("Play")
                        || ccn.contains("Status")
                        || ccn.contains("Battle")
                        || ccn.contains("Actor")
                        || ccn.contains("Char")
                        || ccn.contains("Manager")
                    {
                        push(child, gc, &mut q);
                    }
                }
                k = match api.class_get_parent {
                    Some(parent_fn) => unsafe { parent_fn(k) },
                    None => ptr::null_mut(),
                };
                depth += 1;
            }
        }
    }

    /// Resolve the live game object graph (`PlayerManager`, `PlayManager`, `PlayerBattleStatus`, `PlayerBattleManager`, `PlayerSecondaryStatus`).
    /// Clears any previously cached instance pointers first so map/channel transitions never retain stale references.
    pub unsafe fn resolve(&mut self, gc: &mut GcScope) -> bool {
        self.player_mgr = ptr::null_mut();
        self.play_mgr = ptr::null_mut();
        self.status = ptr::null_mut();
        self.bstatus = ptr::null_mut();
        self.pbm = ptr::null_mut();
        self.pss = ptr::null_mut();
        self.pdm = ptr::null_mut();
        self.pam = ptr::null_mut();
        self.cm = ptr::null_mut();

        let Some(api) = api() else {
            return false;
        };
        self.domain = unsafe { (api.domain_get)() };
        if self.domain.is_null() {
            log_fmt(format_args!("[chain] domain_get null"));
            return false;
        }
        unsafe {
            (api.thread_attach)(self.domain);
        }

        self.image = unsafe { find_image(self.domain, "Assembly-CSharp") };
        self.player_mgr_class = unsafe { find_class(self.image, self.domain, "", "PlayerManager") };
        self.play_mgr_class = unsafe { find_class(self.image, self.domain, "", "PlayManager") };
        self.status_class = unsafe { find_class(self.image, self.domain, "", "PlayerStatus") };
        self.bstatus_class =
            unsafe { find_class(self.image, self.domain, "", "PlayerBattleStatus") };
        self.pbm_class = unsafe { find_class(self.image, self.domain, "", "PlayerBattleManager") };
        self.pss_class =
            unsafe { find_class(self.image, self.domain, "", "PlayerSecondaryStatus") };

        if unsafe { self.resolve_local_player(gc) } {
            return true;
        }

        if !self.player_mgr_class.is_null() {
            self.player_mgr = unsafe { self.resolve_singleton(self.player_mgr_class, gc) };
            if !self.player_mgr.is_null() {
                for fn_name in [
                    "playManager",
                    "_playManager",
                    "m_playManager",
                    "<playManager>k__BackingField",
                ] {
                    self.play_mgr = unsafe {
                        read_inst_ptr(self.player_mgr, self.player_mgr_class, fn_name, gc)
                    };
                    if !self.play_mgr.is_null() {
                        break;
                    }
                }
                if self.play_mgr.is_null() {
                    self.play_mgr = unsafe {
                        call_ptr0(
                            self.player_mgr_class,
                            self.player_mgr,
                            "get_playManager",
                            gc,
                        )
                    };
                }
                if self.pbm.is_null() {
                    self.pbm = unsafe {
                        read_inst_ptr(
                            self.player_mgr,
                            self.player_mgr_class,
                            "playerBattleManager",
                            gc,
                        )
                    };
                }
                if self.pbm.is_null() {
                    self.pbm = unsafe {
                        call_ptr0(
                            self.player_mgr_class,
                            self.player_mgr,
                            "get_PlayerBattleManager",
                            gc,
                        )
                    };
                }
            }
        }

        if self.play_mgr.is_null() && !self.play_mgr_class.is_null() {
            self.play_mgr = unsafe { self.resolve_singleton(self.play_mgr_class, gc) };
        }
        if self.pbm.is_null() && !self.pbm_class.is_null() {
            self.pbm = unsafe { self.resolve_singleton(self.pbm_class, gc) };
        }

        if !self.play_mgr.is_null() && !self.play_mgr_class.is_null() {
            for fn_name in ["playerStatus", "_playerStatus", "m_playerStatus", "status"] {
                self.status =
                    unsafe { read_inst_ptr(self.play_mgr, self.play_mgr_class, fn_name, gc) };
                if !self.status.is_null() {
                    break;
                }
            }
            for fn_name in [
                "playerBattleStatus",
                "_playerBattleStatus",
                "m_playerBattleStatus",
                "battleStatus",
            ] {
                self.bstatus =
                    unsafe { read_inst_ptr(self.play_mgr, self.play_mgr_class, fn_name, gc) };
                if !self.bstatus.is_null() {
                    break;
                }
            }
            for fn_name in ["playerSecondaryStatus", "_playerSecondaryStatus"] {
                self.pss =
                    unsafe { read_inst_ptr(self.play_mgr, self.play_mgr_class, fn_name, gc) };
                if !self.pss.is_null() {
                    break;
                }
            }
        }

        if self.bstatus.is_null() && self.status.is_null() && self.pbm.is_null() {
            unsafe {
                self.hunt_managers(gc);
            }
            if !self.play_mgr.is_null() && !self.play_mgr_class.is_null() {
                if self.status.is_null() {
                    self.status = unsafe {
                        read_inst_ptr(self.play_mgr, self.play_mgr_class, "playerStatus", gc)
                    };
                }
                if self.bstatus.is_null() {
                    self.bstatus = unsafe {
                        read_inst_ptr(self.play_mgr, self.play_mgr_class, "playerBattleStatus", gc)
                    };
                }
                if self.pss.is_null() {
                    self.pss = unsafe {
                        read_inst_ptr(
                            self.play_mgr,
                            self.play_mgr_class,
                            "playerSecondaryStatus",
                            gc,
                        )
                    };
                }
            }
        }

        if !self.bstatus.is_null() && self.status.is_null() && !self.bstatus_class.is_null() {
            self.status =
                unsafe { read_inst_ptr(self.bstatus, self.bstatus_class, "playerStatus", gc) };
        }

        self.pdm_class = unsafe { find_class(self.image, self.domain, "", "PlayerDataManager") };
        self.cm_class = unsafe { find_class(self.image, self.domain, "", "CharacterMove") };
        self.pam_class = unsafe { find_class(self.image, self.domain, "", "PlayerActionManager") };

        if !self.pdm_class.is_null() {
            self.pdm =
                unsafe { call_ptr0(self.pdm_class, ptr::null_mut(), "GetPlayerDataManager", gc) };
            if !self.pdm.is_null() {
                // CharacterMove is accessed via get_CharacterMove()
                self.cm = unsafe { call_ptr0(self.pdm_class, self.pdm, "get_CharacterMove", gc) };
                if self.cm.is_null() && !self.cm_class.is_null() {
                    self.cm =
                        unsafe { read_inst_ptr(self.pdm, self.pdm_class, "CharacterMove", gc) };
                }
                // PlayerActionManager is accessed via get_PlayerActionManager()
                self.pam =
                    unsafe { call_ptr0(self.pdm_class, self.pdm, "get_PlayerActionManager", gc) };
                if self.pam.is_null() && !self.pam_class.is_null() {
                    self.pam = unsafe {
                        read_inst_ptr(self.pdm, self.pdm_class, "PlayerActionManager", gc)
                    };
                }
                // PlayerStatus fallback via get_PlayerStatus()
                if self.status.is_null() {
                    self.status =
                        unsafe { call_ptr0(self.pdm_class, self.pdm, "get_PlayerStatus", gc) };
                    if self.status.is_null() && !self.status_class.is_null() {
                        self.status =
                            unsafe { read_inst_ptr(self.pdm, self.pdm_class, "PlayerStatus", gc) };
                    }
                }
            }
        }

        if self.pbm.is_null() && !self.pam.is_null() && !self.pam_class.is_null() {
            self.pbm = unsafe {
                let mut p = read_inst_ptr(self.pam, self.pam_class, "_playerBattleManager", gc);
                if p.is_null() {
                    p = read_inst_ptr(self.pam, self.pam_class, "playerBattleManager", gc);
                }
                if p.is_null() {
                    p = call_ptr0(self.pam_class, self.pam, "get_PlayerBattleManager", gc);
                }
                if p.is_null() {
                    let off_a0 = (self.pam as usize) + 0xA0;
                    if ptr_ok(off_a0, 8) {
                        p = gc.pin(validated_object(*(off_a0 as *const *mut c_void)));
                    }
                }
                p
            };
        }

        unsafe { self.has_valid_target() }
    }

    unsafe fn has_valid_target(&self) -> bool {
        [self.bstatus, self.status, self.pbm, self.pdm]
            .into_iter()
            .any(|obj| unsafe { is_valid_il2cpp_obj(obj) })
    }
}

/// Verify that `klass` is a non-null, 8-byte aligned, readable `Il2CppClass` metadata block.
#[inline]
pub fn is_valid_il2cpp_class(klass: *const c_void) -> bool {
    let addr = klass as usize;
    addr >= 0x10000 && (addr & 7) == 0 && ptr_ok(addr, 64)
}

/// Verify that `obj` is a non-null, 8-byte aligned, live IL2CPP managed object whose `Il2CppClass*` header is intact.
#[inline]
pub unsafe fn is_valid_il2cpp_obj(obj: *const c_void) -> bool {
    let addr = obj as usize;
    if addr < 0x10000 || (addr & 7) != 0 || !ptr_ok(addr, 16) {
        return false;
    }
    let klass = unsafe { *(addr as *const *const c_void) };
    is_valid_il2cpp_class(klass)
}

/// Read an `i32` field (`field_name`) from `obj` (`klass`).
pub unsafe fn read_i32_field(
    obj: *mut c_void,
    klass: *mut c_void,
    field_name: &str,
) -> Option<i32> {
    if !unsafe { is_valid_il2cpp_obj(obj) } || !is_valid_il2cpp_class(klass) {
        return None;
    }
    let api = api()?;
    let f = unsafe { get_field(klass, field_name) };
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
    Some(unsafe { *(addr as *const i32) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_rejects_readable_objects_with_invalid_class_headers() {
        let mut arena = [0usize; 4];
        let candidate = arena.as_mut_ptr().cast::<c_void>();
        let mut gc = GcScope::new();
        for invalid_class in [0usize, 0xAB, 0x1234] {
            arena[0] = invalid_class;
            unsafe {
                assert!(validated_object(candidate).is_null());
                assert!(gc.pin(candidate).is_null());
                let mut chain = GameChain::new();
                for slot in [
                    &mut chain.bstatus,
                    &mut chain.status,
                    &mut chain.pbm,
                    &mut chain.pdm,
                ] {
                    *slot = candidate;
                }
                assert!(!chain.has_valid_target());
                assert!(read_inst_ptr(candidate, ptr::null_mut(), "field", &mut gc).is_null());
            }
        }
    }
}
