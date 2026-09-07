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
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// How often a child is checked for having exited. Short enough that Ctrl-C
/// feels immediate, long enough to cost nothing.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

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

/// Run every process until one of them exits, then stop the others.
///
/// Output is line-prefixed with the process label, so two interleaved streams
/// stay readable. Children keep their colours: they are spawned with
/// FORCE_COLOR, since piping their output would otherwise make them think they
/// are not on a terminal.
pub fn run_together(processes: Vec<Process>) -> Result<(), String> {
    if processes.is_empty() {
        return Ok(());
    }

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

    // One polling loop over every child rather than a thread each: a restart
    // has to put a NEW child into the same slot, and a thread that has already
    // reported its child's exit cannot do that.
    let outcome: (String, i32, Option<String>) = 'supervise: loop {
        for (index, (label, child)) in shared.iter().enumerate() {
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
            break 'supervise (label.clone(), code, None);
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
        let result = run_together(vec![Process {
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
        let result = run_together(vec![Process {
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
        let result = run_together(vec![
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
        let error = run_together(vec![Process {
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

        let error = run_together(vec![
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

    #[test]
    fn says_which_command_could_not_start() {
        let error = run_together(vec![Process {
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
