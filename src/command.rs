use crate::{err, Result};
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};
use std::{
    fs,
    io::Read,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

// File-backed capture avoids deadlocking on a full pipe from a large Core snapshot.
pub fn output(program: &str, args: &[&str]) -> Result<String> {
    let path = std::env::temp_dir().join(format!(
        "widget-bencher-command-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    use std::os::unix::fs::OpenOptionsExt;
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .map_err(err)?;
    fs::remove_file(&path).map_err(err)?;
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(file.try_clone().map_err(err)?)
        .stderr(Stdio::null())
        .spawn()
        .map_err(err)?;
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().map_err(err)? {
            break status;
        }
        if started.elapsed() > Duration::from_secs(10) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "{program} {} timed out after 10s",
                args.first().unwrap_or(&"")
            ));
        }
        thread::sleep(Duration::from_millis(20));
    };
    use std::io::{Seek, SeekFrom};
    let mut file = file;
    file.seek(SeekFrom::Start(0)).map_err(err)?;
    let mut bytes = Vec::new();
    file.take(2 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(err)?;
    if bytes.len() > 2 * 1024 * 1024 {
        return Err("Command response exceeds 2 MiB".into());
    }
    let text = String::from_utf8(bytes).map_err(err)?;
    if !status.success() {
        return Err(format!(
            "{program} {} failed ({status}): {}",
            args.first().unwrap_or(&""),
            text.chars().take(500).collect::<String>()
        ));
    }
    Ok(text)
}
pub fn core(program: &str, args: &[&str]) -> Result<Value> {
    let value: Value = serde_json::from_str(&output(program, args)?).map_err(err)?;
    if let Some(error) = value["error"].as_str() {
        return Err(error.into());
    }
    Ok(value)
}
