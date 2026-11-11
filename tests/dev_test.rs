//! Integration tests for `ream dev` and the signals sent to it.
//!
//! Hermetic: the PATH is reduced to a directory of stand-in programs, so the
//! "server" is a shell script that records its pid and sleeps, and what is
//! checked is that a signal to `ream dev` reaches it.

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
            "ream-dev-test-{}-{}-{}",
            label,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&path).expect("create temp project dir");
        let project = Self { path };
        project.write("package.json", r#"{ "name": "fixture", "private": true }"#);
        project.write(
            "node_modules/@swc-node/register/package.json",
            r#"{ "name": "@swc-node/register", "version": "0.0.0-stub" }"#,
        );
        project
    }

    fn write(&self, rel: &str, content: &str) {
        let abs = self.path.join(rel);
        if let Some(parent) = abs.parent() {
            std::fs::create_dir_all(parent).expect("create parent");
        }
        std::fs::write(&abs, content).expect("write file");
    }

    /// A shell script; `{root}` in the body is the project's absolute path.
    fn script(&self, rel: &str, body: &str) {
        let body = body.replace("{root}", &self.path.to_string_lossy());
        self.write(rel, &format!("#!/bin/sh\n{body}\n"));
        std::fs::set_permissions(self.path.join(rel), std::fs::Permissions::from_mode(0o755))
            .expect("chmod");
    }

    /// `ream dev` with `stubs/` first on PATH.
    fn dev(&self) -> Child {
        std::fs::create_dir_all(self.path.join("stubs")).expect("stubs dir");
        Command::new(env!("CARGO_BIN_EXE_ream"))
            .arg("dev")
            .current_dir(&self.path)
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.path.join("stubs").display()),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn ream dev")
    }

    /// The pid a stand-in wrote to `rel`, once it has.
    fn pid(&self, rel: &str) -> i32 {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Ok(text) = std::fs::read_to_string(self.path.join(rel)) {
                if let Ok(pid) = text.trim().parse() {
                    return pid;
                }
            }
            assert!(Instant::now() < deadline, "{rel} was never written");
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

/// Whether `pid` is gone within a few seconds; killed by hand if not, so a
/// failing run leaves nothing behind.
fn gone(pid: i32) -> bool {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let alive = Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !alive {
            return true;
        }
        if Instant::now() >= deadline {
            let _ = Command::new("kill")
                .args(["-KILL", &pid.to_string()])
                .status();
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// `ream dev` must end, and by the signal it was sent.
fn ends_by(child: &mut Child, signal: i32) {
    let status = child
        .wait_timeout(Duration::from_secs(15))
        .expect("wait for ream dev");
    let Some(status) = status else {
        let _ = child.kill();
        panic!("ream dev did not end after the signal");
    };
    assert_eq!(status.signal(), Some(signal), "{status:?}");
}

/// What `docker stop`, systemd or a task runner's terminate sends: to
/// `ream dev` alone, never to its children. The server used to be left
/// running, reparented, holding its port.
#[test]
fn sigterm_to_ream_dev_stops_the_server_it_started() {
    let p = TempProject::new("sigterm");
    p.script(
        "stubs/node",
        r#"echo $$ > "{root}/server.pid"; exec sleep 30"#,
    );

    let mut dev = p.dev();
    let server = p.pid("server.pid");
    send("TERM", dev.id());

    ends_by(&mut dev, 15);
    assert!(gone(server), "the server outlived ream dev");
}

/// The same with an assets watcher beside the server, started the way
/// `pnpm css:watch` is: through an intermediate process.
#[test]
fn sighup_to_ream_dev_stops_the_server_and_the_assets_watcher() {
    let p = TempProject::new("sighup-assets");
    p.write("reamrc.ts", "export default {}\n");
    // The rc read is a node call too: answered with an assets block.
    p.script(
        "stubs/node",
        r#"case "$*" in
  *--input-type=module*) echo '{"assets":{"devServer":{"command":"watch","args":[]}},"metaFiles":[]}' ;;
  *) echo $$ > "{root}/server.pid"; exec sleep 30 ;;
esac"#,
    );
    p.script(
        "stubs/watch",
        r#"sleep 30 & echo $! > "{root}/watcher.pid"; wait"#,
    );

    let mut dev = p.dev();
    let server = p.pid("server.pid");
    let watcher = p.pid("watcher.pid");
    send("HUP", dev.id());

    ends_by(&mut dev, 1);
    assert!(gone(server), "the server outlived ream dev");
    assert!(gone(watcher), "the assets watcher outlived ream dev");
}

/// Ctrl-C, sent as the terminal would to the foreground group, which now
/// holds `ream dev` alone: it has to be passed on.
#[test]
fn sigint_to_ream_dev_alone_still_stops_the_server() {
    let p = TempProject::new("sigint");
    p.script(
        "stubs/node",
        r#"echo $$ > "{root}/server.pid"; exec sleep 30"#,
    );

    let mut dev = p.dev();
    let server = p.pid("server.pid");
    send("INT", dev.id());

    ends_by(&mut dev, 2);
    assert!(gone(server), "the server outlived ream dev");
}
