//! Running the dev server alongside whatever builds the assets.
//!
//! An app with a stylesheet to watch has two long-running processes, and they
//! belong together: stopping one must stop the other, or a Ctrl-C leaves an
//! orphan watcher holding the output file. Without this, every project reaches
//! for `concurrently` and re-implements the same three lines — which is exactly
//! what `node ace serve` spares an Adonis app.
//!
//! Declared in `reamrc.ts`:
//!
//! ```ts
//! assets: {
//!   devServer: { command: 'pnpm', args: ['css:watch'] },
//!   build: { command: 'pnpm', args: ['css'] },
//! }
//! ```

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, UNIX_EPOCH};

/// How often a child is checked for having exited. Short enough that Ctrl-C
/// feels immediate, long enough to cost nothing.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// How often the watched directories are re-scanned while the dev server is
/// down. Slower than [`POLL_INTERVAL`] because it walks the project — and it
/// only ever runs while there is nothing left to serve.
const RESCAN_INTERVAL: Duration = Duration::from_millis(300);

/// The project directories `ream dev` watches, when they exist.
///
/// Node's `--watch` alone watches only the modules it managed to LOAD. A file
/// that fails to parse never enters the module graph, so fixing it changes
/// nothing the watcher is looking at: the server stays down, reporting the
/// syntax error you already corrected, until the entry point itself is touched.
/// `--watch-path` watches a directory whatever the graph contains.
///
/// In HMR mode the watcher lives INSIDE the server process (hot-hook registers
/// it), which has the same hole and deeper: a process that dies takes its
/// watcher with it, so nothing is left to notice the fix. These are the
/// directories [`wait_for_change`] then watches from out here.
pub const WATCH_DIRS: &[&str] = &[
    "app",
    "bin",
    "config",
    "start",
    "database",
    "providers",
    "commands",
];

/// One command declared under `assets` in the rc file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    pub command: String,
    pub args: Vec<String>,
}

/// What `assets` declares: a watcher for `dev`, a one-shot for `build`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AssetsConfig {
    pub dev_server: Option<CommandSpec>,
    pub build: Option<CommandSpec>,
}

/// Read `assets` out of the JSON the rc-reading script prints.
pub fn parse_assets(raw: &str) -> Result<AssetsConfig, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed == "null" {
        return Ok(AssetsConfig::default());
    }
    let value: serde_json::Value =
        serde_json::from_str(trimmed).map_err(|e| format!("assets config is not JSON: {e}"))?;

    Ok(AssetsConfig {
        dev_server: parse_spec(value.get("devServer"), "assets.devServer")?,
        build: parse_spec(value.get("build"), "assets.build")?,
    })
}

fn parse_spec(
    value: Option<&serde_json::Value>,
    label: &str,
) -> Result<Option<CommandSpec>, String> {
    let Some(value) = value else { return Ok(None) };
    if value.is_null() {
        return Ok(None);
    }
    let command = value
        .get("command")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("{label} needs a `command` string"))?;
    if command.trim().is_empty() {
        return Err(format!("{label}.command is empty"));
    }
    let args = match value.get("args") {
        None => Vec::new(),
        Some(serde_json::Value::Null) => Vec::new(),
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| format!("{label}.args must be strings"))
            })
            .collect::<Result<Vec<_>, _>>()?,
        Some(_) => return Err(format!("{label}.args must be a list")),
    };
    Ok(Some(CommandSpec {
        command: command.to_string(),
        args,
    }))
}

/// A process to run under the multiplexer.
pub struct Process {
    pub label: String,
    pub colour: &'static str,
    pub spec: CommandSpec,
    /// Whether this process asking to be restarted is honoured.
    ///
    /// Only the dev server sets it, and only in HMR mode: `@c9up/ream/hot`
    /// exits on [`EXIT_RESTART`] when hot-hook reports a change it cannot
    /// swap in place. Upstream hears the same event over Node's IPC channel,
    /// which a Rust parent does not have.
    pub restartable: bool,
}

/// The exit code the dev server uses to ask for a restart.
///
/// Matches `FULL_RELOAD_EXIT_CODE` in `@c9up/ream/hot`. 75 is EX_TEMPFAIL:
/// "try again", which is exactly the message, and it is far from the codes a
/// crashing Node process produces (1, or 128+signal) so a real failure is
/// never mistaken for a restart request.
pub const EXIT_RESTART: i32 = 75;

