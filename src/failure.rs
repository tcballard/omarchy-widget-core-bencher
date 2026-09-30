use crate::{
    err,
    metrics::{self, Identity},
    Result, STOP,
};
use std::{
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    sync::atomic::Ordering,
    thread,
    time::{Duration, Instant},
};

pub struct Target {
    fd: OwnedFd,
}
impl Target {
    pub fn open(id: &Identity) -> Result<Self> {
        metrics::check_proc_namespace()?;
        // pidfds bind signals to a process lifetime, including across PID reuse.
        let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, id.pid, 0) };
        if raw < 0 {
            return Err(format!("pidfd_open: {}", std::io::Error::last_os_error()));
        }
        let target = Self {
            fd: unsafe { OwnedFd::from_raw_fd(raw as i32) },
        };
        if !metrics::alive(id) {
            return Err("Renderer identity changed".into());
        }
        Ok(target)
    }
    pub fn signal(&self, signal: i32) -> Result<()> {
        let result = unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                self.fd.as_raw_fd(),
                signal,
                std::ptr::null::<libc::siginfo_t>(),
                0,
            )
        };
        if result < 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(())
    }
}
struct Resume(Target);
impl Drop for Resume {
    fn drop(&mut self) {
        let _ = self.0.signal(libc::SIGCONT);
    }
}
pub fn pause(id: Identity, seconds: u64) -> Result<()> {
    if !(1..=8).contains(&seconds) {
        return Err("Pause must be 1–8 seconds".into());
    }
    let target = Target::open(&id)?;
    target.signal(libc::SIGSTOP)?;
    let resume = Resume(target);
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(seconds) && !STOP.load(Ordering::Relaxed) {
        thread::sleep(Duration::from_millis(50));
    }
    resume.0.signal(libc::SIGCONT).map_err(err)
}
