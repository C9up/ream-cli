//! Integration tests for `ream template`, against a stand-in `git` that
//! records how it was called and fakes a clone.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output};

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "ream-template-test-{}-{}-{}",
            label,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(path.join("stubs")).expect("create temp dir");
        let git = path.join("stubs/git");
        // One argument per line, so a value holding a space stays one entry.
        // A clone creates its last argument with a `.git` inside, as git does.
        std::fs::write(
            &git,
            format!(
                "#!/bin/sh\nfor arg in \"$@\"; do printf '%s\\n' \"$arg\"; done >> '{calls}'\necho --- >> '{calls}'\n\
                 if [ \"$1\" = clone ]; then for last in \"$@\"; do :; done; mkdir -p \"$last/.git\"; \
                 echo '{{\"name\":\"kitchen-sink\"}}' > \"$last/package.json\"; fi\nexit 0\n",
                calls = path.join("git.calls").display()
            ),
        )
        .expect("write git stub");
        std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        Self { path }
    }

    fn ream(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_ream"))
            .args(args)
            .current_dir(&self.path)
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.path.join("stubs").display()),
            )
            .output()
            .expect("spawn ream template")
    }

    /// Each git invocation, as its list of arguments.
    fn calls(&self) -> Vec<Vec<String>> {
        std::fs::read_to_string(self.path.join("git.calls"))
            .unwrap_or_default()
            .split("---\n")
            .filter(|block| !block.is_empty())
            .map(|block| block.lines().map(str::to_string).collect())
            .collect()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[test]
fn a_destination_shaped_like_an_option_stays_a_path() {
    let dir = TempDir::new("option-dest");
    let output = dir.ream(&["template", "kitchen-sink", "--", "--template=/tmp/hooks"]);
    let calls = dir.calls();
    let clone = calls
        .iter()
        .find(|call| call.first().map(String::as_str) == Some("clone"))
        .unwrap_or_else(|| panic!("no clone: {calls:?}\n{output:?}"));

    let expected_dest = dir.path.join("--template=/tmp/hooks");
    assert_eq!(
        clone,
        &vec![
            "clone".to_string(),
            "--depth=1".to_string(),
            "--".to_string(),
            "https://github.com/C9up/kitchen-sink.git".to_string(),
            expected_dest.to_string_lossy().into_owned(),
        ],
        "{output:?}"
    );
    // The `.git` removal and the rename target what was cloned.
    assert!(output.status.success(), "{output:?}");
    assert!(!expected_dest.join(".git").exists());
    let manifest = std::fs::read_to_string(expected_dest.join("package.json")).unwrap();
    assert!(manifest.contains("\"name\": \"hooks\""), "{manifest}");
}

#[test]
fn the_default_destination_is_cloned_by_absolute_path() {
    let dir = TempDir::new("default-dest");
    let output = dir.ream(&["template", "kitchen-sink"]);
    assert!(output.status.success(), "{output:?}");
    let calls = dir.calls();
    assert_eq!(
        calls[0].last().map(String::as_str),
        Some(dir.path.join("kitchen-sink").to_string_lossy().as_ref())
    );
    assert_eq!(calls[0][2], "--");
    assert!(!dir.path.join("kitchen-sink/.git").exists());
}
