//! `companion` — Core logic for the Toram Online companion control surface:
//! - Direct in-process command dispatch via `wboot::dispatch_to_toram` (zero subprocess spawn overhead)
//!   with automatic fallback to subprocess execution when custom binaries are specified
//! - Canonical `wcore.dll` + `wboot.exe` resolution (`resolve_binary_pair`)
//! - Strict `[result] pid=<pid> cmd=<cmd> status=<status>` receipt validation (`parse_result_receipt`)
//! - Process-matched `SharedTelemetry` cache reader (`read_live_telemetry`)
//! - Account Login / Credential Reset (`reset_toram_account_live`)

use protocol::{
    read_telemetry, TelemetrySnapshot, MP_AMPR_BONUS_10, MP_AMPR_BONUS_25, MP_AMPR_BONUS_50,
};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use wboot::{find_process_id, ProcessIdentity};
mod control;
pub use control::{BatchResult, CombatProfile, CommandPlan, TargetSelection};

pub const WINDOW_TITLE: &str = concat!("Game Companion (v", env!("CARGO_PKG_VERSION"), ")");

/// Evidence is local callback activity, never inferred server acceptance.
pub fn feature_status_label(snapshots: &[TelemetrySnapshot], bit: u32) -> &'static str {
    if snapshots.is_empty() || snapshots.iter().any(|s| !s.valid) {
        return "?";
    }
    if snapshots.iter().all(|s| s.active_toggles & bit == 0) {
        return "OFF";
    }
    if snapshots.iter().any(|s| s.active_toggles & bit == 0) {
        return "MIXED";
    }
    if snapshots.iter().any(|s| s.partial_toggles & bit != 0) {
        return "PARTIAL";
    }
    if bit == protocol::TOGGLE_UNLIMITED_MP
        && snapshots
            .iter()
            .any(|s| s.mp_hit_calls > 0 && s.mp_getter_calls == 0)
    {
        return "NO BONUS CALLS";
    }
    let tracked = protocol::TOGGLE_INVINCIBILITY
        | protocol::TOGGLE_ALWAYS_CRIT
        | protocol::TOGGLE_INSTANT_ATK
        | protocol::TOGGLE_RANGE_30M
        | protocol::TOGGLE_UNLIMITED_MP;
    if bit & tracked == 0 {
        return "ON / UNMEASURED";
    }
    if snapshots.iter().all(|s| s.observed_toggles & bit != 0) {
        "CALLBACK SEEN"
    } else {
        "ARMED"
    }
}

/// Worker readiness and the age of game observations are separate facts.
pub fn worker_status_label(snapshot: &TelemetrySnapshot, now_ms: u64) -> &'static str {
    if !snapshot.valid {
        return "UNATTACHED";
    }
    match snapshot.worker_state {
        1 if snapshot.resource_tick_ms == 0 => "READY / NO SAMPLE",
        1 if now_ms.saturating_sub(snapshot.resource_tick_ms) > 15_000 => "READY / STALE SAMPLE",
        1 => "READY",
        2 => "BUSY",
        0 => "STARTING",
        _ => "STOPPED",
    }
}

/// Human label for the requested crit setting in the telemetry line.
pub fn critical_damage_setting_label(setting: i32) -> String {
    match setting {
        0 => "Native".to_owned(),
        s if s < 0 => format!("Native+{}", s.saturating_neg()),
        s => format!("{}%", s),
    }
}

