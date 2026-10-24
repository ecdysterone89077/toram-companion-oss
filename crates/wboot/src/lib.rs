//! `wboot` — Remote 64-bit PEB-aware module loader, lock-free shadow DLL staging,
//! export RVA resolver, and direct in-process / CLI command dispatcher.
//!
//! Features:
//! - Resolves full 64-bit remote module base via `NtQueryInformationProcess` (`ProcessBasicInformation`)
//!   walking `PEB -> Ldr -> InLoadOrderModuleList` (`entry + 0x30 = DllBase`, `entry + 0x58 = BaseDllName`),
//!   avoiding `GetExitCodeThread` 32-bit `DWORD` truncation on high ASLR module bases.
//! - Stages a content-hashed shadow copy (`%TEMP%\wcore_live_<hash>.dll`) before load so
//!   `src\wcore.dll` and `target\...\wcore.dll` are never locked by `ToramOnline.exe` during rebuilds.
//! - Provides `dispatch_to_toram` for direct zero-subprocess invocation from `companion.exe`.

#![allow(clippy::missing_safety_doc, clippy::not_unsafe_ptr_arg_deref)]

use core::ffi::c_void;
use core::mem::{size_of, zeroed};
use core::ptr;
use core::sync::atomic::{AtomicU32, Ordering};
use protocol::CommandId;
use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

pub type BOOL = i32;
pub type DWORD = u32;
pub type HANDLE = *mut c_void;

pub const TH32CS_SNAPPROCESS: DWORD = 0x0000_0002;
pub const TH32CS_SNAPMODULE: DWORD = 0x0000_0008;
pub const TH32CS_SNAPMODULE32: DWORD = 0x0000_0010;
pub const INVALID_HANDLE_VALUE: HANDLE = (-1isize) as HANDLE;

pub const PROCESS_CREATE_THREAD: DWORD = 0x0002;
pub const PROCESS_VM_OPERATION: DWORD = 0x0008;
pub const PROCESS_VM_READ: DWORD = 0x0010;
pub const PROCESS_VM_WRITE: DWORD = 0x0020;
pub const PROCESS_QUERY_INFORMATION: DWORD = 0x0400;
pub const PROCESS_QUERY_LIMITED_INFORMATION: DWORD = 0x1000;

pub const MEM_COMMIT: DWORD = 0x1000;
pub const MEM_RESERVE: DWORD = 0x2000;
pub const MEM_RELEASE: DWORD = 0x8000;
pub const PAGE_READWRITE: DWORD = 0x04;

pub const WAIT_OBJECT_0: DWORD = 0x0000_0000;
pub const WAIT_TIMEOUT: DWORD = 0x0000_0102;
pub const STILL_ACTIVE: DWORD = 259;

#[repr(C)]
pub struct PROCESSENTRY32W {
    pub dw_size: DWORD,
    pub cnt_usage: DWORD,
    pub th32_process_id: DWORD,
    pub th32_default_heap_id: usize,
    pub th32_module_id: DWORD,
    pub cnt_threads: DWORD,
    pub th32_parent_process_id: DWORD,
    pub pc_pri_class_base: i32,
    pub dw_flags: DWORD,
    pub sz_exe_file: [u16; 260],
}

#[repr(C)]
pub struct MODULEENTRY32W {
    pub dw_size: DWORD,
    pub th32_module_id: DWORD,
    pub th32_process_id: DWORD,
    pub glbl_cnt_usage: DWORD,
    pub proccnt_usage: DWORD,
    pub mod_base_addr: *mut u8,
    pub mod_base_size: DWORD,
    pub h_module: HANDLE,
    pub sz_module: [u16; 256],
    pub sz_exe_path: [u16; 260],
}

#[repr(C)]
pub struct PROCESS_BASIC_INFORMATION {
    pub exit_status: i32,
    pub pad0: u32,
    pub peb_base_address: usize,
    pub affinity_mask: usize,
    pub base_priority: i32,
    pub pad1: u32,
    pub unique_process_id: usize,
    pub inherited_from_unique_process_id: usize,
}

#[repr(C)]
#[derive(Default)]
struct FileTime {
    low: u32,
    high: u32,
}

#[link(name = "kernel32")]
extern "system" {
    pub fn CreateToolhelp32Snapshot(dw_flags: DWORD, th32_process_id: DWORD) -> HANDLE;
    pub fn Process32FirstW(h_snapshot: HANDLE, lppe: *mut PROCESSENTRY32W) -> BOOL;
    pub fn Process32NextW(h_snapshot: HANDLE, lppe: *mut PROCESSENTRY32W) -> BOOL;
    pub fn Module32FirstW(h_snapshot: HANDLE, lpme: *mut MODULEENTRY32W) -> BOOL;
    pub fn Module32NextW(h_snapshot: HANDLE, lpme: *mut MODULEENTRY32W) -> BOOL;
    pub fn OpenProcess(
        dw_desired_access: DWORD,
        b_inherit_handle: BOOL,
        dw_process_id: DWORD,
    ) -> HANDLE;
    pub fn VirtualAllocEx(
        h_process: HANDLE,
        lp_address: *mut c_void,
        dw_size: usize,
        fl_allocation_type: DWORD,
        fl_protect: DWORD,
    ) -> *mut c_void;
    pub fn VirtualFreeEx(
        h_process: HANDLE,
        lp_address: *mut c_void,
        dw_size: usize,
        dw_free_type: DWORD,
    ) -> BOOL;
    pub fn WriteProcessMemory(
        h_process: HANDLE,
        lp_base_address: *mut c_void,
        lp_buffer: *const c_void,
        n_size: usize,
        lp_number_of_bytes_written: *mut usize,
    ) -> BOOL;
    pub fn ReadProcessMemory(
        h_process: HANDLE,
        lp_base_address: *const c_void,
        lp_buffer: *mut c_void,
        n_size: usize,
        lp_number_of_bytes_read: *mut usize,
    ) -> BOOL;
    pub fn CreateRemoteThread(
        h_process: HANDLE,
        lp_thread_attributes: *const c_void,
        dw_stack_size: usize,
        lp_start_address: *const c_void,
        lp_parameter: *mut c_void,
        dw_creation_flags: DWORD,
        lp_thread_id: *mut DWORD,
    ) -> HANDLE;
    pub fn WaitForSingleObject(h_handle: HANDLE, dw_milliseconds: DWORD) -> DWORD;
    pub fn GetExitCodeThread(h_thread: HANDLE, lp_exit_code: *mut DWORD) -> BOOL;
    pub fn GetExitCodeProcess(h_process: HANDLE, lp_exit_code: *mut DWORD) -> BOOL;
    fn QueryFullProcessImageNameW(
        process: HANDLE,
        flags: DWORD,
        path: *mut u16,
        size: *mut DWORD,
    ) -> BOOL;
    fn GetProcessTimes(
        process: HANDLE,
        created: *mut FileTime,
        exited: *mut FileTime,
        kernel: *mut FileTime,
        user: *mut FileTime,
    ) -> BOOL;
    fn GetProcessId(process: HANDLE) -> DWORD;
    pub fn CloseHandle(h_object: HANDLE) -> BOOL;
    fn CreateMutexW(attributes: *const c_void, initial_owner: BOOL, name: *const u16) -> HANDLE;
    fn ReleaseMutex(mutex: HANDLE) -> BOOL;
    pub fn GetModuleHandleW(lp_module_name: *const u16) -> HANDLE;
    pub fn GetProcAddress(h_module: HANDLE, lp_proc_name: *const u8) -> *const c_void;
    pub fn GetLastError() -> DWORD;
    pub fn Sleep(dw_milliseconds: DWORD);
}

#[link(name = "ntdll")]
extern "system" {
    pub fn NtQueryInformationProcess(
        process_handle: HANDLE,
        process_information_class: u32,
        process_information: *mut c_void,
        process_information_length: u32,
        return_length: *mut u32,
    ) -> i32;
}

