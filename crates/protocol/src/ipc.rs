use super::{SharedTelemetry, TelemetryFields, TelemetrySnapshot, TELEMETRY_MAP_NAME};
use core::ffi::c_void;
use core::mem::{size_of, zeroed};
use core::ptr;
use core::sync::atomic::Ordering;
use std::sync::OnceLock;

const FILE_MAP_READ: u32 = 4;
const FILE_MAP_WRITE: u32 = 2;
const PROCESS_DUP_HANDLE: u32 = 0x40;
const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
const ERROR_ALREADY_EXISTS: u32 = 183;
const STILL_ACTIVE: u32 = 259;

#[repr(C)]
struct SecurityAttributes {
    length: u32,
    descriptor: *mut c_void,
    inherit: i32,
}

#[repr(C)]
#[derive(Default)]
struct FileTime {
    low: u32,
    high: u32,
}

#[link(name = "kernel32")]
extern "system" {
    fn CreateFileMappingW(
        file: *mut c_void,
        attributes: *const SecurityAttributes,
        protect: u32,
        high: u32,
        low: u32,
        name: *const u16,
    ) -> *mut c_void;
    fn OpenFileMappingW(access: u32, inherit: i32, name: *const u16) -> *mut c_void;
    fn MapViewOfFile(
        mapping: *mut c_void,
        access: u32,
        high: u32,
        low: u32,
        size: usize,
    ) -> *mut c_void;
    fn UnmapViewOfFile(view: *const c_void) -> i32;
    fn CloseHandle(handle: *mut c_void) -> i32;
    fn LocalFree(memory: *mut c_void) -> *mut c_void;
    fn GetCurrentProcess() -> *mut c_void;
    fn GetCurrentProcessId() -> u32;
    fn GetTickCount64() -> u64;
    fn GetLastError() -> u32;
    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
    fn GetProcessTimes(
        process: *mut c_void,
        created: *mut FileTime,
        exited: *mut FileTime,
        kernel: *mut FileTime,
        user: *mut FileTime,
    ) -> i32;
    fn GetExitCodeProcess(process: *mut c_void, code: *mut u32) -> i32;
    fn DuplicateHandle(
        source_process: *mut c_void,
        source: *mut c_void,
        target_process: *mut c_void,
        target: *mut *mut c_void,
        access: u32,
        inherit: i32,
        options: u32,
    ) -> i32;
}

pub fn monotonic_ms() -> u64 {
    unsafe { GetTickCount64() }
}

#[link(name = "advapi32")]
extern "system" {
    fn OpenProcessToken(process: *mut c_void, access: u32, token: *mut *mut c_void) -> i32;
    fn GetTokenInformation(
        token: *mut c_void,
        class: u32,
        buffer: *mut c_void,
        size: u32,
        needed: *mut u32,
    ) -> i32;
    fn ConvertSidToStringSidW(sid: *const c_void, text: *mut *mut u16) -> i32;
    fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
        text: *const u16,
        revision: u32,
        descriptor: *mut *mut c_void,
        size: *mut u32,
    ) -> i32;
}

struct Handle(*mut c_void);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

struct LocalMemory(*mut c_void);
impl Drop for LocalMemory {
    fn drop(&mut self) {
        unsafe {
            LocalFree(self.0);
        }
    }
}