/// Human label for the requested AMPR bonus in the telemetry line.
pub fn mp_bonus_label(bonus: i32) -> String {
    match bonus {
        0 => "Native".to_owned(),
        MP_AMPR_BONUS_10 => "+10".to_owned(),
        MP_AMPR_BONUS_25 => "+25".to_owned(),
        MP_AMPR_BONUS_50 => "+50".to_owned(),
        _ => "Unknown".to_owned(),
    }
}
pub const TORAM_REG_SUBKEY: &str = r"Software\Asobimo,Inc\ToramOnline";
pub const LAUNCHER_REG_SUBKEY: &str = r"Software\Asobimo,Inc\ASOBIMOLauncher";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandResult {
    pub ok: bool,
    pub cmd: i32,
    pub exit_code: Option<i32>,
    pub remote_status: Option<i32>,
    pub remote_pid: Option<u32>,
    pub message: String,
    pub stdout: String,
    pub stderr: String,
    pub log_tail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResultReceipt {
    pub pid: u32,
    pub cmd: i32,
    pub status: i32,
}

/// Read the existing telemetry cache, discarding values from another process.
/// The protocol has no producer timestamp; resource freshness is unknown.
pub fn read_live_telemetry() -> (u32, TelemetrySnapshot) {
    let live_pid = find_process_id("ToramOnline.exe");
    let snap = read_telemetry(live_pid);
    (live_pid, matching_process_snapshot(live_pid, snap))
}

fn matching_process_snapshot(live_pid: u32, snapshot: TelemetrySnapshot) -> TelemetrySnapshot {
    if live_pid != 0 && snapshot.valid && snapshot.pid == live_pid {
        snapshot
    } else {
        TelemetrySnapshot::default()
    }
}

/// Parse the last matching `[result] pid=<u32> cmd=<i32> status=<i32>` receipt line from text.
pub fn parse_result_receipt(text: &str, expected_cmd: Option<i32>) -> Option<ResultReceipt> {
    let mut found = None;
    for raw_line in text.lines() {
        let line = raw_line.trim();
        let Some(after) = line.strip_prefix("[result] ") else {
            continue;
        };
        let mut pid_opt: Option<u32> = None;
        let mut cmd_opt: Option<i32> = None;
        let mut status_opt: Option<i32> = None;

        for token in after.split_whitespace() {
            if let Some(v) = token.strip_prefix("pid=") {
                pid_opt = v.parse::<u32>().ok();
            } else if let Some(v) = token.strip_prefix("cmd=") {
                cmd_opt = v.parse::<i32>().ok();
            } else if let Some(v) = token.strip_prefix("status=") {
                status_opt = v.parse::<i32>().ok();
            }
        }

        if let (Some(pid), Some(cmd), Some(status)) = (pid_opt, cmd_opt, status_opt) {
            if expected_cmd.is_none() || expected_cmd == Some(cmd) {
                found = Some(ResultReceipt { pid, cmd, status });
            }
        }
    }
    found
}

/// Only one receipt from the selected child's stdout may establish success.
fn parse_target_receipt(stdout: &str, cmd: i32, pid: u32) -> Option<ResultReceipt> {
    if pid == 0 {
        return None;
    }
    let mut found = None;
    for line in stdout.lines() {
        if let Some(receipt) = parse_result_receipt(line, Some(cmd)) {
            if receipt.pid != pid || found.is_some() {
                return None;
            }
            found = Some(receipt);
        }
    }
    found
}

fn parse_version_filename(file_name: &str, prefix: &str, suffix: &str) -> Option<u32> {
    let lower = file_name.to_ascii_lowercase();
    let rest = lower.strip_prefix(prefix)?.strip_suffix(suffix)?;
    if rest.is_empty() || !rest.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    rest.parse::<u32>().ok()
}

/// Prefer the canonical build pair; fall back to the highest complete legacy pair.
pub fn resolve_binary_pair(src_dir: &Path) -> Result<(PathBuf, PathBuf), String> {
    if !src_dir.is_dir() {
        return Err(format!(
            "Source directory does not exist: {}",
            src_dir.display()
        ));
    }

    let canonical_boot = src_dir.join("wboot.exe");
    let canonical_dll = src_dir.join("wcore.dll");
    if canonical_boot.is_file() && canonical_dll.is_file() {
        return Ok((canonical_boot, canonical_dll));
    }

    let mut boot_versions: Vec<(u32, PathBuf)> = Vec::new();
    let entries = fs::read_dir(src_dir)
        .map_err(|e| format!("Failed to read directory {}: {}", src_dir.display(), e))?;

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(fname) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if let Some(ver) = parse_version_filename(fname, "wboot", ".exe") {
            boot_versions.push((ver, path));
        }
    }

    boot_versions.sort_by_key(|a| std::cmp::Reverse(a.0));

    for (ver, boot_path) in &boot_versions {
        let dll_path = src_dir.join(format!("wcore{}.dll", ver));
        if dll_path.is_file() {
            return Ok((boot_path.clone(), dll_path));
        }
    }

    Err(format!(
        "No matching wboot.exe + wcore.dll pair found in {}",
        src_dir.display()
    ))
}

