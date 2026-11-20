//! The commands that run one node process in the foreground — a console
//! command, `test`, `repl`, `inspect` — must let a signal sent to `ream` reach
//! that process, the way it reaches `node ace <command>` upstream.
//!
//! Hermetic: `node` is a shell stand-in on a reduced PATH that records its pid,
//! logs the signal it receives and exits with a code of its own.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use wait_timeout::ChildExt;

struct TempProject {
    path: PathBuf,
}

impl TempProject {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "ream-node-signals-test-{}-{}-{}",
            label,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&path).expect("create temp project dir");
        let project = Self { path };
        project.write("package.json", r#"{ "name": "fixture", "type": "module" }"#);
        project.write(
            "node_modules/@swc-node/register/package.json",
            r#"{ "name": "@swc-node/register", "version": "0.0.0-stub" }"#,
        );
        project.write("reamrc.ts", "export default {}\n");
        project.write("bin/console.js", "// the stand-in node never reads it\n");
        // A long-running process that ends cleanly on the signals it is sent,
        // and says which one it got.
        let root = project.path.to_string_lossy().into_owned();
        project.write(
            "stubs/node",
            &format!(
                "#!/bin/sh\n\
                 echo $$ > \"{root}/node.pid\"\n\
                 trap 'echo TERM > \"{root}/signal.log\"; exit 7' TERM\n\
                 trap 'echo HUP > \"{root}/signal.log\"; exit 8' HUP\n\
                 while :; do sleep 0.05; done\n"
            ),
        );
        std::fs::set_permissions(
            project.path.join("stubs/node"),
            std::fs::Permissions::from_mode(0o755),
        )
        .expect("chmod");
        project
    }

    fn write(&self, rel: &str, content: &str) {
        let abs = self.path.join(rel);
        if let Some(parent) = abs.parent() {
            std::fs::create_dir_all(parent).expect("create parent");
        }
        std::fs::write(&abs, content).expect("write file");
    }

    fn ream(&self, args: &[&str]) -> Child {
        Command::new(env!("CARGO_BIN_EXE_ream"))
            .args(args)
            .current_dir(&self.path)
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.path.join("stubs").display()),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn ream")
    }

    fn node_pid(&self) -> i32 {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Ok(text) = std::fs::read_to_string(self.path.join("node.pid")) {
                if let Ok(pid) = text.trim().parse() {
                    return pid;
                }
            }
            assert!(Instant::now() < deadline, "node never started");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn signal_log(&self) -> String {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let text = std::fs::read_to_string(self.path.join("signal.log")).unwrap_or_default();
            if !text.is_empty() || Instant::now() >= deadline {
                return text.trim().to_string();
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for TempProject {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn send(signal: &str, pid: u32) {
    let status = Command::new("kill")
        .args([format!("-{signal}"), pid.to_string()])
        .status()
        .expect("run kill");
    assert!(status.success(), "kill -{signal} {pid} failed");
}

/// Kill a stand-in that outlived the test, so a failing run leaves nothing.
fn reap(pid: i32) {
    let _ = Command::new("kill")
        .args(["-KILL", &pid.to_string()])
        .stderr(Stdio::null())
        .status();
}

/// Send `signal` to `ream <args>` and expect node to have handled it, with
/// `ream` ending on node's own exit code.
fn signal_reaches_node(label: &str, args: &[&str], signal: &str, logged: &str, code: i32) {
    let p = TempProject::new(label);
    let mut ream = p.ream(args);
    let node = p.node_pid();
    send(signal, ream.id());

    let status = ream
        .wait_timeout(Duration::from_secs(15))
        .expect("wait for ream");
    let log = p.signal_log();
    reap(node);
    let Some(status) = status else {
        let _ = ream.kill();
        panic!("ream did not end after SIG{signal}");
    };
    assert_eq!(log, logged, "node never received SIG{signal}");
    assert_eq!(status.signal(), None, "{status:?}");
    assert_eq!(status.code(), Some(code), "{status:?}");
}

/// `docker stop` on a queue worker: the worker must hear it and shut down.
#[test]
fn sigterm_to_a_console_command_reaches_it() {
    signal_reaches_node("console-term", &["queue:work"], "TERM", "TERM", 7);
}

/// A closed terminal or a dropped SSH session.
#[test]
fn sighup_to_a_console_command_reaches_it() {
    signal_reaches_node("console-hup", &["queue:work"], "HUP", "HUP", 8);
}

/// `timeout 600 ream test` in CI: the run must stop, not carry on orphaned.
#[test]
fn sigterm_to_ream_test_reaches_the_runner() {
    signal_reaches_node("test-term", &["test"], "TERM", "TERM", 7);
}

#[test]
fn sigterm_to_ream_repl_reaches_it() {
    signal_reaches_node("repl-term", &["repl"], "TERM", "TERM", 7);
}

#[test]
fn sigterm_to_ream_inspect_reaches_it() {
    signal_reaches_node("inspect-term", &["inspect"], "TERM", "TERM", 7);
}
