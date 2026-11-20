//! `ream generate:key` and `.env.example`: the variable is recorded there, the
//! secret never is.

use std::path::PathBuf;
use std::process::Command;

struct TempProject {
    path: PathBuf,
}

impl TempProject {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "ream-generate-key-test-{}-{}-{}",
            label,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&path).expect("create temp dir");
        std::fs::write(path.join("package.json"), r#"{ "name": "fixture" }"#).unwrap();
        Self { path }
    }

    fn generate(&self, args: &[&str]) {
        let output = self.run(args);
        assert!(output.status.success(), "{output:?}");
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_ream"))
            .arg("generate:key")
            .args(args)
            .current_dir(&self.path)
            .env_remove("NODE_ENV")
            .output()
            .expect("spawn ream generate:key")
    }

    fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.path.join(rel)).unwrap_or_default()
    }
}

impl Drop for TempProject {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn key_in(text: &str) -> Option<String> {
    text.lines()
        .find_map(|line| line.strip_prefix("APP_KEY="))
        .map(str::to_string)
}

#[test]
fn the_example_records_the_variable_empty() {
    let p = TempProject::new("records");
    std::fs::write(
        p.path.join(".env.example"),
        "PORT=3000\nAPP_KEY=leaked-before\n",
    )
    .unwrap();

    p.generate(&[]);
    let key = key_in(&p.read(".env")).expect("APP_KEY in .env");
    assert!(key.len() >= 32, "{key}");
    let example = p.read(".env.example");
    assert_eq!(example, "PORT=3000\nAPP_KEY=\n");

    // A forced rotation does not reach the example either.
    p.generate(&["--force"]);
    let rotated = key_in(&p.read(".env")).unwrap();
    assert_ne!(rotated, key);
    assert_eq!(p.read(".env.example"), "PORT=3000\nAPP_KEY=\n");
}

#[test]
fn a_missing_variable_is_added_to_the_example() {
    let p = TempProject::new("adds");
    std::fs::write(p.path.join(".env.example"), "PORT=3000\n").unwrap();
    p.generate(&[]);
    assert_eq!(p.read(".env.example"), "PORT=3000\nAPP_KEY=\n");
}

#[test]
fn no_example_is_created_where_the_project_has_none() {
    let p = TempProject::new("none");
    p.generate(&[]);
    assert!(!p.path.join(".env.example").exists());
}

#[cfg(unix)]
fn mode_of(path: &std::path::Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).expect("stat").permissions().mode() & 0o777
}

/// The file holds the key that signs every cookie and session: no other
/// local account may read it, whatever the umask says.
#[cfg(unix)]
#[test]
fn a_created_env_is_readable_by_its_owner_only() {
    let p = TempProject::new("mode-created");
    p.generate(&[]);
    assert_eq!(mode_of(&p.path.join(".env")), 0o600);
}

/// An existing file keeps the mode its owner gave it.
#[cfg(unix)]
#[test]
fn an_existing_env_keeps_its_mode() {
    use std::os::unix::fs::PermissionsExt;
    let p = TempProject::new("mode-kept");
    let env = p.path.join(".env");
    std::fs::write(&env, "PORT=3000\n").unwrap();
    std::fs::set_permissions(&env, std::fs::Permissions::from_mode(0o640)).unwrap();
    p.generate(&[]);
    assert_eq!(mode_of(&env), 0o640);
    assert!(key_in(&p.read(".env")).is_some());
}

/// A cloned repository controls both names: a link must not carry the write
/// into a file elsewhere in the user's account.
#[cfg(unix)]
#[test]
fn a_symlinked_env_is_refused_and_its_target_untouched() {
    let p = TempProject::new("link-env");
    let victim = p.path.join("victim");
    std::fs::write(&victim, "Host *\n  ServerAliveInterval 60\n").unwrap();
    std::os::unix::fs::symlink(&victim, p.path.join(".env")).unwrap();

    let output = p.run(&[]);
    assert!(!output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains(".env"));
    assert_eq!(p.read("victim"), "Host *\n  ServerAliveInterval 60\n");
}

/// The example is checked before `.env` is written, so a refusal writes
/// nothing at all.
#[cfg(unix)]
#[test]
fn a_symlinked_example_is_refused_before_anything_is_written() {
    let p = TempProject::new("link-example");
    let victim = p.path.join("victim");
    std::fs::write(&victim, "export PATH=/usr/bin\n").unwrap();
    std::os::unix::fs::symlink(&victim, p.path.join(".env.example")).unwrap();

    let output = p.run(&[]);
    assert!(!output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains(".env.example"));
    assert_eq!(p.read("victim"), "export PATH=/usr/bin\n");
    assert!(!p.path.join(".env").exists());
}

/// Not a regular file: a FIFO would block the read forever, a directory
/// cannot be written.
#[cfg(unix)]
#[test]
fn an_env_that_is_not_a_regular_file_is_refused() {
    let p = TempProject::new("not-regular");
    std::fs::create_dir(p.path.join(".env")).unwrap();
    let output = p.run(&[]);
    assert!(!output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("not a regular file"));
}
