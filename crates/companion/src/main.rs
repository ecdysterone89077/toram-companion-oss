//! Native Win32 GUI & CLI control surface (`companion.exe` — `"Game Companion (v96.1)"`).
//!
//! Features:
//! - Direct in-process 64-bit load & command dispatch via `wboot` library (zero child process latency)
//! - Real-time `SharedTelemetry` dashboard (`PID`, `Worker`, `HP/MP`, `Crit%`, `Hook Hits`, `Last Crit Dmg`)
//! - Live `[ON]` / `[OFF]` badges on every toggle button
//! - Toggleable Global Hotkeys (`F1`..`F8`) for in-combat control without alt-tabbing
//! - One-click Hot-Reload Core & Account Login Reset

#![allow(
    clippy::missing_safety_doc,
    non_camel_case_types,
    clippy::upper_case_acronyms,
    clippy::too_many_arguments
)]

use companion::{
    critical_damage_setting_label, mp_bonus_label, read_tail_lines, reset_toram_account_live,
    worker_status_label, CombatProfile, CommandPlan, CommandRunner, TargetSelection, WINDOW_TITLE,
};
use core::ffi::c_void;
use core::mem::{size_of, zeroed};
use core::ptr;
use core::sync::atomic::{AtomicBool, Ordering};
use protocol::{
    CommandId, TOGGLE_ALWAYS_CRIT, TOGGLE_ANTI_RIGIDITY, TOGGLE_INSTANT_ATK, TOGGLE_INVINCIBILITY,
    TOGGLE_NO_COLLISION, TOGGLE_RANGE_30M, TOGGLE_SPEED_BOOST, TOGGLE_UNLIMITED_MP,
};
use std::env;
use wboot::{find_all_process_ids, is_toram_identity, parse_cmd_spec, process_identity};
mod cli;
mod ui_layout;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Mutex, OnceLock};
use std::thread;

type HWND = *mut c_void;
type HINSTANCE = *mut c_void;
type HMENU = *mut c_void;
type HBRUSH = *mut c_void;
type HFONT = *mut c_void;
type WPARAM = usize;
type LPARAM = isize;
type LRESULT = isize;
type ATOM = u16;
type UINT = u32;
type DWORD = u32;
type BOOL = i32;

const WS_OVERLAPPED: DWORD = 0x0000_0000;
const WS_CAPTION: DWORD = 0x00C0_0000;
const WS_SYSMENU: DWORD = 0x0008_0000;
const WS_MINIMIZEBOX: DWORD = 0x0002_0000;
const WS_VISIBLE: DWORD = 0x1000_0000;
const WS_CHILD: DWORD = 0x4000_0000;
const WS_TABSTOP: DWORD = 0x0001_0000;
const WS_BORDER: DWORD = 0x0080_0000;
const WS_VSCROLL: DWORD = 0x0020_0000;

const ES_MULTILINE: DWORD = 0x0004;
const ES_AUTOVSCROLL: DWORD = 0x0040;
const ES_READONLY: DWORD = 0x0800;
const BS_PUSHBUTTON: DWORD = 0x0000;

const WM_CREATE: UINT = 0x0001;
const WM_DESTROY: UINT = 0x0002;
const WM_COMMAND: UINT = 0x0111;
const WM_TIMER: UINT = 0x0113;
const WM_HOTKEY: UINT = 0x0312;
const WM_SETFONT: UINT = 0x0030;
const WM_APP_RESULT: UINT = 0x8001;

const COLOR_WINDOW: i32 = 5;
const DEFAULT_GUI_FONT: i32 = 17;
const SW_SHOW: i32 = 5;
const IDC_ARROW: *const u16 = 32512 as *const u16;

// Virtual key codes F1..F8
const VK_F1: UINT = 0x70;
const VK_F2: UINT = 0x71;
const VK_F3: UINT = 0x72;
const VK_F4: UINT = 0x73;
const VK_F5: UINT = 0x74;
const VK_F6: UINT = 0x75;
const VK_F7: UINT = 0x76;
const VK_F8: UINT = 0x77;

const TIMER_TELEMETRY_ID: usize = 1;

#[repr(C)]
struct WNDCLASSEXW {
    cb_size: UINT,
    style: UINT,
    lpfn_wnd_proc: Option<unsafe extern "system" fn(HWND, UINT, WPARAM, LPARAM) -> LRESULT>,
    cb_cls_extra: i32,
    cb_wnd_extra: i32,
    h_instance: HINSTANCE,
    h_icon: *mut c_void,
    h_cursor: *mut c_void,
    hbr_background: HBRUSH,
    lpsz_menu_name: *const u16,
    lpsz_class_name: *const u16,
    h_icon_sm: *mut c_void,
}

