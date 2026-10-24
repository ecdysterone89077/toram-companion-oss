//! `wcore` — Pure Rust IL2CPP internal memory/hook module for Toram Online (v94).
//!
//! Exports:
//! - `DllMain`: records module identity; workers start only on explicit dispatch
//! - `ogl_drv_sync` / `wcore_cmd`: dispatches a supported command to the attached IL2CPP worker thread
//! - `ogl_drv_init` / `wcore_init`: load-time entrypoint compatible with remote-thread startup, wrapping `wcore_cmd`

#![allow(
    clippy::missing_safety_doc,
    clippy::not_unsafe_ptr_arg_deref,
    clippy::too_many_arguments
)]

pub mod chain;
mod combat_observe;
mod combat_response;
mod combat_state;
mod critical_path;
pub mod faunas;
mod faunas_hooks;
pub mod features;
pub mod hook;
pub mod il2cpp;
pub mod peb;
pub mod runtime;
pub mod scanner;
mod scoped_callback;
pub mod win;

use core::ffi::c_void;
use core::sync::atomic::{AtomicUsize, Ordering};
use runtime::{run_wcore_cmd, GLOBAL_RUNTIME};
use win::{BOOL, DLL_PROCESS_ATTACH, DLL_PROCESS_DETACH, HMODULE};

static MODULE_BASE: AtomicUsize = AtomicUsize::new(0);

#[inline]
pub fn module_base() -> usize {
    MODULE_BASE.load(Ordering::Relaxed)
}

#[no_mangle]
pub unsafe extern "C" fn ogl_drv_sync(cmd: i32) -> i32 {
    run_wcore_cmd(cmd)
}

#[no_mangle]
pub unsafe extern "system" fn ogl_drv_init(param: *mut c_void) -> u32 {
    let cmd = (param as usize) as i32;
    run_wcore_cmd(cmd) as u32
}

#[no_mangle]
pub unsafe extern "C" fn wcore_cmd(cmd: i32) -> i32 {
    run_wcore_cmd(cmd)
}

#[no_mangle]
pub unsafe extern "system" fn wcore_init(param: *mut c_void) -> u32 {
    let cmd = (param as usize) as i32;
    run_wcore_cmd(cmd) as u32
}

