//! Win32 FFI bindings, `%TEMP%\wcore.log` mutex logger, cross-page memory range validators,
//! and unhandled exception breadcrumb filter.

#![allow(non_snake_case, non_camel_case_types, dead_code)]

use core::fmt;
use std::ffi::c_void;
use std::ptr;
use std::sync::atomic::{AtomicI32, AtomicPtr, Ordering};

pub type BOOL = i32;
pub type DWORD = u32;
pub type WORD = u16;
pub type USHORT = u16;
pub type LONG = i32;
pub type ULONG = u32;
pub type SIZE_T = usize;
pub type ULONG_PTR = usize;
pub type HANDLE = *mut c_void;
pub type HMODULE = *mut c_void;
pub type LPCSTR = *const u8;
pub type LPCWSTR = *const u16;
pub type LPSTR = *mut u8;
pub type LPVOID = *mut c_void;
pub type LPCVOID = *const c_void;
pub type LPTHREAD_START_ROUTINE = Option<unsafe extern "system" fn(LPVOID) -> DWORD>;

pub const FALSE: BOOL = 0;
pub const TRUE: BOOL = 1;
pub const INVALID_HANDLE_VALUE: HANDLE = -1isize as HANDLE;
pub const MAX_PATH: usize = 260;

pub const DLL_PROCESS_DETACH: DWORD = 0;
pub const DLL_PROCESS_ATTACH: DWORD = 1;
pub const WAIT_OBJECT_0: DWORD = 0x0000_0000;
pub const WAIT_ABANDONED_0: DWORD = 0x0000_0080;
pub const WAIT_TIMEOUT: DWORD = 0x0000_0102;
pub const WAIT_FAILED: DWORD = 0xFFFF_FFFF;
pub const INFINITE: DWORD = 0xFFFF_FFFF;

pub const MEM_COMMIT: DWORD = 0x0000_1000;
pub const MEM_RESERVE: DWORD = 0x0000_2000;
pub const MEM_RELEASE: DWORD = 0x0000_8000;

pub const PAGE_NOACCESS: DWORD = 0x01;
pub const PAGE_READONLY: DWORD = 0x02;
pub const PAGE_READWRITE: DWORD = 0x04;
pub const PAGE_WRITECOPY: DWORD = 0x08;
pub const PAGE_EXECUTE: DWORD = 0x10;
pub const PAGE_EXECUTE_READ: DWORD = 0x20;
pub const PAGE_EXECUTE_READWRITE: DWORD = 0x40;
pub const PAGE_EXECUTE_WRITECOPY: DWORD = 0x80;
pub const PAGE_GUARD: DWORD = 0x100;

pub const FILE_APPEND_DATA: DWORD = 0x0000_0004;
pub const FILE_SHARE_READ: DWORD = 0x0000_0001;
pub const FILE_SHARE_WRITE: DWORD = 0x0000_0002;
pub const OPEN_ALWAYS: DWORD = 4;

pub const GET_MODULE_HANDLE_EX_FLAG_PIN: DWORD = 0x0000_0001;
pub const GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS: DWORD = 0x0000_0004;
pub const EXCEPTION_CONTINUE_SEARCH: LONG = 0;

#[repr(C)]
#[derive(Copy, Clone)]
pub struct SRWLOCK {
    pub Ptr: *mut c_void,
}
unsafe impl Send for SRWLOCK {}
unsafe impl Sync for SRWLOCK {}

pub const SRWLOCK_INIT: SRWLOCK = SRWLOCK {
    Ptr: ptr::null_mut(),
};

#[repr(C)]
#[derive(Copy, Clone, Default)]
pub struct MEMORY_BASIC_INFORMATION {
    pub BaseAddress: *mut c_void,
    pub AllocationBase: *mut c_void,
    pub AllocationProtect: DWORD,
    pub PartitionId: WORD,
    pub RegionSize: SIZE_T,
    pub State: DWORD,
    pub Protect: DWORD,
    pub Type: DWORD,
}

#[repr(C)]
pub struct EXCEPTION_RECORD {
    pub ExceptionCode: DWORD,
    pub ExceptionFlags: DWORD,
    pub ExceptionRecord: *mut EXCEPTION_RECORD,
    pub ExceptionAddress: *mut c_void,
    pub NumberParameters: DWORD,
    pub ExceptionInformation: [ULONG_PTR; 15],
}