struct MappedView {
    handle: usize,
    view: usize,
}
impl Drop for MappedView {
    fn drop(&mut self) {
        unsafe {
            UnmapViewOfFile(self.view as *const c_void);
            CloseHandle(self.handle as *mut c_void);
        }
    }
}
impl MappedView {
    fn shared(&self) -> &SharedTelemetry {
        unsafe { &*(self.view as *const SharedTelemetry) }
    }
    unsafe fn map(handle: *mut c_void, access: u32) -> Option<Self> {
        let view = unsafe { MapViewOfFile(handle, access, 0, 0, size_of::<SharedTelemetry>()) };
        if view.is_null() {
            unsafe {
                CloseHandle(handle);
            }
            return None;
        }
        Some(Self {
            handle: handle as usize,
            view: view as usize,
        })
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

unsafe fn lifetime(process: *mut c_void) -> Option<u64> {
    let (mut created, mut exited, mut kernel, mut user) =
        unsafe { (zeroed(), zeroed(), zeroed(), zeroed()) };
    let mut code = 0;
    if unsafe { GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user) } == 0
        || unsafe { GetExitCodeProcess(process, &mut code) } == 0
        || code != STILL_ACTIVE
    {
        return None;
    }
    Some(((created.high as u64) << 32) | created.low as u64)
}

fn mapping_name(pid: u32, created_at: u64) -> Vec<u16> {
    wide(&format!(
        "{}-{}-{:016x}",
        TELEMETRY_MAP_NAME.trim_end_matches('\0'),
        pid,
        created_at
    ))
}

unsafe fn read_only_descriptor() -> Option<LocalMemory> {
    let mut token = ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), 8, &mut token) } == 0 {
        return None;
    }
    let token = Handle(token);
    let mut needed = 0;
    unsafe {
        GetTokenInformation(token.0, 1, ptr::null_mut(), 0, &mut needed);
    }
    if needed == 0 {
        return None;
    }
    let mut buffer = vec![0u8; needed as usize];
    if unsafe { GetTokenInformation(token.0, 1, buffer.as_mut_ptr().cast(), needed, &mut needed) }
        == 0
    {
        return None;
    }
    let sid = unsafe { ptr::read_unaligned(buffer.as_ptr().cast::<*const c_void>()) };
    let mut sid_text = ptr::null_mut();
    if unsafe { ConvertSidToStringSidW(sid, &mut sid_text) } == 0 {
        return None;
    }
    let sid_allocation = LocalMemory(sid_text.cast());
    let mut len = 0;
    while unsafe { *sid_text.add(len) } != 0 {
        len += 1;
    }
    let sid = String::from_utf16_lossy(unsafe { core::slice::from_raw_parts(sid_text, len) });
    drop(sid_allocation);
    // OWNER RIGHTS also removes the owner's implicit WRITE_DAC permission.
    // The initial creator handle retains full access; every later open is read-only.
    let sddl = wide(&format!(
        "O:{sid}G:{sid}D:P(A;;0x00020004;;;OW)(A;;0x00020004;;;SY)(A;;0x00020004;;;{sid})"
    ));
    let mut descriptor = ptr::null_mut();
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            1,
            &mut descriptor,
            ptr::null_mut(),
        )
    } == 0
    {
        return None;
    }
    Some(LocalMemory(descriptor))
}

/// Only the process holding the original creation handle can publish telemetry.
pub struct TelemetryWriter {
    mapping: MappedView,
    pid: u32,
}
impl TelemetryWriter {
    fn authorized(&self) -> bool {
        self.pid == unsafe { GetCurrentProcessId() }
            && self.mapping.shared().creator_pid.load(Ordering::SeqCst) == self.pid
    }
    pub fn update(&self, write: impl FnOnce(&TelemetryFields)) {
        if self.authorized() {
            self.mapping.shared().update(write);
        }
    }
    pub fn init(&self, pid: u32) {
        if self.authorized() && pid == self.pid {
            self.mapping.shared().init(pid);
        }
    }
    pub fn record_cmd_result(&self, pid: u32, cmd: i32, status: i32) {
        if self.authorized() && pid == self.pid {
            self.mapping.shared().record_cmd_result(pid, cmd, status);
        }
    }
    pub fn set_toggle_bit(&self, bit: u32, enabled: bool) {
        if self.authorized() {
            self.mapping.shared().set_toggle_bit(bit, enabled);
        }
    }
}

unsafe fn create_writer(name: &[u16], pid: u32, created_at: u64) -> Option<TelemetryWriter> {
    let descriptor = unsafe { read_only_descriptor() }?;
    let attributes = SecurityAttributes {
        length: size_of::<SecurityAttributes>() as u32,
        descriptor: descriptor.0,
        inherit: 0,
    };
    let handle = unsafe {
        CreateFileMappingW(
            (-1isize) as *mut c_void,
            &attributes,
            4,
            0,
            size_of::<SharedTelemetry>() as u32,
            name.as_ptr(),
        )
    };
    let error = unsafe { GetLastError() };
    if handle.is_null() {
        return None;
    }
    if error == ERROR_ALREADY_EXISTS {
        unsafe {
            CloseHandle(handle);
        }
        return None;
    }
    let mapping = unsafe { MappedView::map(handle, FILE_MAP_WRITE | FILE_MAP_READ) }?;
    // A new paging-file mapping is zero-filled. Keep even initialization atomic:
    // the name becomes visible before this function finishes publishing the header.
    let shared = mapping.shared();
    shared.creator_pid.store(pid, Ordering::SeqCst);
    shared.created_at.store(created_at, Ordering::SeqCst);
    shared
        .creator_handle
        .store(mapping.handle as u64, Ordering::SeqCst);
    let writer = TelemetryWriter { mapping, pid };
    writer.init(pid);
    Some(writer)
}

static GLOBAL_TELEMETRY: OnceLock<Option<TelemetryWriter>> = OnceLock::new();

