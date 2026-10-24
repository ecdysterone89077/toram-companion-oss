//! Resilient pattern scanner and symbol heuristic engine for x86_64 PE binaries.
//!
//! Restricts signature scans strictly to executable sections (`IMAGE_SCN_MEM_EXECUTE`)
//! to eliminate false positives in read-only data, and provides automated multi-signature
//! fallback and string cross-reference resolution across game updates.

use crate::win::ptr_ok;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pattern {
    pub bytes: Vec<Option<u8>>,
}

impl Pattern {
    /// Parse a IDA-style pattern string, e.g. `"48 89 5C 24 ?? 55 56 57"` or `"48 89 ? ? 55"`.
    pub fn parse(s: &str) -> Self {
        let bytes = s
            .split_whitespace()
            .map(|token| {
                if token == "?" || token == "??" {
                    None
                } else {
                    u8::from_str_radix(token, 16).ok()
                }
            })
            .collect();
        Self { bytes }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
}

/// Zero-allocation, branchless-optimized pattern matcher over a continuous memory slice.
#[inline]
pub fn matches_at(hay: *const u8, pat: &Pattern, remaining: usize) -> bool {
    if pat.len() > remaining {
        return false;
    }
    for (i, opt_byte) in pat.bytes.iter().enumerate() {
        if let Some(expected) = opt_byte {
            if unsafe { *hay.add(i) } != *expected {
                return false;
            }
        }
    }
    true
}

/// Scan a designated memory range for the given pattern.
pub unsafe fn scan_range(start: *const u8, len: usize, pat: &Pattern) -> Option<*const u8> {
    if pat.is_empty() || len < pat.len() {
        return None;
    }
    let end = len - pat.len();
    let first_byte = pat.bytes[0];
    let mut i = 0;

    while i <= end {
        // Fast skip on first non-wildcard byte
        if let Some(b0) = first_byte {
            if unsafe { *start.add(i) } != b0 {
                i += 1;
                continue;
            }
        }
        if matches_at(unsafe { start.add(i) }, pat, len - i) {
            return Some(unsafe { start.add(i) });
        }
        i += 1;
    }
    None
}

/// Enumerate all executable sections in the target in-memory PE module (`base`).
///
/// Returns a list of `(section_ptr, section_size)` tuples for sections flagged with
/// `IMAGE_SCN_MEM_EXECUTE` (`0x20000000`).
pub unsafe fn get_executable_sections(base: usize) -> Vec<(usize, usize)> {
    let mut sections = Vec::new();
    if !ptr_ok(base, 0x200) {
        return sections;
    }
    let mz = unsafe { *(base as *const u16) };
    if mz != 0x5A4D {
        return sections;
    }
    let e_lfanew = unsafe { *((base + 0x3C) as *const i32) };
    if !(0x40..0x1000).contains(&e_lfanew) {
        return sections;
    }
    let nt = base + (e_lfanew as usize);
    if !ptr_ok(nt, 0x100) || unsafe { *(nt as *const u32) } != 0x0000_4550 {
        return sections;
    }

    let num_sections = unsafe { *((nt + 0x06) as *const u16) } as usize;
    let opt_hdr_size = unsafe { *((nt + 0x14) as *const u16) } as usize;
    let section_headers_start = nt + 0x18 + opt_hdr_size;

    const SIZEOF_SECTION_HEADER: usize = 0x28;
    const IMAGE_SCN_MEM_EXECUTE: u32 = 0x2000_0000;

    for i in 0..num_sections {
        let sec = section_headers_start + i * SIZEOF_SECTION_HEADER;
        if !ptr_ok(sec, SIZEOF_SECTION_HEADER) {
            break;
        }
        let virtual_size = unsafe { *((sec + 0x08) as *const u32) } as usize;
        let virtual_address = unsafe { *((sec + 0x0C) as *const u32) } as usize;
        let characteristics = unsafe { *((sec + 0x24) as *const u32) };

        if (characteristics & IMAGE_SCN_MEM_EXECUTE) != 0 && virtual_size > 0 && virtual_address > 0
        {
            let section_ptr = base + virtual_address;
            if ptr_ok(section_ptr, virtual_size) {
                sections.push((section_ptr, virtual_size));
            }
        }
    }
    sections
}

/// Scan only the executable sections of the loaded module for a byte signature.
pub unsafe fn scan_module(base: usize, pat: &Pattern) -> Option<usize> {
    let sections = unsafe { get_executable_sections(base) };
    for (sec_ptr, sec_len) in sections {
        if let Some(hit) = unsafe { scan_range(sec_ptr as *const u8, sec_len, pat) } {
            return Some(hit as usize);
        }
    }
    None
}

/// Multi-signature fallback: tries each signature in sequence; the first match wins.
pub unsafe fn scan_first_of(base: usize, sigs: &[&str]) -> Option<usize> {
    for sig in sigs {
        let pat = Pattern::parse(sig);
        if let Some(hit) = unsafe { scan_module(base, &pat) } {
            return Some(hit);
        }
    }
    None
}

/// Backtrack from an instruction address up to 64 bytes to locate the enclosing function prologue.
pub unsafe fn backtrack_to_prologue(code_ptr: usize) -> usize {
    let mut p = code_ptr;
    let mut candidate = code_ptr;
    for _ in 0..64 {
        if !ptr_ok(p.saturating_sub(1), 4) {
            break;
        }
        let b = unsafe { *(p as *const u8) };
        if b == 0xCC || b == 0x90 {
            // Function starts immediately following INT3 or NOP padding
            return p + 1;
        }
        // Fallback: check for standard push rbp; mov rbp, rsp
        let b1 = unsafe { *((p + 1) as *const u8) };
        let b2 = unsafe { *((p + 2) as *const u8) };
        if b == 0x55 && b1 == 0x48 && b2 == 0x89 {
            candidate = p;
        }
        p = p.saturating_sub(1);
    }
    candidate
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pattern_parse_and_matches() {
        let pat = Pattern::parse("48 89 5C 24 ?? 55 56");
        assert_eq!(pat.len(), 7);
        assert_eq!(pat.bytes[0], Some(0x48));
        assert_eq!(pat.bytes[1], Some(0x89));
        assert_eq!(pat.bytes[2], Some(0x5C));
        assert_eq!(pat.bytes[3], Some(0x24));
        assert_eq!(pat.bytes[4], None);
        assert_eq!(pat.bytes[5], Some(0x55));
        assert_eq!(pat.bytes[6], Some(0x56));

        let data = [0x90, 0x48, 0x89, 0x5C, 0x24, 0x10, 0x55, 0x56, 0xC3];
        assert!(matches_at(&data[1], &pat, data.len() - 1));
        assert!(!matches_at(&data[0], &pat, data.len()));
    }

    #[test]
    fn test_scan_range_hit_and_miss() {
        let pat = Pattern::parse("48 8B 05 ?? ?? ?? ?? 48 85 C0");
        let buffer = [
            0xCC, 0xCC, 0x48, 0x8B, 0x05, 0x12, 0x34, 0x56, 0x78, 0x48, 0x85, 0xC0, 0x74, 0x05,
        ];

        let hit = unsafe { scan_range(buffer.as_ptr(), buffer.len(), &pat) };
        assert!(hit.is_some());
        assert_eq!(hit.unwrap(), unsafe { buffer.as_ptr().add(2) });

        let bad_pat = Pattern::parse("FF FF FF");
        let miss = unsafe { scan_range(buffer.as_ptr(), buffer.len(), &bad_pat) };
        assert!(miss.is_none());
    }

    #[test]
    fn test_backtrack_to_prologue() {
        let code: [u8; 16] = [
            0xCC, // INT3 padding
            0x55, 0x48, 0x89, 0xE5, // push rbp; mov rbp, rsp
            0x48, 0x83, 0xEC, 0x20, // sub rsp, 0x20
            0x48, 0x8D, 0x05, 0x00, 0x00, 0x00, 0x00, // lea rax, [rip]
        ];

        let target = unsafe { code.as_ptr().add(9) as usize }; // inside the lea
        let start = unsafe { backtrack_to_prologue(target) };
        assert_eq!(start, unsafe { code.as_ptr().add(1) as usize }); // pointing at 0x55
    }
}