pub fn read_tail_lines(path: &Path, max_lines: usize) -> String {
    let Ok(content) = fs::read_to_string(path) else {
        return String::new();
    };
    let lines: Vec<&str> = content.lines().collect();
    let start = lines.len().saturating_sub(max_lines);
    lines[start..].join("\n")
}

pub fn read_text_from_offset(path: &Path, offset: u64) -> String {
    let Ok(bytes) = fs::read(path) else {
        return String::new();
    };
    let start = (offset as usize).min(bytes.len());
    String::from_utf8_lossy(&bytes[start..]).into_owned()
}

pub struct CommandRunner {
    pub work_dir: PathBuf,
    pub src_dir: PathBuf,
    pub wboot_path: Option<PathBuf>,
    pub wcore_path: Option<PathBuf>,
    pub wcore_log_path: PathBuf,
    pub wboot_log_path: PathBuf,
    pub timeout_ms: u64,
    pub use_in_process_dispatch: bool,
    in_flight: Arc<AtomicBool>,
    cancel_epoch: AtomicU64,
}

impl CommandRunner {
    pub fn new(
        work_dir: PathBuf,
        wboot_path: Option<PathBuf>,
        wcore_path: Option<PathBuf>,
        wcore_log_path: Option<PathBuf>,
        wboot_log_path: Option<PathBuf>,
        timeout_ms: u64,
    ) -> Self {
        let src_dir = work_dir.join("src");
        let default_wcore_log = env::temp_dir().join("wcore.log");
        let default_wboot_log = work_dir.join("wboot.log");
        let use_in_process_dispatch = wboot_path.is_none();
        Self {
            work_dir,
            src_dir,
            wboot_path,
            wcore_path,
            wcore_log_path: wcore_log_path.unwrap_or(default_wcore_log),
            wboot_log_path: wboot_log_path.unwrap_or(default_wboot_log),
            timeout_ms,
            use_in_process_dispatch,
            in_flight: Arc::new(AtomicBool::new(false)),
            cancel_epoch: AtomicU64::new(0),
        }
    }

    pub fn is_busy(&self) -> bool {
        self.in_flight.load(Ordering::SeqCst)
    }

    pub fn resolve_paths(&self) -> Result<(PathBuf, PathBuf), String> {
        if let (Some(wb), Some(wc)) = (&self.wboot_path, &self.wcore_path) {
            if !wb.is_file() {
                return Err(format!("wboot executable not found: {}", wb.display()));
            }
            if !wc.is_file() {
                return Err(format!("wcore DLL not found: {}", wc.display()));
            }
            return Ok((wb.clone(), wc.clone()));
        }
        resolve_binary_pair(&self.src_dir)
    }

