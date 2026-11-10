//! Integration tests for `ream build`.
//!
//! Hermetic: the PATH is reduced to a directory of stand-in programs, so what
//! is checked is WHICH program the CLI starts, never a real compiler.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_ream"))
}

struct TempProject {
    path: PathBuf,
}

impl TempProject {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "ream-build-test-{}-{}-{}",
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

    /// A shell script; `{root}` in the body is the project's absolute path, so
    /// the script needs no `dirname` from a PATH the test has emptied.
    fn script(&self, rel: &str, body: &str) {
        let body = body.replace("{root}", &self.path.to_string_lossy());
        self.write(rel, &format!("#!/bin/sh\n{body}\n"));
        std::fs::set_permissions(self.path.join(rel), std::fs::Permissions::from_mode(0o755))
            .expect("chmod");
    }

    fn exists(&self, rel: &str) -> bool {
        self.path.join(rel).exists()
    }

    fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.path.join(rel)).unwrap_or_default()
    }

    /// `ream build` with `stubs/` first on PATH, then only the system
    /// directories the stand-in compiler needs `mkdir` from — no user-level
    /// `tsc` or `npx` can answer.
    fn build(&self, args: &[&str]) -> Output {
        self.build_in(&self.path, args)
    }

    fn build_in(&self, dir: &Path, args: &[&str]) -> Output {
        std::fs::create_dir_all(self.path.join("stubs")).expect("stubs dir");
        cli()
            .arg("build")
            .args(args)
            .current_dir(dir)
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.path.join("stubs").display()),
            )
            .output()
            .expect("spawn ream build")
    }
}

impl Drop for TempProject {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn text(output: &Output) -> String {
    format!(
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn npx_trap(p: &TempProject) {
    // `npx tsc` without a local typescript downloads the `tsc` package from
    // the registry and runs it. Any call at all is the bug.
    p.script("stubs/npx", r#"echo "$@" >> "{root}/npx.calls"; exit 0"#);
}

#[test]
fn without_typescript_the_build_says_so_and_never_reaches_npx() {
    let p = TempProject::new("no-typescript");
    npx_trap(&p);
    p.write("dist/stale.js", "old build");

    let output = p.build(&[]);
    assert!(!output.status.success(), "{}", text(&output));
    assert!(
        !p.exists("npx.calls"),
        "npx was started: {}\n{}",
        p.read("npx.calls"),
        text(&output)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("TypeScript is not installed") && stderr.contains("add -D typescript"),
        "{}",
        text(&output)
    );
    // Refused before anything was removed.
    assert!(p.exists("dist/stale.js"), "the previous build was deleted");
}

#[test]
fn the_projects_own_tsc_is_the_one_run() {
    let p = TempProject::new("local-tsc");
    npx_trap(&p);
    // Answers `--showConfig` with an outDir, and records every call.
    p.script(
        "node_modules/.bin/tsc",
        r#"echo "$@" >> "{root}/tsc.calls"
case "$1" in
  --showConfig) echo '{"compilerOptions":{"outDir":"./dist"}}' ;;
  *) mkdir -p dist && echo built > dist/main.js ;;
esac"#,
    );

    let output = p.build(&[]);
    assert!(output.status.success(), "{}", text(&output));
    assert!(
        !p.exists("npx.calls"),
        "npx was started: {}",
        p.read("npx.calls")
    );
    let calls = p.read("tsc.calls");
    assert!(calls.lines().any(|l| l == "--showConfig"), "{calls}");
    assert!(p.exists("dist/main.js"), "{}", text(&output));
    // The manifest rides along so the output can be installed on its own.
    assert!(p.exists("dist/package.json"));
}

#[test]
fn a_workspace_hoisted_tsc_is_found_from_a_package() {
    let p = TempProject::new("hoisted-tsc");
    npx_trap(&p);
    p.script(
        "node_modules/.bin/tsc",
        r#"echo "$@" >> "{root}/tsc.calls"
case "$1" in
  --showConfig) echo '{"compilerOptions":{"outDir":"./dist"}}' ;;
  *) mkdir -p dist ;;
esac"#,
    );
    let app = Path::new("apps/api");
    p.write(
        &app.join("package.json").to_string_lossy(),
        r#"{ "name": "api" }"#,
    );
    p.write(
        &app.join("node_modules/@swc-node/register/package.json")
            .to_string_lossy(),
        "{}",
    );

    let output = p.build_in(&p.path.join(app), &[]);
    assert!(output.status.success(), "{}", text(&output));
    assert!(!p.exists("npx.calls"));
    assert!(!p.read("tsc.calls").is_empty());
}

/// A compiler that emits, then reports a type error — what `tsc` does on a
/// project with one.
fn failing_tsc(p: &TempProject) {
    p.script(
        "node_modules/.bin/tsc",
        r#"echo "$@" >> "{root}/tsc.calls"
case "$1" in
  --showConfig) echo '{"compilerOptions":{"outDir":"./dist"}}' ;;
  *) mkdir -p dist && echo built > dist/main.js; echo "error TS2322" >&2; exit 2 ;;
esac"#,
    );
}

#[test]
fn a_type_error_fails_the_build_and_takes_the_output_with_it() {
    let p = TempProject::new("ts-error");
    failing_tsc(&p);
    let output = p.build(&[]);
    assert!(!output.status.success(), "{}", text(&output));
    assert!(
        !p.exists("dist"),
        "a half-built output was left: {}",
        text(&output)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--ignore-ts-errors"), "{}", text(&output));
}

#[test]
fn ignore_ts_errors_keeps_the_output_and_finishes_the_build() {
    let p = TempProject::new("ignore-ts-errors");
    failing_tsc(&p);
    let output = p.build(&["--ignore-ts-errors"]);
    assert!(output.status.success(), "{}", text(&output));
    assert!(p.exists("dist/main.js"), "{}", text(&output));
    assert!(p.exists("dist/package.json"), "the manifest step still ran");
}

#[test]
fn tsconfig_is_handed_to_every_tsc_call() {
    let p = TempProject::new("tsconfig");
    p.script(
        "node_modules/.bin/tsc",
        r#"echo "$@" >> "{root}/tsc.calls"
case "$*" in
  *--showConfig*) echo '{"compilerOptions":{"outDir":"./build"}}' ;;
  *) mkdir -p build ;;
esac"#,
    );
    p.write("build/stale.js", "old");
    let output = p.build(&["--tsconfig", "tsconfig.build.json"]);
    assert!(output.status.success(), "{}", text(&output));
    let calls = p.read("tsc.calls");
    assert_eq!(
        calls.lines().collect::<Vec<_>>(),
        [
            "--project tsconfig.build.json --showConfig",
            "--project tsconfig.build.json"
        ],
        "{calls}"
    );
    // The outDir of THAT config is what was emptied.
    assert!(!p.exists("build/stale.js"));
}
