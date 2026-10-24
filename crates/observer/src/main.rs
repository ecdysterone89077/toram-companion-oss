//! Pure Rust Combat Observer, Telemetry Sensor, and Geometry Engine.
//!
//! Replaces all legacy Python scripts (`toram_sensor.py`, `toram_geometry.py`,
//! `combat_observer.py`, `callback_observer.py`) with a native, zero-dependency 64-bit engine.

#![allow(
    non_camel_case_types,
    clippy::upper_case_acronyms,
    clippy::result_unit_err
)]

use core::ffi::c_void;
use std::env;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::Path;
use std::process::ExitCode;

// ---------------- Win32 Memory Introspection ----------------

type HANDLE = *mut c_void;
type BOOL = i32;
type DWORD = u32;

const PROCESS_VM_READ: DWORD = 0x0010;
const PROCESS_QUERY_INFORMATION: DWORD = 0x0400;

#[link(name = "kernel32")]
extern "system" {
    fn OpenProcess(dwDesiredAccess: DWORD, bInheritHandle: BOOL, dwProcessId: DWORD) -> HANDLE;
    fn CloseHandle(hObject: HANDLE) -> BOOL;
    fn ReadProcessMemory(
        hProcess: HANDLE,
        lpBaseAddress: *const c_void,
        lpBuffer: *mut c_void,
        nSize: usize,
        lpNumberOfBytesRead: *mut usize,
    ) -> BOOL;
}

pub struct ProcessMemory {
    handle: HANDLE,
    pub pid: u32,
}

impl ProcessMemory {
    pub fn open(pid: u32) -> Result<Self, String> {
        let handle = unsafe { OpenProcess(PROCESS_VM_READ | PROCESS_QUERY_INFORMATION, 0, pid) };
        if handle.is_null() {
            Err(format!(
                "Failed to open process PID={pid} with PROCESS_VM_READ"
            ))
        } else {
            Ok(Self { handle, pid })
        }
    }

    pub fn read<T: Copy + Default>(&self, addr: usize) -> Result<T, ()> {
        let mut val = T::default();
        let mut read = 0usize;
        let ok = unsafe {
            ReadProcessMemory(
                self.handle,
                addr as *const c_void,
                &mut val as *mut T as *mut c_void,
                std::mem::size_of::<T>(),
                &mut read,
            )
        };
        if ok != 0 && read == std::mem::size_of::<T>() {
            Ok(val)
        } else {
            Err(())
        }
    }

    pub fn read_bytes(&self, addr: usize, len: usize) -> Result<Vec<u8>, ()> {
        let mut buf = vec![0u8; len];
        let mut read = 0usize;
        let ok = unsafe {
            ReadProcessMemory(
                self.handle,
                addr as *const c_void,
                buf.as_mut_ptr() as *mut c_void,
                len,
                &mut read,
            )
        };
        if ok != 0 {
            buf.truncate(read);
            Ok(buf)
        } else {
            Err(())
        }
    }
}

impl Drop for ProcessMemory {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe {
                CloseHandle(self.handle);
            }
        }
    }
}

// ---------------- 3D Geometry & Distance Engine ----------------

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Vec3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl Vec3 {
    pub const fn new(x: f32, y: f32, z: f32) -> Self {
        Self { x, y, z }
    }

    #[inline]
    pub fn distance_3d(self, other: Self) -> f32 {
        let dx = self.x - other.x;
        let dy = self.y - other.y;
        let dz = self.z - other.z;
        (dx * dx + dy * dy + dz * dz).sqrt()
    }

    #[inline]
    pub fn horizontal_distance(self, other: Self) -> f32 {
        let dx = self.x - other.x;
        let dz = self.z - other.z;
        (dx * dx + dz * dz).sqrt()
    }
}

/// Convert internal Unity horizontal units to Toram HUD displayed metres: `ceil(distance * 0.5)`.
#[inline]
pub fn hud_metres(distance: f32) -> f32 {
    (distance * 0.5).ceil()
}

