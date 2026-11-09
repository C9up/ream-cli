//! Integration tests for `ream new`.
//!
//! The first test is hermetic: a stand-in `pnpm` on the PATH decides how the
//! install ends, so what is checked is how the CLI reports it and what it wrote
//! before it. The second scaffolds a real project, installs it from the
//! registry and runs its own suites — it needs pnpm and the network, so it is
//! `#[ignore]`d and CI runs it with `--ignored`.

use std::path::{Path, PathBuf};
use std::process::Command;

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_ream"))
}

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "ream-new-test-{}-{}-{}",
            label,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// A `pnpm install` that fails is the command failing. It used to print a
/// yellow warning, then "Done!", and exit 0 — so a script that ran `ream new`
/// went on as if it had a working project.
#[cfg(unix)]
#[test]
fn a_failed_install_fails_the_command() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new("install-fails");
    let bin = dir.path.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let pnpm = bin.join("pnpm");
    std::fs::write(&pnpm, "#!/bin/sh\nexit 3\n").unwrap();
    std::fs::set_permissions(&pnpm, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let output = cli()
        .args(["new", "demo", "--template", "api", "--db", "sqlite", "-y"])
        .current_dir(&dir.path)
        .env("PATH", path)
        .output()
        .expect("spawn ream new");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        !output.status.success(),
        "a failed install must fail `ream new` — stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(stderr.contains("pnpm install failed"), "{stderr}");
    assert!(!stdout.contains("Done!"), "{stdout}");

    // The project is still written, so the message can send the user to it.
    let project = dir.path.join("demo");
    let workspace = read(&project.join("pnpm-workspace.yaml"));
    assert!(
        workspace.contains("allowBuilds:\n  '@swc/core': true"),
        "the build pnpm 11+ refuses to leave unapproved must be approved — got:\n{workspace}"
    );
    assert!(
        workspace.contains("onlyBuiltDependencies:\n  - '@swc/core'"),
        "pnpm 10 reads the same approval under its own key — got:\n{workspace}"
    );

    let manifest = read(&project.join("package.json"));
    assert!(manifest.contains("\"version\": \"0.1.0\""), "{manifest}");
    assert!(
        manifest.contains(&format!(
            "\"@c9up/ream-cli\": \"^{}\"",
            env!("CARGO_PKG_VERSION")
        )),
        "the scripts call `ream`, so the project must carry the CLI that wrote it — got:\n{manifest}"
    );
}

/// A project fresh out of `ream new` installs, and passes its own tests.
///
/// The generated `tests/bootstrap.ts` used to call `configure()` at import
/// time. helix had already called it with the suite's name, so the hook only
/// ever saw a suite named `default`, the `functional` server never started, and
/// the first test failed with E_SERVER_NOT_STARTED. The install, before that,
/// stopped on ERR_PNPM_IGNORED_BUILDS. Neither shows up in the text of a file.
#[test]
#[ignore = "needs pnpm and the npm registry; CI runs it with --ignored"]
fn a_new_project_installs_and_passes_its_own_tests() {
    for template in ["api", "web"] {
        let dir = TempDir::new(&format!("e2e-{template}"));
        let created = cli()
            .args([
                "new",
                "demo",
                "--template",
                template,
                "--db",
                "sqlite",
                "-y",
            ])
            .current_dir(&dir.path)
            .output()
            .expect("spawn ream new");
        assert!(
            created.status.success(),
            "{template}: `ream new` failed — stdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&created.stdout),
            String::from_utf8_lossy(&created.stderr)
        );

        let tested = cli()
            .arg("test")
            .current_dir(dir.path.join("demo"))
            .output()
            .expect("spawn ream test");
        let stdout = String::from_utf8_lossy(&tested.stdout);
        let stderr = String::from_utf8_lossy(&tested.stderr);
        assert!(
            tested.status.success(),
            "{template}: a fresh project fails its own tests — stdout:\n{stdout}\nstderr:\n{stderr}"
        );
        let report = format!("{stdout}{stderr}");
        assert!(
            report.contains("1 passed") && report.contains("0 failed"),
            "{template}: the root route test must run and pass — got:\n{report}"
        );
    }
}
