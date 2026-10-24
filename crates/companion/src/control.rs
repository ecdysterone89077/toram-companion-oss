use super::{CommandResult, CommandRunner};
use protocol::{
    CommandId, TOGGLE_ALWAYS_CRIT, TOGGLE_ANTI_RIGIDITY, TOGGLE_INSTANT_ATK, TOGGLE_INVINCIBILITY,
    TOGGLE_UNLIMITED_MP,
};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use wboot::{
    dispatch_to_toram_expected, find_all_process_ids, is_toram_identity, process_identity,
    validate_command_ids, ProcessIdentity,
};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum TargetSelection {
    #[default]
    All,
    First,
    Pid(u32),
    Pinned(ProcessIdentity),
}
impl TargetSelection {
    pub fn resolve(&self) -> Result<Vec<ProcessIdentity>, String> {
        self.resolve_impl(false)
    }

    pub fn resolve_for_status(&self) -> Result<Vec<ProcessIdentity>, String> {
        self.resolve_impl(true)
    }

    fn resolve_impl(&self, allow_empty: bool) -> Result<Vec<ProcessIdentity>, String> {
        let pids = match self {
            Self::All => find_all_process_ids("ToramOnline.exe"),
            Self::First => find_all_process_ids("ToramOnline.exe")
                .into_iter()
                .take(1)
                .collect(),
            Self::Pid(pid) => vec![*pid],
            Self::Pinned(identity) => vec![identity.pid],
        };
        self.resolve_identities(pids, allow_empty, process_identity)
    }