// -----------------------------------------------------------------------------
// Pure / Deterministically Testable Result & Lifetime Logic
// -----------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteWaitStage {
    LoadLibrary,
    WcoreInit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemoteThreadWaitInput {
    pub wait_result: DWORD,
    pub wait_last_error: DWORD,
    pub exit_code_ok: bool,
    pub exit_code_last_error: DWORD,
    pub thread_exit_code: DWORD,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemoteThreadWaitOutcome {
    pub completed: bool,
    pub timed_out: bool,
    pub release_remote_page: bool,
    pub process_exit_code: i32,
}

pub fn evaluate_remote_thread_wait(
    stage: RemoteWaitStage,
    input: RemoteThreadWaitInput,
) -> RemoteThreadWaitOutcome {
    if input.wait_result == WAIT_TIMEOUT {
        return RemoteThreadWaitOutcome {
            completed: false,
            timed_out: true,
            release_remote_page: false,
            process_exit_code: match stage {
                RemoteWaitStage::LoadLibrary => 6,
                RemoteWaitStage::WcoreInit => 11,
            },
        };
    }

    if input.wait_result != WAIT_OBJECT_0 {
        return RemoteThreadWaitOutcome {
            completed: false,
            timed_out: false,
            release_remote_page: false,
            process_exit_code: match stage {
                RemoteWaitStage::LoadLibrary => 7,
                RemoteWaitStage::WcoreInit => 12,
            },
        };
    }

    if !input.exit_code_ok {
        return RemoteThreadWaitOutcome {
            completed: false,
            timed_out: false,
            release_remote_page: false,
            process_exit_code: match stage {
                RemoteWaitStage::LoadLibrary => 8,
                RemoteWaitStage::WcoreInit => 13,
            },
        };
    }

    if input.thread_exit_code == STILL_ACTIVE {
        return RemoteThreadWaitOutcome {
            completed: false,
            timed_out: false,
            release_remote_page: false,
            process_exit_code: match stage {
                RemoteWaitStage::LoadLibrary => 9,
                RemoteWaitStage::WcoreInit => 14,
            },
        };
    }

    match stage {
        RemoteWaitStage::LoadLibrary => {
            if input.thread_exit_code == 0 {
                RemoteThreadWaitOutcome {
                    completed: false,
                    timed_out: false,
                    release_remote_page: true,
                    process_exit_code: 9,
                }
            } else {
                RemoteThreadWaitOutcome {
                    completed: true,
                    timed_out: false,
                    release_remote_page: true,
                    process_exit_code: 0,
                }
            }
        }
        RemoteWaitStage::WcoreInit => {
            let signed_status = input.thread_exit_code as i32;
            RemoteThreadWaitOutcome {
                completed: signed_status == 0,
                timed_out: false,
                release_remote_page: false,
                process_exit_code: if signed_status == 0 { 0 } else { 14 },
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiagnosticFallbackInput {
    pub target_pid: DWORD,
    pub target_process_known: bool,
    pub target_process_alive: bool,
    pub dll_path_valid: bool,
    pub dll_exists_on_disk: bool,
    pub module_attached: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiagnosticFallbackOutcome {
    pub allowed: bool,
    pub process_exit_code: i32,
}

pub fn evaluate_diagnostic_fallback(input: DiagnosticFallbackInput) -> DiagnosticFallbackOutcome {
    if input.target_pid == 0 || !input.target_process_known || !input.target_process_alive {
        return DiagnosticFallbackOutcome {
            allowed: false,
            process_exit_code: 2,
        };
    }
    if !input.dll_path_valid || !input.dll_exists_on_disk {
        return DiagnosticFallbackOutcome {
            allowed: false,
            process_exit_code: 3,
        };
    }
    if !input.module_attached {
        return DiagnosticFallbackOutcome {
            allowed: false,
            process_exit_code: 9,
        };
    }
    DiagnosticFallbackOutcome {
        allowed: true,
        process_exit_code: 0,
    }
}

// -----------------------------------------------------------------------------
// PE64 On-Disk RVA Export Parser & Shadow DLL Staging
// -----------------------------------------------------------------------------

fn rva_to_file_offset(buf: &[u8], rva: u32) -> Option<usize> {
    if buf.len() < 0x100 {
        return None;
    }
    let e_lfanew = u32::from_le_bytes(buf.get(0x3C..0x40)?.try_into().ok()?) as usize;
    if e_lfanew + 0x18 > buf.len() {
        return None;
    }
    let num_sections =
        u16::from_le_bytes(buf.get(e_lfanew + 6..e_lfanew + 8)?.try_into().ok()?) as usize;
    let opt_hdr_size =
        u16::from_le_bytes(buf.get(e_lfanew + 20..e_lfanew + 22)?.try_into().ok()?) as usize;
    let sec_base = e_lfanew + 0x18 + opt_hdr_size;

    for i in 0..num_sections {
        let sh = sec_base + i * 40;
        if sh + 40 > buf.len() {
            break;
        }
        let virt_size = u32::from_le_bytes(buf[sh + 8..sh + 12].try_into().ok()?);
        let virt_addr = u32::from_le_bytes(buf[sh + 12..sh + 16].try_into().ok()?);
        let raw_size = u32::from_le_bytes(buf[sh + 16..sh + 20].try_into().ok()?);
        let raw_ptr = u32::from_le_bytes(buf[sh + 20..sh + 24].try_into().ok()?);
        let span = virt_size.max(raw_size);
        if rva >= virt_addr && rva < virt_addr.saturating_add(span) {
            let off = (rva - virt_addr) as usize + (raw_ptr as usize);
            if off < buf.len() {
                return Some(off);
            }
        }
    }
    None
}

/// Parse an on-disk PE64 DLL to find the Relative Virtual Address (RVA) of `export_name`.
pub fn find_export_rva_in_bytes(buf: &[u8], export_name: &str) -> Option<u32> {
    if buf.len() < 0x100 || &buf[0..2] != b"MZ" {
        return None;
    }
    let e_lfanew = u32::from_le_bytes(buf.get(0x3C..0x40)?.try_into().ok()?) as usize;
    if e_lfanew + 0x90 > buf.len() || &buf[e_lfanew..e_lfanew + 4] != b"PE\0\0" {
        return None;
    }
    let exp_rva = u32::from_le_bytes(buf.get(e_lfanew + 0x88..e_lfanew + 0x8C)?.try_into().ok()?);
    let exp_size = u32::from_le_bytes(buf.get(e_lfanew + 0x8C..e_lfanew + 0x90)?.try_into().ok()?);
    if exp_rva == 0 || exp_size == 0 {
        return None;
    }

    let exp_off = rva_to_file_offset(buf, exp_rva)?;
    if exp_off + 0x28 > buf.len() {
        return None;
    }

    let num_funcs =
        u32::from_le_bytes(buf[exp_off + 0x14..exp_off + 0x18].try_into().ok()?) as usize;
    let num_names =
        u32::from_le_bytes(buf[exp_off + 0x18..exp_off + 0x1C].try_into().ok()?) as usize;
    let funcs_rva = u32::from_le_bytes(buf[exp_off + 0x1C..exp_off + 0x20].try_into().ok()?);
    let names_rva = u32::from_le_bytes(buf[exp_off + 0x20..exp_off + 0x24].try_into().ok()?);
    let ords_rva = u32::from_le_bytes(buf[exp_off + 0x24..exp_off + 0x28].try_into().ok()?);

    let funcs_off = rva_to_file_offset(buf, funcs_rva)?;
    let names_off = rva_to_file_offset(buf, names_rva)?;
    let ords_off = rva_to_file_offset(buf, ords_rva)?;

    let target_bytes = export_name.as_bytes();
    for i in 0..num_names {
        let nr_pos = names_off + i * 4;
        if nr_pos + 4 > buf.len() {
            break;
        }
        let name_rva = u32::from_le_bytes(buf[nr_pos..nr_pos + 4].try_into().ok()?);
        let Some(name_off) = rva_to_file_offset(buf, name_rva) else {
            continue;
        };
        if name_off + target_bytes.len() + 1 > buf.len() {
            continue;
        }
        if &buf[name_off..name_off + target_bytes.len()] == target_bytes
            && buf[name_off + target_bytes.len()] == 0
        {
            let ord_pos = ords_off + i * 2;
            if ord_pos + 2 > buf.len() {
                return None;
            }
            let ord = u16::from_le_bytes(buf[ord_pos..ord_pos + 2].try_into().ok()?) as usize;
            if ord >= num_funcs {
                return None;
            }
            let fn_pos = funcs_off + ord * 4;
            if fn_pos + 4 > buf.len() {
                return None;
            }
            let fn_rva = u32::from_le_bytes(buf[fn_pos..fn_pos + 4].try_into().ok()?);
            return if fn_rva != 0 { Some(fn_rva) } else { None };
        }
    }
    None
}

pub const SHADOW_DLL_PREFIX: &str = "dxgi_ext_";

pub fn find_export_rva(dll_path: &Path, export_name: &str) -> Option<u32> {
    let bytes = fs::read(dll_path).ok()?;
    find_export_rva_in_bytes(&bytes, export_name)
}

/// Resolve the remote thread entrypoint RVA from `dll_path`, preferring the neutral
/// `"ogl_drv_init"` export and falling back to legacy `"wcore_init"`.
pub fn resolve_init_export_rva(dll_path: &Path) -> Option<u32> {
    let bytes = fs::read(dll_path).ok()?;
    find_export_rva_in_bytes(&bytes, "ogl_drv_init")
        .or_else(|| find_export_rva_in_bytes(&bytes, "wcore_init"))
}

/// Check whether a module filename matches the neutral shadow DLL naming pattern (`dxgi_ext_<16-hex>.dll`).
pub fn is_shadow_dll_filename(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let Some(hex_part) = lower
        .strip_prefix(SHADOW_DLL_PREFIX)
        .and_then(|s| s.strip_suffix(".dll"))
    else {
        return false;
    };
    hex_part.len() == 16 && hex_part.bytes().all(|b| b.is_ascii_hexdigit())
}

fn is_core_dll_filename(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if is_shadow_dll_filename(&lower) || lower == "wcore.dll" {
        return true;
    }
    if let Some(hash) = lower
        .strip_prefix("wcore_live_")
        .and_then(|s| s.strip_suffix(".dll"))
    {
        return hash.len() == 16 && hash.bytes().all(|b| b.is_ascii_hexdigit());
    }
    lower
        .strip_prefix("wcore")
        .and_then(|s| s.strip_suffix(".dll"))
        .is_some_and(|version| !version.is_empty() && version.bytes().all(|b| b.is_ascii_digit()))
}

fn core_filename_hash(name: &str) -> Option<u64> {
    let lower = name.to_ascii_lowercase();
    let hash = lower
        .strip_prefix(SHADOW_DLL_PREFIX)
        .or_else(|| lower.strip_prefix("wcore_live_"))?
        .strip_suffix(".dll")?;
    if hash.len() != 16 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u64::from_str_radix(hash, 16).ok()
}

#[derive(Debug, Clone)]
struct LoadedModule {
    name: String,
    path: String,
    base: usize,
}

#[derive(Debug, PartialEq, Eq)]
enum CoreLoadDecision {
    Load,
    Reuse(usize),
    RestartRequired(String),
}

fn normalized_module_path(path: &str) -> String {
    path.strip_prefix(r"\\?\")
        .unwrap_or(path)
        .replace('/', "\\")
        .to_ascii_lowercase()
}

fn evaluate_core_load(
    requested_name: &str,
    requested_path: &str,
    modules: &[LoadedModule],
) -> CoreLoadDecision {
    let cores: Vec<_> = modules
        .iter()
        .filter(|m| is_core_dll_filename(&m.name) || m.name.eq_ignore_ascii_case(requested_name))
        .collect();
    match cores.as_slice() {
        [] => CoreLoadDecision::Load,
        [loaded]
            if core_filename_hash(requested_name).is_some()
                && loaded.base != 0
                && loaded.name.eq_ignore_ascii_case(requested_name)
                && normalized_module_path(&loaded.path)
                    == normalized_module_path(requested_path) =>
        {
            CoreLoadDecision::Reuse(loaded.base)
        }
        _ => CoreLoadDecision::RestartRequired(
            cores
                .iter()
                .map(|m| m.name.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        ),
    }
}

/// An incomplete module inventory must never be interpreted as an empty one.
fn loaded_modules(pid: DWORD) -> Result<Vec<LoadedModule>, String> {
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32, pid);
        if snap.is_null() || snap == INVALID_HANDLE_VALUE {
            return Err(format!("module snapshot failed (error {})", GetLastError()));
        }
        let mut me: MODULEENTRY32W = zeroed();
        me.dw_size = size_of::<MODULEENTRY32W>() as DWORD;
        if Module32FirstW(snap, &mut me) == 0 {
            let err = GetLastError();
            CloseHandle(snap);
            return Err(format!("module enumeration failed (error {})", err));
        }
        let mut modules = Vec::new();
        loop {
            modules.push(LoadedModule {
                name: wstr_to_string(&me.sz_module),
                path: wstr_to_string(&me.sz_exe_path),
                base: me.mod_base_addr as usize,
            });
            if Module32NextW(snap, &mut me) == 0 {
                let err = GetLastError();
                CloseHandle(snap);
                return if err == 18 {
                    // ERROR_NO_MORE_FILES
                    Ok(modules)
                } else {
                    Err(format!("module enumeration was incomplete (error {})", err))
                };
            }
        }
    }
}

struct DispatchGuard(HANDLE);

impl DispatchGuard {
    fn acquire(pid: DWORD) -> Result<Self, String> {
        let name: Vec<u16> = format!("Local\\ToramCoreDispatch_{}\0", pid)
            .encode_utf16()
            .collect();
        unsafe {
            let handle = CreateMutexW(ptr::null(), 0, name.as_ptr());
            if handle.is_null() {
                return Err(format!("Dispatch lock failed (error {})", GetLastError()));
            }
            match WaitForSingleObject(handle, 0) {
                WAIT_OBJECT_0 | 0x80 => Ok(Self(handle)), // WAIT_ABANDONED also grants ownership.
                WAIT_TIMEOUT => {
                    CloseHandle(handle);
                    Err("Another loader command is already in flight for this process.".to_string())
                }
                _ => {
                    let err = GetLastError();
                    CloseHandle(handle);
                    Err(format!("Dispatch lock wait failed (error {})", err))
                }
            }
        }
    }
}

impl Drop for DispatchGuard {
    fn drop(&mut self) {
        unsafe {
            ReleaseMutex(self.0);
            CloseHandle(self.0);
        }
    }
}

/// Compute a 64-bit FNV-1a hash of a byte slice for lock-free shadow DLL naming.
pub fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

/// Copy `src_dll` to `%TEMP%\dxgi_ext_<hash>.dll` so `ToramOnline.exe` never locks `src\wcore.dll`
/// and no module inside the game process carries `"wcore"` or `"toram"` in its name.
pub fn prepare_shadow_dll(src_dll: &Path) -> Option<PathBuf> {
    let bytes = fs::read(src_dll).ok()?;
    let hash = fnv1a_64(&bytes);
    let shadow_path = env::temp_dir().join(format!("{}{:016x}.dll", SHADOW_DLL_PREFIX, hash));
    if !shadow_path.is_file() {
        let _ = fs::write(&shadow_path, &bytes);
    }
    if fs::read(&shadow_path).ok().as_deref() == Some(bytes.as_slice()) {
        Some(shadow_path)
    } else {
        None
    }
}

// -----------------------------------------------------------------------------
// Process & Remote Module Discovery
// -----------------------------------------------------------------------------

fn wstr_to_string(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

/// Find the PID of a running process by executable name (e.g. `"ToramOnline.exe"`).
/// Every PID running under `exe_name` (multi-client power: all game instances,
/// oldest first). Empty when none is running.
pub fn find_all_process_ids(exe_name: &str) -> Vec<DWORD> {
    let mut found = Vec::new();
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap.is_null() || snap == INVALID_HANDLE_VALUE {
            return found;
        }
        let mut pe: PROCESSENTRY32W = zeroed();
        pe.dw_size = size_of::<PROCESSENTRY32W>() as DWORD;

        if Process32FirstW(snap, &mut pe) != 0 {
            loop {
                let name = wstr_to_string(&pe.sz_exe_file);
                if name.eq_ignore_ascii_case(exe_name) {
                    found.push(pe.th32_process_id);
                }
                if Process32NextW(snap, &mut pe) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snap);
    }
    found
}

/// Find the PID of a running process by executable name (e.g. `"ToramOnline.exe"`).
/// First match wins; use `find_all_process_ids` to reach every client.
pub fn find_process_id(exe_name: &str) -> DWORD {
    find_all_process_ids(exe_name)
        .into_iter()
        .next()
        .unwrap_or(0)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub created_at: u64,
    pub image_path: String,
}

unsafe fn identity_from_handle(process: HANDLE) -> Option<ProcessIdentity> {
    let mut path = vec![0u16; 32768];
    let mut len = path.len() as u32;
    let (mut created, mut exited, mut kernel, mut user) = (
        FileTime::default(),
        FileTime::default(),
        FileTime::default(),
        FileTime::default(),
    );
    let mut exit = 0;
    let mut ok_image =
        unsafe { QueryFullProcessImageNameW(process, 0, path.as_mut_ptr(), &mut len) };
    if ok_image == 0 {
        len = path.len() as u32;
        ok_image = unsafe { QueryFullProcessImageNameW(process, 1, path.as_mut_ptr(), &mut len) };
    }
    if ok_image == 0
        || unsafe { GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user) }
            == 0
        || unsafe { GetExitCodeProcess(process, &mut exit) } == 0
        || exit != STILL_ACTIVE
    {
        return None;
    }
    Some(ProcessIdentity {
        pid: unsafe { GetProcessId(process) },
        created_at: ((created.high as u64) << 32) | created.low as u64,
        image_path: String::from_utf16(&path[..len as usize]).ok()?,
    })
}

pub fn process_identity(pid: u32) -> Option<ProcessIdentity> {
    if pid == 0 {
        return None;
    }
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return None;
        }
        let identity = identity_from_handle(process);
        CloseHandle(process);
        identity
    }
}

fn same_process(expected: &ProcessIdentity, actual: &ProcessIdentity) -> bool {
    expected.pid == actual.pid
        && expected.created_at == actual.created_at
        && normalized_module_path(&expected.image_path)
            == normalized_module_path(&actual.image_path)
}

pub fn is_toram_identity(identity: &ProcessIdentity) -> bool {
    Path::new(&identity.image_path)
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case("ToramOnline.exe"))
}

/// Resolve a remote 64-bit module base address in `pid`:
/// 1. First tries `CreateToolhelp32Snapshot(TH32CS_SNAPMODULE)`
/// 2. Falls back to `NtQueryInformationProcess` (`ProcessBasicInformation`) + `ReadProcessMemory`
///    walking `PEB -> Ldr -> InLoadOrderModuleList` (works even when Toolhelp is blocked).
pub fn find_remote_module_base(pid: DWORD, h_proc: HANDLE, module_name: &str) -> usize {
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32, pid);
        if !snap.is_null() && snap != INVALID_HANDLE_VALUE {
            let mut me: MODULEENTRY32W = zeroed();
            me.dw_size = size_of::<MODULEENTRY32W>() as DWORD;
            if Module32FirstW(snap, &mut me) != 0 {
                loop {
                    let mname = wstr_to_string(&me.sz_module);
                    let mpath = wstr_to_string(&me.sz_exe_path);
                    if mname.eq_ignore_ascii_case(module_name)
                        || mpath
                            .to_ascii_lowercase()
                            .ends_with(&module_name.to_ascii_lowercase())
                    {
                        let base = me.mod_base_addr as usize;
                        CloseHandle(snap);
                        if base != 0 {
                            return base;
                        }
                        break;
                    }
                    if Module32NextW(snap, &mut me) == 0 {
                        break;
                    }
                }
            }
            CloseHandle(snap);
        }

        // Fallback: Remote PEB walk via NtQueryInformationProcess + ReadProcessMemory
        if h_proc.is_null() {
            return 0;
        }
        let mut pbi: PROCESS_BASIC_INFORMATION = zeroed();
        let mut ret_len = 0u32;
        let status = NtQueryInformationProcess(
            h_proc,
            0,
            (&mut pbi as *mut PROCESS_BASIC_INFORMATION).cast(),
            size_of::<PROCESS_BASIC_INFORMATION>() as u32,
            &mut ret_len,
        );
        if status != 0 || pbi.peb_base_address == 0 {
            return 0;
        }

        let read_usize = |addr: usize| -> Option<usize> {
            let mut val = 0usize;
            let mut read = 0usize;
            if ReadProcessMemory(
                h_proc,
                addr as *const c_void,
                (&mut val as *mut usize).cast(),
                8,
                &mut read,
            ) != 0
                && read == 8
            {
                Some(val)
            } else {
                None
            }
        };

        let Some(ldr) = read_usize(pbi.peb_base_address + 0x18) else {
            return 0;
        };
        if ldr == 0 {
            return 0;
        }
        let head = ldr + 0x10;
        let Some(mut curr) = read_usize(head) else {
            return 0;
        };

        let mut steps = 0usize;
        while curr != 0 && curr != head && steps < 512 {
            steps += 1;
            let dll_base = read_usize(curr + 0x30).unwrap_or(0);
            let mut name_len_u16 = 0u16;
            let mut read = 0usize;
            let _ = ReadProcessMemory(
                h_proc,
                (curr + 0x58) as *const c_void,
                (&mut name_len_u16 as *mut u16).cast(),
                2,
                &mut read,
            );
            let name_buf_ptr = read_usize(curr + 0x60).unwrap_or(0);
            let char_len = (name_len_u16 as usize) / 2;

            if dll_base != 0 && (1..128).contains(&char_len) && name_buf_ptr != 0 {
                let mut wbuf = vec![0u16; char_len];
                let mut r2 = 0usize;
                if ReadProcessMemory(
                    h_proc,
                    name_buf_ptr as *const c_void,
                    wbuf.as_mut_ptr().cast(),
                    char_len * 2,
                    &mut r2,
                ) != 0
                    && r2 == char_len * 2
                {
                    let remote_name = String::from_utf16_lossy(&wbuf);
                    if remote_name.eq_ignore_ascii_case(module_name) {
                        return dll_base;
                    }
                }
            }
            curr = match read_usize(curr) {
                Some(next) => next,
                None => break,
            };
        }
        0
    }
}

/// Write a line to both stdout and `wboot.log`.
pub fn log_both(msg: &str) {
    println!("{}", msg);
    let log_path = env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("wboot.log");
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(log_path) {
        let _ = writeln!(f, "{}", msg);
    }
}

/// Resolve the default `wcore.dll` (or `wcore92.dll`) path.
pub fn resolve_default_dll_path() -> Option<PathBuf> {
    let exe_dir = env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."));
    let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    for candidate in [
        exe_dir.join("wcore.dll"),
        exe_dir.join("wcore92.dll"),
        cwd.join("src").join("wcore.dll"),
        cwd.join("src").join("wcore92.dll"),
        cwd.join("wcore.dll"),
        cwd.join("wcore92.dll"),
    ] {
        if candidate.is_file() {
            return fs::canonicalize(&candidate).ok().or(Some(candidate));
        }
    }
    None
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectDispatchOutcome {
    pub ok: bool,
    pub pid: u32,
    pub cmd: i32,
    pub process_exit_code: i32,
    pub remote_status: Option<i32>,
    pub remote_base: usize,
    pub message: String,
}

/// One-shot PID pin for the next `dispatch_to_toram` call (consumed on read).
/// `dispatch_to_all_toram` sets this before each call to walk every live game
/// instance; ordinary callers never set it and keep first-match behavior.
static TARGET_PID_OVERRIDE: AtomicU32 = AtomicU32::new(0);

/// Pin the next dispatch to one client. Multi-client fan-out inside.
pub fn set_target_pid_override(pid: DWORD) {
    TARGET_PID_OVERRIDE.store(pid, Ordering::Relaxed);
}

fn select_dispatch_pid(
    expected: Option<&ProcessIdentity>,
    target_pid_override: &AtomicU32,
    discover_first: impl FnOnce() -> DWORD,
) -> DWORD {
    match expected
        .map(|identity| identity.pid)
        .unwrap_or_else(|| target_pid_override.swap(0, Ordering::Relaxed))
    {
        0 => discover_first(),
        pinned => pinned,
    }
}

/// Execute `wcore_init(cmd)` inside `ToramOnline.exe` directly in-process (zero child process overhead).
/// Uses a content-hashed shadow copy in `%TEMP%` so the workspace `wcore.dll` is never file-locked.
pub fn dispatch_to_toram(
    cmd: i32,
    explicit_dll: Option<PathBuf>,
    use_shadow_copy: bool,
) -> DirectDispatchOutcome {
    dispatch_to_toram_expected(cmd, explicit_dll, use_shadow_copy, None)
}

/// Preserve the parent's selected identity across subprocess startup and DLL staging.
pub fn dispatch_to_toram_expected(
    cmd: i32,
    explicit_dll: Option<PathBuf>,
    use_shadow_copy: bool,
    expected: Option<ProcessIdentity>,
) -> DirectDispatchOutcome {
    if !CommandId::is_valid_range(cmd) {
        return DirectDispatchOutcome {
            ok: false,
            pid: 0,
            cmd,
            process_exit_code: 1,
            remote_status: None,
            remote_base: 0,
            message: format!("Unsupported command {cmd}."),
        };
    }

    if cmd == 44 {
        return DirectDispatchOutcome {
            ok: false,
            pid: 0,
            cmd,
            process_exit_code: 14,
            remote_status: Some(-7),
            remote_base: 0,
            message: "Command 44 is blocked: primary stat mutation triggers server sanction."
                .to_string(),
        };
    }

    if let Some(reason) = CommandId::unavailable_reason(cmd) {
        return DirectDispatchOutcome {
            ok: false,
            pid: 0,
            cmd,
            process_exit_code: 14,
            remote_status: Some(-10),
            remote_base: 0,
            message: format!("Command {cmd} unavailable: {reason}"),
        };
    }

    // One-shot multi-client pin (consumed here); otherwise first match wins.
    let pid = select_dispatch_pid(expected.as_ref(), &TARGET_PID_OVERRIDE, || {
        find_process_id("ToramOnline.exe")
    });
    if pid == 0 {
        return DirectDispatchOutcome {
            ok: false,
            pid: 0,
            cmd,
            process_exit_code: 2,
            remote_status: None,
            remote_base: 0,
            message: "ToramOnline.exe process not found".to_string(),
        };
    }

    let selected = match process_identity(pid) {
        Some(identity)
            if is_toram_identity(&identity)
                && expected
                    .as_ref()
                    .is_none_or(|expected| same_process(expected, &identity)) =>
        {
            identity
        }
        _ => {
            return DirectDispatchOutcome {
                ok: false,
                pid,
                cmd,
                process_exit_code: 4,
                remote_status: None,
                remote_base: 0,
                message: "Target process identity is unavailable or changed; no command was sent."
                    .to_string(),
            }
        }
    };

    let Some(base_dll_path) = explicit_dll.or_else(resolve_default_dll_path) else {
        return DirectDispatchOutcome {
            ok: false,
            pid,
            cmd,
            process_exit_code: 3,
            remote_status: None,
            remote_base: 0,
            message: "wcore.dll not found".to_string(),
        };
    };
    if !base_dll_path.is_file() {
        return DirectDispatchOutcome {
            ok: false,
            pid,
            cmd,
            process_exit_code: 3,
            remote_status: None,
            remote_base: 0,
            message: format!("DLL does not exist: {}", base_dll_path.display()),
        };
    }

    let active_dll_path = if use_shadow_copy {
        let Some(shadow) = prepare_shadow_dll(&base_dll_path) else {
            return DirectDispatchOutcome {
                ok: false,
                pid,
                cmd,
                process_exit_code: 3,
                remote_status: None,
                remote_base: 0,
                message: "Could not stage and verify the core DLL shadow copy.".to_string(),
            };
        };
        shadow
    } else {
        base_dll_path
    };

    let dll_abs = fs::canonicalize(&active_dll_path).unwrap_or(active_dll_path);
    let dll_str = {
        let s = dll_abs.to_string_lossy().into_owned();
        s.strip_prefix(r"\\?\").unwrap_or(&s).to_string()
    };
    let dll_file_name = Path::new(&dll_str)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("wcore.dll")
        .to_string();

    if let Some(expected_hash) = core_filename_hash(&dll_file_name) {
        if fs::read(&dll_abs).ok().as_deref().map(fnv1a_64) != Some(expected_hash) {
            return DirectDispatchOutcome {
                ok: false,
                pid,
                cmd,
                process_exit_code: 3,
                remote_status: None,
                remote_base: 0,
                message: "Core DLL bytes do not match its content-hashed filename.".to_string(),
            };
        }
    }

    let Some(init_rva) = resolve_init_export_rva(Path::new(&dll_str)) else {
        return DirectDispatchOutcome {
            ok: false,
            pid,
            cmd,
            process_exit_code: 3,
            remote_status: None,
            remote_base: 0,
            message: format!("Export ogl_drv_init/wcore_init not found in {}", dll_str),
        };
    };

    let _dispatch_guard = match DispatchGuard::acquire(pid) {
        Ok(guard) => guard,
        Err(message) => {
            return DirectDispatchOutcome {
                ok: false,
                pid,
                cmd,
                process_exit_code: 16,
                remote_status: None,
                remote_base: 0,
                message,
            };
        }
    };

    let mut remote_base = match loaded_modules(pid)
        .map(|modules| evaluate_core_load(&dll_file_name, &dll_str, &modules))
    {
        Ok(CoreLoadDecision::Load) => 0,
        Ok(CoreLoadDecision::Reuse(base)) => base,
        result => {
            let message = match result {
                Ok(CoreLoadDecision::RestartRequired(names)) => format!(
                    "Restart required: ToramOnline.exe (pid={}) already contains core module(s) {}. \
                     Restart the game before loading {} so old hooks cannot be stacked.",
                    pid, names, dll_file_name
                ),
                Err(error) => format!("Cannot verify loaded core modules: {}. No command was sent.", error),
                _ => unreachable!(),
            };
            return DirectDispatchOutcome {
                ok: false,
                pid,
                cmd,
                process_exit_code: 15,
                remote_status: None,
                remote_base: 0,
                message,
            };
        }
    };

    unsafe {
        let access = PROCESS_CREATE_THREAD
            | PROCESS_VM_OPERATION
            | PROCESS_VM_READ
            | PROCESS_VM_WRITE
            | PROCESS_QUERY_INFORMATION
            | PROCESS_QUERY_LIMITED_INFORMATION;
        let h_proc = OpenProcess(access, 0, pid);
        if h_proc.is_null() {
            return DirectDispatchOutcome {
                ok: false,
                pid,
                cmd,
                process_exit_code: 4,
                remote_status: None,
                remote_base: 0,
                message: format!("OpenProcess(pid={}) failed err={}", pid, GetLastError()),
            };
        }

        // This exact handle is used for every allocation, write and thread below.
        // A recycled PID, even with the same filename, must match lifetime and path.
        if !identity_from_handle(h_proc)
            .as_ref()
            .is_some_and(|actual| same_process(&selected, actual))
        {
            CloseHandle(h_proc);
            return DirectDispatchOutcome {
                ok: false,
                pid,
                cmd,
                process_exit_code: 4,
                remote_status: None,
                remote_base: 0,
                message: "Target identity changed before remote access; no command was sent."
                    .to_string(),
            };
        }

        if remote_base == 0 {
            let wpath: Vec<u16> = dll_str.encode_utf16().chain(core::iter::once(0)).collect();
            let byte_len = wpath.len() * 2;
            let remote_mem = VirtualAllocEx(
                h_proc,
                ptr::null_mut(),
                byte_len,
                MEM_COMMIT | MEM_RESERVE,
                PAGE_READWRITE,
            );
            if remote_mem.is_null() {
                CloseHandle(h_proc);
                return DirectDispatchOutcome {
                    ok: false,
                    pid,
                    cmd,
                    process_exit_code: 5,
                    remote_status: None,
                    remote_base: 0,
                    message: "VirtualAllocEx failed".to_string(),
                };
            }
            let mut written = 0usize;
            if WriteProcessMemory(
                h_proc,
                remote_mem,
                wpath.as_ptr().cast(),
                byte_len,
                &mut written,
            ) == 0
                || written != byte_len
            {
                VirtualFreeEx(h_proc, remote_mem, 0, MEM_RELEASE);
                CloseHandle(h_proc);
                return DirectDispatchOutcome {
                    ok: false,
                    pid,
                    cmd,
                    process_exit_code: 5,
                    remote_status: None,
                    remote_base: 0,
                    message: "WriteProcessMemory failed".to_string(),
                };
            }

            let k32_name: Vec<u16> = "kernel32.dll\0".encode_utf16().collect();
            let h_k32 = GetModuleHandleW(k32_name.as_ptr());
            let load_lib = GetProcAddress(h_k32, c"LoadLibraryW".as_ptr().cast());
            if load_lib.is_null() {
                VirtualFreeEx(h_proc, remote_mem, 0, MEM_RELEASE);
                CloseHandle(h_proc);
                return DirectDispatchOutcome {
                    ok: false,
                    pid,
                    cmd,
                    process_exit_code: 5,
                    remote_status: None,
                    remote_base: 0,
                    message: "GetProcAddress(LoadLibraryW) failed".to_string(),
                };
            }

            let h_th = CreateRemoteThread(
                h_proc,
                ptr::null(),
                0,
                load_lib,
                remote_mem,
                0,
                ptr::null_mut(),
            );
            if h_th.is_null() {
                VirtualFreeEx(h_proc, remote_mem, 0, MEM_RELEASE);
                CloseHandle(h_proc);
                return DirectDispatchOutcome {
                    ok: false,
                    pid,
                    cmd,
                    process_exit_code: 5,
                    remote_status: None,
                    remote_base: 0,
                    message: "CreateRemoteThread(LoadLibraryW) failed".to_string(),
                };
            }

            let w_res = WaitForSingleObject(h_th, 15_000);
            let w_err = GetLastError();
            let mut th_exit = 0u32;
            let ec_ok = GetExitCodeThread(h_th, &mut th_exit) != 0;
            let ec_err = GetLastError();
            CloseHandle(h_th);

            let outcome = evaluate_remote_thread_wait(
                RemoteWaitStage::LoadLibrary,
                RemoteThreadWaitInput {
                    wait_result: w_res,
                    wait_last_error: w_err,
                    exit_code_ok: ec_ok,
                    exit_code_last_error: ec_err,
                    thread_exit_code: th_exit,
                },
            );
            if outcome.release_remote_page {
                VirtualFreeEx(h_proc, remote_mem, 0, MEM_RELEASE);
            }
            if !outcome.completed {
                CloseHandle(h_proc);
                return DirectDispatchOutcome {
                    ok: false,
                    pid,
                    cmd,
                    process_exit_code: outcome.process_exit_code,
                    remote_status: None,
                    remote_base: 0,
                    message: format!(
                        "LoadLibraryW remote wait failed (exit={})",
                        outcome.process_exit_code
                    ),
                };
            }

            for _ in 0..30 {
                remote_base = find_remote_module_base(pid, h_proc, &dll_file_name);
                if remote_base != 0 {
                    break;
                }
                Sleep(50);
            }
            if remote_base == 0 {
                CloseHandle(h_proc);
                return DirectDispatchOutcome {
                    ok: false,
                    pid,
                    cmd,
                    process_exit_code: 9,
                    remote_status: None,
                    remote_base: 0,
                    message: "Loaded module not found in remote PEB".to_string(),
                };
            }
        }

        let remote_init = (remote_base + (init_rva as usize)) as *const c_void;
        let h_cmd_th = CreateRemoteThread(
            h_proc,
            ptr::null(),
            0,
            remote_init,
            (cmd as usize) as *mut c_void,
            0,
            ptr::null_mut(),
        );
        if h_cmd_th.is_null() {
            CloseHandle(h_proc);
            return DirectDispatchOutcome {
                ok: false,
                pid,
                cmd,
                process_exit_code: 10,
                remote_status: None,
                remote_base,
                message: "CreateRemoteThread(wcore_init) failed".to_string(),
            };
        }

        let w_res = WaitForSingleObject(h_cmd_th, 65_000);
        let w_err = GetLastError();
        let mut th_exit = 0u32;
        let ec_ok = GetExitCodeThread(h_cmd_th, &mut th_exit) != 0;
        let ec_err = GetLastError();
        CloseHandle(h_cmd_th);
        CloseHandle(h_proc);

        let outcome = evaluate_remote_thread_wait(
            RemoteWaitStage::WcoreInit,
            RemoteThreadWaitInput {
                wait_result: w_res,
                wait_last_error: w_err,
                exit_code_ok: ec_ok,
                exit_code_last_error: ec_err,
                thread_exit_code: th_exit,
            },
        );

        let signed_status = th_exit as i32;
        let ok = outcome.completed && signed_status == 0;
        DirectDispatchOutcome {
            ok,
            pid,
            cmd,
            process_exit_code: outcome.process_exit_code,
            remote_status: Some(signed_status),
            remote_base,
            message: if ok {
                format!(
                    "OK — cmd {} applied (pid={}, base=0x{:X}, status=0)",
                    cmd, pid, remote_base
                )
            } else if signed_status == -11 {
                format!("Command {cmd} partially applied (pid={pid}); some hooks or fields were unavailable. Inspect PARTIAL state and disable the feature before retrying.")
            } else if signed_status == -10 {
                format!("Command {cmd} is unavailable in the loaded core (pid={pid}).")
            } else {
                format!(
                    "Remote cmd {} failed (pid={}, status={}, exit={})",
                    cmd, pid, signed_status, outcome.process_exit_code
                )
            },
        }
    }
}

/// CLI wrapper around `dispatch_to_toram` that emits `[result] pid=<pid> cmd=<cmd> status=<status>`.
pub fn execute_wboot_cli(cmd: i32, explicit_dll: Option<PathBuf>) -> i32 {
    // Fan-out: one line per live client; a single client logs exactly as before.
    let outcomes = dispatch_to_all_toram(cmd, explicit_dll, true);
    let mut code = 0;
    for res in &outcomes {
        if let Some(status) = res.remote_status {
            if res.pid != 0 {
                log_both(&format!(
                    "[result] pid={} cmd={} status={}",
                    res.pid, cmd, status
                ));
            } else {
                log_both(&format!("[err] {}", res.message));
            }
        } else {
            log_both(&format!("[err] {}", res.message));
        }
        if res.process_exit_code != 0 {
            code = res.process_exit_code;
        }
    }
    code
}

pub fn validate_command_ids(commands: &[i32]) -> Result<(), String> {
    if commands.is_empty() || commands.len() > 64 {
        return Err("A batch must contain 1..64 commands.".into());
    }
    for &cmd in commands {
        if !CommandId::is_valid_range(cmd) {
            return Err(format!("Unsupported command {cmd}."));
        }
        if CommandId::is_blocked(cmd) {
            return Err("Command 44 is blocked.".into());
        }
        if let Some(reason) = CommandId::unavailable_reason(cmd) {
            return Err(format!("Command {cmd} unavailable: {reason}"));
        }
    }
    Ok(())
}

pub fn parse_cmd_spec(spec: &str) -> Result<Vec<i32>, String> {
    if spec.len() > 2048 {
        return Err("Command text is too long.".into());
    }
    let mut commands = Vec::new();
    for segment in spec.split([',', '+', ';']) {
        if segment.trim().is_empty() {
            return Err("Empty command in batch.".into());
        }
        for token in segment.split_ascii_whitespace() {
            commands.push(
                token
                    .parse::<i32>()
                    .map_err(|_| format!("Invalid command '{token}'."))?,
            );
        }
    }
    validate_command_ids(&commands)?;
    Ok(commands)
}

/// Compatibility parser: malformed input rejects the entire batch.
pub fn parse_cmd_list(spec: &str) -> Vec<i32> {
    parse_cmd_spec(spec).unwrap_or_default()
}

/// Fan-out power: dispatch `cmd` to every live game client (single client
/// behaves exactly like `dispatch_to_toram`). Staging repeats per client but
/// the shadow copy makes repeats nearly free after the first.
pub fn dispatch_to_all_toram(
    cmd: i32,
    explicit_dll: Option<PathBuf>,
    use_shadow_copy: bool,
) -> Vec<DirectDispatchOutcome> {
    if !CommandId::is_valid_range(cmd)
        || CommandId::is_blocked(cmd)
        || CommandId::unavailable_reason(cmd).is_some()
    {
        return vec![dispatch_to_toram(cmd, explicit_dll, use_shadow_copy)];
    }
    let pids = find_all_process_ids("ToramOnline.exe");
    if pids.is_empty() {
        return vec![dispatch_to_toram(cmd, explicit_dll, use_shadow_copy)];
    }
    let targets: Vec<_> = pids
        .into_iter()
        .map(|pid| (pid, process_identity(pid)))
        .collect();
    let mut outcomes = Vec::with_capacity(targets.len());
    for (pid, identity) in targets {
        outcomes.push(match identity {
            Some(identity) => dispatch_to_toram_expected(
                cmd,
                explicit_dll.clone(),
                use_shadow_copy,
                Some(identity),
            ),
            None => DirectDispatchOutcome {
                ok: false,
                pid,
                cmd,
                process_exit_code: 4,
                remote_status: None,
                remote_base: 0,
                message: "Target exited or its identity is unavailable; no command was sent."
                    .to_string(),
            },
        });
    }
    outcomes
}

/// Batch power: run a stacked spec ("73+72+71") across all live clients.
/// Validate the complete batch first, then stop at the first failed command.
pub fn execute_wboot_cli_batch(spec: &str, explicit_dll: Option<PathBuf>) -> i32 {
    let cmds = match parse_cmd_spec(spec) {
        Ok(commands) => commands,
        Err(error) => {
            log_both(&format!("[err] {error}"));
            return 1;
        }
    };
    let mut code = 0;
    for cmd in cmds {
        let step = execute_wboot_cli(cmd, explicit_dll.clone());
        if step != 0 {
            code = step;
            break;
        }
    }
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unavailable_and_unassigned_commands_fail_before_process_access() {
        for cmd in [4, 10, 31, 45, 67] {
            assert!(validate_command_ids(&[73, cmd, 72]).is_err());
            let result = dispatch_to_toram(cmd, None, true);
            assert!(!result.ok);
            assert_eq!(result.pid, 0);
        }
        for cmd in 96..=99 {
            assert!(validate_command_ids(&[73, cmd, 72])
                .unwrap_err()
                .contains("unavailable"));
            let result = dispatch_to_toram(cmd, None, true);
            assert!(!result.ok);
            assert_eq!(result.pid, 0);
            assert_eq!(result.remote_status, Some(-10));
        }
    }

    #[test]
    fn cmd_list_parses_single_and_stacked_specs() {
        assert_eq!(parse_cmd_list("73"), vec![73]);
        assert_eq!(parse_cmd_list("73,72,71"), vec![73, 72, 71]);
        assert_eq!(parse_cmd_list("73+72 + 71"), vec![73, 72, 71]);
        assert_eq!(parse_cmd_list("73;72"), vec![73, 72]);
        assert_eq!(parse_cmd_list("  "), Vec::<i32>::new());
        assert_eq!(parse_cmd_list("abc"), Vec::<i32>::new());
        assert!(parse_cmd_list("73,abc,72").is_empty());
        assert!(parse_cmd_list("73+44+72").is_empty());
        assert!(parse_cmd_list("73++72").is_empty());
        assert_eq!(parse_cmd_list("83+86+89+115"), vec![83, 86, 89, 115]);
    }

    #[test]
    fn fanout_rejects_bad_commands_without_touching_processes() {
        let bad = dispatch_to_all_toram(CommandId::MAX + 1, None, true);
        assert_eq!(bad.len(), 1);
        assert!(!bad[0].ok);
        assert_eq!(bad[0].pid, 0);
        let blocked = dispatch_to_all_toram(44, None, true);
        assert_eq!(blocked.len(), 1);
        assert_eq!(blocked[0].remote_status, Some(-7));
    }

    #[test]
    fn process_enumeration_finds_nothing_for_unknown_exe() {
        assert!(find_all_process_ids("definitely-not-a-real-process-9f3.exe").is_empty());
        assert_eq!(find_process_id("definitely-not-a-real-process-9f3.exe"), 0);
    }

    #[test]
    fn pid_override_pins_and_consumes_one_shot() {
        let pin = AtomicU32::new(424242);
        let discovery_calls = std::cell::Cell::new(0);
        let discover = || {
            discovery_calls.set(discovery_calls.get() + 1);
            123456
        };
        assert_eq!(select_dispatch_pid(None, &pin, discover), 424242);
        assert_eq!(pin.load(Ordering::Relaxed), 0);
        assert_eq!(discovery_calls.get(), 0);
        assert_eq!(select_dispatch_pid(None, &pin, discover), 123456);
        assert_eq!(discovery_calls.get(), 1);
    }

    #[test]
    fn explicit_identity_does_not_consume_the_pending_pid_override() {
        let pin = AtomicU32::new(424242);
        let expected = ProcessIdentity {
            pid: 123456,
            created_at: 789,
            image_path: r"C:\Game\ToramOnline.exe".into(),
        };
        assert_eq!(
            select_dispatch_pid(Some(&expected), &pin, || panic!("unexpected discovery")),
            expected.pid
        );
        assert_eq!(pin.load(Ordering::Relaxed), 424242);
        assert_eq!(
            select_dispatch_pid(None, &pin, || panic!("unexpected discovery")),
            424242
        );
        assert_eq!(pin.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn invalid_protocol_command_is_rejected_before_process_access() {
        let outcome = dispatch_to_toram(CommandId::MAX + 1, None, true);
        assert!(!outcome.ok);
        assert_eq!(outcome.pid, 0);
        assert_eq!(outcome.remote_base, 0);
        assert!(outcome.message.contains("Unsupported command"));
    }

    #[test]
    fn identity_rejects_recycled_pid_and_same_named_decoy_path() {
        let expected = ProcessIdentity {
            pid: 42,
            created_at: 123,
            image_path: r"C:\Games\Toram\ToramOnline.exe".into(),
        };
        assert!(same_process(&expected, &expected));
        assert!(is_toram_identity(&expected));
        let mut changed = expected.clone();
        changed.created_at += 1;
        assert!(!same_process(&expected, &changed));
        changed = expected.clone();
        changed.image_path = r"C:\Temp\ToramOnline.exe".into();
        assert!(!same_process(&expected, &changed));
        changed = expected.clone();
        changed.pid += 1;
        assert!(!same_process(&expected, &changed));
    }

    #[test]
    fn identity_is_obtained_from_the_live_handle_and_non_game_is_rejected() {
        let actual = process_identity(std::process::id()).unwrap();
        unsafe {
            let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, actual.pid);
            assert!(!process.is_null());
            assert!(same_process(
                &actual,
                &identity_from_handle(process).unwrap()
            ));
            CloseHandle(process);
        }
        assert!(!is_toram_identity(&actual));
        let result = dispatch_to_toram_expected(1, None, true, Some(actual));
        assert!(!result.ok);
        assert_eq!(result.process_exit_code, 4);
    }

    fn module(name: &str, path: &str, base: usize) -> LoadedModule {
        LoadedModule {
            name: name.to_string(),
            path: path.to_string(),
            base,
        }
    }

    #[test]
    fn core_load_reuses_only_a_single_identical_hashed_module() {
        let name = "dxgi_ext_0123456789abcdef.dll";
        let path = format!(r"C:\Temp\{}", name);
        let system = module("kernel32.dll", r"C:\Windows\System32\kernel32.dll", 0x10000);
        assert_eq!(
            evaluate_core_load(name, &path, std::slice::from_ref(&system)),
            CoreLoadDecision::Load
        );
        let matching = module(
            &name.to_uppercase(),
            &format!(r"\\?\{}", path.to_uppercase()),
            0x12340000,
        );
        assert_eq!(
            evaluate_core_load(name, &path, &[system, matching.clone()]),
            CoreLoadDecision::Reuse(0x12340000)
        );
        let legacy_name = "wcore_live_0123456789abcdef.dll";
        let legacy_path = format!(r"C:\Temp\{}", legacy_name);
        assert_eq!(
            evaluate_core_load(
                legacy_name,
                &legacy_path,
                &[module(legacy_name, &legacy_path, 0x12340000)]
            ),
            CoreLoadDecision::Reuse(0x12340000)
        );
        for invalid in [
            module(name, r"D:\Other\dxgi_ext_0123456789abcdef.dll", 0x12340000),
            module(name, &path, 0),
        ] {
            assert!(matches!(
                evaluate_core_load(name, &path, &[invalid]),
                CoreLoadDecision::RestartRequired(_)
            ));
        }
    }

    #[test]
    fn core_load_rejects_old_builds_and_already_stacked_modules() {
        let name = "dxgi_ext_0123456789abcdef.dll";
        let path = format!(r"C:\Temp\{}", name);
        for old_name in [
            "dxgi_ext_fedcba9876543210.dll",
            "wcore_live_fedcba9876543210.dll",
            "wcore.dll",
            "wcore92.dll",
            "WCORE93.DLL",
        ] {
            let old = module(old_name, &format!(r"C:\Temp\{}", old_name), 0x40000);
            for modules in [vec![old.clone()], vec![module(name, &path, 0x50000), old]] {
                let result = evaluate_core_load(name, &path, &modules);
                match result {
                    CoreLoadDecision::RestartRequired(names) => assert!(names.contains(old_name)),
                    other => panic!("Expected restart for {old_name}, got {other:?}"),
                }
            }
        }
    }

    #[test]
    fn same_canonical_or_custom_filename_does_not_prove_same_build() {
        for name in ["wcore.dll", "wcore93.dll", "custom.dll"] {
            let path = format!(r"C:\Build\{}", name);
            assert!(matches!(
                evaluate_core_load(name, &path, &[module(name, &path, 0x70000)]),
                CoreLoadDecision::RestartRequired(_)
            ));
        }
    }

    #[test]
    fn module_inventory_is_complete_for_self_and_errors_for_missing_process() {
        let modules = loaded_modules(std::process::id()).expect("self inventory should succeed");
        let own_name = env::current_exe()
            .unwrap()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(modules
            .iter()
            .any(|m| m.name.eq_ignore_ascii_case(&own_name) && m.base != 0));
        assert!(modules
            .iter()
            .any(|m| m.name.eq_ignore_ascii_case("kernel32.dll")));
        assert!(loaded_modules(u32::MAX).is_err());
    }

    #[test]
    fn dispatch_guard_blocks_concurrent_loaders_and_releases_on_drop() {
        let pid = std::process::id();
        let guard = DispatchGuard::acquire(pid).unwrap();
        assert!(
            std::thread::spawn(move || DispatchGuard::acquire(pid).is_err())
                .join()
                .unwrap()
        );
        drop(guard);
        assert!(
            std::thread::spawn(move || DispatchGuard::acquire(pid).is_ok())
                .join()
                .unwrap()
        );
    }

    #[test]
    fn shadow_copy_rejects_existing_content_mismatch() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let bytes =
            format!("shadow identity fixture {} {}", std::process::id(), unique).into_bytes();
        let source = env::temp_dir().join(format!("wboot_shadow_test_{}.dll", unique));
        let shadow = env::temp_dir().join(format!(
            "{}{:016x}.dll",
            SHADOW_DLL_PREFIX,
            fnv1a_64(&bytes)
        ));
        fs::write(&source, &bytes).unwrap();
        fs::write(&shadow, b"stale or incomplete shadow").unwrap();
        assert_eq!(prepare_shadow_dll(&source), None);
        fs::remove_file(&shadow).unwrap();
        assert_eq!(prepare_shadow_dll(&source), Some(shadow.clone()));
        assert_eq!(fs::read(&shadow).unwrap(), bytes);
        fs::remove_file(&source).unwrap();
        fs::remove_file(&shadow).unwrap();
    }

    #[test]
    fn test_all_24_remote_wait_and_fallback_scenarios() {
        let r1 = evaluate_remote_thread_wait(
            RemoteWaitStage::LoadLibrary,
            RemoteThreadWaitInput {
                wait_result: WAIT_TIMEOUT,
                wait_last_error: 0,
                exit_code_ok: true,
                exit_code_last_error: 0,
                thread_exit_code: STILL_ACTIVE,
            },
        );
        assert!(
            !r1.completed && r1.timed_out && !r1.release_remote_page && r1.process_exit_code == 6
        );

        let r2 = evaluate_remote_thread_wait(
            RemoteWaitStage::LoadLibrary,
            RemoteThreadWaitInput {
                wait_result: 0xFFFF_FFFF,
                wait_last_error: 5,
                exit_code_ok: false,
                exit_code_last_error: 5,
                thread_exit_code: 0,
            },
        );
        assert!(
            !r2.completed && !r2.timed_out && !r2.release_remote_page && r2.process_exit_code == 7
        );

        let r3 = evaluate_remote_thread_wait(
            RemoteWaitStage::LoadLibrary,
            RemoteThreadWaitInput {
                wait_result: WAIT_OBJECT_0,
                wait_last_error: 0,
                exit_code_ok: false,
                exit_code_last_error: 6,
                thread_exit_code: 0,
            },
        );
        assert!(!r3.completed && !r3.release_remote_page && r3.process_exit_code == 8);

        let r4 = evaluate_remote_thread_wait(
            RemoteWaitStage::LoadLibrary,
            RemoteThreadWaitInput {
                wait_result: WAIT_OBJECT_0,
                wait_last_error: 0,
                exit_code_ok: true,
                exit_code_last_error: 0,
                thread_exit_code: STILL_ACTIVE,
            },
        );
        assert!(!r4.completed && !r4.release_remote_page && r4.process_exit_code == 9);

        let r5 = evaluate_remote_thread_wait(
            RemoteWaitStage::LoadLibrary,
            RemoteThreadWaitInput {
                wait_result: WAIT_OBJECT_0,
                wait_last_error: 0,
                exit_code_ok: true,
                exit_code_last_error: 0,
                thread_exit_code: 0,
            },
        );
        assert!(!r5.completed && r5.release_remote_page && r5.process_exit_code == 9);

        let r6 = evaluate_remote_thread_wait(
            RemoteWaitStage::LoadLibrary,
            RemoteThreadWaitInput {
                wait_result: WAIT_OBJECT_0,
                wait_last_error: 0,
                exit_code_ok: true,
                exit_code_last_error: 0,
                thread_exit_code: 0x7AB0_0000,
            },
        );
        assert!(r6.completed && r6.release_remote_page && r6.process_exit_code == 0);

        let w1 = evaluate_remote_thread_wait(
            RemoteWaitStage::WcoreInit,
            RemoteThreadWaitInput {
                wait_result: WAIT_TIMEOUT,
                wait_last_error: 0,
                exit_code_ok: true,
                exit_code_last_error: 0,
                thread_exit_code: STILL_ACTIVE,
            },
        );
        assert!(!w1.completed && w1.timed_out && w1.process_exit_code == 11);

        let w2 = evaluate_remote_thread_wait(
            RemoteWaitStage::WcoreInit,
            RemoteThreadWaitInput {
                wait_result: 0xFFFF_FFFF,
                wait_last_error: 5,
                exit_code_ok: false,
                exit_code_last_error: 5,
                thread_exit_code: 0,
            },
        );
        assert!(!w2.completed && w2.process_exit_code == 12);

        let w3 = evaluate_remote_thread_wait(
            RemoteWaitStage::WcoreInit,
            RemoteThreadWaitInput {
                wait_result: WAIT_OBJECT_0,
                wait_last_error: 0,
                exit_code_ok: false,
                exit_code_last_error: 6,
                thread_exit_code: 0,
            },
        );
        assert!(!w3.completed && w3.process_exit_code == 13);

        let w4 = evaluate_remote_thread_wait(
            RemoteWaitStage::WcoreInit,
            RemoteThreadWaitInput {
                wait_result: WAIT_OBJECT_0,
                wait_last_error: 0,
                exit_code_ok: true,
                exit_code_last_error: 0,
                thread_exit_code: STILL_ACTIVE,
            },
        );
        assert!(!w4.completed && w4.process_exit_code == 14);

        let w5 = evaluate_remote_thread_wait(
            RemoteWaitStage::WcoreInit,
            RemoteThreadWaitInput {
                wait_result: WAIT_OBJECT_0,
                wait_last_error: 0,
                exit_code_ok: true,
                exit_code_last_error: 0,
                thread_exit_code: (-7i32) as u32,
            },
        );
        assert!(!w5.completed && w5.process_exit_code == 14);

        let w6 = evaluate_remote_thread_wait(
            RemoteWaitStage::WcoreInit,
            RemoteThreadWaitInput {
                wait_result: WAIT_OBJECT_0,
                wait_last_error: 0,
                exit_code_ok: true,
                exit_code_last_error: 0,
                thread_exit_code: 0,
            },
        );
        assert!(w6.completed && w6.process_exit_code == 0);

        let base_fb = DiagnosticFallbackInput {
            target_pid: 1234,
            target_process_known: true,
            target_process_alive: true,
            dll_path_valid: true,
            dll_exists_on_disk: true,
            module_attached: true,
        };
        assert_eq!(
            evaluate_diagnostic_fallback(base_fb),
            DiagnosticFallbackOutcome {
                allowed: true,
                process_exit_code: 0
            }
        );
        assert_eq!(
            evaluate_diagnostic_fallback(DiagnosticFallbackInput {
                target_pid: 0,
                ..base_fb
            })
            .process_exit_code,
            2
        );
        assert_eq!(
            evaluate_diagnostic_fallback(DiagnosticFallbackInput {
                target_process_known: false,
                ..base_fb
            })
            .process_exit_code,
            2
        );
        assert_eq!(
            evaluate_diagnostic_fallback(DiagnosticFallbackInput {
                target_process_alive: false,
                ..base_fb
            })
            .process_exit_code,
            2
        );
        assert_eq!(
            evaluate_diagnostic_fallback(DiagnosticFallbackInput {
                dll_path_valid: false,
                ..base_fb
            })
            .process_exit_code,
            3
        );
        assert_eq!(
            evaluate_diagnostic_fallback(DiagnosticFallbackInput {
                dll_exists_on_disk: false,
                ..base_fb
            })
            .process_exit_code,
            3
        );
        assert_eq!(
            evaluate_diagnostic_fallback(DiagnosticFallbackInput {
                module_attached: false,
                ..base_fb
            })
            .process_exit_code,
            9
        );
    }

    #[test]
    fn test_find_export_rva_on_built_wcore_dll() {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let root = manifest_dir.parent().unwrap().parent().unwrap();
        for name in ["wcore.dll", "wcore92.dll"] {
            let dll = root.join("src").join(name);
            if dll.is_file() {
                let rva_init = resolve_init_export_rva(&dll);
                let rva_cmd = find_export_rva(&dll, "wcore_cmd");
                assert!(rva_init.is_some() && rva_init.unwrap() > 0);
                assert!(rva_cmd.is_some() && rva_cmd.unwrap() > 0);
                assert!(find_export_rva(&dll, "non_existent_export").is_none());
            }
        }
    }

    #[test]
    fn test_fnv1a_64_deterministic() {
        let h1 = fnv1a_64(b"toram_wcore_v93");
        let h2 = fnv1a_64(b"toram_wcore_v93");
        let h3 = fnv1a_64(b"toram_wcore_v94");
        assert_eq!(h1, h2);
        assert_ne!(h1, h3);
    }

    #[test]
    fn test_shadow_dll_neutral_naming() {
        assert!(is_shadow_dll_filename("dxgi_ext_0123456789abcdef.dll"));
        assert!(is_shadow_dll_filename("DXGI_EXT_A1B2C3D4E5F60718.DLL"));
        assert!(!is_shadow_dll_filename("wcore_live_0123456789abcdef.dll"));
        assert!(!is_shadow_dll_filename("wcore.dll"));
        assert!(!is_shadow_dll_filename("dxgi_ext_short.dll"));

        for forbidden in ["toram", "wcore", "cheat", "hack", "companion"] {
            assert!(
                !SHADOW_DLL_PREFIX.to_ascii_lowercase().contains(forbidden),
                "SHADOW_DLL_PREFIX must not contain '{}'",
                forbidden
            );
        }

        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let root = manifest_dir.parent().unwrap().parent().unwrap();
        let src_dll = root.join("src").join("wcore.dll");
        if src_dll.is_file() {
            let shadow = prepare_shadow_dll(&src_dll).expect("shadow copy must succeed");
            let fname = shadow.file_name().unwrap().to_str().unwrap().to_string();
            assert!(
                is_shadow_dll_filename(&fname),
                "Expected neutral shadow DLL filename, got {}",
                fname
            );
            for forbidden in ["toram", "wcore", "cheat", "hack", "companion"] {
                assert!(!fname.to_ascii_lowercase().contains(forbidden));
            }
        }
    }
}
