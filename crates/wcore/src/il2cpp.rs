//! IL2CPP runtime reflection bindings, RAII GC handle guard, and safe field/method helpers.

use crate::peb::pe_find_export;
use crate::win::{fn_readable, log_fmt, page_exec_ok, page_write_ok, ptr_ok};
use core::ffi::c_void;
use core::mem::transmute;
use core::ptr;
use std::ffi::{CStr, CString};
use std::sync::OnceLock;

pub type FnDomainGet = unsafe extern "C" fn() -> *mut c_void;
pub type FnDomainGetAssemblies = unsafe extern "C" fn(*mut c_void, *mut usize) -> *mut *mut c_void;
pub type FnThreadAttach = unsafe extern "C" fn(*mut c_void) -> *mut c_void;
pub type FnAssemblyGetImage = unsafe extern "C" fn(*mut c_void) -> *mut c_void;
pub type FnImageGetName = unsafe extern "C" fn(*mut c_void) -> *const i8;
pub type FnClassFromName = unsafe extern "C" fn(*mut c_void, *const i8, *const i8) -> *mut c_void;
pub type FnClassGetFieldFromName = unsafe extern "C" fn(*mut c_void, *const i8) -> *mut c_void;
pub type FnClassGetFields = unsafe extern "C" fn(*mut c_void, *mut *mut c_void) -> *mut c_void;
pub type FnFieldGetName = unsafe extern "C" fn(*mut c_void) -> *const i8;
pub type FnFieldGetOffset = unsafe extern "C" fn(*mut c_void) -> i32;
pub type FnFieldGetType = unsafe extern "C" fn(*mut c_void) -> *const c_void;
pub type FnFieldStaticGetValue = unsafe extern "C" fn(*mut c_void, *mut c_void);
pub type FnFieldStaticSetValue = unsafe extern "C" fn(*mut c_void, *mut c_void);
pub type FnClassGetMethodFromName =
    unsafe extern "C" fn(*mut c_void, *const i8, i32) -> *mut c_void;
pub type FnClassGetMethods = unsafe extern "C" fn(*mut c_void, *mut *mut c_void) -> *const c_void;
pub type FnMethodGetName = unsafe extern "C" fn(*const c_void) -> *const i8;
pub type FnMethodGetParamCount = unsafe extern "C" fn(*const c_void) -> u32;
pub type FnMethodGetParam = unsafe extern "C" fn(*const c_void, u32) -> *const c_void;
pub type FnMethodGetReturnType = unsafe extern "C" fn(*const c_void) -> *const c_void;
pub type FnMethodGetFlags = unsafe extern "C" fn(*const c_void, *mut u32) -> u32;
pub type FnTypeGetName = unsafe extern "C" fn(*const c_void) -> *mut i8;
pub type FnIl2CppFree = unsafe extern "C" fn(*mut c_void);
pub type FnRuntimeInvoke = unsafe extern "C" fn(
    *const c_void,
    *mut c_void,
    *mut *mut c_void,
    *mut *mut c_void,
) -> *mut c_void;
pub type FnObjectUnbox = unsafe extern "C" fn(*mut c_void) -> *mut c_void;
pub type FnObjectGetClass = unsafe extern "C" fn(*mut c_void) -> *mut c_void;
pub type FnClassGetName = unsafe extern "C" fn(*mut c_void) -> *const i8;
pub type FnClassGetNamespace = unsafe extern "C" fn(*mut c_void) -> *const i8;
pub type FnClassGetParent = unsafe extern "C" fn(*mut c_void) -> *mut c_void;
pub type FnClassIsValuetype = unsafe extern "C" fn(*mut c_void) -> bool;
pub type FnGchandleNew = unsafe extern "C" fn(*mut c_void, bool) -> u32;
pub type FnGchandleFree = unsafe extern "C" fn(u32);
pub type FnStringNew = unsafe extern "C" fn(*const i8) -> *mut c_void;
pub type FnClassGetNestedTypes = unsafe extern "C" fn(*mut c_void, *mut *mut c_void) -> *mut c_void;

