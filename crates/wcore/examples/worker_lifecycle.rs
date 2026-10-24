//! Isolated release-mode worker check. No game process or IL2CPP API is used.

use std::process::ExitCode;
use std::sync::atomic::Ordering;
use std::time::Duration;
use wcore::runtime::WorkerRuntime;

fn main() -> ExitCode {
    if !cfg!(panic = "unwind") {
        eprintln!("Worker panic recovery requires panic=unwind; abort profile rejected.");
        return ExitCode::FAILURE;
    }
    let runtime = Box::new(WorkerRuntime::new());
    runtime.set_job_hook(Some(|cmd| match cmd {
        1 => 42,
        2 => {
            std::thread::sleep(Duration::from_millis(150));
            7
        }
        3 => panic!("intentional release worker panic"),
        _ => 0,
    }));
    let warmup = runtime.dispatch_cmd(1);
    runtime.set_call_wait_ms(25);
    let timeout = runtime.dispatch_cmd(2);
    let busy = runtime.dispatch_cmd(1);
    std::thread::sleep(Duration::from_millis(200));
    runtime.set_call_wait_ms(1000);
    let recovered = runtime.dispatch_cmd(1);
    let panic_status = runtime.dispatch_cmd(3);
    let stopped = runtime.stop_worker(1000);
    let released = runtime.worker_thread.load(Ordering::SeqCst) == 0
        && runtime.ev_job.load(Ordering::SeqCst) == 0
        && runtime.ev_done.load(Ordering::SeqCst) == 0;
    let passed = (warmup, timeout, busy, recovered, panic_status, stopped)
        == (42, -3, -2, 42, -6, 0)
        && released;
    println!("warmup={warmup} timeout={timeout} busy={busy} recovered={recovered} panic={panic_status} stopped={stopped} handles_released={released} passed={passed}");
    if !released {
        // Never free an object still referenced by a native worker on failure.
        std::mem::forget(runtime);
    }
    if passed {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
