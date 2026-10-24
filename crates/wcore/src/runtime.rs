//! Background worker thread lifecycle, event synchronization, admission control,
//! Rust panic isolation (`WorkerRuntime`), and `SharedTelemetry` state publishing.

#![allow(function_casts_as_integer)]

use crate::features::{dispatch_game_command, restore_all_hooks};
use crate::il2cpp::{api, resolve_api};
use crate::peb::peb_find_module;
use crate::win::{
    log_fmt, CloseHandle, CreateEventW, CreateThread, GetCurrentProcessId, SetEvent, Sleep,
    WaitForSingleObject, HANDLE, INFINITE, WAIT_OBJECT_0,
};
use core::ffi::c_void;
use core::ptr;
use core::sync::atomic::{AtomicI32, AtomicUsize, Ordering};
use protocol::{telemetry, CommandId};
use std::panic::{catch_unwind, AssertUnwindSafe};

pub type JobHookFn = fn(i32) -> i32;

pub struct WorkerRuntime {
    pub ev_job: AtomicUsize,
    pub ev_done: AtomicUsize,
    pub worker_thread: AtomicUsize,
    pub job_cmd: AtomicI32,
    pub job_status: AtomicI32,
    pub ready: AtomicI32,
    pub stopping: AtomicI32,
    /// 0 = idle, 1 = caller owns the result, 2 = caller timed out, 3 = stopping.
    pub call_busy: AtomicI32,
    pub call_wait_ms: AtomicUsize,
    pub job_hook: AtomicUsize,
}

impl Default for WorkerRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl WorkerRuntime {
    pub const fn new() -> Self {
        Self {
            ev_job: AtomicUsize::new(0),
            ev_done: AtomicUsize::new(0),
            worker_thread: AtomicUsize::new(0),
            job_cmd: AtomicI32::new(0),
            job_status: AtomicI32::new(-1),
            ready: AtomicI32::new(0),
            stopping: AtomicI32::new(0),
            call_busy: AtomicI32::new(0),
            call_wait_ms: AtomicUsize::new(60_000),
            job_hook: AtomicUsize::new(0),
        }
    }

    pub fn set_job_hook(&self, hook: Option<JobHookFn>) {
        let ptr_val = hook.map(|f| f as usize).unwrap_or(0);
        self.job_hook.store(ptr_val, Ordering::SeqCst);
    }

    pub fn set_call_wait_ms(&self, ms: u32) {
        self.call_wait_ms.store(ms as usize, Ordering::SeqCst);
    }

    unsafe fn close_handle_slot(slot: &AtomicUsize) {
        let old = slot.swap(0, Ordering::SeqCst);
        if old != 0 {
            unsafe {
                CloseHandle(old as HANDLE);
            }
        }
    }

    unsafe extern "system" fn worker_thread_entry(param: *mut c_void) -> u32 {
        let rt = unsafe { &*(param as *const WorkerRuntime) };
        rt.worker_loop()
    }

