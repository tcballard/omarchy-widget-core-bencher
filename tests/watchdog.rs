use std::{
    fs,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};
fn matching_namespace() -> bool {
    let matched = fs::read_link("/proc/self")
        .unwrap()
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .parse::<u32>()
        .unwrap()
        == std::process::id();
    if std::env::var_os("CI").is_some() {
        assert!(
            matched,
            "CI must execute native signal tests in a matching PID namespace"
        );
    }
    if !matched {
        eprintln!("SKIP native signals: /proc is in another PID namespace");
    }
    matched
}
fn start(pid: u32) -> u64 {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    stat.rsplit_once(')')
        .unwrap()
        .1
        .split_whitespace()
        .nth(19)
        .unwrap()
        .parse()
        .unwrap()
}
fn state(pid: u32) -> String {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    stat.rsplit_once(')')
        .unwrap()
        .1
        .split_whitespace()
        .next()
        .unwrap()
        .into()
}
#[test]
fn watchdog_resumes_and_rejects_wrong_process_lifetime() {
    if !matching_namespace() {
        return;
    }
    let mut target = Command::new("sleep").arg("20").spawn().unwrap();
    let pid = target.id();
    let birth = start(pid);
    let mut watchdog = Command::new(env!("CARGO_BIN_EXE_omarchy-widget-core-bencher"))
        .args(["__pause", &pid.to_string(), &birth.to_string(), "1"])
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while state(pid) != "T" && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(state(pid), "T");
    assert!(watchdog.wait().unwrap().success());
    assert_ne!(state(pid), "T");
    let wrong = Command::new(env!("CARGO_BIN_EXE_omarchy-widget-core-bencher"))
        .args(["__pause", &pid.to_string(), &(birth + 1).to_string(), "1"])
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(!wrong.success());
    assert_ne!(state(pid), "T");
    target.kill().unwrap();
    target.wait().unwrap();
}
#[test]
fn interrupting_watchdog_resumes_immediately() {
    if !matching_namespace() {
        return;
    }
    let mut target = Command::new("sleep").arg("20").spawn().unwrap();
    let pid = target.id();
    let birth = start(pid);
    let mut watchdog = Command::new(env!("CARGO_BIN_EXE_omarchy-widget-core-bencher"))
        .args(["__pause", &pid.to_string(), &birth.to_string(), "8"])
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while state(pid) != "T" && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(state(pid), "T");
    unsafe {
        libc::kill(watchdog.id() as i32, libc::SIGTERM);
    }
    assert!(watchdog.wait().unwrap().success());
    assert_ne!(state(pid), "T");
    target.kill().unwrap();
    target.wait().unwrap();
}
#[test]
fn live_opt_in_is_required_before_any_core_command() {
    let output = Command::new(env!("CARGO_BIN_EXE_omarchy-widget-core-bencher"))
        .args(["run", "--core", "/does/not/exist"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--live"));
}
