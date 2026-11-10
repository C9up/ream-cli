//! Inspecting a project must not run it.
//!
//! The fixture's console entry writes a `RAN_<argv>` marker whenever it is
//! executed, and its `commands/` declares the names of the binary's own
//! diagnostics. A user who clones a repository and types `ream --help`,
//! `ream`, `ream info` or `ream doctor` to see what it is has not asked to
//! run its code.

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

struct Fixture {
    path: PathBuf,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "ream-untrusted-test-{}-{}-{}",
            label,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(path.join("bin")).expect("create bin");
        std::fs::create_dir_all(path.join("commands")).expect("create commands");
        std::fs::write(
            path.join("package.json"),
            r#"{ "name": "cloned", "type": "module" }"#,
        )
        .expect("write package.json");
        std::fs::write(
            path.join("bin/console.js"),
            "import { writeFileSync } from 'node:fs'\n\
             writeFileSync('RAN_' + process.argv.slice(2).join('_').replace(/[:/]/g, '-'), 'x')\n\
             process.stdout.write('[]')\n",
        )
        .expect("write console entry");
        std::fs::write(
            path.join("commands/takeover.ts"),
            "export class A { static commandName = 'doctor' }\n\
             export class B { static commandName = 'info' }\n\
             export class C { static commandName = 'generate:key' }\n\
             export class D { static commandName = 'start' }\n",
        )
        .expect("write commands");
        Self { path }
    }

    fn ream(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_ream"))
            .args(args)
            .current_dir(&self.path)
            .stdin(Stdio::null())
            .output()
            .expect("spawn ream")
    }

    fn markers(&self) -> Vec<String> {
        let mut found: Vec<String> = std::fs::read_dir(&self.path)
            .expect("read fixture")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with("RAN_"))
            .collect();
        found.sort();
        found
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[test]
fn help_and_a_bare_ream_list_the_binary_without_running_the_project() {
    for args in [vec!["--help"], vec!["-h"], vec!["help"], vec![]] {
        let fixture = Fixture::new("help");
        let output = fixture.ream(&args);
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "{args:?}: {output:?}");
        assert_eq!(
            fixture.markers(),
            Vec::<String>::new(),
            "{args:?} ran the project"
        );
        assert!(stdout.contains("generate:key"), "{args:?}: {stdout}");
        // Where the project's own commands are, said rather than run.
        assert!(stdout.contains("ream list"), "{args:?}: {stdout}");
    }
}

#[test]
fn a_project_file_cannot_take_over_the_diagnostics_or_the_key_writer() {
    for args in [vec!["info"], vec!["doctor"], vec!["generate:key", "--show"]] {
        let fixture = Fixture::new("takeover");
        let output = fixture.ream(&args);
        assert_eq!(
            fixture.markers(),
            Vec::<String>::new(),
            "{args:?} ran the project: {output:?}"
        );
    }
}

#[test]
fn an_explicit_list_still_merges_the_projects_commands() {
    let fixture = Fixture::new("list");
    let output = fixture.ream(&["list"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(fixture.markers(), vec!["RAN_list_--json".to_string()]);
}

#[test]
fn a_command_that_boots_the_app_can_still_be_redefined() {
    // `start` runs the project anyway; an app that declares its own wins.
    let fixture = Fixture::new("start");
    let output = fixture.ream(&["start"]);
    assert_eq!(
        fixture.markers(),
        vec!["RAN_start".to_string()],
        "{output:?}"
    );
}
