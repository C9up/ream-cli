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

/// Run a migration command via Node.js inline script.
/// Boots the app (providers + config), then runs the migration action.
pub fn run_migration(action: &str) -> Result<(), String> {
    if !std::path::Path::new("package.json").exists() {
        return Err("Not in a Ream project (no package.json found)".to_string());
    }

    let script = match action {
        "migrate" => r#"
            import 'reflect-metadata';
            import { Ignitor } from '@c9up/ream';
            const rc = (await import('./reamrc.ts')).default;
            const app = await new Ignitor(new URL('./', import.meta.url))
                .useRcFile(rc).setEnvironment('console').start();
            console.log('Migrations applied.');
            await app.stop();
        "#,
        "migrate:rollback" => r#"
            import 'reflect-metadata';
            import { Ignitor } from '@c9up/ream';
            const rc = (await import('./reamrc.ts')).default;
            const app = await new Ignitor(new URL('./', import.meta.url))
                .useRcFile(rc).setEnvironment('console').start();
            const db = app.getApp().container.resolve('db');
            const last = db.prepare("SELECT MAX(batch) as b FROM _migrations WHERE batch IS NOT NULL").get();
            if (last?.b) {
                const rows = db.prepare("SELECT name FROM _migrations WHERE batch = ?").all(last.b);
                for (const row of rows.reverse()) {
                    const mod = await import('./database/migrations/' + row.name + '.ts');
                    const m = new mod.default('sqlite');
                    for (const sql of await m.getDownSQL()) db.exec(sql);
                    db.prepare("DELETE FROM _migrations WHERE name = ?").run(row.name);
                    console.log('  rolled back:', row.name);
                }
            } else {
                console.log('  Nothing to rollback.');
            }
            await app.stop();
        "#,
        "migrate:status" => r#"
            import 'reflect-metadata';
            import { Ignitor } from '@c9up/ream';
            const rc = (await import('./reamrc.ts')).default;
            const app = await new Ignitor(new URL('./', import.meta.url))
                .useRcFile(rc).setEnvironment('console').start();
            const db = app.getApp().container.resolve('db');
            const rows = db.prepare("SELECT name, executed_at FROM _migrations ORDER BY name").all();
            if (rows.length === 0) { console.log('  No migrations found.'); }
            else { for (const r of rows) console.log('  applied:', r.name, '(' + r.executed_at + ')'); }
            await app.stop();
        "#,
        _ => return Err(format!("Unknown migration action: {}", action)),
    };

    let status = Command::new("node")
        .args(&["--import", "@swc-node/register/esm-register", "--input-type=module", "-e", script])
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status()
        .map_err(|e| format!("Failed to run migration: {}", e))?;

    if !status.success() {
        return Err(format!("Migration failed with code {}", status.code().unwrap_or(-1)));
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
