mod command;
mod failure;
mod metrics;
mod run;
use std::sync::atomic::{AtomicBool, Ordering};
static STOP: AtomicBool = AtomicBool::new(false);
type Result<T> = std::result::Result<T, String>;
fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}
extern "C" fn interrupt(_: i32) {
    STOP.store(true, Ordering::Relaxed);
}
fn main() {
    unsafe {
        libc::signal(libc::SIGINT, interrupt as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, interrupt as *const () as libc::sighandler_t);
    }
    if let Err(error) = dispatch() {
        eprintln!("bencher: {error}");
        std::process::exit(1);
    }
}
fn dispatch() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("__pause") {
        if args.len() != 4 {
            return Err("Invalid watchdog arguments".into());
        }
        return failure::pause(
            metrics::Identity {
                pid: args[1].parse().map_err(err)?,
                start: args[2].parse().map_err(err)?,
            },
            args[3].parse().map_err(err)?,
        );
    }
    let options = run::Options::parse(&args)?;
    match options.action.as_str() {
        "help" => {
            println!("Omarchy Widget Core Bencher\n\nrun --live [--mode all|idle|dashboard|crash|hang] [--counts 1,5,10]\n    [--seconds 20] [--cycles 3] [--core /path/to/omarchy-widget]\n    [--output results/unique-directory]\ncleanup --output results/previous-run [--core /path/to/omarchy-widget]\n\nLinux cgroup v2, active Core >=0.0.3 and a live Hyprland desktop required.\nFailure modes affect only this run's generated package. Hang resumes after 5s.\nSee README.md for measurement limits and recovery.");
            Ok(())
        }
        "cleanup" => run::recover(&options),
        "run" => run::execute(options),
        _ => Err("Unknown command; use --help".into()),
    }
}
