//! Integration tests for `ream add <pkg>`.
//!
//! Hermetic by construction: the `REAM_ADD_DRY_RUN=1` env var skips the
//! actual `<pm> add <pkg>` spawn and writes the would-be command line to the
//! file pointed to by `REAM_ADD_DRY_RUN_LOG`. That keeps PM-detection and
//! `--dev` shape assertions fast and avoids hitting the npm registry.
//!
//! Test hook contract is documented in `src/add.rs`.

use std::path::PathBuf;
use std::process::Command;

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_ream"))
}

/// Give the fixture a loadable `@swc-node/register`, built in place.
///
/// Every command that reaches the app spawns
/// `node --import @swc-node/register/esm-register`, so a fixture without it
/// dies on `ERR_MODULE_NOT_FOUND` before the code under test runs. This used to
/// be solved by symlinking a workspace `node_modules/` from a sibling
/// application — a path that does not exist in this repository at all, so the
/// two tests that needed it returned early and reported success without
/// asserting anything, in CI every time.
///
/// A no-op ESM module is enough: what these tests exercise is how the CLI
/// classifies the probe's exit, not what the loader compiles.
fn install_stub_ts_loader(p: &TempProject) {
    p.write(
        "node_modules/@swc-node/register/package.json",
        r#"{
  "name": "@swc-node/register",
  "version": "0.0.0-stub",
  "type": "module",
  "exports": { "./esm-register": "./esm-register.js" }
}
"#,
    );
    p.write(
        "node_modules/@swc-node/register/esm-register.js",
        "// A loader that registers nothing: the fixture's modules are plain JS.
",
    );
}

struct TempProject {
    path: PathBuf,
}

impl TempProject {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "ream-add-test-{}-{}-{}",
            label,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&path).expect("create temp project dir");
        Self { path }
    }

    fn write(&self, rel: &str, content: &str) {
        let abs = self.path.join(rel);
        if let Some(parent) = abs.parent() {
            std::fs::create_dir_all(parent).expect("create parent");
        }
        std::fs::write(&abs, content).expect("write file");
    }

    fn touch(&self, rel: &str) {
        self.write(rel, "");
    }

    fn dry_run_log(&self) -> PathBuf {
        self.path.join(".dry-run.log")
    }

    fn read_dry_run_log(&self) -> String {
        std::fs::read_to_string(self.dry_run_log()).unwrap_or_default()
    }
}

impl Drop for TempProject {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn ream_project(label: &str) -> TempProject {
    let p = TempProject::new(label);
    p.write("package.json", r#"{ "name": "fixture", "private": true }"#);
    p
}

#[test]
fn add_subcommand_is_registered_in_help() {
    let output = cli().arg("--help").output().expect("ream --help");
    assert!(output.status.success(), "ream --help should succeed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("add"),
        "subcommand `add` missing from --help:\n{}",
        stdout
    );
}

#[test]
fn add_help_lists_dev_and_force() {
    let output = cli()
        .args(["add", "--help"])
        .output()
        .expect("ream add --help");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("--dev"),
        "expected --dev in `ream add --help`:\n{}",
        stdout
    );
    assert!(
        stdout.contains("--force"),
        "expected --force in `ream add --help`:\n{}",
        stdout
    );
}

#[test]
fn add_refuses_outside_ream_project() {
    let temp = TempProject::new("no-pkg-json");
    let output = cli()
        .arg("add")
        .arg("@c9up/atlas")
        .current_dir(&temp.path)
        .output()
        .expect("ream add");
    assert!(!output.status.success(), "expected non-zero exit");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        combined.contains("Not in a Ream project"),
        "expected 'Not in a Ream project' error, got:\n{}",
        combined
    );
}

#[test]
fn add_errors_when_no_lockfile_present() {
    let p = ream_project("no-lockfile");
    let output = cli()
        .arg("add")
        .arg("@c9up/atlas")
        .current_dir(&p.path)
        .env("REAM_ADD_DRY_RUN", "1")
        .env("REAM_ADD_DRY_RUN_LOG", p.dry_run_log())
        .output()
        .expect("ream add");
    assert!(!output.status.success());
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        combined.contains("Couldn't detect package manager"),
        "expected 'Couldn't detect package manager', got:\n{}",
        combined
    );
}

