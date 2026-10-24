//! Heap-based IL2CPP `MethodInfo` + `Il2CppClass` vtable slot hook manager ("The Heap Door").
//!
//! Because AppSign locks `.text` pages in `GameAssembly.dll` at the kernel level,
//! we never modify executable code bytes. Instead, we overwrite:
//! 1. `MethodInfo->methodPointer` (`+0x00`)
//! 2. `MethodInfo->virtualMethodPointer` (`+0x08`)
//! 3. Matching `VirtualInvokeData` slot inside `Il2CppClass` (`k_words[i - 1]` where `k_words[i] == method`)

use crate::il2cpp::{get_method, get_method_exact, MethodSignature};
use crate::win::{log_fmt, page_write_ok, ptr_ok};
use core::ffi::c_void;
use core::ptr;
use core::sync::atomic::{fence, AtomicUsize, Ordering};
use std::sync::Mutex;

static ACTIVE_METHODS: Mutex<Vec<usize>> = Mutex::new(Vec::new());

fn publish_original_then(original: usize, sink: Option<&AtomicUsize>, activate: impl FnOnce()) {
    if let Some(sink) = sink {
        sink.store(original, Ordering::Release);
    }
    fence(Ordering::SeqCst);
    activate();
}

// Il2CppClass layout for the installed metadata-v31 x64 runtime. Only inspect
// actual VirtualInvokeData pairs; adjacent class allocations are not our vtable.
unsafe fn vtable_slot(klass: *mut c_void, method: *mut usize) -> *mut usize {
    let base = klass as usize;
    if !ptr_ok(base, 0x138) {
        return ptr::null_mut();
    }
    let count = unsafe { ptr::read_unaligned((base + 0x12A) as *const u16) } as usize;
    if count > 4096 {
        return ptr::null_mut();
    }
    for index in 0..count {
        let address = base + 0x138 + index * 16;
        if !ptr_ok(address, 16) {
            return ptr::null_mut();
        }
        if unsafe { *((address + 8) as *const usize) } == method as usize
            && page_write_ok(address, 8)
        {
            return address as *mut usize;
        }
    }
    ptr::null_mut()
}

#[derive(Clone, Copy)]
pub struct MethodHookEntry {
    pub method: *mut usize,
    pub orig_fn0: usize,
    pub orig_fn1: usize,
    pub vtable_slot: *mut usize,
    pub orig_vtable_fn: usize,
    pub active: bool,
    pub is_vtable_only: bool,
    installed_fn: usize,
}

unsafe impl Send for MethodHookEntry {}
unsafe impl Sync for MethodHookEntry {}

impl MethodHookEntry {
    pub const fn new() -> Self {
        Self {
            method: ptr::null_mut(),
            orig_fn0: 0,
            orig_fn1: 0,
            vtable_slot: ptr::null_mut(),
            orig_vtable_fn: 0,
            active: false,
            is_vtable_only: false,
            installed_fn: 0,
        }
    }
}

impl Default for MethodHookEntry {
    fn default() -> Self {
        Self::new()
    }
}

impl MethodHookEntry {
    /// Install or remove a heap + vtable hook on `klass::method_name(argc)`.
    pub unsafe fn set(
        &mut self,
        klass: *mut c_void,
        method_name: &str,
        argc: i32,
        hook_fn: usize,
        enable: bool,
        tag: &str,
    ) -> bool {
        unsafe {
            self.set_with_signature(klass, method_name, argc, hook_fn, enable, tag, None, None)
        }
    }

    pub unsafe fn set_exact(
        &mut self,
        klass: *mut c_void,
        method_name: &str,
        argc: i32,
        hook_fn: usize,
        enable: bool,
        tag: &str,
        signature: &MethodSignature,
    ) -> bool {
        unsafe {
            self.set_with_signature(
                klass,
                method_name,
                argc,
                hook_fn,
                enable,
                tag,
                Some(signature),
                None,
            )
        }
    }