#[repr(C)]
pub struct EXCEPTION_POINTERS {
    pub ExceptionRecord: *mut EXCEPTION_RECORD,
    pub ContextRecord: *mut c_void,
}

pub type LPTOP_LEVEL_EXCEPTION_FILTER =
    Option<unsafe extern "system" fn(*mut EXCEPTION_POINTERS) -> LONG>;

#[link(name = "kernel32")]
extern "system" {
    pub fn CreateMutexA(
        lpMutexAttributes: *mut c_void,
        bInitialOwner: BOOL,
        lpName: LPCSTR,
    ) -> HANDLE;
    pub fn ReleaseMutex(hMutex: HANDLE) -> BOOL;
    pub fn CreateEventA(
        lpEventAttributes: *mut c_void,
        bManualReset: BOOL,
        bInitialState: BOOL,
        lpName: LPCSTR,
    ) -> HANDLE;
    pub fn CreateEventW(
        lpEventAttributes: *const c_void,
        bManualReset: BOOL,
        bInitialState: BOOL,
        lpName: LPCWSTR,
    ) -> HANDLE;
    pub fn SetEvent(hEvent: HANDLE) -> BOOL;
    pub fn ResetEvent(hEvent: HANDLE) -> BOOL;
    pub fn WaitForSingleObject(hHandle: HANDLE, dwMilliseconds: DWORD) -> DWORD;
    pub fn WaitForMultipleObjects(
        nCount: DWORD,
        lpHandles: *const HANDLE,
        bWaitAll: BOOL,
        dwMilliseconds: DWORD,
    ) -> DWORD;
    pub fn CreateThread(
        lpThreadAttributes: *const c_void,
        dwStackSize: SIZE_T,
        lpStartAddress: LPTHREAD_START_ROUTINE,
        lpParameter: LPVOID,
        dwCreationFlags: DWORD,
        lpThreadId: *mut DWORD,
    ) -> HANDLE;
    pub fn CloseHandle(hObject: HANDLE) -> BOOL;
    pub fn Sleep(dwMilliseconds: DWORD);
    pub fn GetCurrentProcess() -> HANDLE;
    pub fn GetCurrentProcessId() -> DWORD;
    pub fn GetCurrentThreadId() -> DWORD;
    pub fn QueryPerformanceCounter(value: *mut i64) -> BOOL;
    pub fn QueryPerformanceFrequency(value: *mut i64) -> BOOL;
    pub fn GetLastError() -> DWORD;
    pub fn SetLastError(dwErrCode: DWORD);
    pub fn GetTempPathA(nBufferLength: DWORD, lpBuffer: LPSTR) -> DWORD;
    pub fn CreateFileA(
        lpFileName: LPCSTR,
        dwDesiredAccess: DWORD,
        dwShareMode: DWORD,
        lpSecurityAttributes: *mut c_void,
        dwCreationDisposition: DWORD,
        dwFlagsAndAttributes: DWORD,
        hTemplateFile: HANDLE,
    ) -> HANDLE;
    pub fn WriteFile(
        hFile: HANDLE,
        lpBuffer: LPCVOID,
        nNumberOfBytesToWrite: DWORD,
        lpNumberOfBytesWritten: *mut DWORD,
        lpOverlapped: *mut c_void,
    ) -> BOOL;
    pub fn VirtualAlloc(
        lpAddress: LPVOID,
        dwSize: SIZE_T,
        flAllocationType: DWORD,
        flProtect: DWORD,
    ) -> LPVOID;
    pub fn VirtualFree(lpAddress: LPVOID, dwSize: SIZE_T, dwFreeType: DWORD) -> BOOL;
    pub fn VirtualQuery(
        lpAddress: LPCVOID,
        lpBuffer: *mut MEMORY_BASIC_INFORMATION,
        dwLength: SIZE_T,
    ) -> SIZE_T;
    pub fn VirtualProtect(
        lpAddress: LPVOID,
        dwSize: SIZE_T,
        flNewProtect: DWORD,
        lpflOldProtect: *mut DWORD,
    ) -> BOOL;
    pub fn WriteProcessMemory(
        hProcess: HANDLE,
        lpBaseAddress: LPVOID,
        lpBuffer: LPCVOID,
        nSize: SIZE_T,
        lpNumberOfBytesWritten: *mut SIZE_T,
    ) -> BOOL;
    pub fn FlushInstructionCache(hProcess: HANDLE, lpBaseAddress: LPCVOID, dwSize: SIZE_T) -> BOOL;
    pub fn DisableThreadLibraryCalls(hLibModule: HMODULE) -> BOOL;
    pub fn GetModuleHandleExA(dwFlags: DWORD, lpModuleName: LPCSTR, phModule: *mut HMODULE)
        -> BOOL;
    pub fn GetModuleHandleA(lpModuleName: LPCSTR) -> HMODULE;
    pub fn GetModuleHandleW(lpModuleName: LPCWSTR) -> HMODULE;
    pub fn GetProcAddress(hModule: HMODULE, lpProcName: LPCSTR) -> *mut c_void;
    pub fn SetUnhandledExceptionFilter(
        lpTopLevelExceptionFilter: LPTOP_LEVEL_EXCEPTION_FILTER,
    ) -> LPTOP_LEVEL_EXCEPTION_FILTER;
    pub fn AcquireSRWLockExclusive(SRWLock: *mut SRWLOCK);
    pub fn TryAcquireSRWLockExclusive(SRWLock: *mut SRWLOCK) -> u8;
    pub fn ReleaseSRWLockExclusive(SRWLock: *mut SRWLOCK);
    pub fn AcquireSRWLockShared(SRWLock: *mut SRWLOCK);
    pub fn ReleaseSRWLockShared(SRWLock: *mut SRWLOCK);
}