#[derive(Clone, Copy)]
pub struct Il2CppApi {
    pub domain_get: FnDomainGet,
    pub domain_get_assemblies: FnDomainGetAssemblies,
    pub thread_attach: FnThreadAttach,
    pub assembly_get_image: FnAssemblyGetImage,
    pub image_get_name: FnImageGetName,
    pub class_from_name: FnClassFromName,
    pub class_get_field_from_name: FnClassGetFieldFromName,
    pub class_get_fields: Option<FnClassGetFields>,
    pub field_get_name: Option<FnFieldGetName>,
    pub field_get_offset: FnFieldGetOffset,
    pub field_get_type: Option<FnFieldGetType>,
    pub field_static_get_value: FnFieldStaticGetValue,
    pub field_static_set_value: Option<FnFieldStaticSetValue>,
    pub class_get_method_from_name: FnClassGetMethodFromName,
    pub class_get_methods: Option<FnClassGetMethods>,
    pub method_get_name: Option<FnMethodGetName>,
    pub method_get_param_count: Option<FnMethodGetParamCount>,
    pub method_get_param: Option<FnMethodGetParam>,
    pub method_get_return_type: Option<FnMethodGetReturnType>,
    pub method_get_flags: Option<FnMethodGetFlags>,
    pub type_get_name: Option<FnTypeGetName>,
    pub free: Option<FnIl2CppFree>,
    pub runtime_invoke: FnRuntimeInvoke,
    pub object_unbox: FnObjectUnbox,
    pub object_get_class: Option<FnObjectGetClass>,
    pub class_get_name: Option<FnClassGetName>,
    pub class_get_namespace: Option<FnClassGetNamespace>,
    pub class_get_parent: Option<FnClassGetParent>,
    pub class_is_valuetype: Option<FnClassIsValuetype>,
    pub gchandle_new: Option<FnGchandleNew>,
    pub gchandle_free: Option<FnGchandleFree>,
    pub gc_wbarrier_set_field:
        Option<unsafe extern "C" fn(*mut c_void, *mut *mut c_void, *mut c_void)>,
    pub string_new: Option<FnStringNew>,
    pub class_get_nested_types: Option<FnClassGetNestedTypes>,
}

unsafe impl Send for Il2CppApi {}
unsafe impl Sync for Il2CppApi {}

static API: OnceLock<Il2CppApi> = OnceLock::new();

pub fn api() -> Option<&'static Il2CppApi> {
    API.get()
}

/// Resolve all IL2CPP exports from `GameAssembly.dll` base address via manual PE export walk.
pub unsafe fn resolve_api(ga_base: usize) -> Option<&'static Il2CppApi> {
    if let Some(existing) = API.get() {
        return Some(existing);
    }

    macro_rules! req {
        ($name:literal, $ty:ty) => {{
            let addr = unsafe { pe_find_export(ga_base, $name) };
            if !fn_readable(addr) {
                log_fmt(format_args!(
                    "[api] missing or unreadable export: {}",
                    core::str::from_utf8($name).unwrap_or("?")
                ));
                return None;
            }
            unsafe { transmute::<usize, $ty>(addr) }
        }};
    }

    macro_rules! opt {
        ($name:literal, $ty:ty) => {{
            let addr = unsafe { pe_find_export(ga_base, $name) };
            if fn_readable(addr) {
                Some(unsafe { transmute::<usize, $ty>(addr) })
            } else {
                None
            }
        }};
    }

    let table = Il2CppApi {
        domain_get: req!(b"il2cpp_domain_get", FnDomainGet),
        domain_get_assemblies: req!(b"il2cpp_domain_get_assemblies", FnDomainGetAssemblies),
        thread_attach: req!(b"il2cpp_thread_attach", FnThreadAttach),
        assembly_get_image: req!(b"il2cpp_assembly_get_image", FnAssemblyGetImage),
        image_get_name: req!(b"il2cpp_image_get_name", FnImageGetName),
        class_from_name: req!(b"il2cpp_class_from_name", FnClassFromName),
        class_get_field_from_name: req!(
            b"il2cpp_class_get_field_from_name",
            FnClassGetFieldFromName
        ),
        class_get_fields: opt!(b"il2cpp_class_get_fields", FnClassGetFields),
        field_get_name: opt!(b"il2cpp_field_get_name", FnFieldGetName),
        field_get_offset: req!(b"il2cpp_field_get_offset", FnFieldGetOffset),
        field_get_type: opt!(b"il2cpp_field_get_type", FnFieldGetType),
        field_static_get_value: req!(b"il2cpp_field_static_get_value", FnFieldStaticGetValue),
        field_static_set_value: opt!(b"il2cpp_field_static_set_value", FnFieldStaticSetValue),
        class_get_method_from_name: req!(
            b"il2cpp_class_get_method_from_name",
            FnClassGetMethodFromName
        ),
        class_get_methods: opt!(b"il2cpp_class_get_methods", FnClassGetMethods),
        method_get_name: opt!(b"il2cpp_method_get_name", FnMethodGetName),
        method_get_param_count: opt!(b"il2cpp_method_get_param_count", FnMethodGetParamCount),
        method_get_param: opt!(b"il2cpp_method_get_param", FnMethodGetParam),
        method_get_return_type: opt!(b"il2cpp_method_get_return_type", FnMethodGetReturnType),
        method_get_flags: opt!(b"il2cpp_method_get_flags", FnMethodGetFlags),
        type_get_name: opt!(b"il2cpp_type_get_name", FnTypeGetName),
        free: opt!(b"il2cpp_free", FnIl2CppFree),
        runtime_invoke: req!(b"il2cpp_runtime_invoke", FnRuntimeInvoke),
        object_unbox: req!(b"il2cpp_object_unbox", FnObjectUnbox),
        object_get_class: opt!(b"il2cpp_object_get_class", FnObjectGetClass),
        class_get_name: opt!(b"il2cpp_class_get_name", FnClassGetName),
        class_get_namespace: opt!(b"il2cpp_class_get_namespace", FnClassGetNamespace),
        class_get_parent: opt!(b"il2cpp_class_get_parent", FnClassGetParent),
        class_is_valuetype: opt!(b"il2cpp_class_is_valuetype", FnClassIsValuetype),
        gchandle_new: opt!(b"il2cpp_gchandle_new", FnGchandleNew),
        gchandle_free: opt!(b"il2cpp_gchandle_free", FnGchandleFree),
        gc_wbarrier_set_field: opt!(
            b"il2cpp_gc_wbarrier_set_field",
            unsafe extern "C" fn(*mut c_void, *mut *mut c_void, *mut c_void)
        ),
        string_new: opt!(b"il2cpp_string_new", FnStringNew),
        class_get_nested_types: opt!(b"il2cpp_class_get_nested_types", FnClassGetNestedTypes)
            .or_else(|| opt!(b"mono_class_get_nested_types", FnClassGetNestedTypes)),
    };

    let _ = API.set(table);
    API.get()
}

