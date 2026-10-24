//! Manual PEB module walk, PE export resolver, and in-memory PE header camouflage for x86_64 Windows.
//!
//! Avoids calling `GetModuleHandleW("GameAssembly.dll")` or `GetProcAddress` on
//! protected modules so AppSign IAT/API hooks cannot intercept module resolution.

use crate::win::{page_write_ok, ptr_ok, VirtualProtect, DWORD, LPVOID, PAGE_READWRITE};
use core::arch::asm;
use core::ptr;

/// Read the current thread's Process Environment Block (PEB) pointer via `gs:[0x60]`.
#[inline]
pub unsafe fn read_peb() -> usize {
    let peb: usize;
    #[cfg(target_arch = "x86_64")]
    unsafe {
        asm!(
            "mov {}, gs:[0x60]",
            out(reg) peb,
            options(nostack, nomem, preserves_flags)
        );
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        peb = 0;
    }
    peb
}

/// Compare two UTF-16 slices case-insensitively (ASCII folding).
#[inline]
pub fn utf16_eq_ignore_ascii_case(a: &[u16], b: &str) -> bool {
    let mut b_chars = b.chars();
    for &wc in a {
        let Some(bc) = b_chars.next() else {
            return false;
        };
        let ac = char::from_u32(wc as u32).unwrap_or('\u{FFFD}');
        if !ac.eq_ignore_ascii_case(&bc) {
            return false;
        }
    }
    b_chars.next().is_none()
}

/// Walk `PEB -> Ldr -> InMemoryOrderModuleList` to locate a loaded module by base name.
///
/// Layout on x64 Windows:
/// - `PEB + 0x18`: `PPEB_LDR_DATA Ldr`
/// - `Ldr + 0x20`: `LIST_ENTRY InMemoryOrderModuleList`
/// - Entry (`LDR_DATA_TABLE_ENTRY.InMemoryOrderLinks` at `+0x10`):
///   - `entry + 0x20`: `DllBase` (`LDR_DATA_TABLE_ENTRY + 0x30`)
///   - `entry + 0x48`: `UNICODE_STRING BaseDllName` (`Length` u16 at `+0x48`, `Buffer` at `+0x50`)
pub unsafe fn peb_find_module(target_name: &str) -> usize {
    let peb = unsafe { read_peb() };
    if !ptr_ok(peb, 0x30) {
        return 0;
    }
    let ldr = unsafe { *((peb + 0x18) as *const usize) };
    if !ptr_ok(ldr, 0x40) {
        return 0;
    }
    let head = ldr + 0x20;
    let mut curr = unsafe { *(head as *const usize) };
    let mut guard = 0usize;

    while curr != 0 && curr != head && guard < 512 {
        guard += 1;
        if !ptr_ok(curr, 0x60) {
            break;
        }
        let dll_base = unsafe { *((curr + 0x20) as *const usize) };
        let byte_len = unsafe { *((curr + 0x48) as *const u16) } as usize;
        let char_len = byte_len / 2;
        let buf_ptr = unsafe { *((curr + 0x50) as *const *const u16) };

        if dll_base != 0 && (1..128).contains(&char_len) && ptr_ok(buf_ptr as usize, byte_len) {
            let slice = unsafe { core::slice::from_raw_parts(buf_ptr, char_len) };
            if utf16_eq_ignore_ascii_case(slice, target_name) {
                return dll_base;
            }
        }
        curr = unsafe { *(curr as *const usize) };
    }
    0
}

