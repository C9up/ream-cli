//! `ream generate:update-key` and `ream mcp install` write files a cloned
//! repository can name: a link there must not carry the write elsewhere, and
//! the private update key is readable by its owner only whenever the command
//! says so.

#![cfg(unix)]

use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Repo {
    path: PathBuf,
}

impl Repo {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "ream-update-key-test-{}-{}-{}",
            label,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(path.join("public")).expect("create temp dir");
        std::fs::write(path.join("package.json"), r#"{ "name": "cloned" }"#).unwrap();
        Self { path }
    }

    fn ream(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_ream"))
            .args(args)
            .arg("--no-ansi")
            .current_dir(&self.path)
            .output()
            .expect("spawn ream")
    }

    fn join(&self, rel: &str) -> PathBuf {
        self.path.join(rel)
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn mode_of(path: &Path) -> u32 {
    std::fs::metadata(path).expect("stat").permissions().mode() & 0o777
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// `robledo.key` committed as a link to a file that does not exist yet in
/// `public/`: the private key would land there, served to anyone.
#[test]
fn a_dangling_key_link_is_refused_and_nothing_is_created() {
    let repo = Repo::new("dangling");
    symlink("public/app.js.map", repo.join("robledo.key")).unwrap();

    let output = repo.ream(&["generate:update-key"]);
    assert!(!output.status.success(), "{output:?}");
    assert!(stderr(&output).contains("symbolic link"), "{output:?}");
    assert!(!repo.join("public/app.js.map").exists());

    // `--force` does not let it through either.
    let forced = repo.ream(&["generate:update-key", "--force"]);
    assert!(!forced.status.success(), "{forced:?}");
    assert!(!repo.join("public/app.js.map").exists());
}

/// A link to an existing file is refused too, and the file is untouched.
#[test]
fn a_key_link_to_an_existing_file_leaves_it_alone() {
    let repo = Repo::new("existing-target");
    std::fs::write(repo.join("public/notes.txt"), "public notes\n").unwrap();
    symlink("public/notes.txt", repo.join("robledo.key")).unwrap();

    let output = repo.ream(&["generate:update-key", "--force"]);
    assert!(!output.status.success(), "{output:?}");
    assert_eq!(
        std::fs::read_to_string(repo.join("public/notes.txt")).unwrap(),
        "public notes\n"
    );
}

/// Created owner-only, and kept from being replaced without `--force`.
#[test]
fn a_new_key_is_owner_only_and_not_replaced_without_force() {
    let repo = Repo::new("fresh");
    let output = repo.ream(&["generate:update-key"]);
    assert!(output.status.success(), "{output:?}");
    let key = repo.join("robledo.key");
    assert_eq!(mode_of(&key), 0o600);
    let written = std::fs::read_to_string(&key).unwrap();

    let again = repo.ream(&["generate:update-key"]);
    assert!(!again.status.success(), "{again:?}");
    assert!(stderr(&again).contains("--force"), "{again:?}");
    assert_eq!(std::fs::read_to_string(&key).unwrap(), written);
}

/// `--force` over a file other accounts can read: the new key is not left
/// readable by them, which is what the success message claims.
#[test]
fn force_over_a_world_readable_key_restricts_it() {
    let repo = Repo::new("force-mode");
    let key = repo.join("robledo.key");
    std::fs::write(&key, "old\n").unwrap();
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644)).unwrap();

    let output = repo.ream(&["generate:update-key", "--force"]);
    assert!(output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("readable by you only"),
        "{output:?}"
    );
    assert_eq!(mode_of(&key), 0o600);
    let written = std::fs::read_to_string(&key).unwrap();
    assert_ne!(written, "old\n");
    assert_eq!(written.trim().len(), 44, "a base64 32-byte seed: {written}");
}

/// `.mcp.json` committed as a link to another JSON file of the developer:
/// `install` refuses it and that file keeps its content.
#[test]
fn mcp_install_refuses_a_linked_config() {
    let repo = Repo::new("mcp-link");
    let elsewhere = repo.join("client.json");
    let original = r#"{"mcpServers":{"other":{"command":"x"}},"theme":"dark"}"#;
    std::fs::write(&elsewhere, original).unwrap();
    symlink("client.json", repo.join(".mcp.json")).unwrap();

    let output = repo.ream(&["mcp", "install"]);
    assert!(!output.status.success(), "{output:?}");
    assert!(stderr(&output).contains("symbolic link"), "{output:?}");
    assert_eq!(std::fs::read_to_string(&elsewhere).unwrap(), original);

    let removed = repo.ream(&["mcp", "uninstall"]);
    assert!(!removed.status.success(), "{removed:?}");
    assert_eq!(std::fs::read_to_string(&elsewhere).unwrap(), original);
}