#[repr(C)]
struct POINT {
    x: i32,
    y: i32,
}

#[repr(C)]
struct MSG {
    hwnd: HWND,
    message: UINT,
    w_param: WPARAM,
    l_param: LPARAM,
    time: DWORD,
    pt: POINT,
}

#[link(name = "user32")]
extern "system" {
    fn SetProcessDPIAware() -> BOOL;
    fn RegisterClassExW(lpwcx: *const WNDCLASSEXW) -> ATOM;
    fn CreateWindowExW(
        dw_ex_style: DWORD,
        lp_class_name: *const u16,
        lp_window_name: *const u16,
        dw_style: DWORD,
        x: i32,
        y: i32,
        n_width: i32,
        n_height: i32,
        h_wnd_parent: HWND,
        h_menu: HMENU,
        h_instance: HINSTANCE,
        lp_param: *mut c_void,
    ) -> HWND;
    fn ShowWindow(h_wnd: HWND, n_cmd_show: i32) -> BOOL;
    fn UpdateWindow(h_wnd: HWND) -> BOOL;
    fn GetMessageW(
        lp_msg: *mut MSG,
        h_wnd: HWND,
        w_msg_filter_min: UINT,
        w_msg_filter_max: UINT,
    ) -> BOOL;
    fn TranslateMessage(lp_msg: *const MSG) -> BOOL;
    fn DispatchMessageW(lp_msg: *const MSG) -> LRESULT;
    fn DefWindowProcW(h_wnd: HWND, msg: UINT, w_param: WPARAM, l_param: LPARAM) -> LRESULT;
    fn PostQuitMessage(n_exit_code: i32);
    fn PostMessageW(h_wnd: HWND, msg: UINT, w_param: WPARAM, l_param: LPARAM) -> BOOL;
    fn SendMessageW(h_wnd: HWND, msg: UINT, w_param: WPARAM, l_param: LPARAM) -> LRESULT;
    fn SetWindowTextW(h_wnd: HWND, lp_string: *const u16) -> BOOL;
    fn GetWindowTextW(h_wnd: HWND, lp_string: *mut u16, n_max_count: i32) -> i32;
    fn LoadCursorW(h_instance: HINSTANCE, lp_cursor_name: *const u16) -> *mut c_void;
    fn SetTimer(
        h_wnd: HWND,
        n_id_event: usize,
        u_elapse: UINT,
        lp_timer_func: *const c_void,
    ) -> usize;
    fn KillTimer(h_wnd: HWND, u_id_event: usize) -> BOOL;
    fn RegisterHotKey(h_wnd: HWND, id: i32, fs_modifiers: UINT, vk: UINT) -> BOOL;
    fn UnregisterHotKey(h_wnd: HWND, id: i32) -> BOOL;
}

#[link(name = "gdi32")]
extern "system" {
    fn GetStockObject(i: i32) -> *mut c_void;
}

#[link(name = "kernel32")]
extern "system" {
    fn GetModuleHandleW(lp_module_name: *const u16) -> HINSTANCE;
}

fn to_wstring(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(core::iter::once(0)).collect()
}