/// ANSI colours for the prefixes, in the order processes are given.
pub const COLOURS: [&str; 4] = ["\x1b[34m", "\x1b[35m", "\x1b[36m", "\x1b[33m"];

/// What the watched directories look like, for telling "nothing changed" from
/// "something did".
///
/// Deliberately a scan and not a file-watching dependency: this runs only while
/// the server is down — the one moment nothing else needs the machine — and a
/// crate pulled in for a recovery path is a crate the release binary carries
/// everywhere.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    /// The newest modification time seen, in nanoseconds since the epoch.
    newest: u128,
    /// Counted as well: the newest mtime can only move forward, so DELETING a
    /// file would otherwise leave the snapshot identical.
    files: usize,
}

/// Summarise what is under `dirs` right now.
pub fn snapshot(paths: &[String]) -> Snapshot {
    let mut state = Snapshot::default();
    for path in paths {
        let path = Path::new(path);
        if path.is_dir() {
            visit(path, &mut state);
        } else {
            // A plain file, which is how the env files get here: they sit at
            // the project root, so no watched directory covers them, and a
            // server that died on a bad value in one would never notice the
            // value being fixed.
            record(path, &mut state);
        }
    }
    state
}

/// Count one file into the snapshot and remember how recently it changed.
fn record(file: &Path, state: &mut Snapshot) {
    state.files += 1;
    if let Ok(modified) = std::fs::metadata(file).and_then(|meta| meta.modified()) {
        if let Ok(since) = modified.duration_since(UNIX_EPOCH) {
            state.newest = state.newest.max(since.as_nanos());
        }
    }
}

fn visit(dir: &Path, state: &mut Snapshot) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        // Symlinks are neither followed nor counted: one pointing back into
        // the project would walk forever.
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            visit(&entry.path(), state);
            continue;
        }
        record(&entry.path(), state);
    }
}

/// Which of `dirs` exist, as the paths a snapshot can be taken of.
///
/// An empty result means there is nothing to wait on, and a crash must be
/// reported rather than waited out: a `ream dev` that hangs forever in a
/// project with no `app/` would be worse than the exit it replaces.
pub fn watchable(dirs: &[&str]) -> Vec<String> {
    dirs.iter()
        .filter(|dir| Path::new(dir).is_dir())
        .map(|dir| (*dir).to_string())
        .collect()
}

/// The paths a crashed server waits on: the project's directories, plus the
/// env files at its root.
///
/// The env files are named separately because they are not in any watched
/// directory and are not modules either — nothing imports one, so the loader's
/// own graph never sees them. Inside a running process that is handled by the
/// restart list; out here, it is this.
pub fn recoverable_paths(dirs: &[&str], env_files: &[String]) -> Vec<String> {
    let mut paths = watchable(dirs);
    paths.extend(
        env_files
            .iter()
            .filter(|file| Path::new(file).is_file())
            .cloned(),
    );
    paths
}

/// What the terminal says when the server died and the session did not.
///
/// Upstream's wording is "Underlying HTTP server died. Still watching for
/// changes" — the point being that the thing you are about to fix is still
/// being watched, which is the one fact the old `error: 'node' exited with
/// code 1` left out by ending there.
pub fn crash_notice(code: i32) -> String {
    format!(
        "\x1b[33m[ream] the server exited with code {code} — still watching, it starts again on your next save\x1b[0m"
    )
}

/// Block until something under `dirs` changes.
pub fn wait_for_change(dirs: &[String]) {
    let baseline = snapshot(dirs);
    while snapshot(dirs) == baseline {
        thread::sleep(RESCAN_INTERVAL);
    }
}