    fn worker_loop(&self) -> u32 {
        let has_hook = self.job_hook.load(Ordering::SeqCst) != 0;
        let pid = unsafe { GetCurrentProcessId() };

        if !has_hook {
            log_fmt(format_args!(
                "=== core {} worker PID={} base=0x{:X}; initialized ===",
                env!("CARGO_PKG_VERSION"),
                pid,
                crate::module_base()
            ));
            if let Some(shm) = telemetry() {
                shm.init(pid);
                shm.update(|data| data.worker_state.store(0, Ordering::SeqCst));
            }

            let mut ga = 0usize;
            for _ in 0..300 {
                if self.stopping.load(Ordering::SeqCst) != 0 {
                    self.ready.store(-1, Ordering::SeqCst);
                    if let Some(shm) = telemetry() {
                        shm.update(|data| {
                            data.worker_state.store(-1, Ordering::SeqCst);
                        });
                    }
                    return 0;
                }
                ga = unsafe { peb_find_module("GameAssembly.dll") };
                if ga != 0 {
                    break;
                }
                unsafe { Sleep(100) };
            }
            if ga == 0 || unsafe { resolve_api(ga) }.is_none() {
                log_fmt(format_args!(
                    "[worker] failed to resolve GameAssembly.dll API"
                ));
                self.ready.store(-1, Ordering::SeqCst);
                if let Some(shm) = telemetry() {
                    shm.update(|data| {
                        data.worker_state.store(-1, Ordering::SeqCst);
                    });
                }
                return 0;
            }
            if let Some(api) = api() {
                let mut dom = ptr::null_mut();
                for _ in 0..300 {
                    if self.stopping.load(Ordering::SeqCst) != 0 {
                        self.ready.store(-1, Ordering::SeqCst);
                        if let Some(shm) = telemetry() {
                            shm.update(|data| {
                                data.worker_state.store(-1, Ordering::SeqCst);
                            });
                        }
                        return 0;
                    }
                    dom = unsafe { (api.domain_get)() };
                    if !dom.is_null() {
                        break;
                    }
                    unsafe { Sleep(100) };
                }
                if dom.is_null() {
                    self.ready.store(-1, Ordering::SeqCst);
                    if let Some(shm) = telemetry() {
                        shm.update(|data| {
                            data.worker_state.store(-1, Ordering::SeqCst);
                        });
                    }
                    return 0;
                }
                unsafe {
                    (api.thread_attach)(dom);
                }

                let base = crate::module_base();
                log_fmt(format_args!(
                    "[worker] runtime initialized cleanly for domain {:p} base=0x{:X}",
                    dom, base
                ));
            }
        }

        self.ready.store(1, Ordering::SeqCst);
        if !has_hook {
            if let Some(shm) = telemetry() {
                shm.update(|data| {
                    data.worker_state.store(1, Ordering::SeqCst);
                });
            }
        }

        loop {
            let ev_job = self.ev_job.load(Ordering::SeqCst) as HANDLE;
            if ev_job.is_null() {
                break;
            }
            let w = unsafe { WaitForSingleObject(ev_job, INFINITE) };
            if w != WAIT_OBJECT_0 {
                break;
            }
            let cmd = self.job_cmd.load(Ordering::SeqCst);
            if cmd < 0 || self.stopping.load(Ordering::SeqCst) != 0 {
                self.job_status.store(0, Ordering::SeqCst);
                if !has_hook {
                    if let Some(shm) = telemetry() {
                        shm.update(|data| {
                            data.worker_state.store(-1, Ordering::SeqCst);
                        });
                    }
                }
                let ev_done = self.ev_done.load(Ordering::SeqCst) as HANDLE;
                if !ev_done.is_null() {
                    unsafe { SetEvent(ev_done) };
                }
                break;
            }

            if !has_hook {
                if let Some(shm) = telemetry() {
                    shm.update(|data| {
                        data.worker_state.store(2, Ordering::SeqCst);
                    });
                }
            }

            let hook_ptr = self.job_hook.load(Ordering::SeqCst);
            let res = catch_unwind(AssertUnwindSafe(|| -> i32 {
                if hook_ptr != 0 {
                    let f: JobHookFn = unsafe { core::mem::transmute(hook_ptr) };
                    f(cmd)
                } else {
                    unsafe { dispatch_game_command(cmd) }
                }
            }));

            match res {
                Ok(code) => {
                    self.job_status.store(code, Ordering::SeqCst);
                    if !has_hook {
                        if let Some(shm) = telemetry() {
                            shm.update(|data| {
                                data.worker_state.store(1, Ordering::SeqCst);
                            });
                        }
                    }
                }
                Err(_) => {
                    log_fmt(format_args!("[worker] panic caught during cmd={}", cmd));
                    self.job_status.store(-6, Ordering::SeqCst);
                    self.ready.store(-1, Ordering::SeqCst);
                    if !has_hook {
                        if let Some(shm) = telemetry() {
                            shm.update(|data| {
                                data.worker_state.store(-1, Ordering::SeqCst);
                            });
                        }
                    }
                    let ev_done = self.ev_done.load(Ordering::SeqCst) as HANDLE;
                    if !ev_done.is_null() {
                        unsafe { SetEvent(ev_done) };
                    }
                    break;
                }
            }

            let ev_done = self.ev_done.load(Ordering::SeqCst) as HANDLE;
            if !ev_done.is_null() {
                unsafe { SetEvent(ev_done) };
            }
        }

        if !has_hook && !unsafe { restore_all_hooks() } {
            self.job_status.store(-8, Ordering::SeqCst);
        }
        0
    }

    pub fn ensure_worker(&self) -> bool {
        if self.stopping.load(Ordering::SeqCst) != 0 {
            return false;
        }
        if self.worker_thread.load(Ordering::SeqCst) != 0 {
            return self.ready.load(Ordering::SeqCst) >= 0;
        }

        let ev_j = unsafe { CreateEventW(ptr::null(), 0, 0, ptr::null()) };
        let ev_d = unsafe { CreateEventW(ptr::null(), 0, 0, ptr::null()) };
        if ev_j.is_null() || ev_d.is_null() {
            if !ev_j.is_null() {
                unsafe { CloseHandle(ev_j) };
            }
            if !ev_d.is_null() {
                unsafe { CloseHandle(ev_d) };
            }
            return false;
        }

        self.ev_job.store(ev_j as usize, Ordering::SeqCst);
        self.ev_done.store(ev_d as usize, Ordering::SeqCst);
        self.ready.store(0, Ordering::SeqCst);

        let self_ptr = (self as *const WorkerRuntime) as *mut c_void;
        let th = unsafe {
            CreateThread(
                ptr::null(),
                0,
                Some(Self::worker_thread_entry),
                self_ptr,
                0,
                ptr::null_mut(),
            )
        };
        if th.is_null() {
            unsafe {
                Self::close_handle_slot(&self.ev_job);
                Self::close_handle_slot(&self.ev_done);
            }
            return false;
        }
        self.worker_thread.store(th as usize, Ordering::SeqCst);
        true
    }