// ---------------- Combat State Machine ----------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CombatState {
    Idle,
    Approaching,
    InCombat,
    MobDefeated,
}

pub struct CombatObserver {
    pub state: CombatState,
    pub mob_hp: i32,
    pub max_hp: i32,
    pub last_damage: i32,
}

impl CombatObserver {
    pub fn new(mob_hp: i32, max_hp: i32) -> Self {
        Self {
            state: CombatState::Idle,
            mob_hp,
            max_hp,
            last_damage: 0,
        }
    }

    pub fn update(&mut self, current_hp: i32, distance: f32) -> CombatState {
        let damaged = current_hp < self.mob_hp;
        if damaged {
            self.last_damage = self.mob_hp - current_hp;
        }
        self.mob_hp = current_hp;

        self.state = match self.state {
            CombatState::Idle => {
                if current_hp <= 0 && self.max_hp > 0 {
                    CombatState::MobDefeated
                } else if distance <= 30.0 && damaged {
                    CombatState::InCombat
                } else if distance <= 30.0 {
                    CombatState::Approaching
                } else {
                    CombatState::Idle
                }
            }
            CombatState::Approaching => {
                if current_hp <= 0 && self.max_hp > 0 {
                    CombatState::MobDefeated
                } else if damaged || distance <= 4.0 {
                    CombatState::InCombat
                } else if distance > 35.0 {
                    CombatState::Idle
                } else {
                    CombatState::Approaching
                }
            }
            CombatState::InCombat => {
                if current_hp <= 0 {
                    CombatState::MobDefeated
                } else if distance > 40.0 {
                    CombatState::Idle
                } else {
                    CombatState::InCombat
                }
            }
            CombatState::MobDefeated => {
                if current_hp > 0 {
                    CombatState::Idle
                } else {
                    CombatState::MobDefeated
                }
            }
        };

        self.state
    }
}

// ---------------- JSONL Streaming Recorder ----------------

pub struct JsonlRecorder {
    writer: BufWriter<File>,
}

impl JsonlRecorder {
    pub fn create(path: &Path) -> Result<Self, String> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|e| format!("Failed to open {}: {e}", path.display()))?;
        Ok(Self {
            writer: BufWriter::with_capacity(32 * 1024, file),
        })
    }

    pub fn write_sample(&mut self, json_line: &str) -> Result<(), String> {
        self.writer
            .write_all(json_line.as_bytes())
            .map_err(|e| format!("Write failed: {e}"))?;
        self.writer
            .write_all(b"\n")
            .map_err(|e| format!("Write newline failed: {e}"))?;
        Ok(())
    }

    pub fn flush(&mut self) {
        let _ = self.writer.flush();
    }
}

// ---------------- Log Extraction ----------------

/// Prefixes the worker writes its observation records under.
///
/// The action capture and the response capture are separate record streams and
/// both matter: the action stream carries the phase, the feature mask and the
/// distance, and the response stream carries the server's verdict, the
/// received HP, the allowance, the violation and the received proration.
const OBSERVE_PREFIX: &str = "[combat-observe] ";
const RESPONSE_PREFIX: &str = "[combat-response] ";
/// Diagnostic prefixes worth watching during a live run.
const WATCH_PREFIXES: [&str; 5] = [
    "[speed68]",
    "[mpdiag]",
    "[observe80]",
    "[combat-observe]",
    "[combat-response]",
];

/// Default worker log location, matching the companion's own default.
fn default_log_path() -> std::path::PathBuf {
    env::temp_dir().join("wcore.log")
}

/// Extracts the JSON object from a prefixed log line.
///
/// The payload is a flat JSON object whose string values never contain a
/// brace, but the scan is string-aware anyway so a stray brace inside a
/// value cannot end the object early.
fn extract_json_object(line: &str) -> Option<&str> {
    let start = line.find('{')?;
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escaped = false;
    for (index, byte) in line.as_bytes().iter().enumerate().skip(start) {
        if in_string {
            if escaped {
                escaped = false;
            } else if *byte == b'\\' {
                escaped = true;
            } else if *byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&line[start..=index]);
                }
            }
            _ => {}
        }
    }
    None
}

