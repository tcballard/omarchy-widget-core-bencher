use crate::{
    command, err, failure,
    metrics::{self, Group, Identity},
    Result, STOP,
};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::atomic::Ordering,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
const BASE: &str = "io.github.tcballard.widget-core-bencher";
const HOST: &str = "omarchy-widget-host.service";
pub struct Options {
    pub action: String,
    pub core: String,
    pub mode: String,
    pub counts: Vec<u64>,
    pub seconds: u64,
    pub cycles: u64,
    pub output: PathBuf,
    pub live: bool,
}
impl Options {
    pub fn parse(args: &[String]) -> Result<Self> {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(err)?
            .as_nanos();
        let mut o = Self {
            action: args.first().cloned().unwrap_or("help".into()),
            core: "omarchy-widget".into(),
            mode: "all".into(),
            counts: vec![1, 5, 10],
            seconds: 20,
            cycles: 3,
            output: PathBuf::from(format!("results/run-{nonce}")),
            live: false,
        };
        if matches!(o.action.as_str(), "--help" | "-h" | "help") {
            o.action = "help".into();
            return Ok(o);
        }
        if !matches!(o.action.as_str(), "run" | "cleanup") {
            return Err("Use run, cleanup or --help".into());
        }
        let mut i = 1;
        let mut supplied_output = false;
        while i < args.len() {
            if args[i] == "--live" {
                o.live = true;
                i += 1;
                continue;
            }
            let v = args.get(i + 1).ok_or("Option requires a value")?;
            match args[i].as_str() {
                "--core" => o.core = v.clone(),
                "--mode" => o.mode = v.clone(),
                "--counts" => {
                    o.counts = v
                        .split(',')
                        .map(|s| s.parse().map_err(err))
                        .collect::<Result<_>>()?
                }
                "--seconds" => o.seconds = v.parse().map_err(err)?,
                "--cycles" => o.cycles = v.parse().map_err(err)?,
                "--output" => {
                    o.output = v.into();
                    supplied_output = true;
                }
                _ => return Err(format!("Unknown option {}", args[i])),
            }
            i += 2;
        }
        if !matches!(
            o.mode.as_str(),
            "all" | "idle" | "dashboard" | "crash" | "hang"
        ) {
            return Err("Unknown workload mode".into());
        }
        if o.counts.is_empty()
            || o.counts.len() > 3
            || o.counts.iter().any(|n| !(1..=10).contains(n))
        {
            return Err("Choose 1–3 instance counts, each 1–10".into());
        }
        if !(5..=120).contains(&o.seconds) || !(1..=20).contains(&o.cycles) {
            return Err("Seconds must be 5–120; cycles 1–20".into());
        }
        if o.action == "cleanup" && !supplied_output {
            return Err("cleanup requires --output RUN_DIRECTORY".into());
        }
        if o.action == "run" && !o.live {
            return Err("run requires --live to install temporary packages on this desktop".into());
        }
        Ok(o)
    }
}
fn write_json(path: &Path, value: &Value) -> Result<()> {
    let next = path.with_extension("json.tmp");
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(&next)
        .map_err(err)?;
    file.write_all(serde_json::to_string_pretty(value).map_err(err)?.as_bytes())
        .map_err(err)?;
    file.sync_all().map_err(err)?;
    fs::rename(next, path).map_err(err)
}
fn core(o: &Options, args: &[&str]) -> Result<Value> {
    command::core(&o.core, args)
}
fn host_pid() -> Result<i32> {
    let pid = command::output(
        "systemctl",
        &["--user", "show", HOST, "--property=MainPID", "--value"],
    )?;
    let pid: i32 = pid.trim().parse().map_err(err)?;
    if pid <= 0 {
        return Err("Start Core before benchmarking".into());
    }
    Ok(pid)
}
fn arrays(v: &Value, key: &str) -> Result<Vec<Value>> {
    Ok(v[key]
        .as_array()
        .ok_or(format!("Core response missing {key}"))?
        .clone())
}
fn source(snapshot: &Value, id: &str) -> Result<String> {
    arrays(snapshot, "catalog")?
        .iter()
        .find(|v| v["packageId"] == id)
        .and_then(|v| v["directory"].as_str())
        .map(str::to_owned)
        .ok_or(format!("Missing package source {id}"))
}
fn check_stop() -> Result<()> {
    if STOP.load(Ordering::Relaxed) {
        Err("Interrupted; cleaning up temporary widgets".into())
    } else {
        Ok(())
    }
}
fn wait(seconds: u64, mut test: impl FnMut() -> Result<bool>) -> Result<()> {
    let start = Instant::now();
    loop {
        check_stop()?;
        if test()? {
            return Ok(());
        }
        if start.elapsed() > Duration::from_secs(seconds) {
            return Err("Timed out waiting for desktop runner".into());
        }
        thread::sleep(Duration::from_millis(200));
    }
}
struct Session {
    options: Options,
    run_id: String,
    packages: Vec<String>,
    sources: BTreeMap<String, String>,
    instances: Vec<String>,
    observed: BTreeSet<Identity>,
    host: Identity,
    samples: File,
    report: Value,
    started: Instant,
    watchdog: Option<Child>,
    resume: Option<failure::Target>,
}
impl Session {
    fn journal(&self) -> Result<()> {
        write_json(
            &self.options.output.join("ownership.json"),
            &json!({"schemaVersion":1,"runId":self.run_id,"packages":self.packages,"processes":self.observed.iter().map(|id|json!({"pid":id.pid,"startTicks":id.start})).collect::<Vec<_>>()}),
        )
    }
    fn event(&mut self, name: &str, data: Value) -> Result<()> {
        let v = json!({"type":"event","elapsedSeconds":self.started.elapsed().as_secs_f64(),"name":name,"data":data});
        writeln!(self.samples, "{v}").map_err(err)?;
        self.samples.flush().map_err(err)
    }
    fn install(&mut self, label: &str, mode: &str) -> Result<String> {
        let id = format!("{BASE}.{}.{}", self.run_id, label);
        let path = self.options.output.join(label);
        fs::create_dir(&path).map_err(err)?;
        let mut manifest: Value =
            serde_json::from_str(include_str!("../widget/widget.json")).map_err(err)?;
        manifest["id"] = json!(id);
        manifest["name"] = json!(format!("Bench {} {label}", self.run_id));
        manifest["defaults"] = json!({"mode":mode,"durationSeconds":180,"seed":1});
        write_json(&path.join("widget.json"), &manifest)?;
        fs::write(path.join("View.qml"), include_str!("../widget/View.qml")).map_err(err)?;
        // Record intent before install: cleanup handles an uncertain timeout too.
        self.packages.push(id.clone());
        self.journal()?;
        core(
            &self.options,
            &["install", path.to_str().ok_or("Non-UTF8 package path")?],
        )?;
        let snapshot = core(&self.options, &["list"])?;
        self.sources.insert(label.into(), source(&snapshot, &id)?);
        self.event("installed", json!({"label":label,"packageId":id}))?;
        Ok(id)
    }
    fn create(&mut self, package: &str, count: u64, mode: &str) -> Result<()> {
        for seed in 1..=count {
            check_stop()?;
            let created = core(&self.options, &["create", package, "small"])?;
            let id = created["updated"]
                .as_str()
                .ok_or("Core create returned no instance ID")?
                .to_owned();
            self.instances.push(id.clone());
            let settings =
                json!({"mode":mode,"durationSeconds":self.options.seconds+40,"seed":seed})
                    .to_string();
            core(&self.options, &["configure", &id, &settings])?;
        }
        let snapshot = core(&self.options, &["list"])?;
        let entries = arrays(&snapshot, "installed")?;
        let own: Vec<_> = entries
            .iter()
            .filter(|v| v["packageId"] == package && v["placement"]["enabled"] == true)
            .collect();
        if own.len() != count as usize || own.iter().any(|v| v["effective"].is_null()) {
            return Err("Not enough free desktop cells for every test instance; use fewer --counts or free space".into());
        }
        Ok(())
    }
    fn groups(&self) -> Result<Vec<Group>> {
        if !metrics::alive(&self.host) || host_pid()? != self.host.pid {
            return Err("Core host exited or restarted during measurement".into());
        }
        metrics::discover(&self.sources, self.host.pid)
    }
    fn sample(&mut self, scenario: &str) -> Result<Vec<Value>> {
        let mut values = Vec::new();
        for group in self.groups()? {
            match metrics::sample(&group) {
                Ok(value) => {
                    for p in value["processes"].as_array().unwrap() {
                        if group.label != "core" {
                            self.observed.insert(Identity {
                                pid: p["pid"].as_i64().unwrap() as i32,
                                start: p["startTicks"].as_u64().unwrap(),
                            });
                        }
                    }
                    values.push(value);
                }
                Err(error) => {
                    self.event(
                        "sample-unavailable",
                        json!({"label":group.label,"error":error}),
                    )?;
                }
            }
        }
        let snapshot = core(&self.options, &["list"])?;
        let health: Vec<_> = arrays(&snapshot, "catalog")?
            .into_iter()
            .filter(|v| {
                self.packages
                    .contains(&v["packageId"].as_str().unwrap_or("").to_owned())
            })
            .map(|v| json!({"packageId":v["packageId"],"health":v["health"]}))
            .collect();
        let value = json!({"type":"sample","scenario":scenario,"elapsedSeconds":self.started.elapsed().as_secs_f64(),"groups":values,"health":health});
        writeln!(self.samples, "{value}").map_err(err)?;
        self.samples.flush().map_err(err)?;
        self.journal()?;
        Ok(values)
    }
    fn ready(&mut self, label: &str) -> Result<()> {
        wait(15, || {
            let groups = self.groups()?;
            let Some(group) = groups.iter().find(|g| g.label == label) else {
                return Ok(false);
            };
            Ok(metrics::renderer(group).is_ok())
        })?;
        // Observe the entire package cgroup, including its display proxy.
        self.sample("ready")?;
        Ok(())
    }
    fn measure(&mut self, name: &str) -> Result<()> {
        let start = Instant::now();
        let mut previous: Option<(f64, Vec<Value>)> = None;
        let mut cpu: BTreeMap<String, Vec<f64>> = BTreeMap::new();
        let mut peak: BTreeMap<String, u64> = BTreeMap::new();
        let mut generations: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        while start.elapsed() < Duration::from_secs(self.options.seconds) {
            check_stop()?;
            let time = self.started.elapsed().as_secs_f64();
            let values = self.sample(name)?;
            if !values.iter().any(|v| v["label"] == "core") {
                return Err("Core cgroup could not be measured".into());
            }
            if name != "core-baseline" && !values.iter().any(|v| v["label"] == "witness") {
                return Err("Healthy witness cgroup disappeared during measurement".into());
            }
            if (name.starts_with("idle-") || name.starts_with("dashboard-"))
                && !values.iter().any(|v| v["label"] == "target")
            {
                return Err("Target cgroup disappeared during normal workload".into());
            }
            for v in &values {
                let label = v["label"].as_str().unwrap();
                let memory = v["memoryBytes"].as_u64().unwrap();
                peak.entry(label.into())
                    .and_modify(|m| *m = (*m).max(memory))
                    .or_insert(memory);
                generations
                    .entry(label.into())
                    .or_default()
                    .insert(v["cgroup"].as_str().unwrap().into());
                if let Some((prev_time, previous)) = &previous {
                    if let Some(p) = previous.iter().find(|p| p["label"] == v["label"]) {
                        if let Some(percent) = metrics::cpu_percent(p, v, time - *prev_time) {
                            cpu.entry(label.into()).or_default().push(percent);
                        }
                    }
                }
            }
            previous = Some((time, values));
            thread::sleep(Duration::from_millis(500));
        }
        if let Some(child) = self.watchdog.as_mut() {
            if !child.wait().map_err(err)?.success() {
                return Err("Pause watchdog failed".into());
            }
        }
        self.watchdog = None;
        self.resume = None;
        let labels:Vec<_>=peak.keys().map(|label| {let c=cpu.get(label).cloned().unwrap_or_default(); json!({"label":label,"peakMemoryBytes":peak[label],"meanCpuPercentOneCore":if c.is_empty(){Value::Null}else{json!(c.iter().sum::<f64>()/c.len() as f64)},"cpuIntervals":c.len(),"runnerGenerations":generations[label].len()})}).collect();
        self.report["scenarios"].as_array_mut().unwrap().push(
            json!({"name":name,"durationSeconds":start.elapsed().as_secs_f64(),"groups":labels}),
        );
        write_json(&self.options.output.join("report.json"), &self.report)
    }
    fn refresh_witness(&mut self, package: &str) -> Result<()> {
        let snapshot = core(&self.options, &["list"])?;
        let id = arrays(&snapshot, "installed")?
            .iter()
            .find(|v| v["packageId"] == package && !v["placement"].is_null())
            .and_then(|v| v["instanceId"].as_str().map(str::to_owned))
            .ok_or("Missing witness instance")?;
        let settings = json!({"mode":"dashboard", "durationSeconds":180, "seed":1}).to_string();
        core(&self.options, &["configure", &id, &settings])?;
        Ok(())
    }
    fn remove_targets(&mut self, package: &str) -> Result<()> {
        let snapshot = core(&self.options, &["list"])?;
        let ids: Vec<_> = arrays(&snapshot, "installed")?
            .iter()
            .filter(|v| v["packageId"] == package && !v["placement"].is_null())
            .filter_map(|v| v["instanceId"].as_str().map(str::to_owned))
            .collect();
        for id in ids {
            core(&self.options, &["remove-instance", &id])?;
            self.instances.retain(|v| v != &id);
        }
        wait(10, || {
            Ok(!self.groups()?.iter().any(|g| g.label == "target"))
        })?;
        Ok(())
    }
    fn failure(&mut self, mode: &str, count: u64) -> Result<()> {
        self.ready("target")?;
        self.ready("witness")?;
        let groups = self.groups()?;
        let group = groups
            .iter()
            .find(|g| g.label == "target")
            .ok_or("Missing target runner")?;
        let target = metrics::renderer(group)?;
        let witness = metrics::renderer(
            groups
                .iter()
                .find(|g| g.label == "witness")
                .ok_or("Missing healthy witness")?,
        )?;
        let handle = failure::Target::open(&target)?;
        self.event("failure-injected",json!({"mode":mode,"targetPid":target.pid,"targetStartTicks":target.start,"witnessPid":witness.pid,"pauseSeconds":if mode=="hang" {5} else {0}}))?;
        if mode == "crash" {
            handle.signal(libc::SIGKILL)?;
        } else {
            // Separate watchdog survives SIGKILL of the measurement parent.
            self.resume = Some(handle);
            self.watchdog = Some(
                Command::new(std::env::current_exe().map_err(err)?)
                    .args([
                        "__pause",
                        &target.pid.to_string(),
                        &target.start.to_string(),
                        "5",
                    ])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .map_err(err)?,
            );
        }
        self.measure(&format!("{mode}-{count}"))?;
        if !metrics::alive(&witness) {
            return Err("Healthy witness renderer did not survive the failure test".into());
        }
        self.ready("target")?;
        let replacement = metrics::renderer(
            self.groups()?
                .iter()
                .find(|g| g.label == "target")
                .ok_or("No recovered target")?,
        )?;
        if mode == "crash" && replacement == target {
            return Err("Crash target did not get a new process lifetime".into());
        }
        if mode == "hang" && replacement != target {
            return Err("Hung target restarted instead of resuming".into());
        }
        self.event("failure-recovered",json!({"mode":mode,"targetPid":replacement.pid,"witnessSurvived":true,"coreSurvived":true,"uiResponsiveness":"not-measured"}))
    }
    fn cleanup(&mut self) -> Result<()> {
        if let Some(target) = self.resume.take() {
            let _ = target.signal(libc::SIGCONT);
        }
        if let Some(mut child) = self.watchdog.take() {
            let _ = child.wait();
        }
        let cleanup = cleanup_packages(&self.options, &self.packages);
        let until = Instant::now() + Duration::from_secs(10);
        while self.observed.iter().any(metrics::alive) && Instant::now() < until {
            thread::sleep(Duration::from_millis(200));
        }
        let alive: Vec<_> = self
            .observed
            .iter()
            .filter(|id| metrics::alive(id))
            .map(|id| json!({"pid":id.pid,"startTicks":id.start}))
            .collect();
        let discovered = metrics::discover_owned(&self.sources);
        let discovery_error = discovered.as_ref().err().cloned();
        let leftover_groups = discovered
            .unwrap_or_default()
            .into_iter()
            .filter(|g| g.label != "core")
            .map(|g| g.path)
            .collect::<Vec<_>>();
        self.report["cleanup"] = json!({"packageCleanupError":cleanup.as_ref().err(),"processDiscoveryError":discovery_error,"leftoverProcesses":alive,"leftoverCgroups":leftover_groups,"success":cleanup.is_ok() && discovery_error.is_none() && alive.is_empty() && leftover_groups.is_empty()});
        write_json(&self.options.output.join("report.json"), &self.report)?;
        cleanup?;
        if discovery_error.is_some() || !alive.is_empty() || !leftover_groups.is_empty() {
            return Err("Cleanup found surviving bencher processes; inspect report.json".into());
        }
        Ok(())
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        if let Some(target) = self.resume.take() {
            let _ = target.signal(libc::SIGCONT);
        }
    }
}
fn cleanup_packages(o: &Options, packages: &[String]) -> Result<()> {
    let snapshot = core(o, &["list"])?;
    let catalog = arrays(&snapshot, "catalog")?;
    let mut errors = Vec::new();
    for id in packages.iter().rev() {
        if catalog.iter().any(|v| v["packageId"] == *id) {
            if let Err(e) = core(o, &["uninstall", id, "delete"]) {
                errors.push(e);
            }
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}
pub fn recover(o: &Options) -> Result<()> {
    let journal: Value =
        serde_json::from_slice(&fs::read(o.output.join("ownership.json")).map_err(err)?)
            .map_err(err)?;
    let run = journal["runId"].as_str().ok_or("Missing run ID")?;
    if run.is_empty() || !run.bytes().all(|b| b.is_ascii_digit() || b == b'-') {
        return Err("Invalid run ID".into());
    }
    let allowed = [
        format!("{BASE}.{run}.target"),
        format!("{BASE}.{run}.witness"),
    ];
    let packages: Vec<String> = arrays(&journal, "packages")?
        .into_iter()
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .ok_or("Invalid package ID".into())
        })
        .collect::<Result<_>>()?;
    if packages.len() > 2 || packages.iter().any(|id| !allowed.contains(id)) {
        return Err("Ownership journal contains an unrelated package".into());
    }
    cleanup_packages(o, &packages)?;
    println!("Removed temporary packages owned by run {run}");
    Ok(())
}
pub fn execute(options: Options) -> Result<()> {
    metrics::check_proc_namespace()?;
    let help = core(&options, &["help"])?;
    let version = help["version"].as_str().ok_or("Core returned no version")?;
    let parts: Vec<u64> = version
        .split('.')
        .map(|v| v.parse().map_err(err))
        .collect::<Result<_>>()?;
    if parts.len() != 3 || parts.as_slice() < [0, 0, 3].as_slice() {
        return Err("Core >=0.0.3 required".into());
    }
    let host = metrics::identity(host_pid()?)?;
    let initial = core(&options, &["list"])?;
    if initial["desktop"]["available"] != true
        || initial["repairRequired"] == true
        || initial["runtime"]["shown"] == false
        || initial["runtime"]["editing"] == true
        || !initial["runtime"]["edit"].is_null()
    {
        return Err("A visible, healthy desktop with no active widget editor is required".into());
    }
    if let Some(parent) = options.output.parent() {
        fs::create_dir_all(parent).map_err(err)?;
    }
    fs::create_dir(&options.output).map_err(err)?;
    fs::set_permissions(&options.output, fs::Permissions::from_mode(0o700)).map_err(err)?;
    let samples = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(options.output.join("samples.jsonl"))
        .map_err(err)?;
    let run_id = format!(
        "{}-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(err)?
            .as_nanos(),
        std::process::id()
    );
    let report = json!({"schemaVersion":1,"status":"running","coreVersion":version,"runId":run_id,"parameters":{"mode":options.mode,"counts":options.counts,"seconds":options.seconds,"cycles":options.cycles},"existingInstances":initial["installed"].as_array().map(|v|v.iter().filter(|e|!e["placement"].is_null()).count()),"scenarios":[],"limitations":["CPU is percent of one logical core; cgroup memory includes cache, not RSS/PSS","One isolated runner per package, multiple instances share it","Witness process liveness does not prove UI responsiveness","No live focus, lock, monitor, suspend or reboot assertions","Results include ambient desktop activity; no universal performance pass threshold"]});
    let mut session = Session {
        options,
        run_id,
        packages: vec![],
        sources: BTreeMap::new(),
        instances: vec![],
        observed: BTreeSet::new(),
        host,
        samples,
        report,
        started: Instant::now(),
        watchdog: None,
        resume: None,
    };
    session.journal()?;
    write_json(&session.options.output.join("report.json"), &session.report)?;
    println!("Results: {}", session.options.output.display());
    let result: Result<()> = (|| {
        session.measure("core-baseline")?;
        let target = session.install("target", "idle")?;
        let witness = session.install("witness", "dashboard")?;
        session.create(&witness, 1, "dashboard")?;
        session.ready("witness")?;
        let modes = if session.options.mode == "all" {
            vec!["idle", "dashboard", "crash", "hang"]
        } else {
            vec![session.options.mode.as_str()]
        }
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
        for mode in modes {
            for count in session.options.counts.clone() {
                println!("{mode}: {count} target instances + 1 healthy witness");
                session.refresh_witness(&witness)?;
                core(&session.options, &["package-control", &target, "restart"])?;
                session.create(
                    &target,
                    count,
                    if mode == "idle" { "idle" } else { "dashboard" },
                )?;
                session.ready("target")?;
                if mode == "crash" || mode == "hang" {
                    session.failure(&mode, count)?;
                } else {
                    session.measure(&format!("{mode}-{count}"))?;
                }
                session.remove_targets(&target)?;
            }
        }
        for cycle in 0..session.options.cycles {
            session.refresh_witness(&witness)?;
            session.create(&target, 1, "dashboard")?;
            session.ready("target")?;
            let old = metrics::renderer(
                session
                    .groups()?
                    .iter()
                    .find(|g| g.label == "target")
                    .ok_or("Missing target")?,
            )?;
            core(&session.options, &["package-control", &target, "restart"])?;
            wait(15, || {
                let groups = session.groups()?;
                Ok(groups
                    .iter()
                    .find(|g| g.label == "target")
                    .and_then(|g| metrics::renderer(g).ok())
                    .is_some_and(|id| id != old))
            })?;
            session.sample("restart-cycle")?;
            session.remove_targets(&target)?;
            if metrics::alive(&old) {
                return Err("Old renderer survived package restart".into());
            }
            session.event(
                "churn-cycle",
                json!({"cycle":cycle+1,"oldRendererExited":true}),
            )?;
        }
        Ok(())
    })();
    session.report["status"] = json!(if result.is_ok() {
        "completed"
    } else {
        "failed"
    });
    session.report["error"] = json!(result.as_ref().err());
    let cleanup = session.cleanup();
    if cleanup.is_err() {
        session.report["status"] = json!("failed");
        write_json(&session.options.output.join("report.json"), &session.report)?;
    }
    result?;
    cleanup?;
    println!("Completed; temporary packages removed. Review report.json and samples.jsonl.");
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }
    #[test]
    fn options_bound_workloads() {
        assert!(Options::parse(&args(&["run"])).is_err());
        assert!(Options::parse(&args(&["run", "--live", "--counts", "11"])).is_err());
        assert!(Options::parse(&args(&["run", "--live", "--seconds", "121"])).is_err());
        assert!(Options::parse(&args(&["run", "--live", "--mode", "all"])).is_ok());
        assert!(Options::parse(&args(&["cleanup"])).is_err());
    }
}
