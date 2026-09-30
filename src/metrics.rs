use crate::{err, Result};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Identity {
    pub pid: i32,
    pub start: u64,
}
pub fn check_proc_namespace() -> Result<()> {
    let visible = fs::read_link("/proc/self").map_err(err)?;
    if visible
        .file_name()
        .and_then(|s| s.to_str())
        .and_then(|s| s.parse::<u32>().ok())
        != Some(std::process::id())
    {
        return Err("/proc uses a different PID namespace; run on the host desktop".into());
    }
    Ok(())
}
pub fn identity_at(proc: &Path, pid: i32) -> Result<Identity> {
    let stat = fs::read_to_string(proc.join(pid.to_string()).join("stat")).map_err(err)?;
    let tail = stat.rsplit_once(')').ok_or("Invalid proc stat")?.1;
    let start = tail
        .split_whitespace()
        .nth(19)
        .ok_or("Missing process start time")?
        .parse()
        .map_err(err)?;
    Ok(Identity { pid, start })
}
pub fn identity(pid: i32) -> Result<Identity> {
    check_proc_namespace()?;
    identity_at(Path::new("/proc"), pid)
}
pub fn alive(id: &Identity) -> bool {
    identity(id.pid).ok().as_ref() == Some(id)
}
pub fn stats(text: &str) -> Result<BTreeMap<String, u64>> {
    text.lines()
        .map(|line| {
            let mut parts = line.split_whitespace();
            let key = parts.next().ok_or("Missing stat key")?;
            let value = parts
                .next()
                .ok_or("Missing stat value")?
                .parse()
                .map_err(err)?;
            Ok((key.into(), value))
        })
        .collect()
}
pub fn group_for(pid: i32) -> Result<PathBuf> {
    let text = fs::read_to_string(format!("/proc/{pid}/cgroup")).map_err(err)?;
    let relative = text
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .ok_or("cgroup v2 required")?;
    let path = Path::new(relative);
    if !path.is_absolute()
        || path
            .components()
            .any(|p| matches!(p, std::path::Component::ParentDir))
    {
        return Err("Unsafe cgroup path".into());
    }
    if relative == "/" {
        return Err("Refusing root cgroup".into());
    }
    Ok(Path::new("/sys/fs/cgroup").join(relative.trim_start_matches('/')))
}
pub fn pids(group: &Path) -> Result<Vec<i32>> {
    let mut all = BTreeSet::new();
    fn visit(path: &Path, all: &mut BTreeSet<i32>) -> Result<()> {
        let text = fs::read_to_string(path.join("cgroup.procs")).map_err(err)?;
        for pid in text.split_whitespace() {
            all.insert(pid.parse().map_err(err)?);
        }
        for entry in fs::read_dir(path).map_err(err)? {
            let entry = entry.map_err(err)?;
            if entry.file_type().map_err(err)?.is_dir() {
                visit(&entry.path(), all)?;
            }
        }
        Ok(())
    }
    visit(group, &mut all)?;
    Ok(all.into_iter().collect())
}
#[derive(Clone, Debug)]
pub struct Group {
    pub label: String,
    pub path: PathBuf,
}