/// Reads the worker log as text.
///
/// The log is appended by the worker and can carry bytes that are not valid
/// UTF-8. Refusing to read the log at all would hide every record in it, so
/// the bytes are decoded lossily instead.
fn read_log_text(path: &std::path::Path) -> std::io::Result<String> {
    let bytes = std::fs::read(path)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Pulls the observation records out of the worker log into a JSONL capture
/// file, and reports what the capture contains.
fn run_capture(args: &[String]) -> Result<(), ()> {
    let mut log_path = default_log_path();
    let mut out_path = std::path::PathBuf::from("capture.jsonl");
    let mut index = 0usize;
    while index < args.len() {
        match args[index].as_str() {
            "--log" if index + 1 < args.len() => {
                log_path = std::path::PathBuf::from(&args[index + 1]);
                index += 2;
            }
            "--out" if index + 1 < args.len() => {
                out_path = std::path::PathBuf::from(&args[index + 1]);
                index += 2;
            }
            other => {
                eprintln!("[observer] Unknown capture argument '{other}'.");
                return Err(());
            }
        }
    }
    let text = match read_log_text(&log_path) {
        Ok(text) => text,
        Err(error) => {
            eprintln!("[observer] Cannot read {}: {error}", log_path.display());
            return Err(());
        }
    };
    let mut recorder = JsonlRecorder::create(&out_path).map_err(|error| {
        eprintln!("[observer] {error}");
    })?;
    let mut events = 0usize;
    let mut phases: Vec<u32> = Vec::new();
    let mut distance_samples = 0usize;
    let mut proration_samples = 0usize;
    let mut begin_seen = false;
    let mut end_seen = false;
    for line in text.lines() {
        let payload = line
            .strip_prefix(OBSERVE_PREFIX)
            .or_else(|| line.strip_prefix(RESPONSE_PREFIX));
        let Some(payload) = payload else {
            continue;
        };
        let Some(json) = extract_json_object(payload) else {
            continue;
        };
        if json.contains("\"kind\":\"begin\"") {
            begin_seen = true;
        } else if json.contains("\"kind\":\"end\"") {
            end_seen = true;
        } else {
            events += 1;
            if json.contains("\"distance_source\":1") {
                distance_samples += 1;
            }
            if json.contains("\"normal_exp\":") {
                proration_samples += 1;
            }
            if let Some(value) = field_value(json, "\"phase\":") {
                if let Ok(phase) = value.trim().parse::<u32>() {
                    if !phases.contains(&phase) {
                        phases.push(phase);
                    }
                }
            }
        }
        recorder.write_sample(json).map_err(|error| {
            eprintln!("[observer] {error}");
        })?;
    }
    recorder.flush();
    phases.sort_unstable();
    println!("[observer] Capture written to {}", out_path.display());
    println!("  events:          {events}");
    println!("  distance samples:{distance_samples:>10}");
    println!("  proration samples:{proration_samples:>10}");
    println!(
        "  phases:          {}",
        if phases.is_empty() {
            "none".to_owned()
        } else {
            phases
                .iter()
                .map(|phase| phase.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        }
    );
    println!("  begin record:    {begin_seen}");
    println!("  end record:      {end_seen}");
    if !begin_seen {
        println!("  note: no begin record, the capture may predate this build");
    }
    if begin_seen && !end_seen {
        println!("  note: the capture is still running, so its records are in memory and");
        println!("        reach the log only when the capture is stopped. Stop it and");
        println!("        run this again to pick the records up.");
    }
    Ok(())
}

/// Reads an integer field from a flat JSON object.
fn field_value<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let start = json.find(key)? + key.len();
    let rest = &json[start..];
    let end = rest
        .find(|c: char| !c.is_ascii_digit() && c != '-')
        .unwrap_or(rest.len());
    Some(&rest[..end])
}

/// Follows the worker log for a bounded time and prints the diagnostic lines
/// it produces, so a live run can be watched without the companion window.
fn run_watch(args: &[String]) -> Result<(), ()> {
    let mut log_path = default_log_path();
    let mut seconds = 60u64;
    let mut index = 0usize;
    while index < args.len() {
        match args[index].as_str() {
            "--log" if index + 1 < args.len() => {
                log_path = std::path::PathBuf::from(&args[index + 1]);
                index += 2;
            }
            "--seconds" if index + 1 < args.len() => {
                seconds = args[index + 1].parse().unwrap_or(seconds);
                index += 2;
            }
            other => {
                eprintln!("[observer] Unknown watch argument '{other}'.");
                return Err(());
            }
        }
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(seconds);
    let mut offset = std::fs::metadata(&log_path)
        .map(|meta| meta.len() as usize)
        .unwrap_or(0);
    println!("[observer] Watching {} for {seconds}s", log_path.display());
    let mut pending = String::new();
    while std::time::Instant::now() < deadline {
        if let Ok(text) = read_log_text(&log_path) {
            if text.len() > offset {
                pending.push_str(&text[offset..]);
                offset = text.len();
                while let Some(index) = pending.find('\n') {
                    let rest = pending.split_off(index + 1);
                    let line = pending.trim_end().to_owned();
                    pending = rest;
                    if WATCH_PREFIXES.iter().any(|prefix| line.starts_with(prefix)) {
                        println!("{line}");
                    }
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    Ok(())
}

// ---------------- CLI Entrypoint ----------------

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let subcommand = args.first().map(|s| s.as_str()).unwrap_or("help");

    let res = match subcommand {
        "geometry" => run_geometry_demo(),
        "live" | "sense" => run_live_telemetry(&args[1..]),
        "capture" => run_capture(&args[1..]),
        "watch" => run_watch(&args[1..]),
        "help" | "-h" | "--help" => {
            println!(
                r#"toram-companion observer — 100% Pure Rust Combat Observer & Telemetry Engine

Usage: cargo run -p observer -- <command> [options]

Commands:
    geometry                Run 3D vector distance & HUD metre calculation verification
    live [--pid N]          Read and display real-time combat telemetry and IPC state
    capture [--log PATH]    Extract the observation records from the worker log into a
         [--out PATH]       JSONL capture file, and report the phases and distances
    watch [--log PATH]      Follow the worker log and print the diagnostic lines
         [--seconds N]      it produces, for a bounded time (default 60s)
    help                    Show this help message
"#
            );
            Ok(())
        }
        unknown => {
            eprintln!("[observer] Unknown command: '{unknown}'. Use 'help' for usage.");
            Err(())
        }
    };

    if res.is_ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn run_geometry_demo() -> Result<(), ()> {
    println!("[observer] Running 3D geometry engine verification...");
    let actor = Vec3::new(0.0, 1.0, 0.0);
    let target = Vec3::new(3.0, 1.0, 4.0);

    let dist_3d = actor.distance_3d(target);
    let dist_horiz = actor.horizontal_distance(target);
    let hud = hud_metres(dist_horiz);

    println!("  Actor:       {:?}", actor);
    println!("  Target:      {:?}", target);
    println!("  3D Distance: {:.3} units", dist_3d);
    println!("  Horizontal:  {:.3} units", dist_horiz);
    println!("  HUD Display: {:.0} m", hud);

    assert!((dist_horiz - 5.0).abs() < 1e-4);
    assert_eq!(hud, 3.0);
    println!("[observer] Geometry checks passed.");
    Ok(())
}

fn run_live_telemetry(args: &[String]) -> Result<(), ()> {
    let mut pid: Option<u32> = None;
    for i in 0..args.len() {
        if args[i] == "--pid" && i + 1 < args.len() {
            pid = args[i + 1].parse().ok();
        }
    }

    let target_pid = pid.unwrap_or_else(|| wboot::find_process_id("ToramOnline.exe"));
    let snap = protocol::read_telemetry(target_pid);
    if snap.valid {
        println!("[observer] Live Telemetry Snapshot:");
        println!("  Valid:          {}", snap.valid);
        println!("  PID:            {}", snap.pid);
        println!("  Worker State:   {}", snap.worker_state);
        println!("  HP:             {}/{}", snap.now_hp, snap.max_hp);
        println!("  MP:             {}/{}", snap.now_mp, snap.max_mp);
        println!(
            "  Received MP:    {} (sample tick {})",
            snap.received_mp, snap.mp_sample_tick_ms
        );
        println!("  Callback Seen:  {:#010b}", snap.observed_toggles);
        println!("  Partial Setup:  {:#010b}", snap.partial_toggles);
        println!(
            "  Adaptive Cap:   enabled={} cap=+{}",
            snap.adaptive_enabled != 0,
            snap.skill_cap
        );
        println!("  Active Toggles: {:#010b}", snap.active_toggles);
        println!("  Hook Hits:      {}", snap.hook_hits);
        println!("  Last Damage:    {}", snap.last_damage);
        println!("  Cmd Seq:        {}", snap.cmd_seq);
    } else {
        let diagnosis = protocol::diagnose_telemetry(target_pid);
        let mut block = String::new();
        block.push_str("[observer] No authenticated telemetry snapshot for this process.\n");
        block.push_str(&format!("  PID:            {}\n", diagnosis.pid));
        match diagnosis.created_at {
            Some(created_at) => block.push_str(&format!("  Creation:       {created_at}\n")),
            None => block.push_str("  Creation:       unreadable\n"),
        }
        block.push_str(&format!("  Stage:          {}\n", diagnosis.stage.as_str()));
        let (meaning, action) = match diagnosis.stage {
            protocol::TelemetryStage::NoProcess => (
                "ToramOnline.exe is not running (or the PID is stale).",
                "launch the client, then rerun.",
            ),
            protocol::TelemetryStage::NoLifetime => (
                "the process exited between handle open and identity read.",
                "rerun; if it repeats, the client is crashing on start.",
            ),
            protocol::TelemetryStage::NoMapping => (
                "the client is alive but no module published telemetry.",
                "confirm the core module is loaded into this exact PID.",
            ),
            protocol::TelemetryStage::NoClaim | protocol::TelemetryStage::NoCustody => (
                "a mapping exists but custody is not proved. Do not trust it.",
                "treat the data as foreign; start over with a fresh process.",
            ),
            protocol::TelemetryStage::IdentityMismatch | protocol::TelemetryStage::Restarted => (
                "the mapping belongs to a different process lifetime.",
                "the client restarted; discard the old mapping and re-read.",
            ),
            protocol::TelemetryStage::SnapshotUnset => (
                "identity is proved but the module has published no state.",
                "check that the worker initialised, then read again.",
            ),
            protocol::TelemetryStage::Ok => ("telemetry is current.", "none."),
        };
        block.push_str(&format!("  Meaning:        {meaning}\n"));
        block.push_str(&format!("  Action:         {action}\n"));
        // One write: PowerShell merges stderr into the pipeline and will otherwise
        // split the block around the child probe below.
        print!("{block}");
        let _ = std::io::stdout().flush();
        if let Some(target_pid) = pid {
            println!("[observer] Opening target process PID={target_pid} directly via Win32...");
            match ProcessMemory::open(target_pid) {
                Ok(_proc) => println!("[observer] Successfully attached to PID={target_pid}."),
                Err(e) => eprintln!("[observer] {e}"),
            }
        }
    }
    Ok(())
}

// ---------------- Native Unit Tests ----------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vec3_distance_and_horizontal() {
        let a = Vec3::new(10.0, 5.0, 20.0);
        let b = Vec3::new(13.0, 5.0, 24.0);

        assert_eq!(a.horizontal_distance(b), 5.0);
        assert_eq!(a.distance_3d(b), 5.0);

        let c = Vec3::new(13.0, 9.0, 24.0);
        assert_eq!(a.horizontal_distance(c), 5.0);
        assert!((a.distance_3d(c) - (25.0f32 + 16.0).sqrt()).abs() < 1e-5);
    }

    #[test]
    fn test_hud_metres_formula() {
        assert_eq!(hud_metres(0.0), 0.0);
        assert_eq!(hud_metres(4.0), 2.0);
        assert_eq!(hud_metres(5.0), 3.0);
        assert_eq!(hud_metres(8.0), 4.0);
        assert_eq!(hud_metres(11.0), 6.0);
        assert_eq!(hud_metres(30.0), 15.0);
    }

    #[test]
    fn capture_extracts_the_json_object_from_a_prefixed_line() {
        let line = "[combat-observe] {\"kind\":\"event\",\"phase\":3,\"distance_source\":1}";
        let payload = line.strip_prefix(OBSERVE_PREFIX).expect("prefix");
        let json = extract_json_object(payload).expect("json object");
        assert_eq!(
            json,
            "{\"kind\":\"event\",\"phase\":3,\"distance_source\":1}"
        );
        assert_eq!(field_value(json, "\"phase\":"), Some("3"));
        assert_eq!(field_value(json, "\"distance_source\":"), Some("1"));
        assert_eq!(field_value(json, "\"missing\":"), None);
    }

    #[test]
    fn capture_reads_both_the_action_and_the_response_streams() {
        // The action stream carries the phase, the feature mask and the
        // distance. The response stream carries the server's verdict, the
        // received HP, the allowance, the violation and the received
        // proration. A capture needs both.
        let action = "[combat-observe] {\"kind\":\"event\",\"phase\":2,\"distance_source\":1}";
        let response = "[combat-response] {\"kind\":\"mob\",\"normal_exp\":40,\"physical_exp\":7,\"magic_exp\":9}";
        let action_payload = action.strip_prefix(OBSERVE_PREFIX).expect("action prefix");
        let response_payload = response
            .strip_prefix(RESPONSE_PREFIX)
            .expect("response prefix");
        assert!(extract_json_object(action_payload).is_some());
        assert!(extract_json_object(response_payload).is_some());
        assert!(action.contains("\"distance_source\":1"));
        assert!(response.contains("\"normal_exp\":"));
        // Both streams are watched, and neither is matched by accident.
        for line in [action, response] {
            assert!(
                WATCH_PREFIXES.iter().any(|prefix| line.starts_with(prefix)),
                "the capture must pick up {line}"
            );
        }
    }

    #[test]
    fn capture_ignores_truncated_and_brace_free_lines() {
        assert_eq!(extract_json_object("{\"kind\":\"event\""), None);
        assert_eq!(extract_json_object("no json here"), None);
        // A brace inside a string value must not end the object early.
        let json = extract_json_object("{\"a\":\"}\",\"b\":2}").expect("json object");
        assert_eq!(json, "{\"a\":\"}\",\"b\":2}");
    }

    #[test]
    fn test_combat_observer_state_machine() {
        let mut obs = CombatObserver::new(100, 100);
        assert_eq!(obs.state, CombatState::Idle);

        // Approach
        assert_eq!(obs.update(100, 25.0), CombatState::Approaching);

        // Engaged / InCombat on damage
        assert_eq!(obs.update(90, 3.5), CombatState::InCombat);
        assert_eq!(obs.last_damage, 10);

        // Continues in combat
        assert_eq!(obs.update(50, 3.0), CombatState::InCombat);
        assert_eq!(obs.last_damage, 40);

        // Defeated
        assert_eq!(obs.update(0, 3.0), CombatState::MobDefeated);

        // Respawn / New Mob
        assert_eq!(obs.update(100, 50.0), CombatState::Idle);
    }
}