// ---------------- Logger ----------------
pub const LOG_MUTEX_NAME: &[u8] = b"Local\\WinDiagTraceSyncMtx\0";
static LOG_MTX: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());

pub fn wlog_str(msg: &str) {
    unsafe {
        let mut mtx = LOG_MTX.load(Ordering::Acquire);
        if mtx.is_null() {
            let created = CreateMutexA(ptr::null_mut(), FALSE, LOG_MUTEX_NAME.as_ptr());
            if !created.is_null() {
                match LOG_MTX.compare_exchange(
                    ptr::null_mut(),
                    created,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => mtx = created,
                    Err(existing) => {
                        CloseHandle(created);
                        mtx = existing;
                    }
                }
            }
        }
        if !mtx.is_null() {
            WaitForSingleObject(mtx, 3000);
        }

        let mut path_buf = [0u8; MAX_PATH + 32];
        let len = GetTempPathA(MAX_PATH as DWORD, path_buf.as_mut_ptr()) as usize;
        if len > 0 && len < MAX_PATH {
            #[cfg(not(test))]
            let suffix: &[u8] = b"wcore.log\0";
            #[cfg(test)]
            let suffix: &[u8] = b"wcore-tests.log\0";
            path_buf[len..len + suffix.len()].copy_from_slice(suffix);
            let h = CreateFileA(
                path_buf.as_ptr(),
                FILE_APPEND_DATA,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                ptr::null_mut(),
                OPEN_ALWAYS,
                0,
                ptr::null_mut(),
            );
            if !h.is_null() && h != INVALID_HANDLE_VALUE {
                let mut line = String::with_capacity(msg.len() + 1);
                line.push_str(msg);
                line.push('\n');
                let mut written: DWORD = 0;
                WriteFile(
                    h,
                    line.as_ptr() as LPCVOID,
                    line.len() as DWORD,
                    &mut written,
                    ptr::null_mut(),
                );
                CloseHandle(h);
            }
        }

        if !mtx.is_null() {
            ReleaseMutex(mtx);
        }
    }
}

pub fn log_fmt(args: fmt::Arguments<'_>) {
    wlog_str(&std::fmt::format(args));
}

#[macro_export]
macro_rules! wlog {
    ($($arg:tt)*) => {
        $crate::win::wlog_str(&format!($($arg)*))
    };
}

// ---------------- Memory & Pointer Safety ----------------

/// Verify that `[addr, addr + need)` is committed and readable across all spanned pages.
pub fn ptr_ok(addr: usize, need: usize) -> bool {
    let need = need.max(1);
    if addr < 0x10000 {
        return false;
    }
    let Some(end) = addr.checked_add(need) else {
        return false;
    };
    let readable_mask = PAGE_READONLY
        | PAGE_READWRITE
        | PAGE_WRITECOPY
        | PAGE_EXECUTE_READ
        | PAGE_EXECUTE_READWRITE
        | PAGE_EXECUTE_WRITECOPY;

    let mut cur = addr;
    while cur < end {
        let mut mbi = MEMORY_BASIC_INFORMATION::default();
        if unsafe {
            VirtualQuery(
                cur as LPCVOID,
                &mut mbi,
                std::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
            )
        } == 0
        {
            return false;
        }
        if mbi.State != MEM_COMMIT || (mbi.Protect & (PAGE_NOACCESS | PAGE_GUARD)) != 0 {
            return false;
        }
        if (mbi.Protect & 0xFF & readable_mask) == 0 {
            return false;
        }
        let region_end = (mbi.BaseAddress as usize).saturating_add(mbi.RegionSize);
        if region_end <= cur {
            return false;
        }
        cur = region_end;
    }
    true
}