// Button IDs
const ID_BTN_INVINCIBILITY: usize = 1032;
const ID_BTN_SPEED: usize = 1068;
const ID_BTN_NO_COLLISION: usize = 1069;
const ID_BTN_CLEAR_DELAY: usize = 1070;
const ID_BTN_RANGE: usize = 1071;
const ID_BTN_INSTANT_ATK: usize = 1072;
const ID_BTN_ALWAYS_CRIT: usize = 1073;
const ID_BTN_UNLIMITED_MP: usize = 1074;
const ID_BTN_CRIT_175: usize = 1075;
const ID_BTN_CRIT_200: usize = 1076;
const ID_BTN_CRIT_250: usize = 1077;
const ID_BTN_CRIT_300: usize = 1078;
const ID_BTN_CRIT_RESET: usize = 1079;
const ID_BTN_CRIT_ADD_1000: usize = 1081;
const ID_BTN_MP_10: usize = 1085;
const ID_BTN_MP_25: usize = 1086;
const ID_BTN_MP_50: usize = 1087;
const ID_BTN_CRIT_ADD_100: usize = 1082;
const ID_BTN_CRIT_ADD_200: usize = 1083;
const ID_BTN_CRIT_ADD_300: usize = 1084;
const ID_BTN_HEAL: usize = 1002;
const ID_BTN_PROBE: usize = 1001;
const ID_BTN_UNLOAD: usize = 1000;
const ID_BTN_RUN_CUSTOM: usize = 2001;
const ID_BTN_REFRESH_LOG: usize = 2002;
const ID_BTN_RESET_ACCOUNT: usize = 2003;
const ID_BTN_HOTKEYS_TOGGLE: usize = 2004;
const ID_BTN_HOT_RELOAD: usize = 2005;
const ID_BTN_CAPTURE_ACTION: usize = 2006;
const ID_BTN_CAPTURE_RESPONSES: usize = 2007;
const ID_BTN_CAPTURE_PHASE: usize = 2008;
const ID_BTN_CAPTURE_STOP: usize = 2009;
const ID_EDIT_CUSTOM: usize = 3001;
const ID_COMBO_TARGET: usize = 3002;
const ID_BTN_CLIENTS: usize = 2100;
const ID_BTN_CANCEL: usize = 2101;
const ID_BTN_FEEDBACK_ON: usize = 2102;
const ID_BTN_FEEDBACK_OFF: usize = 2103;
const ID_BTN_RESET_METRICS: usize = 2104;
const ID_BTN_MP_NATIVE: usize = 2105;
const ID_BTN_SAVE_PLAN: usize = 2106;
const ID_BTN_LOAD_PLAN: usize = 2107;
const ID_PROFILE_NATIVE: usize = 2200;
const ID_PROFILE_BALANCED: usize = 2201;
const ID_PROFILE_BURST: usize = 2202;
const ID_PROFILE_RECOVERY: usize = 2203;
const ID_PROFILE_SAFE: usize = 2204;
/// First button id of the ported Faunas feature row. The feature index is
/// the offset from this base.
const ID_BTN_FAUNAS_BASE: usize = 4000;

/// The ported Faunas features shown in the companion window.
///
/// The row order matches the Faunas-port command band: feature `n` is
/// command `200 + n * 2` for OFF and `201 + n * 2` for ON. The one-click
/// preset is not a feature toggle, but it keeps its place in the row so the
/// button order matches the feature table.
const FAUNAS_UI_FEATURES: [(&str, &str); 24] = [
    ("crit", "Always Critical"),
    ("motion", "Max Motion"),
    ("stable", "Max Stability"),
    ("cspd", "Fast Cast"),
    ("range", "Long Range Attack"),
    ("penet", "Always Pierce"),
    ("resist", "Disable Resist"),
    ("mob_no_flee", "Mob No Flee"),
    ("mob_no_guard", "Mob No Guard"),
    ("mob_no_invincible", "Mob No Invincible"),
    ("mob_no_move", "Mob No Move"),
    ("skill_aoe", "Skill Full AOE"),
    ("skill_no_aoe", "Skill No AOE"),
    ("skill_burst", "Skill Burst"),
    ("prorate", "Mob Proration"),
    ("element", "Equip Element"),
    ("critdmg", "Critical Damage"),
    ("critmgcdmg", "Crit Magic Damage"),
    ("skip_battle", "Skip Battle"),
    ("no_domination", "No Domination"),
    ("no_drop_visual", "No Drop Visual"),
    ("one_click", "One-Click Loadout"),
    ("mp_cost", "MP Cost 10%"),
    ("skill_delay", "Skill Delay Override"),
];

/// The telemetry bit a ported Faunas feature occupies.
const fn faunas_bit(index: usize) -> u32 {
    1u32 << (8 + index)
}

/// The explicit command that sets a ported Faunas feature.
fn faunas_command(index: usize, enabled: bool) -> i32 {
    CommandId::faunas_toggle_command(index, enabled).unwrap_or(-4)
}

struct UiHandles {
    hwnd_main: usize,
    hwnd_telemetry: usize,
    hwnd_status: usize,
    hwnd_combat: usize,
    hwnd_targets: usize,
    target_options: Vec<TargetSelection>,
    hwnd_edit_custom: usize,
    hwnd_log: usize,
    btn_invincibility: usize,
    btn_crit: usize,
    btn_inst: usize,
    btn_mp: usize,
    btn_speed: usize,
    btn_no_collision: usize,
    btn_range: usize,
    btn_rigidity: usize,
    btn_hotkeys: usize,
    /// One button per ported Faunas feature, in the same order as
    /// `FAUNAS_UI_FEATURES`.
    btn_faunas: Vec<usize>,
}

static UI_HANDLES: OnceLock<Mutex<UiHandles>> = OnceLock::new();
static PENDING_UI_TEXT: OnceLock<Mutex<(String, String)>> = OnceLock::new();
static RUNNER: OnceLock<CommandRunner> = OnceLock::new();
static SELECTED_TARGET: Mutex<TargetSelection> = Mutex::new(TargetSelection::All);
static HOTKEYS_ENABLED: AtomicBool = AtomicBool::new(false);