    fn run_subprocess(&self, cmd: i32, expected: &ProcessIdentity) -> CommandResult {
        let (wboot, wcore) = match self.resolve_paths() {
            Ok(pair) => pair,
            Err(msg) => {
                return CommandResult {
                    ok: false,
                    cmd,
                    exit_code: None,
                    remote_status: None,
                    remote_pid: None,
                    message: msg,
                    stdout: String::new(),
                    stderr: String::new(),
                    log_tail: String::new(),
                };
            }
        };

        let mut child = match Command::new(&wboot)
            .arg(cmd.to_string())
            .arg(&wcore)
            .arg("--pid")
            .arg(expected.pid.to_string())
            .arg("--created")
            .arg(expected.created_at.to_string())
            .arg("--image")
            .arg(&expected.image_path)
            .current_dir(&self.work_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                return CommandResult {
                    ok: false,
                    cmd,
                    exit_code: None,
                    remote_status: None,
                    remote_pid: None,
                    message: format!("Failed to launch {}: {}", wboot.display(), e),
                    stdout: String::new(),
                    stderr: String::new(),
                    log_tail: String::new(),
                };
            }
        };

        let deadline = Instant::now() + Duration::from_millis(self.timeout_ms);
        loop {
            match child.try_wait() {
                Ok(Some(_status)) => break,
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        let log_tail = read_tail_lines(&self.wcore_log_path, 40);
                        return CommandResult {
                            ok: false,
                            cmd,
                            exit_code: None,
                            remote_status: None,
                            remote_pid: None,
                            message: format!(
                                "Command {} timed out after {} ms",
                                cmd, self.timeout_ms
                            ),
                            stdout: String::new(),
                            stderr: String::new(),
                            log_tail,
                        };
                    }
                    std::thread::sleep(Duration::from_millis(15));
                }
                Err(e) => {
                    return CommandResult {
                        ok: false,
                        cmd,
                        exit_code: None,
                        remote_status: None,
                        remote_pid: None,
                        message: format!("Error waiting on wboot process: {}", e),
                        stdout: String::new(),
                        stderr: String::new(),
                        log_tail: String::new(),
                    };
                }
            }
        }

        let output = match child.wait_with_output() {
            Ok(o) => o,
            Err(e) => {
                return CommandResult {
                    ok: false,
                    cmd,
                    exit_code: None,
                    remote_status: None,
                    remote_pid: None,
                    message: format!("Failed to collect output: {}", e),
                    stdout: String::new(),
                    stderr: String::new(),
                    log_tail: String::new(),
                };
            }
        };

        let exit_code = output.status.code();
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        let log_tail = read_tail_lines(&self.wcore_log_path, 40);

        let receipt = parse_target_receipt(&stdout, cmd, expected.pid);

        if exit_code != Some(0) {
            return CommandResult {
                ok: false,
                cmd,
                exit_code,
                remote_status: receipt.map(|r| r.status),
                remote_pid: receipt.map(|r| r.pid),
                message: format!("wboot exited with code {:?} for cmd {}", exit_code, cmd),
                stdout,
                stderr,
                log_tail,
            };
        }

        let Some(rec) = receipt else {
            return CommandResult {
                ok: false,
                cmd,
                exit_code,
                remote_status: None,
                remote_pid: None,
                message: format!(
                    "Missing [result] receipt for cmd {} despite exit code 0",
                    cmd
                ),
                stdout,
                stderr,
                log_tail,
            };
        };

        if rec.pid == 0 {
            return CommandResult {
                ok: false,
                cmd,
                exit_code,
                remote_status: Some(rec.status),
                remote_pid: Some(0),
                message: format!("Invalid remote PID 0 in receipt for cmd {}", cmd),
                stdout,
                stderr,
                log_tail,
            };
        }

        if rec.status != 0 {
            return CommandResult {
                ok: false,
                cmd,
                exit_code,
                remote_status: Some(rec.status),
                remote_pid: Some(rec.pid),
                message: format!(
                    "Remote worker reported failure status={} for cmd {} (pid={})",
                    rec.status, cmd, rec.pid
                ),
                stdout,
                stderr,
                log_tail,
            };
        }

        CommandResult {
            ok: true,
            cmd,
            exit_code,
            remote_status: Some(0),
            remote_pid: Some(rec.pid),
            message: format!("OK — cmd {} applied (pid={}, status=0)", cmd, rec.pid),
            stdout,
            stderr,
            log_tail,
        }
    }
}