    pub unsafe fn set_with_signature(
        &mut self,
        klass: *mut c_void,
        method_name: &str,
        argc: i32,
        hook_fn: usize,
        enable: bool,
        tag: &str,
        signature: Option<&MethodSignature>,
        orig_sink: Option<&AtomicUsize>,
    ) -> bool {
        if !enable {
            return unsafe { self.restore(method_name, argc, tag) };
        }
        if klass.is_null() || hook_fn == 0 {
            return false;
        }
        let m = if let Some(signature) = signature {
            if argc < 0 || signature.parameters.len() != argc as usize {
                return false;
            }
            unsafe { get_method_exact(klass, method_name, signature) }
        } else {
            unsafe { get_method(klass, method_name, argc) }
        } as *mut usize;
        let installed = unsafe { self.install_resolved(klass, m, hook_fn, orig_sink) };
        log_fmt(format_args!(
            "[{}] {} {}({}) MethodInfo={:p} orig=0x{:X} hook=0x{:X} vtableSlot={:p}",
            tag,
            if installed { "HOOKED" } else { "FAILED" },
            method_name,
            argc,
            m,
            self.orig_fn0,
            hook_fn,
            self.vtable_slot
        ));
        installed
    }

    /// Redirect a proven virtual dispatch without changing inherited MethodInfo.
    pub unsafe fn set_vtable_with_signature(
        &mut self,
        klass: *mut c_void,
        method_name: &str,
        signature: &MethodSignature,
        hook_fn: usize,
        tag: &str,
        orig_sink: Option<&AtomicUsize>,
    ) -> bool {
        let method = unsafe { get_method_exact(klass, method_name, signature) } as *mut usize;
        let installed = unsafe { self.install_vtable_resolved(klass, method, hook_fn, orig_sink) };
        log_fmt(format_args!(
            "[{}] {} {} vtable-only slot={:p}",
            tag,
            if installed { "HOOKED" } else { "FAILED" },
            method_name,
            self.vtable_slot
        ));
        installed
    }

    unsafe fn install_vtable_resolved(
        &mut self,
        klass: *mut c_void,
        method: *mut usize,
        hook_fn: usize,
        orig_sink: Option<&AtomicUsize>,
    ) -> bool {
        if hook_fn == 0 || !ptr_ok(method as usize, 16) {
            return false;
        }
        let slot = unsafe { vtable_slot(klass, method) };
        if slot.is_null() {
            return false;
        }
        if self.active {
            return self.is_vtable_only
                && self.method == method
                && self.vtable_slot == slot
                && self.installed_fn == hook_fn
                && unsafe { self.owns_pointers() };
        }
        let Ok(mut owners) = ACTIVE_METHODS.lock() else {
            return false;
        };
        let original = unsafe { *slot };
        if owners.contains(&(slot as usize)) || original == 0 || original == hook_fn {
            return false;
        }
        self.method = method;
        self.orig_fn0 = original;
        self.orig_fn1 = original;
        self.vtable_slot = slot;
        self.orig_vtable_fn = original;
        self.is_vtable_only = true;
        owners.push(slot as usize);
        publish_original_then(original, orig_sink, || unsafe { *slot = hook_fn });
        self.installed_fn = hook_fn;
        self.active = true;
        true
    }

