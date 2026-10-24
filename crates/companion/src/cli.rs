use super::*;
use std::path::Path;

fn selection(args: &[String]) -> Result<TargetSelection, String> {
    let mut result = TargetSelection::All;
    let mut selected = false;
    let mut index = 0;
    while index < args.len() {
        if selected {
            return Err("Choose only one of --all, --first or --pid.".into());
        }
        result = match args[index].as_str() {
            "--all" => TargetSelection::All,
            "--first" => TargetSelection::First,
            "--pid" => {
                index += 1;
                let pid = args
                    .get(index)
                    .ok_or("Missing --pid value.")?
                    .parse::<u32>()
                    .map_err(|_| "Invalid PID.")?;
                if pid == 0 {
                    return Err("PID must be greater than zero.".into());
                }
                TargetSelection::Pid(pid)
            }
            flag => return Err(format!("Unknown target option '{flag}'.")),
        };
        selected = true;
        index += 1;
    }
    Ok(result)
}

fn help() {
    println!(
        "Toram Companion v{}\n\
Usage:\n\
  companion.exe                            Open the control panel\n\
  companion.exe --clients                   List live game clients\n\
  companion.exe --status [TARGET]            Read authenticated telemetry\n\
  companion.exe --status-json [TARGET]       Export status as JSON to stdout\n\
  companion.exe --profile NAME [TARGET]      Apply native/balanced/burst/recovery/legacy\n\
  companion.exe --cmd \"83+89+111\" [TARGET]    Run one validated batch\n\
  companion.exe --save-plan FILE --profile NAME\n\
  companion.exe --save-plan FILE --cmd SPEC\n\
  companion.exe --load-plan FILE [TARGET]    Run a saved command plan\n\
TARGET: --all (default), --first, or --pid NUMBER.\n\
The historical profile name 'safe' remains an alias for 'legacy'.\n\
Batches stop at their first failure. Profile state commands are idempotent.\n\
89=feedback ON, 90=OFF, 91=native MP, 92=reset feedback counters.\n\
94=adaptive skill cap ON (requires feedback), 95=OFF (configured ratio cap).\n\
96..99 reserved and unavailable: pacing/target acquisition are not implemented.\n\
100..115 explicitly set feature OFF/ON (pairs in toggle-bit order).\n\
200..247 Faunas port feature OFF/ON (pairs in the window's Faunas row order).\n\
300..306 proration rung, 310..320 critical damage, 330..340 critical magic damage,\n\
350..356 equip element.\n\
MP/range acceptance still requires live observation.",
        env!("CARGO_PKG_VERSION")
    );
}