#[no_mangle]
pub unsafe extern "system" fn DllMain(
    hinst_dll: HMODULE,
    fdw_reason: u32,
    _lpv_reserved: *mut c_void,
) -> BOOL {
    match fdw_reason {
        DLL_PROCESS_ATTACH => {
            MODULE_BASE.store(hinst_dll as usize, Ordering::Release);
        }
        DLL_PROCESS_DETACH => {
            // Shutdown must be explicit before FreeLibrary; never wait under loader lock.
            GLOBAL_RUNTIME.stopping.store(1, Ordering::SeqCst);
        }
        _ => {}
    }
    1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chain::{is_valid_il2cpp_class, is_valid_il2cpp_obj};
    use crate::features::{
        force_critical_on_action, hk_calc_base_damage, hk_naa_action_hit, hk_naa_calc_mob_dmg,
        hk_pab_calc_mob_dmg, stamp_skill_damage_data, validate_or_reset_dd_cache,
        validate_or_reset_sdd_cache,
    };

    use crate::hook::MethodHookEntry;
    use crate::il2cpp::{add_primary_stats, resolve_method_code_ptr, GcScope};
    use crate::peb::{pe_find_export, peb_find_module, scrub_module_pe_headers};
    use crate::runtime::WorkerRuntime;
    use crate::win::{
        fn_readable, page_exec_ok, page_write_ok, ptr_ok, VirtualAlloc, VirtualFree,
        VirtualProtect, LOG_MUTEX_NAME, MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_EXECUTE_READ,
        PAGE_NOACCESS, PAGE_READONLY, PAGE_READWRITE,
    };
    use core::ptr;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn test_peb_and_export_resolution() {
        unsafe {
            let k32 = peb_find_module("kernel32.dll");
            assert_ne!(k32, 0, "kernel32.dll must be found via manual PEB walk");

            let sleep_addr = pe_find_export(k32, b"Sleep");
            assert_ne!(
                sleep_addr, 0,
                "Sleep export must be resolved from kernel32.dll"
            );
            assert!(
                fn_readable(sleep_addr),
                "Sleep code pointer must be executable/readable"
            );

            let bogus = pe_find_export(k32, b"NonExistentExport_12345");
            assert_eq!(bogus, 0);
        }
    }

    #[test]
    fn test_memory_page_guards() {
        unsafe {
            assert!(!ptr_ok(0, 4));
            assert!(!page_write_ok(0, 4));
            assert!(!page_exec_ok(0, 4));

            let mem = VirtualAlloc(
                ptr::null_mut(),
                4096,
                MEM_COMMIT | MEM_RESERVE,
                PAGE_READWRITE,
            );
            assert!(!mem.is_null());
            let addr = mem as usize;

            assert!(ptr_ok(addr, 64));
            assert!(page_write_ok(addr, 64));
            assert!(!page_exec_ok(addr, 16));

            let mut old = 0u32;
            assert_ne!(VirtualProtect(mem, 4096, PAGE_READONLY, &mut old), 0);
            assert!(ptr_ok(addr, 64));
            assert!(!page_write_ok(addr, 64));

            assert_ne!(VirtualProtect(mem, 4096, PAGE_EXECUTE_READ, &mut old), 0);
            assert!(page_exec_ok(addr, 16));
            assert!(!page_write_ok(addr, 16));

            // Cross-boundary check into PAGE_NOACCESS
            let two_pages = VirtualAlloc(
                ptr::null_mut(),
                8192,
                MEM_COMMIT | MEM_RESERVE,
                PAGE_READWRITE,
            );
            assert!(!two_pages.is_null());
            let p2 = (two_pages as usize) + 4096;
            assert_ne!(
                VirtualProtect(p2 as *mut c_void, 4096, PAGE_NOACCESS, &mut old),
                0
            );
            assert!(
                !ptr_ok(p2 - 4, 8),
                "Cross-page read into PAGE_NOACCESS must be rejected"
            );
            assert!(
                !page_write_ok(p2 - 4, 8),
                "Cross-page write into PAGE_NOACCESS must be rejected"
            );

            VirtualFree(two_pages, 0, MEM_RELEASE);
            VirtualFree(mem, 0, MEM_RELEASE);
        }
    }

    #[test]
    fn test_resolve_method_code_ptr_validation() {
        unsafe {
            assert_eq!(resolve_method_code_ptr(ptr::null()), None);

            // Non-executable data pointer inside MethodInfo must be rejected
            let mut fake_data_target: u64 = 0x9090_9090_C390_9090;
            let fake_method: usize = (&mut fake_data_target as *mut u64) as usize;
            assert_eq!(
                resolve_method_code_ptr((&fake_method as *const usize).cast()),
                None
            );

            // Real executable function pointer inside MethodInfo must succeed
            let k32 = peb_find_module("kernel32.dll");
            let sleep_fn = pe_find_export(k32, b"Sleep");
            assert_ne!(sleep_fn, 0);
            let valid_method: usize = sleep_fn;
            assert_eq!(
                resolve_method_code_ptr((&valid_method as *const usize).cast()),
                Some(sleep_fn)
            );
        }
    }

    #[test]
    fn test_find_pattern_against_live_module() {
        let (k32, sleep_fn) = unsafe {
            let k32 = peb::peb_find_module("kernel32.dll");
            assert_ne!(k32, 0);
            let sleep_fn = peb::pe_find_export(k32, b"Sleep");
            assert_ne!(sleep_fn, 0);
            (k32, sleep_fn)
        };

        let sleep_bytes = unsafe { core::slice::from_raw_parts(sleep_fn as *const u8, 4) };
        let mask = [0xFF, 0xFF, 0xFF, 0xFF];
        let found = peb::find_pattern(k32, sleep_bytes, &mask);
        assert!(found.is_some());
        assert_eq!(found.unwrap(), sleep_fn);

        let wildcard_mask = [0xFF, 0x00, 0xFF, 0x00];
        let found_wc = peb::find_pattern(k32, sleep_bytes, &wildcard_mask);
        assert!(found_wc.is_some());
    }

    #[test]
    fn test_add_primary_stats_validation_and_mutation() {
        #[repr(C)]
        struct FakePlayerStatus {
            header: [u8; 0x10],
            s_str: i32,
            s_int: i32,
            s_vit: i32,
            s_agi: i32,
            s_dex: i32,
            s_crt: i32,
        }

        let mut st = FakePlayerStatus {
            header: [0; 0x10],
            s_str: 10,
            s_int: 20,
            s_vit: 30,
            s_agi: 40,
            s_dex: 50,
            s_crt: 60,
        };
        let offsets = [0x10, 0x14, 0x18, 0x1C, 0x20, 0x24];

        // Null pointer and non-positive delta
        assert_eq!(add_primary_stats(ptr::null_mut(), &offsets, 100), -1);
        assert_eq!(
            add_primary_stats((&mut st as *mut FakePlayerStatus).cast(), &offsets, 0),
            -1
        );
        assert_eq!(
            add_primary_stats((&mut st as *mut FakePlayerStatus).cast(), &offsets, -5),
            -1
        );

        // Invalid offsets
        let bad_offsets = [0, 0x14, 0x18, 0x1C, 0x20, 0x24];
        assert_eq!(
            add_primary_stats((&mut st as *mut FakePlayerStatus).cast(), &bad_offsets, 10),
            -2
        );
        let huge_offsets = [0x3000, 0x14, 0x18, 0x1C, 0x20, 0x24];
        assert_eq!(
            add_primary_stats((&mut st as *mut FakePlayerStatus).cast(), &huge_offsets, 10),
            -2
        );

        // Valid mutation + readback
        assert_eq!(
            add_primary_stats((&mut st as *mut FakePlayerStatus).cast(), &offsets, 500),
            0
        );
        assert_eq!(st.s_str, 510);
        assert_eq!(st.s_int, 520);
        assert_eq!(st.s_vit, 530);
        assert_eq!(st.s_agi, 540);
        assert_eq!(st.s_dex, 550);
        assert_eq!(st.s_crt, 560);

        // Out-of-bounds (> 1_000_000) rejection without partial write
        assert_eq!(
            add_primary_stats((&mut st as *mut FakePlayerStatus).cast(), &offsets, 999_900),
            -4
        );
        assert_eq!(st.s_str, 510, "Must not partially mutate on bounds failure");

        // Read-only memory page rejection (-3)
        unsafe {
            let ro_mem = VirtualAlloc(
                ptr::null_mut(),
                4096,
                MEM_COMMIT | MEM_RESERVE,
                PAGE_READONLY,
            );
            assert!(!ro_mem.is_null());
            assert_eq!(add_primary_stats(ro_mem, &offsets, 10), -3);
            VirtualFree(ro_mem, 0, MEM_RELEASE);
        }
    }

    #[test]
    fn test_method_hook_entry_swap_and_restore() {
        let mut fake_method: [usize; 4] = [0x1111, 0x2222, 0, 0];
        let mut fake_class: [usize; 32] = [0; 32];
        let m_ptr = fake_method.as_mut_ptr();
        // Place VirtualInvokeData at fake_class[11..=12]
        fake_class[11] = 0x1111;
        fake_class[12] = m_ptr as usize;

        let mut entry = MethodHookEntry::new();
        unsafe {
            entry.method = m_ptr;
            entry.orig_fn0 = fake_method[0];
            entry.orig_fn1 = fake_method[1];
            entry.vtable_slot = &mut fake_class[11];
            entry.orig_vtable_fn = fake_class[11];
            entry.active = true;

            *entry.method = 0x9999;
            *entry.method.add(1) = 0x9999;
            *entry.vtable_slot = 0x9999;

            assert_eq!(fake_method[0], 0x9999);
            assert_eq!(fake_method[1], 0x9999);
            assert_eq!(fake_class[11], 0x9999);

            // Disable and verify full restoration
            entry.set(ptr::null_mut(), "Test", 0, 0x9999, false, "test");
            assert!(!entry.active);
            assert_eq!(fake_method[0], 0x1111);
            assert_eq!(fake_method[1], 0x2222);
            assert_eq!(fake_class[11], 0x1111);
        }
    }

    #[test]
    fn test_gc_scope_drop_cleans_handles() {
        let mut scope = GcScope::new();
        assert!(scope.pin(ptr::null_mut()).is_null());
    }

    #[test]
    fn test_worker_runtime_lifecycle_and_concurrency() {
        let rt = Arc::new(WorkerRuntime::new());
        rt.set_job_hook(Some(|cmd| match cmd {
            1 => 0,
            2 => 42,
            68 => {
                unsafe { win::Sleep(80) };
                0
            }
            69 => {
                unsafe { win::Sleep(250) };
                0
            }
            70 => panic!("simulated worker fault"),
            _ => 0,
        }));

        // Invalid range and blocked cmd=44
        assert_eq!(rt.dispatch_cmd(-1), -4);
        assert_eq!(rt.dispatch_cmd(protocol::CommandId::MAX + 1), -4);
        assert_eq!(rt.dispatch_cmd(44), -7);
        assert_eq!(rt.dispatch_cmd(4), -4);
        assert_eq!(rt.dispatch_cmd(96), -10);
        assert_eq!(rt.worker_thread.load(Ordering::SeqCst), 0);

        // Normal job execution
        assert_eq!(rt.dispatch_cmd(1), 0);
        assert_eq!(rt.dispatch_cmd(2), 42);
        assert_eq!(
            rt.dispatch_cmd(protocol::CommandId::CritDamageAdd1000.as_i32()),
            0
        );
        assert_eq!(
            rt.dispatch_cmd(protocol::CommandId::ObserveAttackResponses.as_i32()),
            0
        );

        // Busy rejection (-2) when a job is already in flight
        let rt_bg = Arc::clone(&rt);
        let t = thread::spawn(move || rt_bg.dispatch_cmd(68));
        unsafe { win::Sleep(20) };
        assert_eq!(rt.dispatch_cmd(1), -2);
        assert_eq!(t.join().unwrap(), 0);

        // Graceful shutdown via cmd=0 and automatic restart on cmd=1
        assert_eq!(rt.dispatch_cmd(0), 0);
        assert_eq!(rt.dispatch_cmd(1), 0);
        assert_eq!(rt.dispatch_cmd(0), 0);
    }

    #[test]
    fn test_worker_runtime_timeout_and_panic_isolation() {
        // Timeout test: job takes 250ms, wait timeout is 40ms -> returns -3 and keeps admission closed (-2)
        let rt_timeout = WorkerRuntime::new();
        rt_timeout.set_call_wait_ms(40);
        rt_timeout.set_job_hook(Some(|_cmd| {
            unsafe { win::Sleep(250) };
            0
        }));
        assert_eq!(rt_timeout.dispatch_cmd(69), -3);
        assert_eq!(
            rt_timeout.dispatch_cmd(1),
            -2,
            "Admission must stay closed after wait timeout"
        );
        unsafe { win::Sleep(260) };
        let stopped = rt_timeout.stop_worker(500);
        if stopped != 0 {
            // Keep the fixture alive until its native worker has exited even
            // when testing the pre-fix failure.
            rt_timeout.call_busy.store(0, Ordering::SeqCst);
            let _ = rt_timeout.stop_worker(500);
        }
        assert_eq!(stopped, 0, "A completed timed-out job must permit shutdown");

        // Panic isolation test: panicking job returns -6 and transitions ready to -1
        let rt_panic = WorkerRuntime::new();
        rt_panic.set_job_hook(Some(|_cmd| {
            panic!("simulated fault in worker");
        }));
        assert_eq!(rt_panic.dispatch_cmd(70), -6);
        assert_eq!(rt_panic.ready.load(Ordering::SeqCst), -1);
        assert_eq!(rt_panic.dispatch_cmd(1), -3);
        let _ = rt_panic.stop_worker(500);
    }

    #[test]
    fn timed_out_completion_is_consumed_before_the_next_job() {
        let rt = WorkerRuntime::new();
        rt.set_job_hook(Some(|cmd| {
            if cmd == 69 {
                unsafe { win::Sleep(150) };
            }
            cmd * 10
        }));
        assert_eq!(rt.dispatch_cmd(1), 10);
        rt.set_call_wait_ms(25);
        assert_eq!(rt.dispatch_cmd(69), -3);
        assert_eq!(rt.dispatch_cmd(2), -2);
        unsafe { win::Sleep(180) };
        rt.set_call_wait_ms(500);
        let next = rt.dispatch_cmd(2);
        let stopped = rt.stop_worker(500);
        assert_eq!(next, 20);
        assert_eq!(stopped, 0);
        assert_eq!(rt.worker_thread.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn concurrent_stop_calls_serialize_handle_cleanup() {
        let rt = Arc::new(WorkerRuntime::new());
        rt.set_job_hook(Some(|_| 0));
        for _ in 0..12 {
            assert_eq!(rt.dispatch_cmd(1), 0);
            let start = Arc::new(std::sync::Barrier::new(3));
            let workers: Vec<_> = (0..2)
                .map(|_| {
                    let rt = Arc::clone(&rt);
                    let start = Arc::clone(&start);
                    thread::spawn(move || {
                        start.wait();
                        rt.stop_worker(500)
                    })
                })
                .collect();
            start.wait();
            let results: Vec<_> = workers.into_iter().map(|t| t.join().unwrap()).collect();
            assert_eq!(results, [0, 0]);
            assert_eq!(rt.worker_thread.load(Ordering::SeqCst), 0);
            assert_eq!(rt.ev_job.load(Ordering::SeqCst), 0);
            assert_eq!(rt.ev_done.load(Ordering::SeqCst), 0);
            assert_eq!(rt.call_busy.load(Ordering::SeqCst), 0);
        }
    }

    #[test]
    fn test_mutex_name_is_neutral() {
        let s = core::str::from_utf8(LOG_MUTEX_NAME)
            .unwrap()
            .to_ascii_lowercase();
        for forbidden in ["toram", "wcore", "cheat", "hack", "companion"] {
            assert!(
                !s.contains(forbidden),
                "LOG_MUTEX_NAME must not contain '{}'",
                forbidden
            );
        }
    }

    #[test]
    fn test_scrub_module_pe_headers_zeros_image_markers_and_export_dir() {
        unsafe {
            let mem = VirtualAlloc(
                ptr::null_mut(),
                4096,
                MEM_COMMIT | MEM_RESERVE,
                PAGE_READWRITE,
            );
            assert!(!mem.is_null());
            let base = mem as usize;

            // Construct a valid synthetic PE64 header at base
            *(base as *mut u16) = 0x5A4D; // "MZ"
            *((base + 0x3C) as *mut i32) = 0x80; // e_lfanew = 0x80
            let nt = base + 0x80;
            *(nt as *mut u32) = 0x0000_4550; // "PE\0\0"
            *((nt + 0x88) as *mut u32) = 0x2000; // Export RVA
            *((nt + 0x8C) as *mut u32) = 0x100; // Export Size
                                                // Preserve SizeOfImage at OptionalHeader + 0x38 = nt + 0x50
            *((nt + 0x50) as *mut u32) = 0x0004_0000;

            let mut old_prot = 0u32;
            assert_ne!(VirtualProtect(mem, 4096, PAGE_READONLY, &mut old_prot), 0);

            assert!(scrub_module_pe_headers(base));

            // MZ, e_lfanew, PE magic, and Export DataDirectory must be zeroed
            assert_eq!(*(base as *const u16), 0);
            assert_eq!(*((base + 0x3C) as *const i32), 0);
            assert_eq!(*(nt as *const u32), 0);
            assert_eq!(*((nt + 0x88) as *const u32), 0);
            assert_eq!(*((nt + 0x8C) as *const u32), 0);
            // SizeOfImage at nt + 0x50 must remain intact
            assert_eq!(*((nt + 0x50) as *const u32), 0x0004_0000);

            // Second call on already-scrubbed header returns false cleanly
            assert!(!scrub_module_pe_headers(base));

            VirtualFree(mem, 0, MEM_RELEASE);
        }
    }

    #[test]
    fn test_combat_hooks_and_sdd_cache_null_and_scene_transition_resilience() {
        unsafe {
            assert!(!is_valid_il2cpp_class(ptr::null()));
            assert!(!is_valid_il2cpp_class(0x1234 as *const c_void));
            assert!(!is_valid_il2cpp_obj(ptr::null()));
            assert!(!is_valid_il2cpp_obj(0x1234 as *const c_void));

            // Allocate a fake object whose class pointer is null/corrupted (simulating destroyed scene object)
            let mut fake_corrupted_obj: [usize; 32] = [0; 32];
            let corrupted_ptr = fake_corrupted_obj.as_mut_ptr() as *mut c_void;
            assert!(!is_valid_il2cpp_obj(corrupted_ptr));

            // Allocate a valid fake class and object
            let fake_class: [usize; 16] = [0xABCD; 16];
            assert!(is_valid_il2cpp_class(fake_class.as_ptr() as *const c_void));
            ptr::write_volatile(&mut fake_corrupted_obj[0], fake_class.as_ptr() as usize);
            assert!(is_valid_il2cpp_obj(corrupted_ptr));

            // All forwarding combat hooks and helpers must safely no-op on null or corrupted pointers
            assert_eq!(
                hk_calc_base_damage(
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                    0,
                    0,
                    false,
                    ptr::null()
                ),
                0
            );
            hk_pab_calc_mob_dmg(
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null(),
            );
            hk_naa_calc_mob_dmg(
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null(),
            );
            hk_naa_action_hit(
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null(),
            );
            stamp_skill_damage_data(ptr::null_mut(), 200);
            force_critical_on_action(ptr::null_mut());
            validate_or_reset_sdd_cache();
            validate_or_reset_dd_cache();
        }
    }
}