/// Resolve an exported function address from an in-memory mapped PE64 image (`base`)
/// by walking its `IMAGE_EXPORT_DIRECTORY` without calling `GetProcAddress`.
pub unsafe fn pe_find_export(base: usize, target: &[u8]) -> usize {
    if !ptr_ok(base, 0x200) {
        return 0;
    }
    let mz = unsafe { *(base as *const u16) };
    if mz != 0x5A4D {
        return 0;
    }
    let e_lfanew = unsafe { *((base + 0x3C) as *const i32) };
    if !(0x40..0x1000).contains(&e_lfanew) {
        return 0;
    }
    let nt = base + (e_lfanew as usize);
    if !ptr_ok(nt, 0x100) || unsafe { *(nt as *const u32) } != 0x0000_4550 {
        return 0;
    }
    // OptionalHeader starts at nt + 0x18; PE32+ magic is 0x20B.
    // DataDirectory[0] (Export Table) in PE32+ is at OptionalHeader + 0x70 = nt + 0x88.
    let exp_rva = unsafe { *((nt + 0x88) as *const u32) } as usize;
    let exp_size = unsafe { *((nt + 0x8C) as *const u32) } as usize;
    if exp_rva == 0 || exp_size == 0 {
        return 0;
    }
    let exp = base + exp_rva;
    if !ptr_ok(exp, 0x28) {
        return 0;
    }

    let num_funcs = unsafe { *((exp + 0x14) as *const u32) } as usize;
    let num_names = unsafe { *((exp + 0x18) as *const u32) } as usize;
    let funcs_rva = unsafe { *((exp + 0x1C) as *const u32) } as usize;
    let names_rva = unsafe { *((exp + 0x20) as *const u32) } as usize;
    let ords_rva = unsafe { *((exp + 0x24) as *const u32) } as usize;

    if num_names == 0 || num_names > 200_000 || funcs_rva == 0 || names_rva == 0 || ords_rva == 0 {
        return 0;
    }

    let funcs = (base + funcs_rva) as *const u32;
    let names = (base + names_rva) as *const u32;
    let ords = (base + ords_rva) as *const u16;

    if !ptr_ok(names as usize, num_names * 4)
        || !ptr_ok(ords as usize, num_names * 2)
        || !ptr_ok(funcs as usize, num_funcs * 4)
    {
        return 0;
    }

    for i in 0..num_names {
        let name_rva = unsafe { *names.add(i) } as usize;
        let name_ptr = (base + name_rva) as *const u8;
        if !ptr_ok(name_ptr as usize, target.len() + 1) {
            continue;
        }
        let slice = unsafe { core::slice::from_raw_parts(name_ptr, target.len() + 1) };
        if &slice[..target.len()] == target && slice[target.len()] == 0 {
            let ord = unsafe { *ords.add(i) } as usize;
            if ord >= num_funcs {
                return 0;
            }
            let fn_rva = unsafe { *funcs.add(ord) } as usize;
            return if fn_rva != 0 { base + fn_rva } else { 0 };
        }
    }
    0
}

/// Unlink module `base` from the Process Environment Block's loader lists:
/// - InLoadOrderModuleList
/// - InMemoryOrderModuleList
/// - InInitializationOrderModuleList
///
/// This removes the module from `EnumProcessModules`, `CreateToolhelp32Snapshot`,
/// and manual PEB list walks without unmapping the memory or breaking internal execution.
pub unsafe fn peb_unlink_module(base: usize) -> bool {
    if base == 0 {
        return false;
    }
    let peb = unsafe { read_peb() };
    if !ptr_ok(peb, 0x30) {
        return false;
    }
    let ldr = unsafe { *((peb + 0x18) as *const usize) };
    if !ptr_ok(ldr, 0x40) {
        return false;
    }

    // InLoadOrderModuleList is at Ldr + 0x10
    let head = ldr + 0x10;
    let mut curr = unsafe { *(head as *const usize) };
    let mut guard = 0usize;
    let mut found = false;

    while curr != 0 && curr != head && guard < 512 {
        guard += 1;
        if !ptr_ok(curr, 0x70) {
            break;
        }
        // In LDR_DATA_TABLE_ENTRY (x64):
        // +0x00: InLoadOrderLinks (LIST_ENTRY)
        // +0x10: InMemoryOrderLinks (LIST_ENTRY)
        // +0x20: InInitializationOrderLinks (LIST_ENTRY)
        // +0x30: DllBase (usize)
        let dll_base = unsafe { *((curr + 0x30) as *const usize) };
        if dll_base == base {
            // Unlink InLoadOrderLinks
            unlink_list_entry(curr);
            // Unlink InMemoryOrderLinks
            unlink_list_entry(curr + 0x10);
            // Unlink InInitializationOrderLinks
            unlink_list_entry(curr + 0x20);

            // Clear BaseDllName and FullDllName buffers if writable
            let base_buf = unsafe { *((curr + 0x60) as *const usize) };
            let base_len = unsafe { *((curr + 0x58) as *const u16) } as usize;
            if base_buf != 0 && base_len > 0 && page_write_ok(base_buf, base_len) {
                ptr::write_bytes(base_buf as *mut u8, 0, base_len);
            }
            let full_buf = unsafe { *((curr + 0x50) as *const usize) };
            let full_len = unsafe { *((curr + 0x48) as *const u16) } as usize;
            if full_buf != 0 && full_len > 0 && page_write_ok(full_buf, full_len) {
                ptr::write_bytes(full_buf as *mut u8, 0, full_len);
            }

            found = true;
            break;
        }
        curr = unsafe { *(curr as *const usize) };
    }
    found
}