/// RAII scope guard that pins managed objects via `il2cpp_gchandle_new(obj, false)`
/// and automatically releases all handles on drop.
pub struct GcScope {
    handles: Vec<u32>,
}

impl GcScope {
    pub fn new() -> Self {
        Self {
            handles: Vec::with_capacity(32),
        }
    }

    pub fn pin(&mut self, obj: *mut c_void) -> *mut c_void {
        if !unsafe { crate::chain::is_valid_il2cpp_obj(obj) } {
            return ptr::null_mut();
        }
        if let Some(api) = api() {
            if let Some(new_fn) = api.gchandle_new {
                if api.gchandle_free.is_some() {
                    let h = unsafe { new_fn(obj, false) };
                    if h != 0 {
                        self.handles.push(h);
                    }
                }
            }
        }
        obj
    }

    pub fn try_pin(&mut self, obj: *mut c_void) -> Option<*mut c_void> {
        if obj.is_null() {
            return Some(obj);
        }
        if !unsafe { crate::chain::is_valid_il2cpp_obj(obj) } {
            return None;
        }
        let api = api()?;
        let create = api.gchandle_new?;
        api.gchandle_free?;
        let handle = unsafe { create(obj, true) };
        if handle == 0 {
            return None;
        }
        self.handles.push(handle);
        Some(obj)
    }
}

