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

impl TempProject {
    /// The content of `rel` once it is not empty, or nothing within `wait`.
    fn read_within(&self, rel: &str, wait: Duration) -> Option<String> {
        let deadline = Instant::now() + wait;
        loop {
            if let Ok(text) = std::fs::read_to_string(self.path.join(rel)) {
                if !text.is_empty() {
                    return Some(text);
                }
            }
            if Instant::now() >= deadline {
                return None;
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

/// `tailwindcss --watch` ends when its stdin does, so as not to outlive its
/// parent. Given `/dev/null`, it built once, read EOF and exited 0, and
/// `ream dev` took that for the end of the session and stopped the server
/// before it had bound its port.
#[test]
fn a_watcher_that_ends_with_its_stdin_keeps_running_beside_the_server() {
    let p = TempProject::new("stdin-watcher");
    p.write("reamrc.ts", "export default {}\n");
    p.script(
        "stubs/node",
        r#"case "$*" in
  *--input-type=module*) echo '{"assets":{"devServer":{"command":"watch","args":[]}},"metaFiles":[]}' ;;
  *) echo $$ > "{root}/server.pid"; exec sleep 30 ;;
esac"#,
    );
    p.script(
        "stubs/watch",
        r#"echo $$ > "{root}/watcher.pid"; cat > /dev/null; echo eof > "{root}/watcher.eof""#,
    );

    let mut dev = p.dev();
    let server = p.pid("server.pid");
    let watcher = p.pid("watcher.pid");
    std::thread::sleep(Duration::from_secs(2));
    let still_running = matches!(dev.try_wait(), Ok(None));
    let watcher_saw_eof = p.path.join("watcher.eof").exists();
    if still_running {
        send("TERM", dev.id());
        ends_by(&mut dev, 15);
    }

    assert!(gone(server), "the server outlived ream dev");
    assert!(gone(watcher), "the assets watcher outlived ream dev");
    assert!(!watcher_saw_eof, "the watcher's stdin ended under ream dev");
    assert!(
        still_running,
        "ream dev ended with the watcher's first build"
    );
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

/// A pseudo-terminal for `ream dev` to run on, the way a person's terminal
/// is: its controlling terminal, with `ream dev` in the foreground group.
struct Terminal {
    master: std::fs::File,
    slave: std::os::fd::OwnedFd,
}

impl Terminal {
    fn open() -> Self {
        use std::os::fd::FromRawFd;
        // SAFETY: plain libc calls on descriptors this function owns; every
        // return value is checked.
        unsafe {
            let master = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC);
            assert!(master >= 0, "posix_openpt");
            assert_eq!(libc::grantpt(master), 0, "grantpt");
            assert_eq!(libc::unlockpt(master), 0, "unlockpt");
            let name = libc::ptsname(master);
            assert!(!name.is_null(), "ptsname");
            let slave = libc::open(name, libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC);
            assert!(slave >= 0, "open the slave");
            Self {
                master: std::fs::File::from_raw_fd(master),
                slave: std::os::fd::OwnedFd::from_raw_fd(slave),
            }
        }
    }

    /// `ream dev` in a session of its own, on this terminal.
    fn dev(&self, project: &TempProject) -> Child {
        use std::os::unix::process::CommandExt;
        std::fs::create_dir_all(project.path.join("stubs")).expect("stubs dir");
        let io = || Stdio::from(self.slave.try_clone().expect("dup the slave"));
        let mut command = Command::new(env!("CARGO_BIN_EXE_ream"));
        command
            .arg("dev")
            .current_dir(&project.path)
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", project.path.join("stubs").display()),
            )
            .stdin(io())
            .stdout(io())
            .stderr(io());
        // SAFETY: setsid and ioctl are async-signal-safe.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn().expect("spawn ream dev on the terminal");
        // What `ream dev` writes is read, so it never blocks on a full buffer.
        let mut output = self.master.try_clone().expect("dup the master");
        std::thread::spawn(move || {
            let _ = std::io::copy(&mut output, &mut std::io::sink());
        });
        child
    }

    fn press(&self, keys: &str) {
        use std::io::Write;
        (&self.master)
            .write_all(keys.as_bytes())
            .expect("type on the terminal");
    }

    fn local_flags(&self) -> libc::tcflag_t {
        use std::os::fd::AsRawFd;
        // SAFETY: tcgetattr fills a zeroed termios for a descriptor we own.
        unsafe {
            let mut mode: libc::termios = std::mem::zeroed();
            assert_eq!(libc::tcgetattr(self.slave.as_raw_fd(), &mut mode), 0);
            mode.c_lflag
        }
    }
}

/// The server's keys (r, c, o, h) are read by the server, which needs a
/// terminal for them. Since its stdin stopped being ream dev's terminal, they
/// did nothing even when `ream dev` ran in one.
#[test]
fn in_a_terminal_the_servers_keys_reach_it() {
    let p = TempProject::new("keys");
    p.script(
        "stubs/node",
        r#"echo $$ > "{root}/server.pid"
if [ -t 0 ]; then echo tty; else echo pipe; fi > "{root}/stdin.kind"
dd bs=1 count=1 of="{root}/key" 2>/dev/null
exec sleep 30"#,
    );
    let terminal = Terminal::open();
    let cooked = terminal.local_flags();

    let mut dev = terminal.dev(&p);
    let server = p.pid("server.pid");
    let kind = p
        .read_within("stdin.kind", Duration::from_secs(5))
        .unwrap_or_default();
    terminal.press("h");
    let key = p.read_within("key", Duration::from_secs(5));

    send("TERM", dev.id());
    ends_by(&mut dev, 15);
    assert!(gone(server), "the server outlived ream dev");
    assert_eq!(kind.trim(), "tty", "the server's stdin is not a terminal");
    assert_eq!(
        key.as_deref(),
        Some("h"),
        "the key never reached the server"
    );
    // Echo and line editing are back once `ream dev` is gone.
    assert_eq!(
        terminal.local_flags() & (libc::ECHO | libc::ICANON),
        cooked & (libc::ECHO | libc::ICANON),
        "ream dev left the terminal in raw mode"
    );
}

/// With no terminal — an agent, CI, `tail -f /dev/null | ream dev` — nothing
/// is passed on and nobody reads a terminal: the server's stdin stays the
/// pipe `ream dev` holds, and the server offers no keys.
#[test]
fn without_a_terminal_the_server_keeps_a_pipe() {
    let p = TempProject::new("no-terminal");
    p.script(
        "stubs/node",
        r#"echo $$ > "{root}/server.pid"
if [ -t 0 ]; then echo tty; else echo pipe; fi > "{root}/stdin.kind"
exec sleep 30"#,
    );

    let mut dev = p.dev();
    let server = p.pid("server.pid");
    let kind = p
        .read_within("stdin.kind", Duration::from_secs(5))
        .unwrap_or_default();
    send("TERM", dev.id());

    ends_by(&mut dev, 15);
    assert!(gone(server), "the server outlived ream dev");
    assert_eq!(kind.trim(), "pipe");
}