fn json_string(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn execute(args: &[String]) -> Result<(), String> {
    match args[0].as_str() {
        "--help" | "-h" if args.len() == 1 => {
            help();
            return Ok(());
        }
        "--reset-account" if args.len() == 1 => {
            let result = reset_toram_account_live();
            println!("{}", result.message);
            return if result.ok {
                Ok(())
            } else {
                Err(result.message)
            };
        }
        "--clients" if args.len() == 1 => {
            for pid in find_all_process_ids("ToramOnline.exe") {
                if let Some(identity) = process_identity(pid).filter(is_toram_identity) {
                    println!(
                        "PID {} | created {} | {}",
                        identity.pid, identity.created_at, identity.image_path
                    );
                }
            }
            return Ok(());
        }
        "--status" | "--status-json" => {
            let target = selection(&args[1..])?;
            let targets = target.resolve_for_status()?;
            if args[0] == "--status-json" {
                let rows:Vec<_>=targets.iter().map(|identity| {
                    let s=protocol::read_telemetry(identity.pid);
                    let age=if s.resource_tick_ms==0 {"null".into()}else{protocol::monotonic_ms().saturating_sub(s.resource_tick_ms).to_string()};
                    format!(concat!("{{\"pid\":{},\"image\":{},\"valid\":{},\"worker\":{},\"toggles\":{},",
                        "\"hp\":{},\"max_hp\":{},\"mp\":{},\"max_mp\":{},\"resource_age_ms\":{},",
                        "\"crit_request\":{},\"mp_bonus\":{},\"feedback\":{},\"accepted\":{},\"rejected\":{},",
                        "\"unknown\":{},\"skill_cap\":{},\"server_hp\":{},\"violation\":{},\"mp_hits\":{},\"mp_getters\":{},",
                        "\"observed_toggles\":{},\"partial_toggles\":{},\"adaptive_enabled\":{},",
                        "\"received_mp\":{},\"mp_sample_tick_ms\":{},\"wire_version\":{}}}"),
                        identity.pid,json_string(&identity.image_path),s.valid,s.worker_state,s.active_toggles,
                        s.now_hp,s.max_hp,s.now_mp,s.max_mp,age,s.crit_multiplier_pct,s.mp_ampr_bonus,
                        s.combat_monitor!=0,s.responses_accepted,s.responses_rejected,s.responses_unknown,
                        s.skill_cap,s.last_received_hp,s.last_violation,s.mp_hit_calls,s.mp_getter_calls,
                        s.observed_toggles,s.partial_toggles,s.adaptive_enabled!=0,s.received_mp,s.mp_sample_tick_ms,protocol::TELEMETRY_VERSION)
                }).collect();
                println!("[{}]", rows.join(","));
            } else {
                if targets.is_empty() {
                    println!("No accessible game clients.");
                }
                for identity in targets {
                    let s = protocol::read_telemetry(identity.pid);
                    println!("PID {} | attached={} | HP {}/{} MP {}/{} | crit={} AMPR={} | feedback={} accept={} reject={} unknown={} skill_cap={} | MP hits={} getters={}",
                        identity.pid,s.valid,s.now_hp,s.max_hp,s.now_mp,s.max_mp,s.crit_multiplier_pct,s.mp_ampr_bonus,
                        s.combat_monitor,s.responses_accepted,s.responses_rejected,s.responses_unknown,s.skill_cap,s.mp_hit_calls,s.mp_getter_calls);
                }
            }
            return Ok(());
        }
        "--save-plan" => {
            if args.len() != 4 {
                return Err("Use --save-plan FILE --cmd SPEC or --profile NAME.".into());
            }
            let plan = match args[2].as_str() {
                "--profile" => CombatProfile::parse(&args[3])?.plan(),
                "--cmd" => CommandPlan {
                    name: "Custom".into(),
                    commands: parse_cmd_spec(&args[3])?,
                },
                _ => return Err("Expected --cmd or --profile.".into()),
            };
            plan.save(Path::new(&args[1]))?;
            println!("Saved {} to {}", plan.name, args[1]);
            return Ok(());
        }
        _ => {}
    }
    if args.len() < 2 {
        return Err("Missing operation value. Use --help.".into());
    }
    let plan = match args[0].as_str() {
        "--cmd" => CommandPlan {
            name: "Custom".into(),
            commands: parse_cmd_spec(&args[1])?,
        },
        "--profile" => CombatProfile::parse(&args[1])?.plan(),
        "--load-plan" => CommandPlan::load(Path::new(&args[1]))?,
        op => return Err(format!("Unknown operation '{op}'. Use --help.")),
    };
    let target = selection(&args[2..])?;
    let runner = CommandRunner::new(resolve_workspace_dir(), None, None, None, None, 20_000);
    let result = runner.run_batch(&plan.commands, &target);
    println!("{}\n{}", plan.name, result.details());
    if result.ok() {
        println!("{}", result.summary());
        Ok(())
    } else {
        Err(result.summary())
    }
}

pub(super) fn run(args: &[String]) -> Option<u8> {
    if args.is_empty() {
        return None;
    }
    Some(match execute(args) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("{error}");
            1
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|s| s.to_string()).collect()
    }
    #[test]
    fn target_options_reject_ambiguous_or_missing_identities() {
        assert!(selection(&strings(&["--pid"])).is_err());
        assert!(selection(&strings(&["--pid", "0"])).is_err());
        assert!(selection(&strings(&["--all", "--pid", "42"])).is_err());
        assert_eq!(
            selection(&strings(&["--pid", "42"])).unwrap(),
            TargetSelection::Pid(42)
        );
    }
    #[test]
    fn malformed_command_batch_is_rejected_before_dispatch() {
        assert!(execute(&strings(&["--cmd", "73+44+72"]))
            .unwrap_err()
            .contains("44"));
        assert!(execute(&strings(&["--cmd", "73+oops+72"]))
            .unwrap_err()
            .contains("oops"));
        assert_eq!(json_string("a\"\\\n"), "\"a\\\"\\\\\\n\"");
    }
}