impl Default for GcScope {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for GcScope {
    fn drop(&mut self) {
        if let Some(api) = api() {
            if let Some(free_fn) = api.gchandle_free {
                for &h in self.handles.iter().rev() {
                    if h != 0 {
                        unsafe { free_fn(h) };
                    }
                }
            }
        }
        self.handles.clear();
    }
}

/// Convert a C string pointer into a `&str` safely if readable.
pub unsafe fn cstr_to_str<'a>(p: *const i8) -> &'a str {
    if p.is_null() || !ptr_ok(p as usize, 1) {
        return "";
    }
    let mut len = 0usize;
    while len < 256 {
        let addr = (p as usize) + len;
        if !ptr_ok(addr, 1) {
            break;
        }
        let ch = unsafe { *(addr as *const u8) };
        if ch == 0 {
            break;
        }
        len += 1;
    }
    let slice = unsafe { core::slice::from_raw_parts(p as *const u8, len) };
    core::str::from_utf8(slice).unwrap_or("")
}

pub unsafe fn field_type_matches(field: *mut c_void, expected: &str) -> bool {
    let Some(api) = api() else { return false };
    let (Some(field_type), Some(type_name), Some(free)) =
        (api.field_get_type, api.type_get_name, api.free)
    else {
        return false;
    };
    if field.is_null() {
        return false;
    }
    let ty = unsafe { field_type(field) };
    if ty.is_null() {
        return false;
    }
    let name = unsafe { type_name(ty) };
    if name.is_null() {
        return false;
    }
    let matches = unsafe { CStr::from_ptr(name) }.to_bytes() == expected.as_bytes();
    unsafe { free(name.cast()) };
    matches
}

/// Find a loaded IL2CPP image by substring match (e.g. `"Assembly-CSharp"`).
pub unsafe fn find_image(domain: *mut c_void, needle: &str) -> *mut c_void {
    let Some(api) = api() else {
        return ptr::null_mut();
    };
    let mut count = 0usize;
    let list = unsafe { (api.domain_get_assemblies)(domain, &mut count) };
    if list.is_null() || count == 0 || count > 4096 || !ptr_ok(list as usize, count * 8) {
        return ptr::null_mut();
    }
    for i in 0..count {
        let ass = unsafe { *list.add(i) };
        if ass.is_null() {
            continue;
        }
        let img = unsafe { (api.assembly_get_image)(ass) };
        if img.is_null() {
            continue;
        }
        let name = unsafe { cstr_to_str((api.image_get_name)(img)) };
        if name.contains(needle) {
            return img;
        }
    }
    ptr::null_mut()
}

/// Look up an `Il2CppClass*` by namespace and name across the primary image or all assemblies.
pub unsafe fn find_class(
    primary_img: *mut c_void,
    domain: *mut c_void,
    ns: &str,
    name: &str,
) -> *mut c_void {
    let Some(api) = api() else {
        return ptr::null_mut();
    };
    let Ok(c_ns) = CString::new(ns) else {
        return ptr::null_mut();
    };
    let Ok(c_name) = CString::new(name) else {
        return ptr::null_mut();
    };

    if !primary_img.is_null() {
        let k = unsafe { (api.class_from_name)(primary_img, c_ns.as_ptr(), c_name.as_ptr()) };
        if !k.is_null() {
            return k;
        }
    }

    if !domain.is_null() {
        let mut count = 0usize;
        let list = unsafe { (api.domain_get_assemblies)(domain, &mut count) };
        if !list.is_null() && count > 0 && count < 4096 && ptr_ok(list as usize, count * 8) {
            for i in 0..count {
                let ass = unsafe { *list.add(i) };
                if ass.is_null() {
                    continue;
                }
                let img = unsafe { (api.assembly_get_image)(ass) };
                if img.is_null() {
                    continue;
                }
                let k = unsafe { (api.class_from_name)(img, c_ns.as_ptr(), c_name.as_ptr()) };
                if !k.is_null() {
                    return k;
                }
            }
        }
    }
    ptr::null_mut()
}

/// Find a nested class within `parent_klass` by name (e.g. `DamageData` within `SkillActionBase`).
pub unsafe fn find_nested_class(parent_klass: *mut c_void, name: &str) -> *mut c_void {
    if parent_klass.is_null() {
        return ptr::null_mut();
    }
    let Some(api) = api() else {
        return ptr::null_mut();
    };
    if let (Some(nested_fn), Some(cn_fn)) = (api.class_get_nested_types, api.class_get_name) {
        let mut iter: *mut c_void = ptr::null_mut();
        loop {
            let child = unsafe { nested_fn(parent_klass, &mut iter) };
            if child.is_null() {
                break;
            }
            let cname = unsafe { cstr_to_str(cn_fn(child)) };
            if cname == name {
                return child;
            }
        }
    }
    ptr::null_mut()
}

/// Walk class hierarchy (`class_get_parent`) to find a field by name.
pub unsafe fn get_field(mut klass: *mut c_void, name: &str) -> *mut c_void {
    let Some(api) = api() else {
        return ptr::null_mut();
    };
    let Ok(c_name) = CString::new(name) else {
        return ptr::null_mut();
    };
    let mut depth = 0;
    while !klass.is_null() && depth < 16 {
        let f = unsafe { (api.class_get_field_from_name)(klass, c_name.as_ptr()) };
        if !f.is_null() {
            return f;
        }
        klass = match api.class_get_parent {
            Some(parent_fn) => unsafe { parent_fn(klass) },
            None => ptr::null_mut(),
        };
        depth += 1;
    }
    ptr::null_mut()
}

/// Managed type names must match IL2CPP reflection exactly, including namespaces and by-ref suffixes.
#[derive(Clone, Copy, Debug)]
pub struct MethodSignature {
    pub parameters: &'static [&'static str],
    pub return_type: &'static str,
}

#[derive(Clone, Copy)]
struct MethodReflection {
    methods: FnClassGetMethods,
    name: FnMethodGetName,
    parameter_count: FnMethodGetParamCount,
    parameter: FnMethodGetParam,
    return_type: FnMethodGetReturnType,
    type_name: FnTypeGetName,
    free: FnIl2CppFree,
    parent: Option<FnClassGetParent>,
}

impl MethodReflection {
    fn from_api(api: &Il2CppApi) -> Option<Self> {
        Some(Self {
            methods: api.class_get_methods?,
            name: api.method_get_name?,
            parameter_count: api.method_get_param_count?,
            parameter: api.method_get_param?,
            return_type: api.method_get_return_type?,
            type_name: api.type_get_name?,
            free: api.free?,
            parent: api.class_get_parent,
        })
    }

