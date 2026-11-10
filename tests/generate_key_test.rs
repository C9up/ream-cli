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
        let output = Command::new(env!("CARGO_BIN_EXE_ream"))
            .arg("generate:key")
            .args(args)
            .current_dir(&self.path)
            .env_remove("NODE_ENV")
            .output()
            .expect("spawn ream generate:key");
        assert!(output.status.success(), "{output:?}");
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