/// Create a private writer view for this process; never attach to an existing mapping.
pub fn telemetry() -> Option<&'static TelemetryWriter> {
    GLOBAL_TELEMETRY
        .get_or_init(|| unsafe {
            let pid = GetCurrentProcessId();
            let created_at = lifetime(GetCurrentProcess())?;
            create_writer(&mapping_name(pid, created_at), pid, created_at)
        })
        .as_ref()
}

/// Where a telemetry read stopped. Ordered along the path a read actually takes,
/// so a caller can report *why* a snapshot is absent instead of only that it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TelemetryStage {
    /// PID 0, or `OpenProcess` refused the handle: the process is gone.
    NoProcess,
    /// No readable creation time, or the process has already exited.
    NoLifetime,
    /// No mapping under this process identity: the DLL has not published yet.
    NoMapping,
    /// The mapping header carries no creator handle.
    NoClaim,
    /// The claimed handle is absent from the target's handle table: a squatter.
    NoCustody,
    /// Custody proved, but the header names another PID or creation time.
    IdentityMismatch,
    /// Custody and identity match, but no writer has published a snapshot.
    SnapshotUnset,
    /// The process restarted between the identity check and the read.
    Restarted,
    /// Every check passed.
    Ok,
}

impl TelemetryStage {
    /// Short stable token for logs and diagnostics.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoProcess => "no-process",
            Self::NoLifetime => "no-lifetime",
            Self::NoMapping => "no-mapping",
            Self::NoClaim => "no-claim",
            Self::NoCustody => "no-custody",
            Self::IdentityMismatch => "identity-mismatch",
            Self::SnapshotUnset => "snapshot-unset",
            Self::Restarted => "restarted",
            Self::Ok => "ok",
        }
    }
}

/// Full result of a staged telemetry read, including the stage that stopped it.
#[derive(Debug, Clone, Copy)]
pub struct TelemetryDiagnosis {
    pub pid: u32,
    /// Process creation time, when the handle allowed reading it.
    pub created_at: Option<u64>,
    pub stage: TelemetryStage,
    pub snapshot: TelemetrySnapshot,
}

unsafe fn read_named(
    process: *mut c_void,
    pid: u32,
    created_at: u64,
    name: &[u16],
) -> Result<TelemetrySnapshot, TelemetryStage> {
    let public_handle = unsafe { OpenFileMappingW(FILE_MAP_READ, 0, name.as_ptr()) };
    if public_handle.is_null() {
        return Err(TelemetryStage::NoMapping);
    }
    let public_view = unsafe { MappedView::map(public_handle, FILE_MAP_READ) }
        .ok_or(TelemetryStage::NoMapping)?;
    let claimed_handle = public_view.shared().creator_handle.load(Ordering::SeqCst);
    drop(public_view);
    if claimed_handle == 0 {
        return Err(TelemetryStage::NoClaim);
    }
    // Authenticate custody with the kernel, rather than believing the public header.
    // A squatter cannot put its mapping handle in the target's handle table.
    let mut duplicate = ptr::null_mut();
    if unsafe {
        DuplicateHandle(
            process,
            claimed_handle as *mut c_void,
            GetCurrentProcess(),
            &mut duplicate,
            FILE_MAP_READ,
            0,
            0,
        )
    } == 0
    {
        return Err(TelemetryStage::NoCustody);
    }
    let trusted_view =
        unsafe { MappedView::map(duplicate, FILE_MAP_READ) }.ok_or(TelemetryStage::NoCustody)?;
    let shared = trusted_view.shared();
    if shared.creator_pid.load(Ordering::SeqCst) != pid
        || shared.created_at.load(Ordering::SeqCst) != created_at
        || shared.creator_handle.load(Ordering::SeqCst) != claimed_handle
    {
        return Err(TelemetryStage::IdentityMismatch);
    }
    let snap = shared.snapshot();
    if !snap.valid || snap.pid != pid {
        return Err(TelemetryStage::SnapshotUnset);
    }
    if unsafe { lifetime(process) } != Some(created_at) {
        return Err(TelemetryStage::Restarted);
    }
    Ok(snap)
}

/// Read only a mapping held by this live process; no writer reference escapes.
pub fn read_telemetry(pid: u32) -> TelemetrySnapshot {
    diagnose_telemetry(pid).snapshot
}