#[inline]
unsafe fn unlink_list_entry(entry_ptr: usize) {
    if !ptr_ok(entry_ptr, 16) {
        return;
    }
    let flink = unsafe { *(entry_ptr as *const usize) };
    let blink = unsafe { *((entry_ptr + 8) as *const usize) };
    if ptr_ok(flink, 16) && ptr_ok(blink, 16) {
        unsafe {
            *(blink as *mut usize) = flink;
            *((flink + 8) as *mut usize) = blink;
        }
    }
}

/// Safely clears the in-memory PE image markers (`MZ`, DOS stub, `PE\0\0`, and Export Directory DataDirectory entry)
/// at `base` without touching `SizeOfImage`. Retained from
/// earlier builds; not called during worker startup.
pub unsafe fn scrub_module_pe_headers(base: usize) -> bool {
    if !ptr_ok(base, 0x400) {
        return false;
    }
    let mz = unsafe { *(base as *const u16) };
    if mz != 0x5A4D {
        return false;
    }
    let e_lfanew = unsafe { *((base + 0x3C) as *const i32) };
    if !(0x40..0x300).contains(&e_lfanew) {
        return false;
    }
    let nt = base + (e_lfanew as usize);
    if !ptr_ok(nt, 0x98) || unsafe { *(nt as *const u32) } != 0x0000_4550 {
        return false;
    }

    let mut old_prot: DWORD = 0;
    let changed_prot =
        unsafe { VirtualProtect(base as LPVOID, 0x1000, PAGE_READWRITE, &mut old_prot) != 0 };

    if !page_write_ok(base, 0x400) {
        return false;
    }

    unsafe {
        // Zero DOS header + stub up to NT header
        ptr::write_bytes(base as *mut u8, 0, e_lfanew as usize);
        // Zero PE\0\0 signature
        ptr::write_bytes(nt as *mut u8, 0, 4);
        // Zero Export Table DataDirectory[0] (RVA + Size at nt + 0x88..nt + 0x90)
        ptr::write_bytes((nt + 0x88) as *mut u8, 0, 8);
    }

    if changed_prot {
        let mut dummy: DWORD = 0;
        let _ = unsafe { VirtualProtect(base as LPVOID, 0x1000, old_prot, &mut dummy) };
    }
    true
}

/// Find a byte pattern with a wildcard mask (`0xFF` = match exact, `0x00` = wildcard/skip)
/// within the executable (`IMAGE_SCN_MEM_EXECUTE`) sections of module `base`.
pub fn find_pattern(base: usize, pattern: &[u8], mask: &[u8]) -> Option<usize> {
    if pattern.len() != mask.len() || pattern.is_empty() || !ptr_ok(base, 0x1000) {
        return None;
    }
    let e_lfanew = unsafe { *((base + 0x3C) as *const i32) } as usize;
    if e_lfanew > 0x1000 || !ptr_ok(base + e_lfanew, 0x108) {
        return None;
    }
    let nt = base + e_lfanew;
    let num_sections = unsafe { *((nt + 6) as *const u16) } as usize;
    let opt_hdr_size = unsafe { *((nt + 20) as *const u16) } as usize;
    let section_headers = nt + 24 + opt_hdr_size;

    for s in 0..num_sections {
        let sec = section_headers + s * 40;
        if !ptr_ok(sec, 40) {
            break;
        }
        let sec_chars = unsafe { *((sec + 36) as *const u32) };
        // 0x2000_0000 = IMAGE_SCN_MEM_EXECUTE
        if (sec_chars & 0x2000_0000) != 0 {
            let v_addr = unsafe { *((sec + 12) as *const u32) } as usize;
            let v_size = unsafe { *((sec + 8) as *const u32) } as usize;
            if v_addr == 0 || v_size < pattern.len() {
                continue;
            }
            let sec_start = base + v_addr;
            if !ptr_ok(sec_start, v_size) {
                continue;
            }
            let bytes = unsafe { core::slice::from_raw_parts(sec_start as *const u8, v_size) };
            'scan: for i in 0..=(v_size - pattern.len()) {
                for j in 0..pattern.len() {
                    if mask[j] != 0 && bytes[i + j] != pattern[j] {
                        continue 'scan;
                    }
                }
                return Some(sec_start + i);
            }
        }
    }
    None
}