    unsafe fn install_resolved(
        &mut self,
        klass: *mut c_void,
        method: *mut usize,
        hook_fn: usize,
        orig_sink: Option<&AtomicUsize>,
    ) -> bool {
        if method.is_null() || hook_fn == 0 || !page_write_ok(method as usize, 16) {
            return false;
        }
        let Ok(mut owners) = ACTIVE_METHODS.lock() else {
            return false;
        };
        if self.active {
            if self.method != method
                || self.orig_fn0 == 0
                || self.orig_fn0 == hook_fn
                || self.installed_fn != hook_fn
                || unsafe { vtable_slot(klass, method) } != self.vtable_slot
            {
                return false;
            }
            if !self.vtable_slot.is_null() && !page_write_ok(self.vtable_slot as usize, 8) {
                return false;
            }
            if !unsafe { self.owns_pointers() } {
                return false;
            }
            // Repeated activation is an ownership check, not another install.
            // A derived-only entry must never write the shared MethodInfo.
            publish_original_then(self.orig_fn0, orig_sink, || {});
            return true;
        } else {
            let original = unsafe { *method };
            let virtual_original = unsafe { *method.add(1) };
            if owners.contains(&(method as usize)) {
                // Derived class vtable-only hook requires the MethodInfo to already be hooked to the SAME hook_fn.
                if original != hook_fn && virtual_original != hook_fn {
                    return false;
                }
                let vtable_slot = unsafe { vtable_slot(klass, method) };
                let orig_vtable_fn = if vtable_slot.is_null() {
                    0
                } else {
                    unsafe { *vtable_slot }
                };
                if orig_vtable_fn == 0 || orig_vtable_fn == hook_fn {
                    return false;
                }
                if vtable_slot.is_null() {
                    return false;
                }
                if owners.contains(&(vtable_slot as usize)) {
                    return false;
                }
                self.method = method;
                self.orig_fn0 = orig_vtable_fn;
                self.orig_fn1 = orig_vtable_fn;
                self.vtable_slot = vtable_slot;
                self.orig_vtable_fn = orig_vtable_fn;
                self.is_vtable_only = true;
                owners.push(vtable_slot as usize);
                publish_original_then(orig_vtable_fn, orig_sink, || unsafe {
                    *self.vtable_slot = hook_fn;
                });
                self.active = true;
                self.installed_fn = hook_fn;
                return true;
            }
            if original == 0 || original == hook_fn || virtual_original == hook_fn {
                return false;
            }
            self.method = method;
            self.orig_fn0 = original;
            self.orig_fn1 = virtual_original;
            self.vtable_slot = ptr::null_mut();
            self.orig_vtable_fn = 0;
            self.is_vtable_only = false;

            self.vtable_slot = unsafe { vtable_slot(klass, method) };
            if !self.vtable_slot.is_null() {
                if owners.contains(&(self.vtable_slot as usize)) {
                    return false;
                }
                self.orig_vtable_fn = unsafe { *self.vtable_slot };
            }
            owners.push(method as usize);
            if !self.vtable_slot.is_null() {
                owners.push(self.vtable_slot as usize);
            }
        }
        // The callback may run as soon as the first pointer is replaced.
        publish_original_then(self.orig_fn0, orig_sink, || unsafe {
            *method = hook_fn;
            *method.add(1) = hook_fn;
            if !self.vtable_slot.is_null() {
                *self.vtable_slot = hook_fn;
            }
        });
        self.active = true;
        self.installed_fn = hook_fn;
        true
    }

    unsafe fn owns_pointers(&self) -> bool {
        unsafe {
            (self.is_vtable_only
                || (*self.method == self.installed_fn && *self.method.add(1) == self.installed_fn))
                && (self.vtable_slot.is_null() || *self.vtable_slot == self.installed_fn)
        }
    }

    unsafe fn restore(&mut self, method_name: &str, argc: i32, tag: &str) -> bool {
        if !self.active {
            return true;
        }
        if self.method.is_null()
            || (!self.is_vtable_only && !page_write_ok(self.method as usize, 16))
            || (!self.vtable_slot.is_null() && !page_write_ok(self.vtable_slot as usize, 8))
        {
            return false;
        }
        if self.installed_fn != 0 && !unsafe { self.owns_pointers() } {
            log_fmt(format_args!(
                "[{}] restore conflict for {}({})",
                tag, method_name, argc
            ));
            return false;
        }
        let Ok(mut owners) = ACTIVE_METHODS.lock() else {
            return false;
        };
        unsafe {
            if !self.is_vtable_only {
                *self.method = self.orig_fn0;
                *self.method.add(1) = self.orig_fn1;
            }
            if !self.vtable_slot.is_null() {
                *self.vtable_slot = self.orig_vtable_fn;
            }
        }
        if !self.is_vtable_only {
            owners.retain(|&method| method != self.method as usize);
        }
        owners.retain(|&slot| slot != self.vtable_slot as usize);
        self.active = false;
        self.installed_fn = 0;
        self.is_vtable_only = false;
        log_fmt(format_args!(
            "[{}] RESTORED {}({}) MethodInfo={:p} -> orig=0x{:X}",
            tag, method_name, argc, self.method, self.orig_fn0
        ));
        true
    }
}

/// RAII guard tracking active executions of an in-process detour.
pub struct InflightGuard<'a>(&'a AtomicUsize);