    unsafe fn type_matches(&self, ty: *const c_void, expected: &str) -> bool {
        if ty.is_null() {
            return false;
        }
        let name = unsafe { (self.type_name)(ty) };
        if name.is_null() {
            return false;
        }
        let matches = unsafe { CStr::from_ptr(name) }.to_bytes() == expected.as_bytes();
        unsafe { (self.free)(name.cast()) };
        matches
    }

    unsafe fn matches(
        &self,
        method: *const c_void,
        name: &str,
        signature: &MethodSignature,
    ) -> bool {
        let method_name = unsafe { (self.name)(method) };
        if method_name.is_null()
            || unsafe { CStr::from_ptr(method_name) }.to_bytes() != name.as_bytes()
            || unsafe { (self.parameter_count)(method) } as usize != signature.parameters.len()
            || !unsafe { self.type_matches((self.return_type)(method), signature.return_type) }
        {
            return false;
        }
        signature
            .parameters
            .iter()
            .enumerate()
            .all(|(index, expected)| unsafe {
                self.type_matches((self.parameter)(method, index as u32), expected)
            })
    }
}

unsafe fn find_method_exact(
    mut klass: *mut c_void,
    name: &str,
    signature: &MethodSignature,
    reflection: &MethodReflection,
) -> *const c_void {
    for _ in 0..16 {
        if klass.is_null() {
            return ptr::null();
        }
        let mut iter = ptr::null_mut();
        let mut found: *const c_void = ptr::null();
        let mut exhausted = false;
        for _ in 0..65_536 {
            let method = unsafe { (reflection.methods)(klass, &mut iter) };
            if method.is_null() {
                exhausted = true;
                break;
            }
            if unsafe { reflection.matches(method, name, signature) } {
                if !found.is_null() {
                    log_fmt(format_args!("[api] ambiguous exact method {}", name));
                    return ptr::null();
                }
                found = method;
            }
        }
        if !exhausted {
            return ptr::null();
        }
        if !found.is_null() {
            return found;
        }
        klass = match reflection.parent {
            Some(parent) => unsafe { parent(klass) },
            None => ptr::null_mut(),
        };
    }
    ptr::null()
}

/// Resolve a unique full signature in the nearest declaring class. Missing reflection
/// exports or ambiguous signatures fail closed; name/arity lookup is never a fallback.
pub unsafe fn get_method_exact(
    klass: *mut c_void,
    name: &str,
    signature: &MethodSignature,
) -> *const c_void {
    let Some(reflection) = api().and_then(MethodReflection::from_api) else {
        log_fmt(format_args!(
            "[api] exact method reflection unavailable for {}",
            name
        ));
        return ptr::null();
    };
    unsafe { find_method_exact(klass, name, signature, &reflection) }
}

/// Walk class hierarchy to find a method by name and parameter count.
pub unsafe fn get_method(mut klass: *mut c_void, name: &str, argc: i32) -> *const c_void {
    let Some(api) = api() else {
        return ptr::null();
    };
    let Ok(c_name) = CString::new(name) else {
        return ptr::null();
    };
    let mut depth = 0;
    while !klass.is_null() && depth < 16 {
        let m = unsafe { (api.class_get_method_from_name)(klass, c_name.as_ptr(), argc) };
        if !m.is_null() {
            return m;
        }
        klass = match api.class_get_parent {
            Some(parent_fn) => unsafe { parent_fn(klass) },
            None => ptr::null_mut(),
        };
        depth += 1;
    }
    ptr::null()
}

/// Read a reference-type instance field from `instance` via `fieldName` and pin it in `gc`.
pub unsafe fn read_inst_ptr(
    instance: *mut c_void,
    klass: *mut c_void,
    field_name: &str,
    gc: &mut GcScope,
) -> *mut c_void {
    if instance.is_null() || klass.is_null() {
        return ptr::null_mut();
    }
    let Some(api) = api() else {
        return ptr::null_mut();
    };
    let f = unsafe { get_field(klass, field_name) };
    if f.is_null() {
        return ptr::null_mut();
    }
    let off = unsafe { (api.field_get_offset)(f) };
    if !(0..=0x4000).contains(&off) {
        return ptr::null_mut();
    }
    let addr = (instance as usize) + (off as usize);
    if !ptr_ok(addr, 8) {
        return ptr::null_mut();
    }
    let val = unsafe { *(addr as *const *mut c_void) };
    if !val.is_null() && ptr_ok(val as usize, 16) {
        gc.pin(val)
    } else {
        ptr::null_mut()
    }
}

