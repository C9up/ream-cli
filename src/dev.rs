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
use std::sync::{mpsc, Arc, Mutex};
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
}

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
    let (tx, rx) = mpsc::channel::<(String, i32)>();
    let mut children: Vec<(String, Child)> = Vec::new();

    for process in &processes {
        let spawned = Command::new(&process.spec.command)
            .args(&process.spec.args)
            .env("FORCE_COLOR", "1")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        let mut child = match spawned {
            Ok(child) => child,
            Err(e) => {
                // Whatever already started has to go with it. `Child` has no
                // `Drop` that kills, so returning here left the server running
                // and holding its port: `ream dev` reported that the assets
                // command does not exist, exited, and the next run failed to
                // bind.
                stop_all(&mut children);
                return Err(format!("failed to start `{}`: {e}", process.spec.command));
            }
        };

        let prefix = format!(
            "{}{:width$}\x1b[0m │ ",
            process.colour,
            process.label,
            width = width
        );
        if let Some(stdout) = child.stdout.take() {
            pump(stdout, prefix.clone(), false);
        }
        if let Some(stderr) = child.stderr.take() {
            pump(stderr, prefix, true);
        }
        children.push((process.label.clone(), child));
    }

    // Each child is watched through a shared handle rather than moved into its
    // waiting thread: a moved child can no longer be killed from here, so the
    // survivor would only be reaped once it ended on its own — which for a
    // watcher is never.
    let shared: Vec<(String, Arc<Mutex<Child>>)> = children
        .drain(..)
        .map(|(label, child)| (label, Arc::new(Mutex::new(child))))
        .collect();

    for (label, child) in &shared {
        let (label, child, tx) = (label.clone(), Arc::clone(child), tx.clone());
        thread::spawn(move || loop {
            let finished = child
                .lock()
                .ok()
                .and_then(|mut c| c.try_wait().ok().flatten());
            if let Some(status) = finished {
                let _ = tx.send((label, status.code().unwrap_or(1)));
                return;
            }
            thread::sleep(POLL_INTERVAL);
        });
    }
    drop(tx);

    let (finished, code) = rx
        .recv()
        .map_err(|_| "no process reported an exit".to_string())?;

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

    if code == 0 {
        Ok(())
    } else {
        Err(format!("`{finished}` exited with code {code}"))
    }
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
    #[test]
    fn stops_the_survivor_when_one_process_exits() {
        let start = std::time::Instant::now();
        let result = run_together(vec![
            Process {
                label: "short".to_string(),
                colour: COLOURS[0],
                spec: CommandSpec {
                    command: "sh".to_string(),
                    args: vec!["-c".to_string(), "exit 0".to_string()],
                },
            },
            Process {
                label: "long".to_string(),
                colour: COLOURS[1],
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
                spec: CommandSpec {
                    command: "sh".to_string(),
                    args: vec!["-c".to_string(), script],
                },
            },
            Process {
                label: "assets".to_string(),
                colour: COLOURS[1],
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
            spec: CommandSpec {
                command: "ream-no-such-binary".to_string(),
                args: Vec::new(),
            },
        }])
        .unwrap_err();

        assert!(error.contains("ream-no-such-binary"), "{error}");
    }
}