const HOTKEY_MAP: &[(i32, UINT, i32)] = &[
    (1, VK_F1, 32), // F1 -> Invincibility
    (2, VK_F2, 73), // F2 -> stability and critical damage settings
    (3, VK_F3, 72), // F3 -> Instant Attack
    (4, VK_F4, 74), // F4 -> Unlimited MP
    (5, VK_F5, 68), // F5 -> Speed Boost
    (6, VK_F6, 69), // F6 -> No Collision
    (7, VK_F7, 71), // F7 -> Engage (assist-move)
    (8, VK_F8, 70), // F8 -> Clear Delay
];

unsafe fn set_hotkeys_active(hwnd: HWND, enable: bool) -> bool {
    let mut success = true;
    for &(hk_id, vk, _) in HOTKEY_MAP {
        if enable {
            unsafe {
                success &= RegisterHotKey(hwnd, hk_id, 0x4000, vk) != 0;
            }
        } else {
            unsafe {
                UnregisterHotKey(hwnd, hk_id);
            }
        }
    }
    if enable && !success {
        for &(hk_id, _, _) in HOTKEY_MAP {
            unsafe {
                UnregisterHotKey(hwnd, hk_id);
            }
        }
    }
    HOTKEYS_ENABLED.store(enable && success, Ordering::Relaxed);
    success
}

fn resolve_workspace_dir() -> PathBuf {
    let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    if let Ok(exe) = env::current_exe() {
        if let Some(parent) = exe.parent() {
            if parent.join("src").is_dir() {
                return parent.to_path_buf();
            }
            if let Some(grand) = parent.parent() {
                if grand.join("src").is_dir() {
                    return grand.to_path_buf();
                }
            }
        }
    }
    cwd
}

unsafe fn create_ctrl(
    parent: HWND,
    class_name: &str,
    text: &str,
    style: DWORD,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    id: usize,
    font: HFONT,
) -> HWND {
    let wclass = to_wstring(class_name);
    let wtext = to_wstring(text);
    let hwnd = unsafe {
        CreateWindowExW(
            0,
            wclass.as_ptr(),
            wtext.as_ptr(),
            style,
            x,
            y,
            w,
            h,
            parent,
            id as HMENU,
            ptr::null_mut(),
            ptr::null_mut(),
        )
    };
    if !hwnd.is_null() && !font.is_null() {
        unsafe {
            SendMessageW(hwnd, WM_SETFONT, font as WPARAM, 1);
        }
    }
    hwnd
}

unsafe fn set_wnd_str(hwnd_usize: usize, s: &str) {
    if hwnd_usize != 0 {
        let ws = to_wstring(s);
        unsafe {
            SetWindowTextW(hwnd_usize as HWND, ws.as_ptr());
        }
    }
}

fn selected_target() -> TargetSelection {
    SELECTED_TARGET
        .lock()
        .map(|v| v.clone())
        .unwrap_or_default()
}

/// Reads the live state of a ported Faunas feature from the telemetry block.
///
/// The Faunas command band is explicit ON/OFF rather than self-inverting, so
/// the button has to know the current state before it picks a command.
fn faunas_feature_active(index: usize) -> bool {
    let targets = selected_target().resolve().unwrap_or_default();
    let Some(target) = targets.first() else {
        return false;
    };
    let snap = protocol::read_telemetry(target.pid);
    snap.valid && snap.active_toggles & faunas_bit(index) != 0
}

fn trigger_async_cmd(cmd: i32) {
    trigger_async_cmds(vec![cmd]);
}
fn trigger_async_cmds(cmds: Vec<i32>) {
    let Some(runner) = RUNNER.get() else {
        return;
    };
    if runner.is_busy() {
        set_ui_status("Busy: a command batch is already running.", None);
        return;
    }
    let selection = selected_target();
    set_ui_status(&format!("Dispatching {} command(s)...", cmds.len()), None);
    thread::spawn(move || {
        let batch = runner.run_batch(&cmds, &selection);
        let mut details = batch.details();
        if let Some(last) = batch.results.last() {
            if !last.log_tail.is_empty() {
                details.push_str("\n\n");
                details.push_str(&last.log_tail);
            }
        }
        post_ui_update(batch.summary(), details);
    });
}
fn trigger_async_hot_reload() {
    trigger_async_cmds(vec![0, 1]);
}

fn custom_command_text() -> String {
    let mut buffer = [0u16; 2049];
    let count = UI_HANDLES
        .get()
        .and_then(|lock| lock.lock().ok())
        .map(|h| unsafe {
            GetWindowTextW(
                h.hwnd_edit_custom as HWND,
                buffer.as_mut_ptr(),
                buffer.len() as i32,
            )
        })
        .unwrap_or(0);
    String::from_utf16_lossy(&buffer[..count.max(0) as usize])
}