/// Read a static reference-type field from `klass` and pin it in `gc`.
pub unsafe fn read_static_ptr(
    klass: *mut c_void,
    field_name: &str,
    gc: &mut GcScope,
) -> *mut c_void {
    if klass.is_null() {
        return ptr::null_mut();
    }
    let Some(api) = api() else {
        return ptr::null_mut();
    };
    let f = unsafe { get_field(klass, field_name) };
    if f.is_null() {
        return ptr::null_mut();
    }
    let mut val: *mut c_void = ptr::null_mut();
    unsafe {
        (api.field_static_get_value)(f, (&mut val as *mut *mut c_void).cast());
    }
    if !val.is_null() && ptr_ok(val as usize, 16) {
        gc.pin(val)
    } else {
        ptr::null_mut()
    }
}

/// Invoke a 0-arg method returning an object reference and pin the result in `gc`.
pub unsafe fn call_ptr0(
    klass: *mut c_void,
    instance: *mut c_void,
    method_name: &str,
    gc: &mut GcScope,
) -> *mut c_void {
    if klass.is_null() {
        return ptr::null_mut();
    }
    let Some(api) = api() else {
        return ptr::null_mut();
    };
    let m = unsafe { get_method(klass, method_name, 0) };
    if m.is_null() {
        return ptr::null_mut();
    }
    let mut exc: *mut c_void = ptr::null_mut();
    let res = unsafe { (api.runtime_invoke)(m, instance, ptr::null_mut(), &mut exc) };
    if !exc.is_null() || res.is_null() || !ptr_ok(res as usize, 16) {
        return ptr::null_mut();
    }
    gc.pin(res)
}

/// Invoke a 0-arg method returning `i32` (unboxed).
pub unsafe fn call_i32_0(
    klass: *mut c_void,
    instance: *mut c_void,
    method_name: &str,
) -> Option<i32> {
    if klass.is_null() {
        return None;
    }
    let api = api()?;
    let m = unsafe { get_method(klass, method_name, 0) };
    if m.is_null() {
        return None;
    }
    let mut exc: *mut c_void = ptr::null_mut();
    let boxed = unsafe { (api.runtime_invoke)(m, instance, ptr::null_mut(), &mut exc) };
    if !exc.is_null() || boxed.is_null() {
        return None;
    }
    let raw = unsafe { (api.object_unbox)(boxed) };
    if raw.is_null() || !ptr_ok(raw as usize, 4) {
        return None;
    }
    Some(unsafe { *(raw as *const i32) })
}

/// Invoke a 1-arg (`i32` or `bool`) void/ignored method on `instance`.
pub unsafe fn call_void_i32(
    klass: *mut c_void,
    instance: *mut c_void,
    method_name: &str,
    val: i32,
) -> bool {
    if klass.is_null() || instance.is_null() {
        return false;
    }
    let Some(api) = api() else {
        return false;
    };
    let m = unsafe { get_method(klass, method_name, 1) };
    if m.is_null() {
        return false;
    }
    let mut arg = val;
    let mut args: [*mut c_void; 1] = [(&mut arg as *mut i32).cast()];
    let mut exc: *mut c_void = ptr::null_mut();
    unsafe {
        (api.runtime_invoke)(m, instance, args.as_mut_ptr(), &mut exc);
    }
    exc.is_null()
}

/// Invoke a 1-arg (`bool`) void method on `instance`.
pub unsafe fn call_void_bool(
    klass: *mut c_void,
    instance: *mut c_void,
    method_name: &str,
    val: bool,
) -> bool {
    if klass.is_null() || instance.is_null() {
        return false;
    }
    let Some(api) = api() else {
        return false;
    };
    let m = unsafe { get_method(klass, method_name, 1) };
    if m.is_null() {
        return false;
    }
    let mut arg: u8 = if val { 1 } else { 0 };
    let mut args: [*mut c_void; 1] = [(&mut arg as *mut u8).cast()];
    let mut exc: *mut c_void = ptr::null_mut();
    unsafe {
        (api.runtime_invoke)(m, instance, args.as_mut_ptr(), &mut exc);
    }
    exc.is_null()
}

/// Invoke a 1-arg (`f32`) void method on `instance`.
pub unsafe fn call_void_f32(
    klass: *mut c_void,
    instance: *mut c_void,
    method_name: &str,
    val: f32,
) -> bool {
    if klass.is_null() || instance.is_null() {
        return false;
    }
    let Some(api) = api() else {
        return false;
    };
    let m = unsafe { get_method(klass, method_name, 1) };
    if m.is_null() {
        return false;
    }
    let mut arg = val;
    let mut args: [*mut c_void; 1] = [(&mut arg as *mut f32).cast()];
    let mut exc: *mut c_void = ptr::null_mut();
    unsafe {
        (api.runtime_invoke)(m, instance, args.as_mut_ptr(), &mut exc);
    }
    exc.is_null()
}

