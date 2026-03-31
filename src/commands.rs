//! Commands — spawn Node.js processes and show info.

use std::process::Command;

/// Spawn a Node.js command, forwarding stdio.
pub fn spawn_node(cmd: &str, args: &[&str]) -> Result<(), String> {
    // Check we're in a Ream project
    if !std::path::Path::new("package.json").exists() {
        return Err("Not in a Ream project (no package.json found)".to_string());
    }

    let status = Command::new(cmd)
        .args(args)
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status()
        .map_err(|e| format!("Failed to run '{}': {}", cmd, e))?;

    if !status.success() {
        return Err(format!("'{}' exited with code {}", cmd, status.code().unwrap_or(-1)));
    }

    Ok(())
}

/// Show version and environment info.
pub fn info() -> Result<(), String> {
    println!("ream {}", env!("CARGO_PKG_VERSION"));
    println!();

    // Node.js version
    match Command::new("node").arg("--version").output() {
        Ok(output) => {
            let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
            println!("  Node.js:  {}", version);
        }
        Err(_) => println!("  Node.js:  not found"),
    }

    // pnpm version
    match Command::new("pnpm").arg("--version").output() {
        Ok(output) => {
            let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
            println!("  pnpm:     {}", version);
        }
        Err(_) => println!("  pnpm:     not found"),
    }

    // Rust version
    match Command::new("rustc").arg("--version").output() {
        Ok(output) => {
            let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
            println!("  Rust:     {}", version);
        }
        Err(_) => println!("  Rust:     not found"),
    }

    // Check if in a Ream project
    if std::path::Path::new("reamrc.ts").exists() {
        println!();
        println!("  Project:  reamrc.ts found (framework mode)");
    } else if std::path::Path::new("package.json").exists() {
        println!();
        println!("  Project:  package.json found (toolkit mode)");
    }

    Ok(())
}