/// Run every process until one of them exits, then stop the others.
///
/// Output is line-prefixed with the process label, so two interleaved streams
/// stay readable. Children keep their colours: they are spawned with
/// FORCE_COLOR, since piping their output would otherwise make them think they
/// are not on a terminal.
///
/// `watch_dirs` is what a CRASHED restartable process waits on before it is
/// started again — pass an empty slice to keep a crash fatal.
pub fn run_together(processes: Vec<Process>, watch_dirs: &[&str]) -> Result<(), String> {
    if processes.is_empty() {
        return Ok(());
    }
    let recoverable = watchable(watch_dirs);

    let width = processes.iter().map(|p| p.label.len()).max().unwrap_or(0);
    let prefixes: Vec<String> = processes
        .iter()
        .map(|p| format!("{}{:width$}\x1b[0m │ ", p.colour, p.label, width = width))
        .collect();
    let mut children: Vec<(String, Child)> = Vec::new();

    for (index, process) in processes.iter().enumerate() {
        match spawn_piped(&process.spec, &prefixes[index]) {
            Ok(child) => children.push((process.label.clone(), child)),
            Err(message) => {
                // Whatever already started has to go with it. `Child` has no
                // `Drop` that kills, so returning here left the server running
                // and holding its port: `ream dev` reported that the assets
                // command does not exist, exited, and the next run failed to
                // bind.
                stop_all(&mut children);
                return Err(message);
            }
        }
    }

    // Each child is watched through a shared handle rather than moved into its
    // waiting thread: a moved child can no longer be killed from here, so the
    // survivor would only be reaped once it ended on its own — which for a
    // watcher is never.
    let shared: Vec<(String, Arc<Mutex<Child>>)> = children
        .drain(..)
        .map(|(label, child)| (label, Arc::new(Mutex::new(child))))
        .collect();

    // A crashed server is not a finished session: its slot holds the snapshot
    // taken when it died, and the next edit starts it again.
    let mut waiting: Vec<Option<Snapshot>> = vec![None; shared.len()];
    let mut last_scan = Instant::now();

    // One polling loop over every child rather than a thread each: a restart
    // has to put a NEW child into the same slot, and a thread that has already
    // reported its child's exit cannot do that.
    let outcome: (String, i32, Option<String>) = 'supervise: loop {
        for (index, (label, child)) in shared.iter().enumerate() {
            // Already reaped and waiting for an edit: `try_wait` would hand
            // back the same exit on every poll.
            if waiting[index].is_some() {
                continue;
            }
            let finished = child
                .lock()
                .ok()
                .and_then(|mut c| c.try_wait().ok().flatten());
            let Some(status) = finished else { continue };
            let code = status.code().unwrap_or(1);

            // A restart request is not an exit to report — it is the next boot.
            if processes[index].restartable && code == EXIT_RESTART {
                match spawn_piped(&processes[index].spec, &prefixes[index]) {
                    Ok(replacement) => {
                        if let Ok(mut slot) = child.lock() {
                            *slot = replacement;
                        }
                        continue;
                    }
                    // Could not start it again: THAT is an exit, and saying why
                    // beats looping on a spawn that will not work.
                    Err(message) => break 'supervise (label.clone(), 1, Some(message)),
                }
            }

            // A crash is one file away from being fixed, and in HMR mode the
            // watcher that would have seen the fix died with the process. So
            // the session stays up and watches from out here — a syntax error
            // used to end `ream dev`, and the next save had nobody left to
            // tell.
            if processes[index].restartable && code != 0 && !recoverable.is_empty() {
                eprintln!("{}{}", prefixes[index], crash_notice(code));
                waiting[index] = Some(snapshot(&recoverable));
                continue;
            }

            break 'supervise (label.clone(), code, None);
        }

        // Anything waiting for an edit starts again as soon as one lands.
        if waiting.iter().any(Option::is_some) && last_scan.elapsed() >= RESCAN_INTERVAL {
            last_scan = Instant::now();
            let current = snapshot(&recoverable);
            for index in 0..waiting.len() {
                let unchanged = match waiting[index].as_ref() {
                    None => true,
                    Some(baseline) => *baseline == current,
                };
                if unchanged {
                    continue;
                }
                match spawn_piped(&processes[index].spec, &prefixes[index]) {
                    Ok(replacement) => {
                        if let Ok(mut slot) = shared[index].1.lock() {
                            *slot = replacement;
                        }
                        waiting[index] = None;
                    }
                    Err(message) => {
                        break 'supervise (processes[index].label.clone(), 1, Some(message))
                    }
                }
            }
        }

        thread::sleep(POLL_INTERVAL);
    };
    let (finished, code, spawn_error) = outcome;

    // Stop the rest — the whole point of running them together. A watcher left
    // behind keeps writing to the output file after the server is gone.
    for (label, child) in &shared {
        if *label == finished {
            continue;
        }
        if let Ok(mut child) = child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    if let Some(message) = spawn_error {
        return Err(message);
    }
    if code == 0 {
        Ok(())
    } else {
        Err(format!("`{finished}` exited with code {code}"))
    }
}

