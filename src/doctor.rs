//! Doctor — environment health checks (pure Rust, instant).

use std::path::Path;
use std::process::Command;

struct Check {
    name: &'static str,
    status: Status,
    message: String,
    fix: Option<String>,
}

enum Status { Pass, Warn, Fail }

pub fn run() -> Result<(), String> {
    println!("\n  \x1b[1mReam Doctor\x1b[0m\n");

    let checks = vec![
        check_node_version(),
        check_pnpm(),
        check_env_file(),
        check_reamrc(),
        check_package_json(),
        check_tsconfig(),
    ];

    let mut passed = 0;
    let mut warns = 0;
    let mut fails = 0;

    for check in &checks {
        let icon = match check.status {
            Status::Pass => { passed += 1; "\x1b[32m[OK]\x1b[0m" }
            Status::Warn => { warns += 1; "\x1b[33m[!!]\x1b[0m" }
            Status::Fail => { fails += 1; "\x1b[31m[XX]\x1b[0m" }
        };
        println!("  {} {}: {}", icon, check.name, check.message);
        if let Some(ref fix) = check.fix {
            println!("      Fix: {}", fix);
        }
    }

    println!("\n  {} passed, {} warnings, {} failed\n", passed, warns, fails);

    if fails > 0 {
        Err(format!("{} check(s) failed", fails))
    } else {
        Ok(())
    }
}

fn check_node_version() -> Check {
    match Command::new("node").arg("--version").output() {
        Ok(output) => {
            let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let major: u32 = version.trim_start_matches('v').split('.').next()
                .and_then(|s| s.parse().ok()).unwrap_or(0);
            if major >= 22 {
                Check { name: "Node.js", status: Status::Pass, message: format!("{} (>= 22 required)", version), fix: None }
            } else if major >= 20 {
                Check { name: "Node.js", status: Status::Warn, message: format!("{} — Node.js 22+ recommended", version), fix: Some("Install Node.js 22 LTS".to_string()) }
            } else {
                Check { name: "Node.js", status: Status::Fail, message: format!("{} — Node.js 22+ required", version), fix: Some("Install Node.js 22 LTS: https://nodejs.org/".to_string()) }
            }
        }
        Err(_) => Check { name: "Node.js", status: Status::Fail, message: "not found".to_string(), fix: Some("Install Node.js 22 LTS: https://nodejs.org/".to_string()) },
    }
}

fn check_pnpm() -> Check {
    match Command::new("pnpm").arg("--version").output() {
        Ok(output) => {
            let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
            Check { name: "pnpm", status: Status::Pass, message: version, fix: None }
        }
        Err(_) => Check { name: "pnpm", status: Status::Warn, message: "not found".to_string(), fix: Some("Install: npm install -g pnpm".to_string()) },
    }
}

fn check_env_file() -> Check {
    if Path::new(".env").exists() {
        Check { name: ".env", status: Status::Pass, message: "found".to_string(), fix: None }
    } else {
        Check { name: ".env", status: Status::Warn, message: "not found".to_string(), fix: Some("Create a .env file".to_string()) }
    }
}

fn check_reamrc() -> Check {
    if Path::new("reamrc.ts").exists() {
        Check { name: "reamrc.ts", status: Status::Pass, message: "found (framework mode)".to_string(), fix: None }
    } else {
        Check { name: "reamrc.ts", status: Status::Warn, message: "not found (toolkit mode)".to_string(), fix: None }
    }
}

fn check_package_json() -> Check {
    if !Path::new("package.json").exists() {
        return Check { name: "package.json", status: Status::Fail, message: "not found".to_string(), fix: Some("Run: pnpm init".to_string()) };
    }
    match std::fs::read_to_string("package.json") {
        Ok(content) => {
            if content.contains("@c9up/ream") {
                Check { name: "package.json", status: Status::Pass, message: "@c9up/ream found".to_string(), fix: None }
            } else {
                Check { name: "package.json", status: Status::Warn, message: "@c9up/ream not in dependencies".to_string(), fix: Some("Run: pnpm add @c9up/ream".to_string()) }
            }
        }
        Err(_) => Check { name: "package.json", status: Status::Fail, message: "unreadable".to_string(), fix: None },
    }
}

fn check_tsconfig() -> Check {
    if !Path::new("tsconfig.json").exists() {
        return Check { name: "tsconfig.json", status: Status::Warn, message: "not found".to_string(), fix: Some("Create tsconfig.json".to_string()) };
    }
    match std::fs::read_to_string("tsconfig.json") {
        Ok(content) => {
            let has_decorators = content.contains("experimentalDecorators");
            let has_metadata = content.contains("emitDecoratorMetadata");
            if has_decorators && has_metadata {
                Check { name: "tsconfig.json", status: Status::Pass, message: "decorators enabled".to_string(), fix: None }
            } else {
                Check { name: "tsconfig.json", status: Status::Warn, message: "missing decorator config".to_string(), fix: Some("Add experimentalDecorators and emitDecoratorMetadata".to_string()) }
            }
        }
        Err(_) => Check { name: "tsconfig.json", status: Status::Fail, message: "unreadable".to_string(), fix: None },
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
}