fn refresh_clients() {
    let previous = selected_target();
    let identities: Vec<_> = find_all_process_ids("ToramOnline.exe")
        .into_iter()
        .filter_map(process_identity)
        .filter(is_toram_identity)
        .collect();
    let mut options = vec![TargetSelection::All];
    let mut labels = vec![format!("All clients ({})", identities.len())];
    for identity in identities {
        labels.push(format!("PID {} | {}", identity.pid, identity.image_path));
        options.push(TargetSelection::Pinned(identity));
    }
    let selected = if let Some(index) = options.iter().position(|v| v == &previous) {
        index
    } else {
        labels.push("Selected client unavailable".into());
        options.push(previous);
        options.len() - 1
    };
    if let Some(mut h) = UI_HANDLES.get().and_then(|lock| lock.lock().ok()) {
        unsafe {
            SendMessageW(h.hwnd_targets as HWND, 0x014B, 0, 0);
        }
        for label in labels {
            let text = to_wstring(&label);
            unsafe {
                SendMessageW(h.hwnd_targets as HWND, 0x0143, 0, text.as_ptr() as LPARAM);
            }
        }
        unsafe {
            SendMessageW(h.hwnd_targets as HWND, 0x014E, selected, 0);
        }
        h.target_options = options;
    }
}
fn trigger_async_account_reset() {
    set_ui_status("Resetting Toram account login credentials...", None);
    thread::spawn(move || {
        let res = reset_toram_account_live();
        let details = if res.removed_values.is_empty() {
            res.message.clone()
        } else {
            format!(
                "{}\r\nRemoved keys:\r\n{}",
                res.message,
                res.removed_values.join("\r\n")
            )
        };
        post_ui_update(res.message, details);
    });
}

fn set_ui_status(status: &str, log: Option<&str>) {
    if let Some(lock) = UI_HANDLES.get() {
        if let Ok(handles) = lock.lock() {
            unsafe {
                set_wnd_str(handles.hwnd_status, status);
            }
            if let Some(l) = log {
                let normalized = l.replace("\r\n", "\n").replace('\n', "\r\n");
                unsafe {
                    set_wnd_str(handles.hwnd_log, &normalized);
                }
            }
        }
    }
}

fn post_ui_update(status: String, log: String) {
    if let Some(lock) = PENDING_UI_TEXT.get() {
        if let Ok(mut pending) = lock.lock() {
            *pending = (status, log);
        }
    }
    if let Some(lock) = UI_HANDLES.get() {
        if let Ok(handles) = lock.lock() {
            if handles.hwnd_main != 0 {
                unsafe {
                    PostMessageW(handles.hwnd_main as HWND, WM_APP_RESULT, 0, 0);
                }
            }
        }
    }
}

