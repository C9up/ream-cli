//! The `o` key of `ream dev`: the server asks, `ream dev` opens.
//!
//! The application knows its address (the port is the one it bound) but does
//! not start a process for the browser: a framework that spawns a shell at run
//! time is what supply-chain scanners flag. It writes the address to the file
//! named by `REAM_DEV_OPEN`, in a directory only this user can enter, and a
//! thread here opens what appears there — without a shell on any platform.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The directory the request file lives in, removed when `ream dev` ends.
static DIRECTORY: OnceLock<PathBuf> = OnceLock::new();

/// How often the request file is looked for. A keypress, not a stream.
const POLL: Duration = Duration::from_millis(200);

/// Create the directory, name the file in `REAM_DEV_OPEN`, and start
/// watching it. Call before any thread exists: it sets an environment variable.
pub fn start() {
    let Some(directory) = private_directory() else {
        // No temporary directory to write in: `o` prints the address instead.
        return;
    };
    let file = directory.join("open");
    // SAFETY: `ream dev` calls this before spawning anything.
    unsafe { std::env::set_var("REAM_DEV_OPEN", &file) };
    if DIRECTORY.set(directory).is_err() {
        return;
    }
    std::thread::spawn(move || loop {
        std::thread::sleep(POLL);
        if let Some(url) = take_request(&file) {
            open(&url);
        }
    });
}

/// Remove the directory. `ream dev` calls it on the way out.
pub fn stop() {
    if let Some(directory) = DIRECTORY.get() {
        let _ = std::fs::remove_dir_all(directory);
    }
}

/// A directory of our own: created, not reused, so nobody else prepared it.
fn private_directory() -> Option<PathBuf> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!("ream-dev-{}-{nanos}", std::process::id()));
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(&path).ok().map(|()| path)
}

/// The address waiting in `file`, if any; the file is consumed either way.
fn take_request(file: &Path) -> Option<String> {
    let text = std::fs::read_to_string(file).ok()?;
    let _ = std::fs::remove_file(file);
    accept(&text)
}

/// An address worth handing to the browser: `http(s)://`, one token, bounded.
pub fn accept(text: &str) -> Option<String> {
    let url = text.trim();
    let scheme = url.starts_with("http://") || url.starts_with("https://");
    let clean = !url.chars().any(|c| c.is_whitespace() || c.is_control());
    (scheme && clean && url.len() <= 2048).then(|| url.to_string())
}

/// The platform's opener, given the URL as one argument — never a shell line.
fn opener(url: &str) -> Command {
    #[cfg(target_os = "macos")]
    {
        let mut command = Command::new("open");
        command.arg(url);
        command
    }
    #[cfg(target_os = "windows")]
    {
        // `start` is a cmd built-in, so it needs a shell; this does not.
        let mut command = Command::new("rundll32");
        command.args(["url.dll,FileProtocolHandler", url]);
        command
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let mut command = Command::new("xdg-open");
        command.arg(url);
        command
    }
}

fn open(url: &str) {
    let spawned = opener(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    match spawned {
        // Reaped off the loop, so an opener that lingers leaves no zombie.
        Ok(mut child) => {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
        // No opener on this machine — a headless container, say.
        Err(_) => eprintln!("Open {url} in your browser"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_the_address_the_server_writes() {
        assert_eq!(
            accept("http://localhost:3333\n").as_deref(),
            Some("http://localhost:3333")
        );
        assert_eq!(
            accept("https://app.test/x").as_deref(),
            Some("https://app.test/x")
        );
    }

    #[test]
    fn refuses_anything_that_is_not_one_web_address() {
        for text in [
            "",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "http://a b",
            "http://a\u{7}b",
            "calc.exe",
        ] {
            assert_eq!(accept(text), None, "{text:?}");
        }
        assert_eq!(accept(&format!("http://{}", "a".repeat(2050))), None);
    }

    #[test]
    fn consumes_the_request_once() {
        let directory = private_directory().expect("a temporary directory");
        let file = directory.join("open");
        std::fs::write(&file, "http://localhost:4000").expect("write");
        assert_eq!(
            take_request(&file).as_deref(),
            Some("http://localhost:4000")
        );
        assert!(!file.exists());
        assert_eq!(take_request(&file), None);
        let _ = std::fs::remove_dir_all(directory);
    }

    #[cfg(unix)]
    #[test]
    fn keeps_the_directory_to_this_user() {
        use std::os::unix::fs::PermissionsExt;
        let directory = private_directory().expect("a temporary directory");
        let mode = std::fs::metadata(&directory)
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
        let _ = std::fs::remove_dir_all(directory);
    }
}
