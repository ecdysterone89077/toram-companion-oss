//! CLI entrypoint for `wboot92.exe`.
//! Usage: `wboot.exe <cmd[,cmd...]: 0..93 or 100..115> [optional_path_to_wcore.dll]`
//! Stacked specs ("73+72+71", "73,72,71") run back-to-back across all clients.

use std::env;
use std::path::PathBuf;
use std::process::ExitCode;
use wboot::{dispatch_to_toram_expected, execute_wboot_cli_batch, log_both, ProcessIdentity};

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    let spec: String = if args.len() >= 2 {
        args[1].clone()
    } else {
        "1".to_string()
    };

    let explicit_dll = if args.len() >= 3 {
        Some(PathBuf::from(&args[2]))
    } else {
        None
    };

    let code = if args.len() > 3 {
        // A subprocess receipt must belong to precisely the parent's selected target.
        let expected = (|| -> Option<ProcessIdentity> {
            if args.len() != 9
                || args[3] != "--pid"
                || args[5] != "--created"
                || args[7] != "--image"
            {
                return None;
            }
            Some(ProcessIdentity {
                pid: args[4].parse().ok()?,
                created_at: args[6].parse().ok()?,
                image_path: args[8].clone(),
            })
        })();
        let (Some(expected), Ok(cmd)) = (expected, spec.parse::<i32>()) else {
            log_both("[err] Invalid target identity arguments");
            return ExitCode::FAILURE;
        };
        let outcome = dispatch_to_toram_expected(cmd, explicit_dll, true, Some(expected));
        if let Some(status) = outcome.remote_status {
            if outcome.pid != 0 {
                log_both(&format!(
                    "[result] pid={} cmd={} status={}",
                    outcome.pid, cmd, status
                ));
            }
        } else {
            log_both(&format!("[err] {}", outcome.message));
        }
        outcome.process_exit_code
    } else {
        execute_wboot_cli_batch(&spec, explicit_dll)
    };
    ExitCode::from((code.clamp(0, 255)) as u8)
}