/// The same read, reporting which stage stopped it. Never skips a check:
/// every failure leaves `snapshot` at its default.
pub fn diagnose_telemetry(pid: u32) -> TelemetryDiagnosis {
    let mut diagnosis = TelemetryDiagnosis {
        pid,
        created_at: None,
        stage: TelemetryStage::NoProcess,
        snapshot: TelemetrySnapshot::default(),
    };
    if pid == 0 {
        return diagnosis;
    }
    unsafe {
        let process = OpenProcess(
            PROCESS_DUP_HANDLE | PROCESS_QUERY_LIMITED_INFORMATION,
            0,
            pid,
        );
        if process.is_null() {
            return diagnosis;
        }
        let process = Handle(process);
        let Some(created_at) = lifetime(process.0) else {
            diagnosis.stage = TelemetryStage::NoLifetime;
            return diagnosis;
        };
        diagnosis.created_at = Some(created_at);
        match read_named(process.0, pid, created_at, &mapping_name(pid, created_at)) {
            Ok(snapshot) => {
                diagnosis.stage = TelemetryStage::Ok;
                diagnosis.snapshot = snapshot;
            }
            Err(stage) => diagnosis.stage = stage,
        }
        diagnosis
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;
    static TEST_ID: AtomicU32 = AtomicU32::new(0);
    fn name() -> Vec<u16> {
        wide(&format!(
            "Local\\TelemetrySecurityTest-{}-{}",
            unsafe { GetCurrentProcessId() },
            TEST_ID.fetch_add(1, Ordering::SeqCst)
        ))
    }

    #[test]
    fn child_reader_probe() {
        let Ok(pid) = std::env::var("TELEMETRY_SECURITY_PARENT") else {
            return;
        };
        let pid: u32 = pid.parse().unwrap();
        let snap = read_telemetry(pid);
        assert!(snap.valid);
        assert_eq!(snap.pid, pid);
        assert_eq!(snap.last_cmd, 73);
        assert_eq!(snap.last_status, -8);
    }

    #[test]
    fn consumer_process_reads_authenticated_read_only_view() {
        let writer = telemetry().unwrap();
        writer.record_cmd_result(unsafe { GetCurrentProcessId() }, 73, -8);
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "ipc::tests::child_reader_probe", "--nocapture"])
            .env(
                "TELEMETRY_SECURITY_PARENT",
                unsafe { GetCurrentProcessId() }.to_string(),
            )
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[test]
    fn diagnosis_reports_stage_for_absent_process() {
        let diagnosis = diagnose_telemetry(0);
        assert_eq!(diagnosis.stage, TelemetryStage::NoProcess);
        assert_eq!(diagnosis.stage.as_str(), "no-process");
        assert_eq!(diagnosis.created_at, None);
        assert!(!diagnosis.snapshot.valid);
    }

    #[test]
    fn diagnosis_reports_ok_for_own_published_writer() {
        let pid = unsafe { GetCurrentProcessId() };
        let writer = telemetry().expect("telemetry writer");
        writer.init(pid);
        let diagnosis = diagnose_telemetry(pid);
        assert_eq!(
            diagnosis.stage,
            TelemetryStage::Ok,
            "stage {}",
            diagnosis.stage.as_str()
        );
        assert_eq!(diagnosis.snapshot.pid, pid);
        assert!(diagnosis.created_at.is_some());
    }

    #[test]
    fn creator_reads_but_named_writers_and_dacl_changes_are_denied() {
        unsafe {
            let pid = GetCurrentProcessId();
            let created_at = lifetime(GetCurrentProcess()).unwrap();
            let name = name();
            let writer = create_writer(&name, pid, created_at).unwrap();
            writer.record_cmd_result(pid, 73, 0);
            assert!(OpenFileMappingW(FILE_MAP_WRITE, 0, name.as_ptr()).is_null());
            assert!(OpenFileMappingW(0x00040000, 0, name.as_ptr()).is_null());
            let snap = read_named(GetCurrentProcess(), pid, created_at, &name).unwrap();
            assert_eq!(snap.last_cmd, 73);
            assert!(create_writer(&name, pid, created_at).is_none());
        }
    }

    #[test]
    fn forged_mapping_and_wrong_creator_cannot_authenticate() {
        unsafe {
            let pid = GetCurrentProcessId();
            let created_at = lifetime(GetCurrentProcess()).unwrap();
            let name = name();
            let writer = create_writer(&name, pid, created_at).unwrap();
            writer
                .mapping
                .shared()
                .creator_handle
                .store(u64::MAX, Ordering::SeqCst);
            assert_eq!(
                read_named(GetCurrentProcess(), pid, created_at, &name),
                Err(TelemetryStage::NoCustody)
            );
            writer
                .mapping
                .shared()
                .creator_pid
                .store(pid.wrapping_add(1), Ordering::SeqCst);
            writer.record_cmd_result(pid, 73, 0);
            assert_eq!(writer.mapping.shared().snapshot().cmd_seq, 0);
            // `creator_handle` was corrupted above and stays corrupted, so this
            // call still stops at custody before identity is ever compared.
            assert_eq!(
                read_named(GetCurrentProcess(), pid, created_at + 1, &name),
                Err(TelemetryStage::NoCustody)
            );
        }
    }
}
