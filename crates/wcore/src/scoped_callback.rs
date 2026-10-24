//! Per-method native forwarding, limited to the exact bound local instance.

use crate::win::ptr_ok;
use core::ffi::c_void;
use core::sync::atomic::{AtomicUsize, Ordering};

pub struct ScopedCallback {
    pub original: AtomicUsize,
    instance: AtomicUsize,
    class: AtomicUsize,
}

impl ScopedCallback {
    pub const fn new() -> Self {
        Self {
            original: AtomicUsize::new(0),
            instance: AtomicUsize::new(0),
            class: AtomicUsize::new(0),
        }
    }

    pub unsafe fn bind(&self, instance: *mut c_void) -> bool {
        self.instance.store(0, Ordering::Release);
        if !ptr_ok(instance as usize, core::mem::size_of::<usize>()) {
            return false;
        }
        let class = unsafe { *(instance as *const usize) };
        if class == 0 {
            return false;
        }
        self.class.store(class, Ordering::Relaxed);
        self.instance.store(instance as usize, Ordering::Release);
        true
    }

    pub fn clear(&self) {
        self.instance.store(0, Ordering::Release);
    }

    pub fn matches(&self, instance: *mut c_void, enabled: bool) -> bool {
        enabled
            && !instance.is_null()
            && self.instance.load(Ordering::Acquire) == instance as usize
            && ptr_ok(instance as usize, core::mem::size_of::<usize>())
            && unsafe { *(instance as *const usize) } == self.class.load(Ordering::Relaxed)
    }

    pub unsafe fn i32_getter(
        &self,
        instance: *mut c_void,
        method: *const c_void,
        enabled: bool,
        value: i32,
    ) -> i32 {
        if self.matches(instance, enabled) {
            return value;
        }
        let address = self.original.load(Ordering::Acquire);
        if address == 0 {
            return 0;
        }
        let native: unsafe extern "C" fn(*mut c_void, *const c_void) -> i32 =
            unsafe { core::mem::transmute(address) };
        unsafe { native(instance, method) }
    }

    pub unsafe fn bool_getter(
        &self,
        instance: *mut c_void,
        method: *const c_void,
        enabled: bool,
        value: bool,
    ) -> bool {
        if self.matches(instance, enabled) {
            return value;
        }
        let address = self.original.load(Ordering::Acquire);
        if address == 0 {
            return false;
        }
        let native: unsafe extern "C" fn(*mut c_void, *const c_void) -> bool =
            unsafe { core::mem::transmute(address) };
        unsafe { native(instance, method) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicI32;

    static CALLS: AtomicI32 = AtomicI32::new(0);
    unsafe extern "C" fn native(instance: *mut c_void, method: *const c_void) -> i32 {
        assert!(!instance.is_null());
        assert_eq!(method as usize, 123);
        CALLS.fetch_add(1, Ordering::Relaxed);
        47
    }

    #[test]
    fn foreign_disabled_changed_class_and_cleared_instances_forward_native() {
        let hook = ScopedCallback::new();
        hook.original
            .store(native as *const () as usize, Ordering::Release);
        let mut local = [0x1111usize, 0];
        let mut other = [0x1111usize, 0];
        let local_ptr = local.as_mut_ptr().cast();
        let other_ptr = other.as_mut_ptr().cast();
        let method = 123usize as *const c_void;
        unsafe {
            assert!(hook.bind(local_ptr));
            assert_eq!(hook.i32_getter(local_ptr, method, true, 100), 100);
            assert_eq!(hook.i32_getter(other_ptr, method, true, 100), 47);
            assert_eq!(hook.i32_getter(local_ptr, method, false, 100), 47);
            local[0] = 0x2222;
            assert_eq!(local[0], 0x2222);
            assert_eq!(hook.i32_getter(local_ptr, method, true, 100), 47);
            hook.clear();
            assert_eq!(hook.i32_getter(local_ptr, method, true, 100), 47);
            assert!(!hook.bind(core::ptr::null_mut()));
        }
        assert_eq!(CALLS.load(Ordering::Relaxed), 4);
    }
}