fn run_add_dry(p: &TempProject, args: &[&str]) -> std::process::Output {
    cli()
        .arg("add")
        .args(args)
        .current_dir(&p.path)
        .env("REAM_ADD_DRY_RUN", "1")
        .env("REAM_ADD_DRY_RUN_LOG", p.dry_run_log())
        .output()
        .expect("ream add (dry-run)")
}

#[test]
fn pm_detection_pnpm() {
    let p = ream_project("pnpm");
    p.touch("pnpm-lock.yaml");
    let _ = run_add_dry(&p, &["@c9up/atlas"]);
    let log = p.read_dry_run_log();
    assert!(
        log.starts_with("pnpm add @c9up/atlas"),
        "expected pnpm install line, got: {:?}",
        log
    );
}

#[test]
fn pm_detection_yarn() {
    let p = ream_project("yarn");
    p.touch("yarn.lock");
    let _ = run_add_dry(&p, &["@c9up/atlas"]);
    let log = p.read_dry_run_log();
    assert!(
        log.starts_with("yarn add @c9up/atlas"),
        "expected yarn install line, got: {:?}",
        log
    );
}

#[test]
fn pm_detection_npm() {
    let p = ream_project("npm");
    p.touch("package-lock.json");
    let _ = run_add_dry(&p, &["@c9up/atlas"]);
    let log = p.read_dry_run_log();
    assert!(
        log.starts_with("npm install @c9up/atlas"),
        "expected npm install line, got: {:?}",
        log
    );
}

#[test]
fn dev_flag_pnpm_uses_dash_capital_d() {
    let p = ream_project("dev-pnpm");
    p.touch("pnpm-lock.yaml");
    let _ = run_add_dry(&p, &["@c9up/photon", "--dev"]);
    let log = p.read_dry_run_log();
    assert!(
        log.contains("pnpm add -D @c9up/photon"),
        "expected '-D' for pnpm dev install, got: {:?}",
        log
    );
}

#[test]
fn dev_flag_yarn_uses_dash_capital_d() {
    let p = ream_project("dev-yarn");
    p.touch("yarn.lock");
    let _ = run_add_dry(&p, &["@c9up/photon", "--dev"]);
    let log = p.read_dry_run_log();
    assert!(
        log.contains("yarn add -D @c9up/photon"),
        "expected '-D' for yarn dev install, got: {:?}",
        log
    );
}

#[test]
fn dev_flag_npm_uses_save_dev() {
    let p = ream_project("dev-npm");
    p.touch("package-lock.json");
    let _ = run_add_dry(&p, &["@c9up/photon", "--dev"]);
    let log = p.read_dry_run_log();
    assert!(
        log.contains("npm install --save-dev @c9up/photon"),
        "expected '--save-dev' for npm dev install, got: {:?}",
        log
    );
}

#[test]
fn multi_lockfile_precedence_pnpm_wins_over_npm() {
    let p = ream_project("multi-lockfile");
    p.touch("pnpm-lock.yaml");
    p.touch("package-lock.json");
    let output = run_add_dry(&p, &["@c9up/atlas"]);
    let log = p.read_dry_run_log();
    assert!(
        log.starts_with("pnpm add @c9up/atlas"),
        "expected pnpm precedence, got: {:?}",
        log
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("ignoring secondary lockfile package-lock.json"),
        "expected secondary-lockfile warning, got: {:?}",
        stderr
    );
}

#[test]
fn multi_lockfile_precedence_yarn_wins_over_npm() {
    let p = ream_project("multi-lockfile-yarn-npm");
    p.touch("yarn.lock");
    p.touch("package-lock.json");
    let output = run_add_dry(&p, &["@c9up/atlas"]);
    let log = p.read_dry_run_log();
    assert!(
        log.starts_with("yarn add @c9up/atlas"),
        "expected yarn precedence, got: {:?}",
        log
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("ignoring secondary lockfile package-lock.json"),
        "expected secondary-lockfile warning, got: {:?}",
        stderr
    );
}