// -----------------------------------------------------------------------------
// Account Login / Credential Reset (`AccountResetter`)
// -----------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountResetResult {
    pub ok: bool,
    pub process_killed: bool,
    pub removed_values: Vec<String>,
    pub message: String,
}

pub fn should_delete_registry_value(name: &str) -> bool {
    name.starts_with("AsobimoOptionKey")
        || name.starts_with("ASOBIMO_ID")
        || name.starts_with("saveAppIdKey")
        || name.starts_with("saveIsAccountUserKey")
        || name.starts_with("unity.cloud_userid")
        || name.starts_with("unity.player_sessionid")
        || name.starts_with("unity.player_session_count")
        || name.starts_with("TellHistorySaveKey")
        || name.starts_with("MarketNameSearchHistoryKey")
        || name.starts_with("ONCE_FLAG")
        || name.starts_with("AB0007")
        || name.starts_with("ASB_")
        || name.starts_with("CountExecution")
        || name.starts_with("Language")
        || name.starts_with("KEY_LAST")
        || name.starts_with("KEY_Last")
}

const HKEY_CURRENT_USER: isize = -2147483647i32 as isize;
const KEY_READ: u32 = 0x20019;
const KEY_SET_VALUE: u32 = 0x0002;
const ERROR_SUCCESS: u32 = 0;

#[link(name = "advapi32")]
extern "system" {
    fn RegOpenKeyExW(
        h_key: isize,
        lp_sub_key: *const u16,
        ul_options: u32,
        sam_desired: u32,
        phk_result: *mut isize,
    ) -> u32;
    fn RegEnumValueW(
        h_key: isize,
        dw_index: u32,
        lp_value_name: *mut u16,
        lpcch_value_name: *mut u32,
        lp_reserved: *mut u32,
        lp_type: *mut u32,
        lp_data: *mut u8,
        lpcb_data: *mut u32,
    ) -> u32;
    fn RegDeleteValueW(h_key: isize, lp_value_name: *const u16) -> u32;
    fn RegCloseKey(h_key: isize) -> u32;
}

unsafe fn wipe_registry_values_under(subkey: &str) -> Vec<String> {
    let subkey_w: Vec<u16> = subkey.encode_utf16().chain(core::iter::once(0)).collect();
    let mut hkey: isize = 0;
    let rc = RegOpenKeyExW(
        HKEY_CURRENT_USER,
        subkey_w.as_ptr(),
        0,
        KEY_READ | KEY_SET_VALUE,
        &mut hkey,
    );
    if rc != ERROR_SUCCESS {
        return Vec::new();
    }

    let mut candidates: Vec<String> = Vec::new();
    let mut index = 0u32;
    loop {
        let mut name_buf = [0u16; 512];
        let mut name_len = name_buf.len() as u32;
        let enum_rc = RegEnumValueW(
            hkey,
            index,
            name_buf.as_mut_ptr(),
            &mut name_len,
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            core::ptr::null_mut(),
        );
        if enum_rc != ERROR_SUCCESS {
            break;
        }
        let name = String::from_utf16_lossy(&name_buf[..name_len as usize]);
        if should_delete_registry_value(&name) {
            candidates.push(name);
        }
        index += 1;
    }

    let mut removed = Vec::new();
    for name in candidates {
        let wname: Vec<u16> = name.encode_utf16().chain(core::iter::once(0)).collect();
        if RegDeleteValueW(hkey, wname.as_ptr()) == ERROR_SUCCESS {
            removed.push(name);
        }
    }

    RegCloseKey(hkey);
    removed
}