/// Invoke a 2-arg (`i32`, `i32`) void method on `instance`.
pub unsafe fn call_void_2i32(
    klass: *mut c_void,
    instance: *mut c_void,
    method_name: &str,
    val1: i32,
    val2: i32,
) -> bool {
    if klass.is_null() || instance.is_null() {
        return false;
    }
    let Some(api) = api() else {
        return false;
    };
    let m = unsafe { get_method(klass, method_name, 2) };
    if m.is_null() {
        return false;
    }
    let mut arg1 = val1;
    let mut arg2 = val2;
    let mut args: [*mut c_void; 2] = [
        (&mut arg1 as *mut i32).cast(),
        (&mut arg2 as *mut i32).cast(),
    ];
    let mut exc: *mut c_void = ptr::null_mut();
    unsafe {
        (api.runtime_invoke)(m, instance, args.as_mut_ptr(), &mut exc);
    }
    exc.is_null()
}

/// Invoke a 2-arg (`i32`, `bool`) void method on `instance`.
pub unsafe fn call_void_i32_bool(
    klass: *mut c_void,
    instance: *mut c_void,
    method_name: &str,
    val1: i32,
    val2: bool,
) -> bool {
    if klass.is_null() || instance.is_null() {
        return false;
    }
    let Some(api) = api() else {
        return false;
    };
    let m = unsafe { get_method(klass, method_name, 2) };
    if m.is_null() {
        return false;
    }
    let mut arg1 = val1;
    let mut arg2: u8 = if val2 { 1 } else { 0 };
    let mut args: [*mut c_void; 2] = [
        (&mut arg1 as *mut i32).cast(),
        (&mut arg2 as *mut u8).cast(),
    ];
    let mut exc: *mut c_void = ptr::null_mut();
    unsafe {
        (api.runtime_invoke)(m, instance, args.as_mut_ptr(), &mut exc);
    }
    exc.is_null()
}

/// Invoke a 0-arg void method on `instance`.
pub unsafe fn call_void0(klass: *mut c_void, instance: *mut c_void, method_name: &str) -> bool {
    if klass.is_null() || instance.is_null() {
        return false;
    }
    let Some(api) = api() else {
        return false;
    };
    let m = unsafe { get_method(klass, method_name, 0) };
    if m.is_null() {
        return false;
    }
    let mut exc: *mut c_void = ptr::null_mut();
    unsafe {
        (api.runtime_invoke)(m, instance, ptr::null_mut(), &mut exc);
    }
    exc.is_null()
}

/// Resolve the underlying native code pointer from an `Il2CppMethodInfo*` (`*(usize*)method`).
pub unsafe fn resolve_method_code_ptr(method: *const c_void) -> Option<usize> {
    if method.is_null() || !ptr_ok(method as usize, 8) {
        return None;
    }
    let code = unsafe { *(method as *const usize) };
    if code == 0 || !page_exec_ok(code, 16) {
        return None;
    }
    Some(code)
}

/// Safely add `delta` to the 6 primary stats (`str`, `int`, `vit`, `agi`, `dex`, `crt`) on a
/// `PlayerStatus` instance using pre-resolved field offsets (`[i32; 6]`).
///
/// Kept for deterministic unit test parity with `test_wcore`:
/// - `0`: all 6 fields updated and readback verified
/// - `-1`: null status pointer or non-positive delta
/// - `-2`: invalid field offset (`<= 0` or `> 0x2000`)
/// - `-3`: target field address not writable (`page_write_ok` failed)
/// - `-4`: current field value out of sane bounds (`< 0` or `> 1_000_000`) or integer overflow
/// - `-5`: readback verification mismatch
pub fn add_primary_stats(status: *mut c_void, offsets: &[i32; 6], delta: i32) -> i32 {
    if status.is_null() || delta <= 0 {
        return -1;
    }
    let base = status as usize;
    let mut addrs = [0usize; 6];
    let mut targets = [0i32; 6];

    for i in 0..6 {
        let off = offsets[i];
        if off <= 0 || off > 0x2000 {
            return -2;
        }
        let addr = base.wrapping_add(off as usize);
        if !page_write_ok(addr, 4) {
            return -3;
        }
        let cur = unsafe { *(addr as *const i32) };
        if !(0..=1_000_000).contains(&cur) {
            return -4;
        }
        let Some(next) = cur.checked_add(delta) else {
            return -4;
        };
        if next > 1_000_000 {
            return -4;
        }
        addrs[i] = addr;
        targets[i] = next;
    }

    for i in 0..6 {
        unsafe {
            *(addrs[i] as *mut i32) = targets[i];
        }
    }

    for i in 0..6 {
        let rb = unsafe { *(addrs[i] as *const i32) };
        if rb != targets[i] {
            return -5;
        }
    }

    0
}