#[test]
fn multi_lockfile_precedence_pnpm_wins_over_yarn() {
    let p = ream_project("multi-lockfile-pnpm-yarn");
    p.touch("pnpm-lock.yaml");
    p.touch("yarn.lock");
    let output = run_add_dry(&p, &["@c9up/atlas"]);
    let log = p.read_dry_run_log();
    assert!(
        log.starts_with("pnpm add @c9up/atlas"),
        "expected pnpm precedence, got: {:?}",
        log
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("ignoring secondary lockfile yarn.lock"),
        "expected secondary-lockfile warning, got: {:?}",
        stderr
    );
}

#[test]
fn dry_run_without_log_fails_loudly() {
    // P9 — REAM_ADD_DRY_RUN=1 without REAM_ADD_DRY_RUN_LOG must abort with a
    // clear error so a stray export in a developer's shell can't silently skip
    // every install.
    let p = ream_project("dry-run-no-log");
    p.touch("pnpm-lock.yaml");
    let output = cli()
        .arg("add")
        .arg("@c9up/atlas")
        .current_dir(&p.path)
        .env("REAM_ADD_DRY_RUN", "1")
        .env_remove("REAM_ADD_DRY_RUN_LOG")
        .output()
        .expect("ream add");
    assert!(!output.status.success(), "expected non-zero exit");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("REAM_ADD_DRY_RUN_LOG"),
        "expected env-var name in error, got: {:?}",
        stderr
    );
}

#[test]
fn rejects_positional_arg_and_short_circuits_install() {
    // Positional args after the package name are rejected by parse_flag_pairs,
    // which runs BEFORE the install dispatch — the dry-run log must stay
    // empty (no install line written).
    let p = ream_project("positional");
    p.touch("pnpm-lock.yaml");
    let output = run_add_dry(&p, &["@c9up/atlas", "extra-positional"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Unexpected positional argument"),
        "expected positional-arg error, got: {:?}",
        stderr
    );
    let log = p.read_dry_run_log();
    assert!(
        log.is_empty(),
        "install line should not be written when flag parsing fails, got: {:?}",
        log
    );
}

#[test]
fn missing_configure_hook_warns_not_errors_under_add() {
    // No package is actually installed (DRY_RUN skips the install). The
    // configure existence-check therefore fails and `ream add` downgrades to
    // a Note line + exit 0. This exercises the `ConfigureOutcome::NoHook`
    // path through `ream add` end-to-end.
    let p = ream_project("nohook");
    p.touch("pnpm-lock.yaml");
    install_stub_ts_loader(&p);
    let output = run_add_dry(&p, &["@community/something"]);
    assert!(
        output.status.success(),
        "ream add should exit 0 when configure hook is missing, got status={:?}\nstdout: {}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        combined.contains("has no configure() hook"),
        "expected 'has no configure() hook' note, got:\n{}",
        combined
    );
}

#[test]
fn configure_subcommand_errors_when_no_hook_export() {
    // Mirror of the above but for `ream configure`: a missing configure
    // export MUST exit 1 (the legacy contract preserved through the
    // configure_with_flags refactor).
    let p = ream_project("configure-nohook");
    install_stub_ts_loader(&p);
    let output = cli()
        .arg("configure")
        .arg("@community/something")
        .current_dir(&p.path)
        .output()
        .expect("ream configure");
    assert!(
        !output.status.success(),
        "ream configure should exit non-zero when configure hook is missing, stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("does not export a configure() function"),
        "expected configure-export error, got: {}",
        stderr
    );
}

#[test]
fn configure_subcommand_accepts_trailing_flags() {
    // No-hook path is fine for asserting the flag-parsing entry path: the
    // `parse_flag_pairs` step runs before the existence check, so a malformed
    // flag halts before the configure attempt.
    let p = ream_project("configure-flags");
    let output = cli()
        .arg("configure")
        .arg("@community/something")
        .arg("positional-not-allowed")
        .current_dir(&p.path)
        .output()
        .expect("ream configure");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Unexpected positional argument"),
        "expected positional-arg parse error, got: {}",
        stderr
    );
}
