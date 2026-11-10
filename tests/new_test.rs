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
        .env_remove("npm_config_user_agent")
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

    // The committed record of the variables, without the generated key.
    let example = read(&project.join(".env.example"));
    assert!(example.contains("APP_KEY=\n"), "{example}");
    assert!(example.contains("DB_CONNECTION=sqlite"), "{example}");
    assert!(
        !read(&project.join(".gitignore"))
            .lines()
            .any(|l| l == ".env.example"),
        ".env.example must be committed"
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

/// A directory of stand-in package managers and git, each recording
/// `<tool> <cwd> <args>` into `calls.log` and succeeding.
#[cfg(unix)]
fn recording_tools(dir: &Path) -> String {
    use std::os::unix::fs::PermissionsExt;
    let bin = dir.join("tools");
    std::fs::create_dir_all(&bin).unwrap();
    for tool in ["pnpm", "npm", "yarn", "git"] {
        let path = bin.join(tool);
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\necho \"{tool} $PWD $*\" >> '{}'\nexit 0\n",
                dir.join("calls.log").display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    )
}

#[cfg(unix)]
fn ream_new(cwd: &Path, path: &str, args: &[&str]) -> std::process::Output {
    cli()
        .arg("new")
        .args(args)
        .args(["--template", "api", "--db", "sqlite", "-y"])
        .current_dir(cwd)
        .env("PATH", path)
        .env_remove("npm_config_user_agent")
        .output()
        .expect("spawn ream new")
}

#[cfg(unix)]
fn calls(dir: &Path) -> Vec<String> {
    std::fs::read_to_string(dir.join("calls.log"))
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

/// A monorepo scaffolds into `apps/api`, named `api`.
#[cfg(unix)]
#[test]
fn the_destination_is_a_path_and_names_the_package_by_its_last_directory() {
    let dir = TempDir::new("path-dest");
    let path = recording_tools(&dir.path);
    let output = ream_new(&dir.path, &path, &["apps/api"]);
    assert!(output.status.success(), "{output:?}");

    let project = dir.path.join("apps/api");
    let manifest = read(&project.join("package.json"));
    assert!(manifest.contains("\"name\": \"api\""), "{manifest}");
    let project = std::fs::canonicalize(project).unwrap();
    assert_eq!(
        calls(&dir.path),
        [format!("pnpm {} install", project.display())]
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("cd apps/api"));
}

/// `ream new .` fills the empty directory it is typed in.
#[cfg(unix)]
#[test]
fn an_empty_directory_is_filled_in_place() {
    let dir = TempDir::new("dot");
    let here = dir.path.join("here");
    std::fs::create_dir_all(&here).unwrap();
    let path = recording_tools(&dir.path);
    let output = ream_new(&here, &path, &["."]);
    assert!(output.status.success(), "{output:?}");
    assert!(read(&here.join("package.json")).contains("\"name\": \"here\""));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("cd ."));
}

#[cfg(unix)]
#[test]
fn a_directory_with_anything_in_it_is_refused_untouched() {
    let dir = TempDir::new("not-empty");
    let taken = dir.path.join("taken");
    std::fs::create_dir_all(&taken).unwrap();
    std::fs::write(taken.join("notes.txt"), "mine").unwrap();
    let path = recording_tools(&dir.path);
    let output = ream_new(&dir.path, &path, &["taken"]);
    assert!(!output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("not empty"));
    assert!(!taken.join("package.json").exists());
    assert!(calls(&dir.path).is_empty());
}

#[cfg(unix)]
#[test]
fn pkg_chooses_the_installer_and_only_pnpm_gets_its_workspace_file() {
    for (pm, workspace) in [("npm", false), ("yarn", false), ("pnpm", true)] {
        let dir = TempDir::new(&format!("pkg-{pm}"));
        let path = recording_tools(&dir.path);
        let output = ream_new(&dir.path, &path, &["demo", "--pkg", pm]);
        assert!(output.status.success(), "{pm}: {output:?}");
        let project = std::fs::canonicalize(dir.path.join("demo")).unwrap();
        assert_eq!(
            calls(&dir.path),
            [format!("{pm} {} install", project.display())]
        );
        assert_eq!(
            project.join("pnpm-workspace.yaml").exists(),
            workspace,
            "{pm}"
        );
    }
}

#[cfg(unix)]
#[test]
fn an_unknown_pkg_is_refused_before_anything_is_written() {
    let dir = TempDir::new("pkg-bun");
    let path = recording_tools(&dir.path);
    let output = ream_new(&dir.path, &path, &["demo", "--pkg", "bun"]);
    assert!(!output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("pnpm"),
        "{output:?}"
    );
    assert!(!dir.path.join("demo").exists());
}

#[cfg(unix)]
#[test]
fn the_launching_package_manager_installs_when_none_is_named() {
    let dir = TempDir::new("user-agent");
    let path = recording_tools(&dir.path);
    let output = cli()
        .args(["new", "demo", "--template", "api", "--db", "sqlite", "-y"])
        .current_dir(&dir.path)
        .env("PATH", &path)
        .env(
            "npm_config_user_agent",
            "yarn/4.5.0 npm/? node/v22.0.0 linux x64",
        )
        .output()
        .expect("spawn ream new");
    assert!(output.status.success(), "{output:?}");
    assert!(
        calls(&dir.path)[0].starts_with("yarn "),
        "{:?}",
        calls(&dir.path)
    );
}

#[cfg(unix)]
#[test]
fn git_init_runs_in_the_project_before_the_install() {
    let dir = TempDir::new("git-init");
    let path = recording_tools(&dir.path);
    let output = ream_new(&dir.path, &path, &["demo", "--git-init"]);
    assert!(output.status.success(), "{output:?}");
    let project = std::fs::canonicalize(dir.path.join("demo")).unwrap();
    assert_eq!(
        calls(&dir.path),
        [
            format!("git {} init", project.display()),
            format!("pnpm {} install", project.display()),
        ]
    );
}

/// On Windows pnpm is a `.cmd` shim (npm, corepack), and Rust's `Command` only
/// appends `.exe` to a bare name — so `ream new` used to stop on "program not
/// found" with pnpm installed. The stand-in exits 3, so reaching it is proven by
/// the install being reported as FAILED rather than as missing.
#[cfg(windows)]
#[test]
fn a_cmd_shim_package_manager_is_started_on_windows() {
    let dir = TempDir::new("cmd-shim");
    let bin = dir.path.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("pnpm.cmd"), "@exit /b 3\r\n").unwrap();
    let path = format!(
        "{};{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let output = cli()
        .args(["new", "demo", "--template", "api", "--db", "sqlite", "-y"])
        .current_dir(&dir.path)
        .env("PATH", path)
        .output()
        .expect("spawn ream new");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!output.status.success(), "{stderr}");
    assert!(stderr.contains("pnpm install failed"), "{stderr}");
    assert!(!stderr.contains("not found"), "{stderr}");
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