impl<'a> InflightGuard<'a> {
    #[inline]
    pub fn enter(counter: &'a AtomicUsize) -> Self {
        counter.fetch_add(1, Ordering::AcqRel);
        Self(counter)
    }
}

impl<'a> Drop for InflightGuard<'a> {
    #[inline]
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Spin-wait until all in-flight detour executions have drained before unhooking.
pub fn drain_inflight(counter: &AtomicUsize) {
    while counter.load(Ordering::Acquire) != 0 {
        core::hint::spin_loop();
        unsafe {
            crate::win::Sleep(0);
        }
    }
}

/// Declarative macro for defining type-safe detour controllers with atomic gating and in-flight tracking.
#[macro_export]
macro_rules! declare_heap_hook {
    ($Hook:ident, $Ret:ty, ($($Arg:ident: $ArgTy:ty),*)) => {
        #[allow(dead_code)]
        pub struct $Hook {
            pub original: core::sync::atomic::AtomicUsize,
            pub enabled: core::sync::atomic::AtomicBool,
            pub inflight: core::sync::atomic::AtomicUsize,
        }
        #[allow(dead_code)]
        impl $Hook {
            pub const fn new() -> Self {
                Self {
                    original: core::sync::atomic::AtomicUsize::new(0),
                    enabled: core::sync::atomic::AtomicBool::new(false),
                    inflight: core::sync::atomic::AtomicUsize::new(0),
                }
            }
            #[inline]
            pub fn is_enabled(&self) -> bool {
                self.enabled.load(core::sync::atomic::Ordering::Acquire)
            }
            #[inline]
            pub fn set_enabled(&self, val: bool) {
                self.enabled.store(val, core::sync::atomic::Ordering::Release);
                if !val {
                    $crate::hook::drain_inflight(&self.inflight);
                }
            }
            #[inline]
            pub fn enter(&self) -> $crate::hook::InflightGuard<'_> {
                $crate::hook::InflightGuard::enter(&self.inflight)
            }
            #[inline]
            pub unsafe fn call_original(&self, $($Arg: $ArgTy),*) -> $Ret {
                let orig = self.original.load(core::sync::atomic::Ordering::Acquire);
                let func: unsafe extern "C" fn($($ArgTy),*) -> $Ret = core::mem::transmute(orig);
                func($($Arg),*)
            }
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derived_virtual_hook_preserves_inherited_method_and_sibling_dispatch() {
        let mut method = [0x1111usize, 0x2222];
        let mut local_class = [0usize; 60];
        let mut sibling_class = [0usize; 60];
        for class in [&mut local_class, &mut sibling_class] {
            class[39] = 0x3333;
            class[40] = method.as_mut_ptr() as usize;
            unsafe {
                *((class.as_mut_ptr() as usize + 0x12A) as *mut u16) = 1;
            }
        }
        let sink = AtomicUsize::new(0);
        let mut hook = MethodHookEntry::new();
        let mut duplicate = MethodHookEntry::new();
        unsafe {
            assert!(hook.install_vtable_resolved(
                local_class.as_mut_ptr().cast(),
                method.as_mut_ptr(),
                0x9999,
                Some(&sink)
            ));
            assert_eq!(sink.load(Ordering::Acquire), 0x3333);
            assert_eq!(method, [0x1111, 0x2222]);
            assert_eq!(local_class[39], 0x9999);
            assert_eq!(sibling_class[39], 0x3333);
            assert!(!duplicate.install_vtable_resolved(
                local_class.as_mut_ptr().cast(),
                method.as_mut_ptr(),
                0x8888,
                None
            ));
            assert!(hook.restore("virtual", 0, "test"));
        }
        assert_eq!(local_class[39], 0x3333);
        assert_eq!(method, [0x1111, 0x2222]);
    }

    #[test]
    fn original_is_published_before_activation() {
        let sink = AtomicUsize::new(0);
        let mut method = [0x1111usize, 0x2222];
        publish_original_then(method[0], Some(&sink), || {
            assert_eq!(sink.load(Ordering::Acquire), 0x1111);
            assert_eq!(method, [0x1111, 0x2222]);
            method.fill(0x9999);
        });
        assert_eq!(method, [0x9999, 0x9999]);
    }

    #[test]
    fn vtable_scan_ignores_header_and_adjacent_class_matches() {
        let mut class = [0usize; 90];
        let mut method = [0x1111usize, 0x2222];
        class[12] = method.as_mut_ptr() as usize;
        class[42] = method.as_mut_ptr() as usize;
        unsafe {
            *((class.as_mut_ptr() as usize + 0x12A) as *mut u16) = 1;
            assert!(vtable_slot(class.as_mut_ptr().cast(), method.as_mut_ptr()).is_null());
            class[40] = method.as_mut_ptr() as usize;
            assert_eq!(
                vtable_slot(class.as_mut_ptr().cast(), method.as_mut_ptr()),
                class.as_mut_ptr().add(39)
            );
        }
    }

    #[test]
    fn duplicate_and_self_hooks_fail_without_losing_originals() {
        let mut method = [0x1111usize, 0];
        let mut class = [0usize; 300];
        class[39] = 0x3333;
        class[40] = method.as_mut_ptr() as usize;
        let sink = AtomicUsize::new(0);
        let mut first = MethodHookEntry::new();
        let mut duplicate = MethodHookEntry::new();
        unsafe {
            *((class.as_mut_ptr() as usize + 0x12A) as *mut u16) = 1;
        }
        let class_ptr = class.as_mut_ptr().cast();
        unsafe {
            assert!(first.install_resolved(class_ptr, method.as_mut_ptr(), 0x9999, Some(&sink)));
            assert_eq!(sink.load(Ordering::Acquire), 0x1111);
            assert_eq!(method, [0x9999, 0x9999]);
            assert_eq!(class[39], 0x9999);
            assert!(!duplicate.install_resolved(class_ptr, method.as_mut_ptr(), 0x8888, None));
            assert!(!duplicate.active);
            assert_eq!(first.orig_fn0, 0x1111);
            assert!(first.restore("fake", 0, "test"));
            assert_eq!(method, [0x1111, 0]);
            assert_eq!(class[39], 0x3333);
            // Removing a hook leaves its original available to already-running callbacks.
            assert_eq!(sink.load(Ordering::Acquire), 0x1111);
            assert!(!duplicate.install_resolved(class_ptr, method.as_mut_ptr(), 0x1111, None));
            assert!(duplicate.install_resolved(class_ptr, method.as_mut_ptr(), 0x8888, None));
            assert!(duplicate.restore("fake", 0, "test"));
        }
    }

    #[test]
    fn restore_failure_preserves_ownership_and_external_changes() {
        let mut method = [0x1111usize, 0x2222];
        let method_ptr = method.as_mut_ptr();
        let mut entry = MethodHookEntry::new();
        let mut other = MethodHookEntry::new();
        unsafe {
            assert!(entry.install_resolved(ptr::null_mut(), method_ptr, 0x9999, None));
            method[0] = 0x7777;
            assert!(!entry.restore("fake", 0, "test"));
            assert!(entry.active);
            assert_eq!(method[0], 0x7777);
            assert!(!other.install_resolved(ptr::null_mut(), method_ptr, 0x8888, None));
            method[0] = 0x9999;
            entry.method = 0x1234 as *mut usize;
            assert!(!entry.restore("fake", 0, "test"));
            assert!(entry.active);
            entry.method = method_ptr;
            assert!(entry.restore("fake", 0, "test"));
            assert_eq!(method, [0x1111, 0x2222]);
            assert!(other.install_resolved(ptr::null_mut(), method_ptr, 0x8888, None));
            assert!(other.restore("fake", 0, "test"));
        }
    }

    #[test]
    fn derived_class_vtable_only_hook_succeeds_when_base_hooked() {
        let mut method = [0x1111usize, 0x2222];
        let mut base_class = [0usize; 300];
        base_class[39] = 0x3333;
        base_class[40] = method.as_mut_ptr() as usize;

        let mut derived_class = [0usize; 300];
        derived_class[41] = 0x4444; // original derived vtable slot
        derived_class[42] = method.as_mut_ptr() as usize;

        unsafe {
            *((base_class.as_mut_ptr() as usize + 0x12A) as *mut u16) = 1;
            *((derived_class.as_mut_ptr() as usize + 0x12A) as *mut u16) = 2;
        }
        let mut base_entry = MethodHookEntry::new();
        let mut derived_entry = MethodHookEntry::new();
        let derived_original = AtomicUsize::new(0);

        unsafe {
            // 1. Install on base class
            assert!(base_entry.install_resolved(
                base_class.as_mut_ptr().cast(),
                method.as_mut_ptr(),
                0x9999,
                None,
            ));
            assert_eq!(method, [0x9999, 0x9999]);
            assert_eq!(base_class[39], 0x9999);

            // 2. Install on derived class sharing same method
            assert!(derived_entry.install_resolved(
                derived_class.as_mut_ptr().cast(),
                method.as_mut_ptr(),
                0x9999,
                Some(&derived_original),
            ));
            assert_eq!(derived_original.load(Ordering::Acquire), 0x4444);
            assert!(derived_entry.is_vtable_only);
            assert_eq!(derived_class[41], 0x9999);
            assert_eq!(method, [0x9999, 0x9999]);

            // 3. Restore derived entry
            assert!(derived_entry.restore("fake", 0, "test"));
            assert_eq!(derived_class[41], 0x4444);
            assert_eq!(method, [0x9999, 0x9999]); // method still hooked by base!

            // 4. Restore base entry
            assert!(base_entry.restore("fake", 0, "test"));
            assert_eq!(method, [0x1111, 0x2222]);
            assert_eq!(base_class[39], 0x3333);
        }
    }

    #[test]
    fn reactivating_a_derived_hook_never_rewrites_the_restored_base_method() {
        let mut method = [0x1111usize, 0x2222];
        let mut class = [0usize; 60];
        class[39] = 0x3333;
        class[40] = method.as_mut_ptr() as usize;
        let mut base = MethodHookEntry::new();
        let mut derived = MethodHookEntry::new();
        unsafe {
            *((class.as_mut_ptr() as usize + 0x12A) as *mut u16) = 1;
            assert!(base.install_resolved(ptr::null_mut(), method.as_mut_ptr(), 0x9999, None));
            assert!(derived.install_resolved(
                class.as_mut_ptr().cast(),
                method.as_mut_ptr(),
                0x9999,
                None
            ));
            assert!(base.restore("base", 0, "test"));
            assert!(derived.install_resolved(
                class.as_mut_ptr().cast(),
                method.as_mut_ptr(),
                0x9999,
                None
            ));
            let after_reactivation = method;
            assert!(derived.restore("derived", 0, "test"));
            assert_eq!(after_reactivation, [0x1111, 0x2222]);
            assert_eq!(method, [0x1111, 0x2222]);
            assert_eq!(class[39], 0x3333);
        }
    }

    #[test]
    fn a_derived_hook_rejects_a_null_native_dispatch() {
        let mut method = [0x1111usize, 0x2222];
        let mut class = [0usize; 60];
        class[40] = method.as_mut_ptr() as usize;
        let mut base = MethodHookEntry::new();
        let mut derived = MethodHookEntry::new();
        unsafe {
            *((class.as_mut_ptr() as usize + 0x12A) as *mut u16) = 1;
            assert!(base.install_resolved(ptr::null_mut(), method.as_mut_ptr(), 0x9999, None));
            let installed = derived.install_resolved(
                class.as_mut_ptr().cast(),
                method.as_mut_ptr(),
                0x9999,
                None,
            );
            assert!(derived.restore("derived", 0, "test"));
            assert!(base.restore("base", 0, "test"));
            assert!(!installed);
            assert_eq!(class[39], 0);
        }
    }

    declare_heap_hook!(TestHook, i32, (a: i32, b: i32));

    #[test]
    fn test_inflight_guard_and_declare_heap_hook() {
        static HOOK: TestHook = TestHook::new();
        assert!(!HOOK.is_enabled());

        HOOK.set_enabled(true);
        assert!(HOOK.is_enabled());

        {
            let _guard = HOOK.enter();
            assert_eq!(HOOK.inflight.load(Ordering::Acquire), 1);
        }
        assert_eq!(HOOK.inflight.load(Ordering::Acquire), 0);

        HOOK.set_enabled(false);
        assert!(!HOOK.is_enabled());
    }
}