fn refresh_telemetry_ui() {
    let targets = selected_target().resolve().unwrap_or_default();
    let snapshots: Vec<_> = targets
        .iter()
        .map(|target| protocol::read_telemetry(target.pid))
        .collect();
    let snap = snapshots.first().copied().unwrap_or_default();
    let pid = targets.first().map(|target| target.pid).unwrap_or(0);
    let worker = if pid == 0 {
        "OFFLINE"
    } else if !snap.valid {
        "UNATTACHED"
    } else {
        worker_status_label(&snap, protocol::monotonic_ms())
    };
    let age = if snap.valid && snap.resource_tick_ms != 0 {
        format!(
            "{:.1}s",
            protocol::monotonic_ms().saturating_sub(snap.resource_tick_ms) as f64 / 1000.0
        )
    } else {
        "unknown".into()
    };
    let line = format!(
        "Clients {} | Focus PID {} {} | HP {}/{} MP {}/{} | Resource age {} | Crit {} | AMPR {}",
        targets.len(),
        pid,
        worker,
        snap.now_hp,
        snap.max_hp,
        snap.now_mp,
        snap.max_mp,
        age,
        critical_damage_setting_label(snap.crit_multiplier_pct),
        mp_bonus_label(snap.mp_ampr_bonus)
    );
    let combat = format!(
        "Feedback {}: accept {} reject {} unknown {} | Adaptive {} cap +{} | MP hits/getters {}/{} received {}",
        if snap.combat_monitor != 0 { "ON" } else { "OFF" },
        snap.responses_accepted, snap.responses_rejected, snap.responses_unknown,
        if snap.adaptive_enabled != 0 { "ON" } else { "OFF" }, snap.skill_cap,
        snap.mp_hit_calls, snap.mp_getter_calls,
        if snap.mp_sample_tick_ms == 0 { "?".into() } else { snap.received_mp.to_string() }
    );
    let line = if snap.valid {
        line
    } else {
        format!(
            "Clients {} | Focus PID {} {} | HP -/- MP -/- | No resource sample",
            targets.len(),
            pid,
            worker
        )
    };
    let combat = if snap.valid {
        combat
    } else {
        "No connected client | Awaiting combat feedback".to_owned()
    };
    let Some(h) = UI_HANDLES.get().and_then(|lock| lock.lock().ok()) else {
        return;
    };
    let badge = |bit| companion::feature_status_label(&snapshots, bit);
    unsafe {
        set_wnd_str(h.hwnd_telemetry, &line);
        set_wnd_str(h.hwnd_combat, &combat);
        for (handle, name, bit, hotkey, cmd) in [
            (
                h.btn_invincibility,
                "Invincible",
                TOGGLE_INVINCIBILITY,
                "F1",
                32,
            ),
            (h.btn_crit, "Stability + Crit", TOGGLE_ALWAYS_CRIT, "F2", 73),
            (h.btn_inst, "Instant Attack", TOGGLE_INSTANT_ATK, "F3", 72),
            (
                h.btn_mp,
                "MP Recovery (experimental)",
                TOGGLE_UNLIMITED_MP,
                "F4",
                74,
            ),
            (h.btn_speed, "Speed", TOGGLE_SPEED_BOOST, "F5", 68),
            (
                h.btn_no_collision,
                "No Collision",
                TOGGLE_NO_COLLISION,
                "F6",
                69,
            ),
            (
                h.btn_range,
                "Engage (assist-move)",
                TOGGLE_RANGE_30M,
                "F7",
                71,
            ),
            (
                h.btn_rigidity,
                "Anti-Rigidity",
                TOGGLE_ANTI_RIGIDITY,
                "F8",
                70,
            ),
        ] {
            set_wnd_str(
                handle,
                &format!("{name} [{}] ({hotkey} / {cmd})", badge(bit)),
            );
        }
        for (index, handle) in h.btn_faunas.iter().enumerate() {
            let active = snap.valid && snap.active_toggles & faunas_bit(index) != 0;
            set_wnd_str(
                *handle,
                &format!(
                    "{} [{}]",
                    FAUNAS_UI_FEATURES[index].1,
                    if active { "ON" } else { "OFF" }
                ),
            );
        }
    }
}
unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: UINT,
    w_param: WPARAM,
    l_param: LPARAM,
) -> LRESULT {
    match msg {
        WM_CREATE => {
            let handles = unsafe { ui_layout::create_controls(hwnd) };
            let _ = UI_HANDLES.set(Mutex::new(handles));
            unsafe {
                SetTimer(hwnd, TIMER_TELEMETRY_ID, 300, ptr::null());
            }
            refresh_clients();
            refresh_telemetry_ui();
            0
        }
        WM_TIMER => {
            if w_param == TIMER_TELEMETRY_ID {
                refresh_telemetry_ui();
            }
            0
        }
        WM_HOTKEY => {
            if !HOTKEYS_ENABLED.load(Ordering::Acquire) {
                return 0;
            }
            let hk_id = w_param as i32;
            for &(id, _, cmd) in HOTKEY_MAP {
                if id == hk_id {
                    trigger_async_cmd(cmd);
                    break;
                }
            }
            0
        }
        WM_COMMAND => {
            let id = w_param & 0xFFFF;
            match id {
                ID_BTN_INVINCIBILITY => trigger_async_cmd(32),
                ID_BTN_SPEED => trigger_async_cmd(68),
                ID_BTN_NO_COLLISION => trigger_async_cmd(69),
                ID_BTN_CLEAR_DELAY => trigger_async_cmd(70),
                ID_BTN_RANGE => trigger_async_cmd(71),
                ID_BTN_INSTANT_ATK => trigger_async_cmd(72),
                ID_BTN_ALWAYS_CRIT => trigger_async_cmd(73),
                ID_BTN_UNLIMITED_MP => trigger_async_cmd(74),
                ID_BTN_CRIT_175 => trigger_async_cmd(75),
                ID_BTN_CRIT_200 => trigger_async_cmd(76),
                ID_BTN_CRIT_250 => trigger_async_cmd(77),
                ID_BTN_CRIT_300 => trigger_async_cmd(78),
                ID_BTN_CRIT_RESET => trigger_async_cmd(79),
                ID_BTN_CRIT_ADD_1000 => trigger_async_cmd(CommandId::CritDamageAdd1000.as_i32()),
                ID_BTN_CRIT_ADD_100 => trigger_async_cmd(CommandId::CritDamageAdd100.as_i32()),
                ID_BTN_CRIT_ADD_200 => trigger_async_cmd(CommandId::CritDamageAdd200.as_i32()),
                ID_BTN_CRIT_ADD_300 => trigger_async_cmd(CommandId::CritDamageAdd300.as_i32()),
                ID_BTN_MP_10 => trigger_async_cmd(CommandId::MpAmprPlus10.as_i32()),
                ID_BTN_MP_25 => trigger_async_cmd(CommandId::MpAmprPlus25.as_i32()),
                ID_BTN_MP_50 => trigger_async_cmd(CommandId::MpAmprPlus50.as_i32()),
                id if id >= ID_BTN_FAUNAS_BASE
                    && id < ID_BTN_FAUNAS_BASE + FAUNAS_UI_FEATURES.len() =>
                {
                    let index = id - ID_BTN_FAUNAS_BASE;
                    let enabled = !faunas_feature_active(index);
                    trigger_async_cmd(faunas_command(index, enabled));
                }
                ID_BTN_HEAL => trigger_async_cmd(2),
                ID_BTN_PROBE => trigger_async_cmd(1),
                ID_BTN_UNLOAD => trigger_async_cmd(0),
                ID_BTN_HOT_RELOAD => trigger_async_hot_reload(),
                ID_BTN_RESET_ACCOUNT => trigger_async_account_reset(),
                ID_BTN_HOTKEYS_TOGGLE => {
                    let requested = !HOTKEYS_ENABLED.load(Ordering::Relaxed);
                    let registered = unsafe { set_hotkeys_active(hwnd, requested) };
                    let next = HOTKEYS_ENABLED.load(Ordering::Relaxed);
                    if let Some(lock) = UI_HANDLES.get() {
                        if let Ok(h) = lock.lock() {
                            unsafe {
                                set_wnd_str(
                                    h.btn_hotkeys,
                                    if next {
                                        "Hotkeys F1-F8: ON"
                                    } else {
                                        "Hotkeys F1-F8: OFF"
                                    },
                                );
                            }
                        }
                    }
                    set_ui_status(
                        if requested && !registered {
                            "Hotkey registration failed; another application may own F1-F8."
                        } else if next {
                            "Global Hotkeys F1-F8 ENABLED."
                        } else {
                            "Global Hotkeys F1-F8 DISABLED."
                        },
                        None,
                    );
                }
                ID_BTN_REFRESH_LOG => {
                    if let Some(r) = RUNNER.get() {
                        let tail = read_tail_lines(&r.wcore_log_path, 40);
                        set_ui_status("Refreshed wcore.log tail.", Some(&tail));
                    }
                }
                ID_COMBO_TARGET if (w_param >> 16) & 0xffff == 1 => {
                    let selected =
                        UI_HANDLES
                            .get()
                            .and_then(|lock| lock.lock().ok())
                            .and_then(|h| {
                                let index =
                                    unsafe { SendMessageW(h.hwnd_targets as HWND, 0x0147, 0, 0) };
                                h.target_options.get(index as usize).cloned()
                            });
                    if let Some(selected) = selected {
                        if let Ok(mut target) = SELECTED_TARGET.lock() {
                            *target = selected;
                        }
                    }
                    refresh_telemetry_ui();
                }
                ID_BTN_CLIENTS => {
                    refresh_clients();
                    refresh_telemetry_ui();
                }
                ID_BTN_CANCEL => {
                    if let Some(r) = RUNNER.get() {
                        r.cancel();
                    }
                    set_ui_status(
                        "Cancel requested; the current delivery will finish first.",
                        None,
                    );
                }
                ID_PROFILE_NATIVE => trigger_async_cmds(CombatProfile::Native.plan().commands),
                ID_PROFILE_BALANCED => trigger_async_cmds(CombatProfile::Balanced.plan().commands),
                ID_PROFILE_BURST => trigger_async_cmds(CombatProfile::Burst.plan().commands),
                ID_PROFILE_RECOVERY => trigger_async_cmds(CombatProfile::Recovery.plan().commands),
                ID_PROFILE_SAFE => trigger_async_cmds(CombatProfile::Safe.plan().commands),
                ID_BTN_FEEDBACK_ON => trigger_async_cmd(89),
                ID_BTN_FEEDBACK_OFF => trigger_async_cmd(90),
                ID_BTN_RESET_METRICS => trigger_async_cmd(92),
                ID_BTN_MP_NATIVE => trigger_async_cmd(91),
                ID_BTN_CAPTURE_ACTION => trigger_async_cmd(80),
                ID_BTN_CAPTURE_RESPONSES => trigger_async_cmd(82),
                ID_BTN_CAPTURE_PHASE => trigger_async_cmd(116),
                ID_BTN_CAPTURE_STOP => trigger_async_cmd(93),
                ID_BTN_RUN_CUSTOM => match parse_cmd_spec(&custom_command_text()) {
                    Ok(commands) => trigger_async_cmds(commands),
                    Err(error) => set_ui_status(&error, None),
                },
                ID_BTN_SAVE_PLAN => {
                    let result = (|| -> Result<(), String> {
                        let commands = parse_cmd_spec(&custom_command_text())?;
                        let runner = RUNNER.get().ok_or("Runner unavailable")?;
                        CommandPlan {
                            name: "Custom".into(),
                            commands,
                        }
                        .save(&runner.work_dir.join("commands.tcmp"))
                    })();
                    set_ui_status(
                        &match result {
                            Ok(()) => "Saved commands.tcmp in the release folder.".into(),
                            Err(e) => e,
                        },
                        None,
                    );
                }
                ID_BTN_LOAD_PLAN => {
                    let result = RUNNER
                        .get()
                        .ok_or_else(|| "Runner unavailable".to_string())
                        .and_then(|r| CommandPlan::load(&r.work_dir.join("commands.tcmp")));
                    match result {
                        Ok(plan) => {
                            let text = plan
                                .commands
                                .iter()
                                .map(i32::to_string)
                                .collect::<Vec<_>>()
                                .join("+");
                            if let Some(h) = UI_HANDLES.get().and_then(|lock| lock.lock().ok()) {
                                unsafe {
                                    set_wnd_str(h.hwnd_edit_custom, &text);
                                }
                            }
                            set_ui_status(
                                "Plan loaded. Run batch applies it to the selected clients.",
                                None,
                            );
                        }
                        Err(e) => set_ui_status(&e, None),
                    }
                }
                _ => {}
            }
            0
        }
        WM_APP_RESULT => {
            if let Some(lock) = PENDING_UI_TEXT.get() {
                if let Ok(pending) = lock.lock() {
                    set_ui_status(&pending.0, Some(&pending.1));
                }
            }
            refresh_telemetry_ui();
            0
        }
        WM_DESTROY => {
            unsafe {
                KillTimer(hwnd, TIMER_TELEMETRY_ID);
                set_hotkeys_active(hwnd, false);
                PostQuitMessage(0);
            }
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, w_param, l_param) },
    }
}