    fn reap_timed_out_call(&self) {
        if self
            .call_busy
            .compare_exchange(2, 1, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }
        let event = self.ev_done.load(Ordering::SeqCst) as HANDLE;
        let complete =
            !event.is_null() && unsafe { WaitForSingleObject(event, 0) } == WAIT_OBJECT_0;
        // Consume the previous auto-reset completion before admitting another
        // job, so its caller cannot receive the timed-out job's result.
        self.call_busy
            .store(if complete { 0 } else { 2 }, Ordering::SeqCst);
    }

    pub fn stop_worker(&self, wait_ms: u32) -> i32 {
        self.stopping.store(1, Ordering::SeqCst);

        let steps = (wait_ms / 10).max(1);
        let mut owns_admission = false;
        for _ in 0..steps {
            self.reap_timed_out_call();
            if self
                .call_busy
                .compare_exchange(0, 3, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                owns_admission = true;
                break;
            }
            unsafe { Sleep(10) };
        }
        if !owns_admission {
            return -2;
        }

        let th = self.worker_thread.load(Ordering::SeqCst) as HANDLE;
        if !th.is_null() {
            self.job_cmd.store(-1, Ordering::SeqCst);
            let ev_j = self.ev_job.load(Ordering::SeqCst) as HANDLE;
            if !ev_j.is_null() {
                unsafe { SetEvent(ev_j) };
            }
            let w = unsafe { WaitForSingleObject(th, wait_ms.max(100)) };
            if w != WAIT_OBJECT_0 {
                self.call_busy.store(0, Ordering::SeqCst);
                return -3;
            }
        }

        unsafe {
            Self::close_handle_slot(&self.worker_thread);
            Self::close_handle_slot(&self.ev_job);
            Self::close_handle_slot(&self.ev_done);
        }
        self.ready.store(0, Ordering::SeqCst);
        self.stopping.store(0, Ordering::SeqCst);
        self.call_busy.store(0, Ordering::SeqCst);
        if self.job_status.load(Ordering::SeqCst) == -8 {
            -8
        } else {
            0
        }
    }

    pub fn dispatch_cmd(&self, cmd: i32) -> i32 {
        if !CommandId::is_valid_range(cmd) {
            return -4;
        }
        if CommandId::unavailable_reason(cmd).is_some() {
            return -10;
        }
        if cmd == 44 {
            log_fmt(format_args!(
                "[guard] cmd=44 blocked (primary stat mutation causes server sanction)"
            ));
            return -7;
        }
        if cmd == 0 {
            let wait_ms = self.call_wait_ms.load(Ordering::SeqCst) as u32;
            return self.stop_worker(wait_ms);
        }
        if self.stopping.load(Ordering::SeqCst) != 0 {
            return -5;
        }

        self.reap_timed_out_call();
        if self
            .call_busy
            .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return -2;
        }

        if !self.ensure_worker() {
            self.call_busy.store(0, Ordering::SeqCst);
            return -3;
        }

        let wait_ms = self.call_wait_ms.load(Ordering::SeqCst) as u32;
        let ready_iters = (wait_ms / 10).max(1);
        for _ in 0..ready_iters {
            if self.ready.load(Ordering::SeqCst) != 0 {
                break;
            }
            unsafe { Sleep(10) };
        }
        if self.ready.load(Ordering::SeqCst) <= 0 {
            self.call_busy.store(0, Ordering::SeqCst);
            return -3;
        }
        if self.stopping.load(Ordering::SeqCst) != 0 {
            self.call_busy.store(0, Ordering::SeqCst);
            return -5;
        }

        self.job_status.store(-1, Ordering::SeqCst);
        self.job_cmd.store(cmd, Ordering::SeqCst);

        let ev_j = self.ev_job.load(Ordering::SeqCst) as HANDLE;
        let ev_d = self.ev_done.load(Ordering::SeqCst) as HANDLE;
        if ev_j.is_null() || ev_d.is_null() {
            self.call_busy.store(0, Ordering::SeqCst);
            return -3;
        }

        unsafe {
            SetEvent(ev_j);
        }
        let w = unsafe { WaitForSingleObject(ev_d, wait_ms) };
        if w != WAIT_OBJECT_0 {
            self.call_busy.store(2, Ordering::SeqCst);
            return -3;
        }

        let status = self.job_status.load(Ordering::SeqCst);
        self.call_busy.store(0, Ordering::SeqCst);
        status
    }
}

pub static GLOBAL_RUNTIME: WorkerRuntime = WorkerRuntime::new();

pub fn run_wcore_cmd(cmd: i32) -> i32 {
    let status = GLOBAL_RUNTIME.dispatch_cmd(cmd);
    let pid = unsafe { GetCurrentProcessId() };
    if let Some(shm) = telemetry() {
        shm.record_cmd_result(pid, cmd, status);
    }
    log_fmt(format_args!(
        "[result] pid={} cmd={} status={}",
        pid, cmd, status
    ));
    status
}
