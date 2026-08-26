//! Doctor — environment health checks (pure Rust, instant).

use std::path::Path;
use std::process::Command;

struct Check {
    name: &'static str,
    status: Status,
    message: String,
    fix: Option<String>,
}

enum Status {
    Pass,
    Warn,
    Fail,
}

pub fn run() -> Result<(), String> {
    println!("\n  \x1b[1mReam Doctor\x1b[0m\n");

    let checks = vec![
        check_node_version(),
        check_pnpm(),
        check_env_file(),
        check_reamrc(),
        check_package_json(),
        check_tsconfig(),
        check_ts_loader(),
    ];

    let mut passed = 0;
    let mut warns = 0;
    let mut fails = 0;

    for check in &checks {
        let icon = match check.status {
            Status::Pass => {
                passed += 1;
                "\x1b[32m[OK]\x1b[0m"
            }
            Status::Warn => {
                warns += 1;
                "\x1b[33m[!!]\x1b[0m"
            }
            Status::Fail => {
                fails += 1;
                "\x1b[31m[XX]\x1b[0m"
            }
        };
        println!("  {} {}: {}", icon, check.name, check.message);
        if let Some(ref fix) = check.fix {
            println!("      Fix: {}", fix);
        }
    }

    println!(
        "\n  {} passed, {} warnings, {} failed\n",
        passed, warns, fails
    );

    if fails > 0 {
        Err(format!("{} check(s) failed", fails))
    } else {
        Ok(())
    }
}

fn check_node_version() -> Check {
    match Command::new("node").arg("--version").output() {
        // Gate on exit status (mirrors commands.rs): a broken node shim that
        // exits non-zero with empty stdout would otherwise parse major=0 and
        // report a confusing Fail with an empty version string (audit 2026-06-13).
        Ok(output) if output.status.success() => {
            let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let major: u32 = version
                .trim_start_matches('v')
                .split('.')
                .next()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            if major >= 22 {
                Check {
                    name: "Node.js",
                    status: Status::Pass,
                    message: format!("{} (>= 22 required)", version),
                    fix: None,
                }
            } else if major >= 20 {
                Check {
                    name: "Node.js",
                    status: Status::Warn,
                    message: format!("{} — Node.js 22+ recommended", version),
                    fix: Some("Install Node.js 22 LTS".to_string()),
                }
            } else {
                Check {
                    name: "Node.js",
                    status: Status::Fail,
                    message: format!("{} — Node.js 22+ required", version),
                    fix: Some("Install Node.js 22 LTS: https://nodejs.org/".to_string()),
                }
            }
        }
        Ok(_) => Check {
            name: "Node.js",
            status: Status::Fail,
            message: "`node --version` exited non-zero — check your Node install or shim"
                .to_string(),
            fix: Some("Install Node.js 22 LTS: https://nodejs.org/".to_string()),
        },
        Err(_) => Check {
            name: "Node.js",
            status: Status::Fail,
            message: "not found".to_string(),
            fix: Some("Install Node.js 22 LTS: https://nodejs.org/".to_string()),
        },
    }
}

fn check_pnpm() -> Check {
    match Command::new("pnpm").arg("--version").output() {
        Ok(output) => {
            let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
            Check {
                name: "pnpm",
                status: Status::Pass,
                message: version,
                fix: None,
            }
        }
        Err(_) => Check {
            name: "pnpm",
            status: Status::Warn,
            message: "not found".to_string(),
            fix: Some("Install: npm install -g pnpm".to_string()),
        },
    }
}

fn check_env_file() -> Check {
    if Path::new(".env").exists() {
        Check {
            name: ".env",
            status: Status::Pass,
            message: "found".to_string(),
            fix: None,
        }
    } else {
        Check {
            name: ".env",
            status: Status::Warn,
            message: "not found".to_string(),
            fix: Some("Create a .env file".to_string()),
        }
    }
}

fn check_reamrc() -> Check {
    if Path::new("reamrc.ts").exists() {
        Check {
            name: "reamrc.ts",
            status: Status::Pass,
            message: "found (framework mode)".to_string(),
            fix: None,
        }
    } else {
        Check {
            name: "reamrc.ts",
            status: Status::Warn,
            message: "not found (toolkit mode)".to_string(),
            fix: None,
        }
    }
}

fn check_package_json() -> Check {
    if !Path::new("package.json").exists() {
        return Check {
            name: "package.json",
            status: Status::Fail,
            message: "not found".to_string(),
            fix: Some("Run: pnpm init".to_string()),
        };
    }
    match std::fs::read_to_string("package.json") {
        Ok(content) => {
            if content.contains("@c9up/ream") {
                Check {
                    name: "package.json",
                    status: Status::Pass,
                    message: "@c9up/ream found".to_string(),
                    fix: None,
                }
            } else {
                Check {
                    name: "package.json",
                    status: Status::Warn,
                    message: "@c9up/ream not in dependencies".to_string(),
                    fix: Some("Run: pnpm add @c9up/ream".to_string()),
                }
            }
        }
        Err(_) => Check {
            name: "package.json",
            status: Status::Fail,
            message: "unreadable".to_string(),
            fix: None,
        },
    }
}