/// Start one process with its output piped through the label prefix.
///
/// Split out because a restart has to do exactly what the first start did,
/// pumps included: a replacement whose streams are not pumped is a server whose
/// output silently stops after the first hot reload.
fn spawn_piped(spec: &CommandSpec, prefix: &str) -> Result<Child, String> {
    let mut child = Command::new(&spec.command)
        .args(&spec.args)
        .env("FORCE_COLOR", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to start `{}`: {e}", spec.command))?;
    if let Some(stdout) = child.stdout.take() {
        pump(stdout, prefix.to_string(), false);
    }
    if let Some(stderr) = child.stderr.take() {
        pump(stderr, prefix.to_string(), true);
    }
    Ok(child)
}

/// Stop every child started so far, and wait for it.
///
/// Used when a later process cannot start: the ones already running are this
/// function's to clean up, and nothing else will.
fn stop_all(children: &mut Vec<(String, Child)>) {
    for (_, child) in children.iter_mut() {
        let _ = child.kill();
        let _ = child.wait();
    }
    children.clear();
}

/// Forward one stream, a line at a time, behind its prefix.
fn pump<R: std::io::Read + Send + 'static>(stream: R, prefix: String, to_stderr: bool) {
    thread::spawn(move || {
        for line in BufReader::new(stream).lines() {
            let Ok(line) = line else { break };
            if to_stderr {
                eprintln!("{prefix}{line}");
            } else {
                println!("{prefix}{line}");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `run_together` with a crash still fatal — what every test wrote before
    /// crash recovery existed, and what most of them still mean.
    fn run_together_no_recovery(processes: Vec<Process>) -> Result<(), String> {
        run_together(processes, &[])
    }

    /// A directory the snapshot tests own, removed when they are done.
    fn scratch(tag: u32) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("ream-dev-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a temp directory");
        dir
    }

    #[test]
    fn no_assets_key_means_nothing_to_run_alongside() {
        assert_eq!(parse_assets("null").unwrap(), AssetsConfig::default());
        assert_eq!(parse_assets("").unwrap(), AssetsConfig::default());
    }

    #[test]
    fn reads_both_commands() {
        let config = parse_assets(
            r#"{"devServer":{"command":"pnpm","args":["css:watch"]},"build":{"command":"pnpm","args":["css"]}}"#,
        )
        .unwrap();

        assert_eq!(
            config.dev_server,
            Some(CommandSpec {
                command: "pnpm".to_string(),
                args: vec!["css:watch".to_string()],
            })
        );
        assert_eq!(
            config.build,
            Some(CommandSpec {
                command: "pnpm".to_string(),
                args: vec!["css".to_string()],
            })
        );
    }

    #[test]
    fn a_command_without_args_is_valid() {
        let config = parse_assets(r#"{"devServer":{"command":"vite"}}"#).unwrap();
        assert_eq!(config.dev_server.unwrap().args, Vec::<String>::new());
    }

    /// A half-written entry must name what is wrong. Silently ignoring it would
    /// start a server whose stylesheet nobody rebuilds — the failure would then
    /// look like a broken template.
    #[test]
    fn refuses_an_entry_without_a_command() {
        let error = parse_assets(r#"{"devServer":{"args":["css:watch"]}}"#).unwrap_err();
        assert!(error.contains("assets.devServer"), "{error}");
        assert!(error.contains("command"), "{error}");
    }

    #[test]
    fn refuses_an_empty_command() {
        let error = parse_assets(r#"{"build":{"command":"  "}}"#).unwrap_err();
        assert!(error.contains("assets.build"), "{error}");
    }

    #[test]
    fn refuses_args_that_are_not_strings() {
        let error = parse_assets(r#"{"devServer":{"command":"pnpm","args":[42]}}"#).unwrap_err();
        assert!(error.contains("must be strings"), "{error}");
    }

    #[test]
    fn refuses_args_that_are_not_a_list() {
        let error =
            parse_assets(r#"{"devServer":{"command":"pnpm","args":"css:watch"}}"#).unwrap_err();
        assert!(error.contains("must be a list"), "{error}");
    }

    /// The reason these processes run together: when one ends, the other must
    /// not survive it. With `&` in a script, the watcher outlives the server.
    /// The exit code that means "start me again".
    ///
    /// Upstream hears this over Node's IPC channel; a Rust parent has none, so
    /// the request arrives as an exit code and the supervisor has to tell it
    /// apart from a process that genuinely stopped. Without this it reads as
    /// the dev server quitting on the first change it cannot hot-swap.
    #[test]
    fn a_restartable_process_asking_for_a_restart_is_started_again() {
        let counter =
            std::env::temp_dir().join(format!("ream-restart-{}-{}", std::process::id(), line!()));
        let _ = std::fs::remove_file(&counter);
        let path = counter.display().to_string();

        // Exits 75 (restart) the first two times, then 0. A supervisor that
        // does not restart sees only the first exit and stops there.
        let script = format!(
            "n=$(cat {path} 2>/dev/null || echo 0); n=$((n+1)); echo $n > {path}; \
             if [ $n -lt 3 ]; then exit {EXIT_RESTART}; fi; exit 0"
        );
        let result = run_together_no_recovery(vec![Process {
            label: "server".to_string(),
            colour: COLOURS[0],
            restartable: true,
            spec: CommandSpec {
                command: "sh".to_string(),
                args: vec!["-c".to_string(), script],
            },
        }]);

        let runs = std::fs::read_to_string(&counter).unwrap_or_default();
        let _ = std::fs::remove_file(&counter);
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(runs.trim(), "3", "it should have been started three times");
    }

    /// And only when it opted in: a watcher that exits 75 has genuinely
    /// stopped, and restarting it forever would hide that.
    #[test]
    fn a_process_that_did_not_opt_in_is_not_restarted() {
        let result = run_together_no_recovery(vec![Process {
            label: "assets".to_string(),
            colour: COLOURS[0],
            restartable: false,
            spec: CommandSpec {
                command: "sh".to_string(),
                args: vec!["-c".to_string(), format!("exit {EXIT_RESTART}")],
            },
        }]);

        let message = result.expect_err("it must report the exit");
        assert!(message.contains(&EXIT_RESTART.to_string()), "{message}");
    }

    #[test]
    fn stops_the_survivor_when_one_process_exits() {
        let start = std::time::Instant::now();
        let result = run_together_no_recovery(vec![
            Process {
                label: "short".to_string(),
                colour: COLOURS[0],
                restartable: false,
                spec: CommandSpec {
                    command: "sh".to_string(),
                    args: vec!["-c".to_string(), "exit 0".to_string()],
                },
            },
            Process {
                label: "long".to_string(),
                colour: COLOURS[1],
                restartable: false,
                spec: CommandSpec {
                    command: "sh".to_string(),
                    args: vec!["-c".to_string(), "sleep 30".to_string()],
                },
            },
        ]);

        assert!(result.is_ok(), "{result:?}");
        // The sleeper was killed rather than waited on: 30s would have elapsed.
        assert!(start.elapsed() < std::time::Duration::from_secs(10));
    }

    #[test]
    fn reports_the_process_that_failed() {
        let error = run_together_no_recovery(vec![Process {
            label: "assets".to_string(),
            colour: COLOURS[0],
            restartable: false,
            spec: CommandSpec {
                command: "sh".to_string(),
                args: vec!["-c".to_string(), "exit 3".to_string()],
            },
        }])
        .unwrap_err();

        assert!(error.contains("assets"), "{error}");
        assert!(error.contains("3"), "{error}");
    }

    /// The reason the two run together, in the failure direction.
    ///
    /// A `Child` that is dropped is not killed, so returning on the second
    /// spawn left the first one running: `ream dev` with a typo in the assets
    /// command reported the typo, exited, and left a node server holding the
    /// port — the next run then failed to bind, naming neither cause.
    #[test]
    fn kills_what_it_already_started_when_a_later_process_cannot_start() {
        let marker = std::env::temp_dir().join(format!("ream-orphan-{}", std::process::id()));
        let _ = std::fs::remove_file(&marker);
        let script = format!(
            "sleep 5; printf x > {}",
            marker.to_str().expect("a utf-8 temp path")
        );

        let error = run_together_no_recovery(vec![
            Process {
                label: "server".to_string(),
                colour: COLOURS[0],
                restartable: false,
                spec: CommandSpec {
                    command: "sh".to_string(),
                    args: vec!["-c".to_string(), script],
                },
            },
            Process {
                label: "assets".to_string(),
                colour: COLOURS[1],
                restartable: false,
                spec: CommandSpec {
                    command: "ream-no-such-binary".to_string(),
                    args: Vec::new(),
                },
            },
        ])
        .unwrap_err();
        assert!(error.contains("ream-no-such-binary"), "{error}");

        // Long enough for the survivor to have reached its write, had it lived.
        std::thread::sleep(std::time::Duration::from_millis(6_000));
        let orphaned = marker.exists();
        let _ = std::fs::remove_file(&marker);
        assert!(
            !orphaned,
            "the first process outlived the failure and kept running"
        );
    }

    /// The snapshot is what stands in for a file watcher, so it has to see
    /// the three things an edit can be.
    #[test]
    fn a_snapshot_sees_a_write_an_addition_and_a_deletion() {
        let dir = scratch(line!());
        let file = dir.join("service.ts");
        std::fs::write(&file, "before").unwrap();
        let dirs = vec![dir.display().to_string()];

        let start = snapshot(&dirs);
        assert_eq!(
            start,
            snapshot(&dirs),
            "an idle project must look unchanged"
        );

        // mtime granularity is coarser than this test is fast.
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&file, "after").unwrap();
        let written = snapshot(&dirs);
        assert_ne!(start, written);

        std::fs::write(dir.join("nested.ts"), "new").unwrap();
        let added = snapshot(&dirs);
        assert_ne!(written, added);

        // A deletion moves no mtime forward: without the file count this one
        // would read as "nothing happened".
        std::fs::remove_file(&file).unwrap();
        assert_ne!(added, snapshot(&dirs));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_the_directories_that_exist_are_watched() {
        let dir = scratch(line!());
        let existing = dir.display().to_string();

        let found = watchable(&[existing.as_str(), "ream-no-such-directory"]);

        assert_eq!(found, vec![existing]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The bug this exists for: a syntax error ended `ream dev`, and the
    /// watcher that would have seen the fix had died inside the process.
    #[test]
    fn a_crash_waits_for_the_next_edit_instead_of_ending_the_session() {
        let watched = scratch(line!());
        let counter = std::env::temp_dir().join(format!("ream-crash-runs-{}", std::process::id()));
        let _ = std::fs::remove_file(&counter);
        let path = counter.display().to_string();

        // Crashes the first time, boots the second — exactly a typo that gets
        // fixed. The counter lives OUTSIDE the watched directory: writing it
        // inside would be a change of its own.
        let script = format!(
            "n=$(cat {path} 2>/dev/null || echo 0); n=$((n+1)); echo $n > {path}; \
             if [ $n -lt 2 ]; then exit 1; fi; exit 0"
        );

        // The edit that fixes it, once the crash has been seen.
        let editing = watched.clone();
        let editor = thread::spawn(move || {
            thread::sleep(Duration::from_millis(800));
            std::fs::write(editing.join("fixed.ts"), "ok").expect("the edit");
        });

        let result = run_together(
            vec![Process {
                label: "server".to_string(),
                colour: COLOURS[0],
                restartable: true,
                spec: CommandSpec {
                    command: "sh".to_string(),
                    args: vec!["-c".to_string(), script],
                },
            }],
            &[watched.to_str().expect("a utf-8 temp path")],
        );

        editor.join().expect("the editing thread");
        let runs = std::fs::read_to_string(&counter).unwrap_or_default();
        let _ = std::fs::remove_file(&counter);
        let _ = std::fs::remove_dir_all(&watched);

        assert!(result.is_ok(), "{result:?}");
        assert_eq!(runs.trim(), "2", "the edit should have started it again");
    }

    /// And it must not wait on nothing: a project with none of the watched
    /// directories would hang forever instead of reporting the crash.
    #[test]
    fn a_crash_is_still_reported_when_there_is_nothing_to_watch() {
        let error = run_together(
            vec![Process {
                label: "server".to_string(),
                colour: COLOURS[0],
                restartable: true,
                spec: CommandSpec {
                    command: "sh".to_string(),
                    args: vec!["-c".to_string(), "exit 1".to_string()],
                },
            }],
            &["ream-no-such-directory"],
        )
        .expect_err("it must report the exit");

        assert!(error.contains("1"), "{error}");
    }

    #[test]
    fn says_which_command_could_not_start() {
        let error = run_together_no_recovery(vec![Process {
            label: "assets".to_string(),
            colour: COLOURS[0],
            restartable: false,
            spec: CommandSpec {
                command: "ream-no-such-binary".to_string(),
                args: Vec::new(),
            },
        }])
        .unwrap_err();

        assert!(error.contains("ream-no-such-binary"), "{error}");
    }
}
