//! Integration smoke test for `ream nova:vapid:generate`.
//!
//! Verifies that the subcommand is wired into the CLI binary and that it
//! refuses to run outside a Ream project (no `package.json` present).
//! End-to-end VAPID generation (which spawns Node + @c9up/nova) is covered
//! by the TypeScript suite in `packages/nova/tests/unit/vapid.test.ts`.

use std::process::Command;

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_ream"))
}

#[test]
fn nova_vapid_generate_subcommand_is_registered() {
    let output = cli()
        .arg("--help")
        .output()
        .expect("Failed to run ream --help");
    assert!(output.status.success(), "ream --help should succeed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("nova:vapid:generate"),
        "subcommand `nova:vapid:generate` missing from --help output:\n{}",
        stdout
    );
}

#[test]
fn nova_vapid_generate_refuses_without_package_json() {
    let temp = std::env::temp_dir().join(format!(
        "ream-nova-vapid-test-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&temp).expect("create temp dir");

    let output = cli()
        .arg("nova:vapid:generate")
        .current_dir(&temp)
        .output()
        .expect("Failed to invoke ream nova:vapid:generate");

    let _ = std::fs::remove_dir_all(&temp);

    assert!(
        !output.status.success(),
        "Expected non-zero exit when run outside a Ream project"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let combined = format!("{}{}", stdout, stderr);
    assert!(
        combined.contains("Not in a Ream project") || combined.contains("package.json"),
        "Expected a 'no package.json' error, got:\nstdout: {}\nstderr: {}",
        stdout,
        stderr
    );
}