fn run_gui() -> i32 {
    // Use physical pixels so display scaling does not push the fixed panel off screen.
    unsafe {
        SetProcessDPIAware();
    }
    let work_dir = resolve_workspace_dir();
    let _ = RUNNER.set(CommandRunner::new(work_dir, None, None, None, None, 20_000));
    let _ = PENDING_UI_TEXT.set(Mutex::new((String::new(), String::new())));

    unsafe {
        let h_inst = GetModuleHandleW(ptr::null());
        let class_name = to_wstring("GameCompanionV94WndClass");
        let title = to_wstring(WINDOW_TITLE);

        let wc = WNDCLASSEXW {
            cb_size: size_of::<WNDCLASSEXW>() as UINT,
            style: 0,
            lpfn_wnd_proc: Some(wnd_proc),
            cb_cls_extra: 0,
            cb_wnd_extra: 0,
            h_instance: h_inst,
            h_icon: ptr::null_mut(),
            h_cursor: LoadCursorW(ptr::null_mut(), IDC_ARROW),
            hbr_background: (COLOR_WINDOW + 1) as usize as HBRUSH,
            lpsz_menu_name: ptr::null(),
            lpsz_class_name: class_name.as_ptr(),
            h_icon_sm: ptr::null_mut(),
        };

        if RegisterClassExW(&wc) == 0 {
            return 1;
        }

        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_VISIBLE;
        let hwnd = CreateWindowExW(
            0,
            class_name.as_ptr(),
            title.as_ptr(),
            style,
            120,
            120,
            1040,
            1040,
            ptr::null_mut(),
            ptr::null_mut(),
            h_inst,
            ptr::null_mut(),
        );
        if hwnd.is_null() {
            return 1;
        }

        ShowWindow(hwnd, SW_SHOW);
        UpdateWindow(hwnd);

        let mut msg: MSG = zeroed();
        while GetMessageW(&mut msg, ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        msg.w_param as i32
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    if let Some(code) = cli::run(&args) {
        return ExitCode::from(code);
    }
    ExitCode::from(run_gui().clamp(0, 255) as u8)
}