fn check_tsconfig() -> Check {
    if !Path::new("tsconfig.json").exists() {
        return Check {
            name: "tsconfig.json",
            status: Status::Warn,
            message: "not found".to_string(),
            fix: Some("Create tsconfig.json".to_string()),
        };
    }
    match std::fs::read_to_string("tsconfig.json") {
        Ok(content) => {
            let uncommented: String = content
                .lines()
                .filter(|l| !l.trim_start().starts_with("//"))
                .collect::<Vec<_>>()
                .join("\n");
            let has_decorators = uncommented.contains("experimentalDecorators");
            let has_metadata = uncommented.contains("emitDecoratorMetadata");
            // The scaffolded app inherits decorators from the framework base
            // (`extends: @c9up/ream/tsconfig.app.json`) rather than re-declaring
            // them, so an `extends` onto the ream base satisfies the check too.
            let extends_ream_base = uncommented.contains("@c9up/ream/tsconfig");
            if (has_decorators && has_metadata) || extends_ream_base {
                Check {
                    name: "tsconfig.json",
                    status: Status::Pass,
                    message: "decorators enabled".to_string(),
                    fix: None,
                }
            } else {
                Check {
                    name: "tsconfig.json",
                    status: Status::Warn,
                    message: "missing decorator config".to_string(),
                    fix: Some("Add experimentalDecorators and emitDecoratorMetadata".to_string()),
                }
            }
        }
        Err(_) => Check {
            name: "tsconfig.json",
            status: Status::Fail,
            message: "unreadable".to_string(),
            fix: None,
        },
    }
}

/// The TypeScript loader every app-booting command spawns Node with.
///
/// `ream dev`, `build`, `inspect` and `schedule:list` all run
/// `node --import @swc-node/register/esm-register`, resolved from the PROJECT's
/// node_modules — the CLI is a Rust binary and carries no JS dependencies of
/// its own. Without it those four commands die on Node's raw
/// `ERR_MODULE_NOT_FOUND` while the generators still work, so a project can
/// look healthy while two thirds of the CLI is unusable. That is the gap this
/// check exists to close.
fn check_ts_loader() -> Check {
    check_ts_loader_at(Path::new("."))
}

/// Rooted so a test can point it at a fixture instead of changing the process
/// working directory, which the whole test binary shares.
fn check_ts_loader_at(root: &Path) -> Check {
    const NAME: &str = "@swc-node/register";
    if root.join("node_modules/@swc-node/register").exists() {
        return Check {
            name: NAME,
            status: Status::Pass,
            message: "TypeScript loader present".to_string(),
            fix: None,
        };
    }
    // Declared but not installed is a different sentence: the manifest is right
    // and the tree is stale, so `pnpm install` is the fix, not `pnpm add`.
    let declared = std::fs::read_to_string(root.join("package.json"))
        .map(|c| c.contains("@swc-node/register"))
        .unwrap_or(false);
    if declared {
        return Check {
            name: NAME,
            status: Status::Fail,
            message: "declared but not installed".to_string(),
            fix: Some("Run `pnpm install`".to_string()),
        };
    }
    Check {
        name: NAME,
        status: Status::Fail,
        message: "missing — `ream dev`, `build`, `console`, `test`, `inspect`, `repl`, \
                  `migration:*` and `schedule:*` cannot run"
            .to_string(),
        fix: Some("Run `pnpm add -D @swc-node/register`".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_check_node_version() {
        let check = check_node_version();
        // Should at least not crash
        assert!(!check.message.is_empty());
    }

    #[test]
    fn test_check_pnpm() {
        let check = check_pnpm();
        assert!(!check.message.is_empty());
    }

    /// A unique directory under the system temp dir. No `tempfile` dependency:
    /// the CLI ships none and this is the only test that needs a fixture tree.
    fn fixture(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("ream-doctor-{}-{}", std::process::id(), name));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("fixture dir");
        dir
    }

    #[test]
    fn ts_loader_passes_when_installed() {
        let dir = fixture("installed");
        std::fs::create_dir_all(dir.join("node_modules/@swc-node/register")).unwrap();
        let check = check_ts_loader_at(&dir);
        assert!(matches!(check.status, Status::Pass));
        assert!(check.fix.is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn ts_loader_fails_when_absent_and_says_what_to_install() {
        let dir = fixture("absent");
        std::fs::write(dir.join("package.json"), r#"{"name":"app"}"#).unwrap();
        let check = check_ts_loader_at(&dir);
        assert!(matches!(check.status, Status::Fail));
        // Without this, `doctor` reports a healthy project while `ream dev`
        // dies on ERR_MODULE_NOT_FOUND — the case this check exists for.
        assert!(check
            .fix
            .unwrap()
            .contains("pnpm add -D @swc-node/register"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn ts_loader_tells_a_stale_tree_to_install_not_to_add() {
        let dir = fixture("declared");
        std::fs::write(
            dir.join("package.json"),
            r#"{"devDependencies":{"@swc-node/register":"^0.9.0"}}"#,
        )
        .unwrap();
        let check = check_ts_loader_at(&dir);
        assert!(matches!(check.status, Status::Fail));
        assert_eq!(check.fix.as_deref(), Some("Run `pnpm install`"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