#[cfg(test)]
mod signature_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Method {
        name: CString,
        parameters: Vec<CString>,
        return_type: CString,
    }

    impl Method {
        fn new(parameters: &[&str], return_type: &str) -> Self {
            Self {
                name: CString::new("calcBaseDamage").unwrap(),
                parameters: parameters
                    .iter()
                    .map(|name| CString::new(*name).unwrap())
                    .collect(),
                return_type: CString::new(return_type).unwrap(),
            }
        }
    }

    struct Class {
        methods: Vec<Method>,
        parent: *mut c_void,
    }

    static ALLOCATED_NAMES: AtomicUsize = AtomicUsize::new(0);
    static FREED_NAMES: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn methods(class: *mut c_void, iter: *mut *mut c_void) -> *const c_void {
        let class = unsafe { &*class.cast::<Class>() };
        let index = unsafe { *iter } as usize;
        unsafe { *iter = (index + 1) as *mut c_void };
        class
            .methods
            .get(index)
            .map_or(ptr::null(), |method| (method as *const Method).cast())
    }
    unsafe extern "C" fn method_name(method: *const c_void) -> *const i8 {
        unsafe { &*method.cast::<Method>() }.name.as_ptr()
    }
    unsafe extern "C" fn parameter_count(method: *const c_void) -> u32 {
        unsafe { &*method.cast::<Method>() }.parameters.len() as u32
    }
    unsafe extern "C" fn parameter(method: *const c_void, index: u32) -> *const c_void {
        unsafe { &*method.cast::<Method>() }
            .parameters
            .get(index as usize)
            .map_or(ptr::null(), |name| (name as *const CString).cast())
    }
    unsafe extern "C" fn return_type(method: *const c_void) -> *const c_void {
        (&unsafe { &*method.cast::<Method>() }.return_type as *const CString).cast()
    }
    unsafe extern "C" fn type_name(ty: *const c_void) -> *mut i8 {
        ALLOCATED_NAMES.fetch_add(1, Ordering::Relaxed);
        unsafe { &*ty.cast::<CString>() }.clone().into_raw()
    }
    unsafe extern "C" fn free(name: *mut c_void) {
        FREED_NAMES.fetch_add(1, Ordering::Relaxed);
        drop(unsafe { CString::from_raw(name.cast()) });
    }
    unsafe extern "C" fn parent(class: *mut c_void) -> *mut c_void {
        unsafe { &*class.cast::<Class>() }.parent
    }

    #[test]
    fn exact_signature_rejects_same_arity_wrong_types_and_ambiguity() {
        let reflection = MethodReflection {
            methods,
            name: method_name,
            parameter_count,
            parameter,
            return_type,
            type_name,
            free,
            parent: Some(parent),
        };
        let signature = MethodSignature {
            parameters: &[
                "PlayerActionManagerBase",
                "MobActionManagerBase",
                "SkillAttackType",
                "SkillEqLimitFlag",
                "System.Boolean",
            ],
            return_type: "System.Int32",
        };
        let mut class = Class {
            methods: vec![
                Method::new(
                    &[
                        "System.Int32",
                        "System.Int32",
                        "System.Int32",
                        "System.Int32",
                        "System.Single",
                    ],
                    "System.Int32",
                ),
                Method::new(signature.parameters, "System.Single"),
                Method::new(signature.parameters, signature.return_type),
            ],
            parent: ptr::null_mut(),
        };
        let class_ptr = (&mut class as *mut Class).cast();
        let expected = (&class.methods[2] as *const Method).cast();
        assert_eq!(
            unsafe { find_method_exact(class_ptr, "calcBaseDamage", &signature, &reflection) },
            expected
        );
        assert!(
            unsafe { find_method_exact(class_ptr, "CalcBaseDamage", &signature, &reflection) }
                .is_null()
        );
        let mut child = Class {
            methods: Vec::new(),
            parent: class_ptr,
        };
        assert_eq!(
            unsafe {
                find_method_exact(
                    (&mut child as *mut Class).cast(),
                    "calcBaseDamage",
                    &signature,
                    &reflection,
                )
            },
            expected
        );
        class
            .methods
            .push(Method::new(signature.parameters, signature.return_type));
        assert!(
            unsafe { find_method_exact(class_ptr, "calcBaseDamage", &signature, &reflection) }
                .is_null()
        );
        class.methods.clear();
        assert!(
            unsafe { find_method_exact(class_ptr, "calcBaseDamage", &signature, &reflection) }
                .is_null()
        );
        assert_eq!(
            ALLOCATED_NAMES.load(Ordering::Relaxed),
            FREED_NAMES.load(Ordering::Relaxed)
        );
        assert!(ALLOCATED_NAMES.load(Ordering::Relaxed) > 0);
    }
}