fn wipe_local_asobimo_cache() {
    if let Ok(userprofile) = env::var("USERPROFILE") {
        let local_low = PathBuf::from(userprofile)
            .join("AppData")
            .join("LocalLow")
            .join("Asobimo,Inc");
        let dirs = [
            local_low.join("ToramOnline").join("Unity"),
            local_low.join("ASOBIMOLauncher").join("Unity"),
        ];
        for d in &dirs {
            if d.exists() {
                let _ = fs::remove_dir_all(d);
            }
        }
        let files = [
            local_low.join("ToramOnline").join("Player.log"),
            local_low.join("ToramOnline").join("Player-prev.log"),
            local_low.join("ASOBIMOLauncher").join("Player.log"),
            local_low.join("ASOBIMOLauncher").join("Player-prev.log"),
        ];
        for f in &files {
            if f.exists() {
                let _ = fs::remove_file(f);
            }
        }
    }
}

pub fn reset_toram_account_live() -> AccountResetResult {
    let mut process_killed = false;
    for proc_name in ["ToramOnline.exe", "ASOBIMOLauncher.exe"] {
        let kill_out = Command::new("taskkill")
            .args(["/f", "/t", "/im", proc_name])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output();

        if let Ok(out) = kill_out {
            if out.status.code() == Some(0) {
                process_killed = true;
            } else {
                let combined = format!(
                    "{}\n{}",
                    String::from_utf8_lossy(&out.stdout),
                    String::from_utf8_lossy(&out.stderr)
                )
                .to_ascii_lowercase();
                if out.status.code() != Some(128) && !combined.contains("not found") {
                    return AccountResetResult {
                        ok: false,
                        process_killed: false,
                        removed_values: Vec::new(),
                        message: format!("Failed to stop {}: {}", proc_name, combined.trim()),
                    };
                }
            }
        }
    }

    unsafe {
        let mut removed = wipe_registry_values_under(TORAM_REG_SUBKEY);
        let mut launcher_removed = wipe_registry_values_under(LAUNCHER_REG_SUBKEY);
        removed.append(&mut launcher_removed);

        wipe_local_asobimo_cache();

        let msg = if removed.is_empty() {
            "Account login reset complete (0 stored values found — registry & cache clean)."
                .to_string()
        } else {
            format!(
                "Account login reset complete: wiped {} credential/session values & local Unity device cache.",
                removed.len()
            )
        };

        AccountResetResult {
            ok: true,
            process_killed,
            removed_values: removed,
            message: msg,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feature_labels_distinguish_activation_execution_and_partial_coverage() {
        use protocol::TOGGLE_UNLIMITED_MP as MP;
        let mut s = TelemetrySnapshot {
            valid: true,
            active_toggles: MP,
            ..Default::default()
        };
        assert_eq!(feature_status_label(&[], MP), "?");
        assert_eq!(feature_status_label(&[s], MP), "ARMED");
        s.mp_hit_calls = 58;
        assert_eq!(feature_status_label(&[s], MP), "NO BONUS CALLS");
        s.mp_getter_calls = 1;
        assert_eq!(feature_status_label(&[s], MP), "ARMED");
        s.observed_toggles = MP;
        assert_eq!(feature_status_label(&[s], MP), "CALLBACK SEEN");
        s.partial_toggles = MP;
        assert_eq!(feature_status_label(&[s], MP), "PARTIAL");
        let off = TelemetrySnapshot {
            valid: true,
            ..Default::default()
        };
        assert_eq!(feature_status_label(&[s, off], MP), "MIXED");
        assert_eq!(feature_status_label(&[off], MP), "OFF");
    }
    use std::time::{SystemTime, UNIX_EPOCH};

    fn make_temp_test_dir(tag: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = env::temp_dir().join(format!("toram_rust_test_{}_{}", tag, nanos));
        fs::create_dir_all(dir.join("src")).unwrap();
        dir
    }

    #[test]
    fn old_resource_sample_is_not_presented_as_current_game_state() {
        let mut snapshot = TelemetrySnapshot {
            valid: true,
            worker_state: 1,
            ..Default::default()
        };
        assert_eq!(worker_status_label(&snapshot, 100_000), "READY / NO SAMPLE");
        snapshot.resource_tick_ms = 100_000;
        assert_eq!(worker_status_label(&snapshot, 100_001), "READY");
        assert_eq!(
            worker_status_label(&snapshot, 479_000),
            "READY / STALE SAMPLE"
        );
        snapshot.valid = false;
        assert_eq!(worker_status_label(&snapshot, 100_001), "UNATTACHED");
    }

    #[test]
    fn test_window_title_is_neutral_v95() {
        assert_eq!(
            WINDOW_TITLE,
            format!("Game Companion (v{})", env!("CARGO_PKG_VERSION"))
        );
        assert!(!WINDOW_TITLE.to_ascii_lowercase().contains("toram"));
        assert!(!WINDOW_TITLE.to_ascii_lowercase().contains("cheat"));
    }

    #[test]
    fn telemetry_cache_requires_the_current_process() {
        let snapshot = TelemetrySnapshot {
            valid: true,
            pid: 42,
            now_hp: 397,
            max_hp: 397,
            now_mp: 319,
            max_mp: 319,
            hook_hits: 10,
            ..TelemetrySnapshot::default()
        };
        assert_eq!(matching_process_snapshot(42, snapshot), snapshot);
        for pid in [0, 43] {
            assert_eq!(
                matching_process_snapshot(pid, snapshot),
                TelemetrySnapshot::default()
            );
        }
        assert_eq!(
            matching_process_snapshot(
                42,
                TelemetrySnapshot {
                    valid: false,
                    ..snapshot
                }
            ),
            TelemetrySnapshot::default()
        );
    }

    #[test]
    fn test_parse_result_receipt_cases() {
        let log = "some noise\n[result] pid=4812 cmd=73 status=0\n";
        let r = parse_result_receipt(log, Some(73)).unwrap();
        assert_eq!(r.pid, 4812);
        assert_eq!(r.cmd, 73);
        assert_eq!(r.status, 0);

        assert!(parse_result_receipt(log, Some(72)).is_none());

        let err_log = "[result] pid=4812 cmd=44 status=-7";
        let r_err = parse_result_receipt(err_log, Some(44)).unwrap();
        assert_eq!(r_err.status, -7);
    }

    #[test]
    fn subprocess_receipt_rejects_foreign_pid_duplicate_and_embedded_log_lines() {
        assert!(parse_target_receipt("[result] pid=99 cmd=73 status=0", 73, 4812).is_none());
        assert!(parse_target_receipt("[result] pid=0 cmd=73 status=0", 73, 0).is_none());
        assert!(parse_target_receipt("log: [result] pid=4812 cmd=73 status=0", 73, 4812).is_none());
        assert!(parse_target_receipt("[result] pid=4812 cmd=72 status=0", 73, 4812).is_none());
        let duplicate = "[result] pid=4812 cmd=73 status=-8\n[result] pid=4812 cmd=73 status=0";
        assert!(parse_target_receipt(duplicate, 73, 4812).is_none());
        let receipt = parse_target_receipt("[result] pid=4812 cmd=73 status=-8", 73, 4812).unwrap();
        assert_eq!(receipt.status, -8);
        assert_eq!(
            parse_target_receipt("[result] pid=4812 cmd=73 status=0", 73, 4812)
                .unwrap()
                .status,
            0
        );
    }

    #[test]
    fn test_resolve_binary_pair_prefers_canonical_over_legacy() {
        let root = make_temp_test_dir("pair");
        let src = root.join("src");

        fs::write(src.join("wboot.exe"), b"stub").unwrap();
        fs::write(src.join("wcore.dll"), b"stub").unwrap();
        let (wb0, wc0) = resolve_binary_pair(&src).unwrap();
        assert_eq!(wb0.file_name().unwrap().to_str().unwrap(), "wboot.exe");
        assert_eq!(wc0.file_name().unwrap().to_str().unwrap(), "wcore.dll");

        fs::write(src.join("wboot92.exe"), b"stub").unwrap();
        fs::write(src.join("wcore92.dll"), b"stub").unwrap();
        fs::write(src.join("wboot99.exe"), b"stub").unwrap();

        let (wb, wc) = resolve_binary_pair(&src).unwrap();
        assert_eq!(wb.file_name().unwrap().to_str().unwrap(), "wboot.exe");
        assert_eq!(wc.file_name().unwrap().to_str().unwrap(), "wcore.dll");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn test_resolve_binary_pair_legacy_fallback_requires_a_complete_pair() {
        let root = make_temp_test_dir("legacy_pair");
        let src = root.join("src");
        fs::write(src.join("wboot.exe"), b"stub").unwrap();
        fs::write(src.join("wboot91.exe"), b"stub").unwrap();
        fs::write(src.join("wcore91.dll"), b"stub").unwrap();
        fs::write(src.join("wboot92.exe"), b"stub").unwrap();
        fs::write(src.join("wcore92.dll"), b"stub").unwrap();
        fs::write(src.join("wboot99.exe"), b"stub").unwrap();
        fs::write(src.join("wcore100.dll"), b"stub").unwrap();

        let (wb, wc) = resolve_binary_pair(&src).unwrap();
        assert_eq!(wb.file_name().unwrap().to_str().unwrap(), "wboot92.exe");
        assert_eq!(wc.file_name().unwrap().to_str().unwrap(), "wcore92.dll");

        fs::remove_file(src.join("wcore91.dll")).unwrap();
        fs::remove_file(src.join("wcore92.dll")).unwrap();
        assert!(resolve_binary_pair(&src).is_err());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn test_command_runner_validation_and_cmd44_block() {
        let root = make_temp_test_dir("runner");
        let runner = CommandRunner::new(root.clone(), None, None, None, None, 2_000);

        let r_neg = runner.run(-1);
        assert!(!r_neg.ok);

        let r_high = runner.run(protocol::CommandId::MAX + 1);
        assert!(!r_high.ok);

        let r_44 = runner.run(44);
        assert!(!r_44.ok);
        assert_eq!(r_44.remote_status, Some(-7));
        assert!(r_44.message.contains("blocked"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn test_account_reset_registry_value_filter() {
        assert!(should_delete_registry_value("AB0007_h123456"));
        assert!(should_delete_registry_value("ASB_Token_h999"));
        assert!(should_delete_registry_value("CountExecution_h111"));
        assert!(should_delete_registry_value("Language_h222"));
        assert!(should_delete_registry_value(
            "AsobimoOptionKey_Guest_h3614151626"
        ));
        assert!(should_delete_registry_value("ASOBIMO_ID_HASH_h103762478"));
        assert!(should_delete_registry_value("saveAppIdKey_h2630172831"));
        assert!(should_delete_registry_value(
            "unity.cloud_userid_h2665564582"
        ));
        assert!(should_delete_registry_value(
            "unity.player_sessionid_h1351336811"
        ));
        assert!(should_delete_registry_value(
            "unity.player_session_count_h922449978"
        ));
        assert!(should_delete_registry_value(
            "TellHistorySaveKey_h1716730432"
        ));
        assert!(should_delete_registry_value(
            "MarketNameSearchHistoryKey_h1501206205"
        ));
        assert!(should_delete_registry_value("ONCE_FLAG_1_h1085346495"));
        assert!(should_delete_registry_value("saveIsAccountUserKey_h9999"));
        assert!(should_delete_registry_value(
            "KEY_LAST_STARTUPTIME_h3124215816"
        ));
        assert!(should_delete_registry_value(
            "KEY_LastUpdatedFileTime_h1912302977"
        ));
        assert!(should_delete_registry_value(
            "KEY_LastPlayedTitle_h1867091202"
        ));

        assert!(!should_delete_registry_value(
            "Screenmanager Resolution Width_h182942802"
        ));
        assert!(!should_delete_registry_value(
            "SoundOptionPlaySetting_h1895703159"
        ));
        assert!(!should_delete_registry_value(
            "SystemOptionBattleSetting_h2293876959"
        ));
    }
}