// Map production island workers by exact argv source, never substring matches or all qs processes.
pub fn discover(sources: &BTreeMap<String, String>, host_pid: i32) -> Result<Vec<Group>> {
    let mut groups = discover_owned(sources)?;
    groups.push(Group {
        label: "core".into(),
        path: group_for(host_pid)?,
    });
    validate_groups(&groups)?;
    Ok(groups)
}
pub fn discover_owned(sources: &BTreeMap<String, String>) -> Result<Vec<Group>> {
    let mut groups = Vec::new();
    for entry in fs::read_dir("/proc").map_err(err)? {
        let entry = entry.map_err(err)?;
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<i32>() else {
            continue;
        };
        let Ok(bytes) = fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        let args: Vec<_> = bytes
            .split(|b| *b == 0)
            .filter(|b| !b.is_empty())
            .map(|b| String::from_utf8_lossy(b).into_owned())
            .collect();
        if args.len() != 5 || args[1] != "island-worker" {
            continue;
        }
        if let Some((label, _)) = sources.iter().find(|(_, source)| **source == args[3]) {
            if let Ok(path) = group_for(pid) {
                groups.push(Group {
                    label: label.clone(),
                    path,
                });
            }
        }
    }
    groups.sort_by(|a, b| a.label.cmp(&b.label));
    groups.dedup_by(|a, b| a.label == b.label && a.path == b.path);
    validate_groups(&groups)?;
    Ok(groups)
}
fn validate_groups(groups: &[Group]) -> Result<()> {
    for (i, a) in groups.iter().enumerate() {
        for b in &groups[i + 1..] {
            if a.path.starts_with(&b.path) || b.path.starts_with(&a.path) {
                return Err("Overlapping cgroups would double-count resources".into());
            }
        }
    }
    Ok(())
}
pub fn sample(group: &Group) -> Result<Value> {
    let cpu = stats(&fs::read_to_string(group.path.join("cpu.stat")).map_err(err)?)?;
    let usage = *cpu.get("usage_usec").ok_or("Missing CPU usage")?;
    let memory: u64 = fs::read_to_string(group.path.join("memory.current"))
        .map_err(err)?
        .trim()
        .parse()
        .map_err(err)?;
    let events = stats(&fs::read_to_string(group.path.join("memory.events")).map_err(err)?)?;
    let processes: Vec<_> = pids(&group.path)?
        .into_iter()
        .filter_map(|pid| identity(pid).ok())
        .map(|id| json!({"pid":id.pid,"startTicks":id.start}))
        .collect();
    Ok(
        json!({"label":group.label,"cgroup":group.path,"cpuUsageUsec":usage,"memoryBytes":memory,"cpuThrottledUsec":cpu.get("throttled_usec"),"oomKills":events.get("oom_kill"),"processes":processes}),
    )
}
pub fn renderer(group: &Group) -> Result<Identity> {
    let mut found = Vec::new();
    for pid in pids(&group.path)? {
        let Ok(exe) = fs::read_link(format!("/proc/{pid}/exe")) else {
            continue;
        };
        if matches!(
            exe.file_name().and_then(|s| s.to_str()),
            Some("qs" | "quickshell")
        ) {
            found.push(identity(pid)?);
        }
    }
    if found.len() != 1 {
        return Err(format!(
            "Expected one bencher renderer, found {}",
            found.len()
        ));
    }
    Ok(found.remove(0))
}
pub fn cpu_percent(previous: &Value, next: &Value, elapsed_seconds: f64) -> Option<f64> {
    if elapsed_seconds <= 0.0 || previous["cgroup"] != next["cgroup"] {
        return None;
    }
    let delta = next["cpuUsageUsec"]
        .as_u64()?
        .checked_sub(previous["cpuUsageUsec"].as_u64()?)?;
    Some(delta as f64 / (elapsed_seconds * 10000.0))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cpu_accounts_for_interval_and_generation() {
        let a = json!({"cgroup":"/a","cpuUsageUsec":100000});
        let b = json!({"cgroup":"/a","cpuUsageUsec":600000});
        assert_eq!(cpu_percent(&a, &b, 2.0), Some(25.0));
        assert_eq!(cpu_percent(&b, &a, 2.0), None);
        assert_eq!(
            cpu_percent(&a, &json!({"cgroup":"/b","cpuUsageUsec":600000}), 2.0),
            None
        );
        assert_eq!(cpu_percent(&a, &b, 0.0), None);
    }
    #[test]
    fn proc_comm_can_contain_spaces_and_parentheses() {
        let p = std::env::temp_dir().join(format!("bencher-stat-{}", std::process::id()));
        fs::create_dir_all(p.join("123")).unwrap();
        fs::write(
            p.join("123/stat"),
            format!(
                "123 (a name ) with parentheses) S {} 987 0",
                vec!["0"; 18].join(" ")
            ),
        )
        .unwrap();
        assert_eq!(identity_at(&p, 123).unwrap().start, 987);
        fs::remove_dir_all(p).unwrap();
    }
    #[test]
    fn malformed_stats_are_errors() {
        assert!(stats("usage_usec nope").is_err());
        assert!(stats("usage_usec").is_err());
    }
}
