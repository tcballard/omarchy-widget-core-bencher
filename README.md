# Omarchy Widget Core Bencher

Find out what desktop widgets cost—and what happens when one breaks.

Bencher is a developer tool for [Widget Core](https://github.com/tcballard/omarchy-widget-core), with a synthetic dashboard and a Rust measurement runner. It creates temporary packages, runs timed scenarios, records resource use and removes its widgets afterwards. This is an experimental tool, version **0.0.1**.

| Workload | Behaviour | Evidence |
| --- | --- | --- |
| Idle | Static card, no refresh loop | Package and instance overhead |
| Dashboard | Deterministic chart and sensor rows at 10 Hz | Everyday update and rendering load |
| Crash | Kills only the generated target package’s QML renderer | Core and a separate healthy renderer survive; target relaunches |
| Hang | Pauses that renderer for five seconds | Sibling remains alive; the same target resumes |
| Churn | Creates, restarts and removes instances | Old renderers exit; tracked processes disappear on cleanup |

**Core isolates packages, not individual instances.** Ten instances of one package share a runner. Each scenario includes a separate dashboard package as a healthy witness, so failures exercise the actual package boundary. Other installed widgets remain present and contribute ambient desktop load.

## Run on your Omarchy desktop

Requirements: Linux with cgroup v2 and pidfd support, an active Widget Core **0.0.3 or newer**, Hyprland, and Rust **1.89+**. Core’s regular sandbox and resource limits remain in effect. Keep the desktop visible and close widget settings before starting. The largest default scenario needs eleven free small-widget cells.

```bash
git clone https://github.com/tcballard/omarchy-widget-core-bencher.git
cd omarchy-widget-core-bencher
cargo build --release --locked
./target/release/omarchy-widget-core-bencher run --live
```

The default runs all four modes with **1, 5 and 10 target instances**, samples every roughly 500 ms for **20 seconds** per scenario, and finishes with **3 restart/removal cycles**. Including the baseline and runner recovery, allow around six minutes. Keep the machine otherwise quiet for comparisons.

Start with a smaller run:

```bash
./target/release/omarchy-widget-core-bencher run --live --mode dashboard --counts 1,5 --seconds 10 --cycles 1
```

Run failure checks:

```bash
./target/release/omarchy-widget-core-bencher run --live --mode crash --counts 1 --seconds 15
./target/release/omarchy-widget-core-bencher run --live --mode hang --counts 1 --seconds 10
```

Options: `--mode all|idle|dashboard|crash|hang`, `--counts` (up to three counts, each 1–10), `--seconds` (5–120), `--cycles` (1–20), `--core` (CLI path) and `--output` (a new results directory). Core must already be running: the tool never starts, stops or restarts your whole desktop host. Churn restarts only the generated target package.

## Read the results

The tool prints the results directory. It contains:

- `report.json`: parameters, Core version, per-scenario mean CPU, peak memory, observed runner generations, errors and cleanup results.
- `samples.jsonl`: raw cgroup samples, PID/start-time identities, package health and lifecycle events.
- `ownership.json`: unique package IDs and observed process identities for interrupted-run recovery.
- `target/` and `witness/`: the actual package inputs used for this run.

CPU is **percent of one logical core**: 100% means one core, not the entire computer. Deltas use elapsed monotonic time; a replaced cgroup starts a new baseline. Memory is `memory.current` for the whole cgroup, including its display proxy, descendants and cache. It is **not** RSS or PSS. Core, target and witness are reported separately, with overlapping cgroups rejected to prevent double-counting.

`runnerGenerations` counts distinct cgroups observed during the sampling window; very short generations between samples may be missed. Core’s own health/failure counters are also recorded. Short-lived processes between samples may be missed by the final process check; cgroup removal and production Core resource tests provide complementary evidence.

A completed report means the lifecycle assertions passed, not that CPU or memory met a universal performance target. Compare runs on the same device, scale and desktop configuration. Review the raw data alongside the summary.

**These checks prove process liveness and recovery, not UI responsiveness.** During crash and hang scenarios, try interacting with the healthy witness and Widget Library. Focus, lock/unlock, monitor changes, suspend and reboot remain manual checks. The first version does not include CPU saturation, memory exhaustion, animation or network workloads.

## Stop and recover

Ctrl+C or SIGTERM requests cleanup. Each dashboard also stops its update loop after a bounded duration (maximum 180 seconds); changing its settings rearms it. A separate pause watchdog sends SIGCONT after five seconds even if the measurement parent is killed. Signals use pidfds plus start-time verification to avoid targeting a reused PID.

After power loss or SIGKILL, use the run directory printed at startup:

```bash
./target/release/omarchy-widget-core-bencher cleanup --output results/run-REPLACE_WITH_YOUR_RUN
```

Recovery deletes only the two exact run-specific package IDs in its ownership journal. It never uses a wildcard package deletion or kills arbitrary Quickshell processes. If cleanup fails, the command exits unsuccessfully and the report records the problem. Keep the results directory until recovery finishes.

You can also install `widget/` manually with `omarchy-widget install "$PWD/widget"`; its default is an idle card. Use Core’s `configure` command for dashboard settings. Failure injection belongs to the Rust controller and is not exposed inside the widget.

## Development checks

```bash
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

CI also builds the exact Core v0.0.3 commit, validates and stages the real package, renders idle/dashboard in all three sizes and both light/dark palettes with Qt, and checks timed stop and rearming. Python/PySide6 is a **test dependency only**; the bencher runtime is Rust + QML.

Native signal tests require `/proc` to match the process PID namespace. They explicitly skip in mismatched container environments; a live run refuses that environment. CI runs those tests on a normal Linux runner. Offscreen rendering does not claim Hyprland or on-device performance validation.

MIT licensed.