    fn resolve_identities(
        &self,
        pids: Vec<u32>,
        allow_empty: bool,
        mut lookup: impl FnMut(u32) -> Option<ProcessIdentity>,
    ) -> Result<Vec<ProcessIdentity>, String> {
        if pids.is_empty() {
            if allow_empty && *self == Self::All {
                return Ok(Vec::new());
            }
            return Err("ToramOnline.exe is not running.".into());
        }
        pids.into_iter()
            .map(|pid| {
                let identity = lookup(pid).filter(is_toram_identity).ok_or_else(|| {
                    format!("Client {pid} exited or its identity is unavailable.")
                })?;
                if let Self::Pinned(expected) = self {
                    if expected != &identity {
                        return Err(format!("Client {pid} was replaced; refresh clients."));
                    }
                }
                Ok(identity)
            })
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CombatProfile {
    Native,
    Balanced,
    Burst,
    Recovery,
    /// Legacy preset. The variant and CLI alias remain for compatibility;
    /// the name does not establish gameplay correctness or acceptance risk.
    Safe,
}
impl CombatProfile {
    pub const ALL: [Self; 5] = [
        Self::Native,
        Self::Balanced,
        Self::Burst,
        Self::Recovery,
        Self::Safe,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Self::Native => "Native",
            Self::Balanced => "Balanced",
            Self::Burst => "Burst",
            Self::Recovery => "Recovery",
            Self::Safe => "Legacy",
        }
    }
    pub fn parse(name: &str) -> Result<Self, String> {
        if name.eq_ignore_ascii_case("safe") {
            return Ok(Self::Safe);
        }
        Self::ALL
            .into_iter()
            .find(|profile| profile.name().eq_ignore_ascii_case(name))
            .ok_or_else(|| {
                format!(
                    "Unknown profile '{name}'; use native, balanced, burst, recovery or legacy (alias: safe)."
                )
            })
    }
    pub fn plan(self) -> CommandPlan {
        let combat = TOGGLE_ALWAYS_CRIT | TOGGLE_INSTANT_ATK | TOGGLE_ANTI_RIGIDITY;
        let desired = match self {
            Self::Native => 0,
            Self::Recovery => combat | TOGGLE_UNLIMITED_MP,
            Self::Safe => combat | TOGGLE_INVINCIBILITY,
            _ => combat,
        };
        let mut commands = vec![93, if self == Self::Native { 90 } else { 89 }];
        commands.push(if self == Self::Native { 95 } else { 94 });
        commands.push(match self {
            Self::Native => 79,
            // Preserve the legacy request; the core applies its existing cap.
            Self::Safe => 76,
            Self::Burst => 85,
            _ => 83,
        });
        commands.push(if self == Self::Recovery { 86 } else { 91 });
        for index in 0..8 {
            let bit = 1 << index;
            commands.push(CommandId::set_toggle_command(bit, desired & bit != 0).unwrap());
        }
        CommandPlan {
            name: self.name().into(),
            commands,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandPlan {
    pub name: String,
    pub commands: Vec<i32>,
}
impl CommandPlan {
    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty()
            || self.name.len() > 64
            || self.name.chars().any(char::is_control)
        {
            return Err("Plan name must contain 1..64 bytes without control characters.".into());
        }
        validate_command_ids(&self.commands)
    }
    pub fn save(&self, path: &Path) -> Result<(), String> {
        self.validate()?;
        let commands = self
            .commands
            .iter()
            .map(i32::to_string)
            .collect::<Vec<_>>()
            .join("+");
        std::fs::write(
            path,
            format!("toram-plan-v1\nname={}\ncommands={commands}\n", self.name),
        )
        .map_err(|e| e.to_string())
    }
    pub fn load(path: &Path) -> Result<Self, String> {
        if std::fs::metadata(path).map_err(|e| e.to_string())?.len() > 8192 {
            return Err("Plan file exceeds 8 KiB.".into());
        }
        let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let lines: Vec<_> = text.lines().collect();
        if lines.len() != 3 || lines[0] != "toram-plan-v1" {
            return Err("Invalid plan file format.".into());
        }
        let name = lines[1]
            .strip_prefix("name=")
            .ok_or("Missing plan name.")?
            .to_owned();
        let commands = wboot::parse_cmd_spec(
            lines[2]
                .strip_prefix("commands=")
                .ok_or("Missing command list.")?,
        )?;
        let plan = Self { name, commands };
        plan.validate()?;
        Ok(plan)
    }
}

#[derive(Debug, Clone, Default)]
pub struct BatchResult {
    pub results: Vec<CommandResult>,
    pub planned: usize,
    pub target_count: usize,
    pub cancelled: bool,
    pub error: Option<String>,
}
impl BatchResult {
    pub fn ok(&self) -> bool {
        self.error.is_none()
            && !self.cancelled
            && self.planned != 0
            && self.results.len() == self.planned
            && self.results.iter().all(|result| result.ok)
    }
    pub fn summary(&self) -> String {
        let passed = self.results.iter().filter(|result| result.ok).count();
        let state = if self.cancelled {
            "Cancelled"
        } else if self.ok() {
            "Complete"
        } else {
            "Stopped"
        };
        let mut text = format!(
            "{state}: {passed}/{} deliveries succeeded across {} client(s).",
            self.planned, self.target_count
        );
        if let Some(error) = &self.error {
            text.push(' ');
            text.push_str(error);
        }
        text
    }
    pub fn details(&self) -> String {
        self.results
            .iter()
            .map(|result| result.message.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn run_captured_batch(
    commands: &[i32],
    targets: &[ProcessIdentity],
    cancelled: impl Fn() -> bool,
    mut dispatch: impl FnMut(i32, &ProcessIdentity) -> CommandResult,
) -> BatchResult {
    let mut batch = BatchResult {
        planned: commands.len() * targets.len(),
        target_count: targets.len(),
        ..Default::default()
    };
    for &cmd in commands {
        for target in targets {
            if cancelled() {
                batch.cancelled = true;
                return batch;
            }
            let mut result = dispatch(cmd, target);
            if result.ok
                && (result.remote_pid != Some(target.pid) || result.remote_status != Some(0))
            {
                result.ok = false;
                result.message = format!(
                    "Receipt mismatch for cmd {cmd}, selected PID {}.",
                    target.pid
                );
            }
            let failed = !result.ok;
            if failed {
                batch.error = Some(result.message.clone());
            }
            batch.results.push(result);
            if failed {
                return batch;
            }
        }
    }
    batch
}

impl CommandRunner {
    pub fn cancel(&self) {
        self.cancel_epoch.fetch_add(1, Ordering::AcqRel);
    }

    pub fn run_batch(&self, commands: &[i32], selection: &TargetSelection) -> BatchResult {
        if let Err(error) = validate_command_ids(commands) {
            return BatchResult {
                error: Some(error),
                ..Default::default()
            };
        }
        let cancel_epoch = self.cancel_epoch.load(Ordering::Acquire);
        if self
            .in_flight
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return BatchResult {
                error: Some("Another command or batch is in flight (busy).".into()),
                ..Default::default()
            };
        }
        struct Release<'a>(&'a AtomicBool);
        impl Drop for Release<'_> {
            fn drop(&mut self) {
                self.0.store(false, Ordering::Release);
            }
        }
        let _release = Release(&self.in_flight);
        let targets = match selection.resolve() {
            Ok(targets) => targets,
            Err(error) => {
                return BatchResult {
                    error: Some(error),
                    ..Default::default()
                }
            }
        };
        run_captured_batch(
            commands,
            &targets,
            || self.cancel_epoch.load(Ordering::Acquire) != cancel_epoch,
            |cmd, target| {
                if !self.use_in_process_dispatch {
                    return self.run_subprocess(cmd, target);
                }
                let dll = self
                    .wcore_path
                    .clone()
                    .or_else(|| self.resolve_paths().ok().map(|pair| pair.1));
                let outcome = dispatch_to_toram_expected(cmd, dll, true, Some(target.clone()));
                CommandResult {
                    ok: outcome.ok,
                    cmd,
                    exit_code: Some(outcome.process_exit_code),
                    remote_status: outcome.remote_status,
                    remote_pid: Some(target.pid),
                    message: outcome.message,
                    stdout: String::new(),
                    stderr: String::new(),
                    log_tail: super::read_tail_lines(&self.wcore_log_path, 40),
                }
            },
        )
    }

    pub fn run_on(&self, cmd: i32, selection: &TargetSelection) -> CommandResult {
        let batch = self.run_batch(&[cmd], selection);
        if batch.planned == 1 && batch.results.len() == 1 {
            return batch.results[0].clone();
        }
        CommandResult {
            ok: batch.ok(),
            cmd,
            exit_code: if batch.ok() {
                Some(0)
            } else {
                batch
                    .results
                    .iter()
                    .find(|result| !result.ok)
                    .and_then(|result| result.exit_code)
            },
            remote_status: if cmd == 44 {
                Some(-7)
            } else if batch.ok() {
                Some(0)
            } else {
                batch
                    .results
                    .iter()
                    .find(|result| !result.ok)
                    .and_then(|result| result.remote_status)
            },
            remote_pid: None,
            message: batch.summary(),
            stdout: String::new(),
            stderr: String::new(),
            log_tail: batch.details(),
        }
    }

    pub fn run(&self, cmd: i32) -> CommandResult {
        self.run_on(cmd, &TargetSelection::All)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn target(pid: u32) -> ProcessIdentity {
        ProcessIdentity {
            pid,
            created_at: 123,
            image_path: r"C:\Game\ToramOnline.exe".into(),
        }
    }
    #[test]
    fn status_only_allows_an_empty_result_when_no_clients_were_found() {
        assert_eq!(
            TargetSelection::All.resolve_identities(Vec::new(), true, |_| unreachable!()),
            Ok(Vec::new())
        );
        assert!(TargetSelection::All
            .resolve_identities(Vec::new(), false, |_| unreachable!())
            .is_err());
        assert!(TargetSelection::First
            .resolve_identities(Vec::new(), true, |_| unreachable!())
            .is_err());
        let error = TargetSelection::All
            .resolve_identities(vec![42, 43], true, |pid| (pid == 42).then(|| target(pid)))
            .unwrap_err();
        assert!(error.contains("43"));
        let expected = target(42);
        assert!(TargetSelection::Pinned(expected.clone())
            .resolve_identities(vec![42], true, |pid| {
                let mut replacement = target(pid);
                replacement.created_at += 1;
                Some(replacement)
            })
            .unwrap_err()
            .contains("replaced"));
    }
    fn receipt(cmd: i32, pid: u32) -> CommandResult {
        CommandResult {
            ok: true,
            cmd,
            exit_code: Some(0),
            remote_status: Some(0),
            remote_pid: Some(pid),
            message: format!("pid={pid} cmd={cmd}"),
            stdout: String::new(),
            stderr: String::new(),
            log_tail: String::new(),
        }
    }
    #[test]
    fn legacy_profile_accepts_existing_alias_and_names_round_trip() {
        for name in ["legacy", "LEGACY", "safe", "SaFe"] {
            assert_eq!(CombatProfile::parse(name).unwrap(), CombatProfile::Safe);
        }
        assert_eq!(CombatProfile::Safe.name(), "Legacy");
        for profile in CombatProfile::ALL {
            assert_eq!(CombatProfile::parse(profile.name()).unwrap(), profile);
        }
        assert!(CombatProfile::parse("unknown").is_err());
    }
    #[test]
    fn profiles_are_explicit_and_repeat_application_does_not_invert_features() {
        for profile in CombatProfile::ALL {
            let plan = profile.plan();
            plan.validate().unwrap();
            let mut toggles = 0xff;
            let mut apply = || {
                for &cmd in &plan.commands {
                    if let Some((_, bit, enabled)) = CommandId::decode_toggle_command(cmd) {
                        if enabled {
                            toggles |= bit;
                        } else {
                            toggles &= !bit;
                        }
                    }
                }
            };
            apply();
            apply();
            let expected = match profile {
                CombatProfile::Native => 0,
                CombatProfile::Recovery => {
                    TOGGLE_ALWAYS_CRIT
                        | TOGGLE_INSTANT_ATK
                        | TOGGLE_ANTI_RIGIDITY
                        | TOGGLE_UNLIMITED_MP
                }
                CombatProfile::Safe => {
                    TOGGLE_ALWAYS_CRIT
                        | TOGGLE_INSTANT_ATK
                        | TOGGLE_ANTI_RIGIDITY
                        | TOGGLE_INVINCIBILITY
                }
                _ => TOGGLE_ALWAYS_CRIT | TOGGLE_INSTANT_ATK | TOGGLE_ANTI_RIGIDITY,
            };
            assert_eq!(toggles, expected);
        }
    }
    #[test]
    fn batches_pin_targets_fail_fast_cancel_and_reject_false_success() {
        let cancel = AtomicBool::new(false);
        let targets = [target(42), target(43)];
        let mut calls = Vec::new();
        let result = run_captured_batch(
            &[83, 89],
            &targets,
            || cancel.load(Ordering::Acquire),
            |cmd, target| {
                calls.push((cmd, target.pid));
                let mut result = receipt(cmd, target.pid);
                if target.pid == 43 {
                    result.ok = false;
                }
                result
            },
        );
        assert!(!result.ok());
        assert_eq!(calls, vec![(83, 42), (83, 43)]);
        let result = run_captured_batch(
            &[83, 89],
            &targets,
            || cancel.load(Ordering::Acquire),
            |cmd, target| {
                cancel.store(true, Ordering::Release);
                receipt(cmd, target.pid)
            },
        );
        assert!(result.cancelled);
        assert_eq!(result.results.len(), 1);
        cancel.store(false, Ordering::Release);
        let result = run_captured_batch(
            &[83],
            &targets,
            || cancel.load(Ordering::Acquire),
            |cmd, _| receipt(cmd, 999),
        );
        assert!(!result.ok());
        assert_eq!(result.results.len(), 1);
    }
    #[test]
    fn plans_roundtrip_and_malformed_plans_fail_closed() {
        let path = std::env::temp_dir().join(format!("toram-plan-test-{}.txt", std::process::id()));
        let plan = CombatProfile::Burst.plan();
        plan.save(&path).unwrap();
        assert_eq!(CommandPlan::load(&path).unwrap(), plan);
        std::fs::write(&path, "toram-plan-v1\nname=bad\ncommands=73+44+72\n").unwrap();
        assert!(CommandPlan::load(&path).is_err());
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn invalid_whole_batch_and_busy_runner_fail_before_target_resolution() {
        let runner = CommandRunner::new(std::env::temp_dir(), None, None, None, None, 1000);
        assert!(runner
            .run_batch(&[73, 44, 72], &TargetSelection::Pid(0))
            .error
            .unwrap()
            .contains("44"));
        runner.in_flight.store(true, Ordering::Release);
        assert!(runner
            .run_batch(&[83, 89], &TargetSelection::Pid(0))
            .error
            .unwrap()
            .contains("busy"));
        assert!(runner.is_busy());
    }

    #[test]
    fn damage_mp_and_feedback_commands_pass_the_runner_gate() {
        let runner = CommandRunner::new(std::env::temp_dir(), None, None, None, None, 1000);
        for cmd in [81, 82, 83, 84, 85, 86, 87, 88, 89, 90, 91, 92, 93] {
            let batch = runner.run_batch(&[cmd], &TargetSelection::Pid(0));
            assert!(
                batch.error.unwrap().contains("Client 0"),
                "command {cmd} failed before target resolution"
            );
            assert!(!runner.is_busy());
        }
    }
}