/// Verify that `addr` points to at least 16 bytes of readable & executable code.
pub fn fn_readable(addr: usize) -> bool {
    page_exec_ok(addr, 16)
}

/// Verify that `[addr, addr + need)` is committed and writable across all spanned pages.
pub fn page_write_ok(addr: usize, need: usize) -> bool {
    let need = need.max(1);
    if addr < 0x10000 {
        return false;
    }
    let Some(end) = addr.checked_add(need) else {
        return false;
    };
    let writable_mask =
        PAGE_READWRITE | PAGE_WRITECOPY | PAGE_EXECUTE_READWRITE | PAGE_EXECUTE_WRITECOPY;

    let mut cur = addr;
    while cur < end {
        let mut mbi = MEMORY_BASIC_INFORMATION::default();
        if unsafe {
            VirtualQuery(
                cur as LPCVOID,
                &mut mbi,
                std::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
            )
        } == 0
        {
            return false;
        }
        if mbi.State != MEM_COMMIT || (mbi.Protect & (PAGE_NOACCESS | PAGE_GUARD)) != 0 {
            return false;
        }
        if (mbi.Protect & 0xFF & writable_mask) == 0 {
            return false;
        }
        let region_end = (mbi.BaseAddress as usize).saturating_add(mbi.RegionSize);
        if region_end <= cur {
            return false;
        }
        cur = region_end;
    }
    true
}

/// Verify that `[addr, addr + need)` is committed and executable across all spanned pages.
pub fn page_exec_ok(addr: usize, need: usize) -> bool {
    let need = need.max(1);
    if addr < 0x10000 {
        return false;
    }
    let Some(end) = addr.checked_add(need) else {
        return false;
    };
    let exec_mask =
        PAGE_EXECUTE | PAGE_EXECUTE_READ | PAGE_EXECUTE_READWRITE | PAGE_EXECUTE_WRITECOPY;

    let mut cur = addr;
    while cur < end {
        let mut mbi = MEMORY_BASIC_INFORMATION::default();
        if unsafe {
            VirtualQuery(
                cur as LPCVOID,
                &mut mbi,
                std::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
            )
        } == 0
        {
            return false;
        }
        if mbi.State != MEM_COMMIT || (mbi.Protect & (PAGE_NOACCESS | PAGE_GUARD)) != 0 {
            return false;
        }
        if (mbi.Protect & 0xFF & exec_mask) == 0 {
            return false;
        }
        let region_end = (mbi.BaseAddress as usize).saturating_add(mbi.RegionSize);
        if region_end <= cur {
            return false;
        }
        cur = region_end;
    }
    true
}

// ---------------- Breadcrumb Crash Diagnostics ----------------
static BC_DEPTH: AtomicI32 = AtomicI32::new(-1);
static BC_IDX: AtomicI32 = AtomicI32::new(-1);

pub fn bc_set(d: i32, i: i32) {
    BC_DEPTH.store(d, Ordering::Relaxed);
    BC_IDX.store(i, Ordering::Relaxed);
}

pub unsafe extern "system" fn bc_filter(ep: *mut EXCEPTION_POINTERS) -> LONG {
    let code = if !ep.is_null() && !unsafe { (*ep).ExceptionRecord }.is_null() {
        unsafe { (*(*ep).ExceptionRecord).ExceptionCode }
    } else {
        0
    };
    if code == 0xC000_0005 || code == 0xC000_00FD || code == 0xC000_0094 || code == 0xC000_0409 {
        wlog!(
            "[!!!] CRASH {:08X} di traversal: depth={} idx={}",
            code,
            BC_DEPTH.load(Ordering::Relaxed),
            BC_IDX.load(Ordering::Relaxed)
        );
    }
    EXCEPTION_CONTINUE_SEARCH
}
