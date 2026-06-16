//! Commands — spawn Node.js processes and show info.

use std::process::{Command, ExitStatus, Stdio};

fn inherited_status(cmd: &str, args: &[&str]) -> Result<ExitStatus, String> {
    Command::new(cmd)
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|e| format!("Failed to run '{}': {}", cmd, e))
}

/// Spawn a Node.js command, forwarding stdio.
pub fn spawn_node(cmd: &str, args: &[&str]) -> Result<(), String> {
    // Check we're in a Ream project
    if !std::path::Path::new("package.json").exists() {
        return Err("Not in a Ream project (no package.json found)".to_string());
    }

    let status = inherited_status(cmd, args)?;

    if !status.success() {
        return Err(format!("'{}' exited with code {}", cmd, status.code().unwrap_or(-1)));
    }

    Ok(())
}

/// Args for `ream dev` — Node's native `--watch` + `@swc-node/register`.
///
/// swc-node reads `.swcrc` (which extends `@c9up/ream/swcrc.app.json`,
/// `decoratorMetadata: true`) and EMITS `design:paramtypes` — required for IoC
/// constructor injection. `tsx` / esbuild can NOT emit it, which silently broke
/// DI in dev (every injected dependency resolved to `undefined`).
pub fn dev_args() -> [&'static str; 4] {
    [
        "--import",
        "@swc-node/register/esm-register",
        "--watch",
        "bin/server.ts",
    ]
}

/// Run a migration command via Node.js inline script.
/// Boots the app, resolves db from the container, creates a MigrationRunner, and delegates.
pub fn run_migration(action: &str) -> Result<(), String> {
    if !std::path::Path::new("package.json").exists() {
        return Err("Not in a Ream project (no package.json found)".to_string());
    }
    if !std::path::Path::new("database/migrations").exists() {
        return Err("database/migrations/ directory not found — run 'ream make:migration' to create your first migration".to_string());
    }

    let runner_action = match action {
        "migrate" => r#"
            const executed = await runner.migrate();
            if (executed.length === 0) { console.log('  Nothing to migrate.'); }
            else { for (const n of executed) console.log('  migrated:', n); }
        "#,
        "migrate:rollback" => r#"
            const rolled = await runner.rollback();
            if (rolled.length === 0) { console.log('  Nothing to rollback.'); }
            else { for (const n of rolled) console.log('  rolled back:', n); }
        "#,
        "migrate:status" => r#"
            const statuses = await runner.status();
            if (statuses.length === 0) { console.log('  No migrations found.'); }
            else { for (const s of statuses) console.log(`  ${s.status === 'applied' ? '✓' : '○'} ${s.name}${s.batch ? ' (batch ' + s.batch + ')' : ''}`); }
        "#,
        _ => return Err(format!("Unknown migration action: {}", action)),
    };

    let script = format!(r#"
        import 'reflect-metadata';
        import {{ Ignitor }} from '@c9up/ream';
        import {{ MigrationRunner }} from '@c9up/atlas';
        const rc = (await import('./reamrc.ts')).default;
        const app = await new Ignitor(new URL('./', import.meta.url))
            .useRcFile(rc).setEnvironment('console').start();
        const db = app.getApp().container.resolve('db');
        const runner = new MigrationRunner(db, {{ migrationsDir: 'database/migrations', dialect: db.dialect }});
        {}
        await app.stop();
        // Force-exit: app.stop() doesn't close app-owned handles (atlas pool,
        // an ioredis client built when config loads), so the event loop would
        // otherwise stay alive and the one-shot command would hang (exit 124).
        process.exit(0);
    "#, runner_action);

    let status = inherited_status("node", &["--import", "@swc-node/register/esm-register", "--input-type=module", "-e", &script])?;

    if !status.success() {
        return Err(format!("Migration failed with code {}", status.code().unwrap_or(-1)));
    }

    Ok(())
}

/// Inspect: list registered routes, providers, and decorated services.
/// Boots the app in console mode and dumps an introspection summary to stdout.
pub fn run_inspect() -> Result<(), String> {
    if !std::path::Path::new("package.json").exists() {
        return Err("Not in a Ream project (no package.json found)".to_string());
    }
    if !std::path::Path::new("reamrc.ts").exists() {
        return Err("reamrc.ts not found — inspect requires a Ream framework project".to_string());
    }

    let script = r#"
        import 'reflect-metadata';
        import { Ignitor } from '@c9up/ream';
        const rc = (await import('./reamrc.ts')).default;
        const app = await new Ignitor(new URL('./', import.meta.url))
            .useRcFile(rc).setEnvironment('console').start();
        const router = app.getApp().container.resolve('router');

        console.log('\nRoutes:');
        const routes = router.getRoutes ? router.getRoutes() : [];
        if (routes.length === 0) {
            console.log('  (none)');
        } else {
            for (const r of routes) {
                const name = r.name ? `  [${r.name}]` : '';
                const guards = (r.guards?.length ?? 0) > 0 ? `  guards=${r.guards.join(',')}` : '';
                const roles = (r.roles?.length ?? 0) > 0 ? `  roles=${r.roles.join(',')}` : '';
                console.log(`  ${r.method.padEnd(6)} ${r.path}${name}${guards}${roles}`);
            }
        }

        console.log('\nProviders:');
        const providers = app.getApp().providers ?? [];
        for (const p of providers) {
            console.log('  -', p.constructor?.name ?? '(anonymous)');
        }

        console.log(`\nTotal: ${routes.length} routes, ${providers.length} providers.`);
        await app.stop();
        process.exit(0); // one-shot CLI: don't let app-owned handles keep it alive
    "#;

    let status = inherited_status("node", &["--import", "@swc-node/register/esm-register", "--input-type=module", "-e", script])?;

    if !status.success() {
        return Err(format!("Inspect failed with code {}", status.code().unwrap_or(-1)));
    }

    Ok(())
}

/// List registered scheduled tasks (Story 28.5).
pub fn run_schedule_list() -> Result<(), String> {
    if !std::path::Path::new("package.json").exists() {
        return Err("Not in a Ream project (no package.json found)".to_string());
    }
    if !std::path::Path::new("reamrc.ts").exists() {
        return Err("reamrc.ts not found — schedule:list requires a Ream framework project".to_string());
    }

    let script = r#"
        import 'reflect-metadata';
        import { Ignitor } from '@c9up/ream';
        const rc = (await import('./reamrc.ts')).default;
        const app = await new Ignitor(new URL('./', import.meta.url))
            .useRcFile(rc).setEnvironment('console').start();
        let scheduler;
        try {
            scheduler = app.getApp().container.resolve('scheduler');
        } catch {
            console.error("No ScheduleProvider is registered. Add ScheduleProvider to the providers list in reamrc.ts.");
            await app.stop();
            process.exit(2);
        }
        const tasks = scheduler.listTasks();

        if (tasks.length === 0) {
            console.log('No scheduled tasks registered.');
            await app.stop();
            process.exit(0);
        }

        // Truncate over-long column values with an ellipsis so the
        // table stays readable even for verbose task names / crons.
        const trunc = (s, n) => (s.length > n ? s.slice(0, n - 1) + '…' : s);

        console.log(
            'NAME'.padEnd(32),
            'CRON'.padEnd(18),
            'NEXT RUN'.padEnd(22),
            'LAST RUN'.padEnd(22),
            'RUNS'.padStart(6),
            'ERR'.padStart(5),
            'AVG(ms)'.padStart(9),
        );
        for (const t of tasks) {
            const s = scheduler.getStats(t.name);
            console.log(
                trunc(t.name, 32).padEnd(32),
                trunc(t.cronExpr, 18).padEnd(18),
                (t.nextRun ? new Date(t.nextRun).toISOString() : '—').padEnd(22),
                (s.lastRunMs ? new Date(s.lastRunMs).toISOString() : '—').padEnd(22),
                String(s.runCount).padStart(6),
                String(s.errorCount).padStart(5),
                String(Math.round(s.avgDurationMs)).padStart(9),
            );
        }

        await app.stop();
        process.exit(0);
    "#;

    let status = inherited_status(
        "node",
        &["--import", "@swc-node/register/esm-register", "--input-type=module", "-e", script],
    )?;

    if !status.success() {
        return Err(format!("schedule:list failed with code {}", status.code().unwrap_or(-1)));
    }
    Ok(())
}

/// Run a single registered scheduled task once, bypassing the cron schedule
/// AND the distributed lock backend (admin override). See Story 28.5 AC 4.
pub fn run_schedule_run(name: &str) -> Result<(), String> {
    if !std::path::Path::new("package.json").exists() {
        return Err("Not in a Ream project (no package.json found)".to_string());
    }
    if !std::path::Path::new("reamrc.ts").exists() {
        return Err("reamrc.ts not found — schedule:run requires a Ream framework project".to_string());
    }

    // Reject empty / whitespace-only names at the CLI boundary so
    // the error message stays user-friendly (the scheduler also
    // rejects empty names, but its error happens later after Ignitor
    // boot overhead).
    if name.trim().is_empty() {
        return Err("Task name cannot be empty. Usage: ream schedule:run <name>".to_string());
    }

    // JSON-escape the task name so it is safe to embed inside a Node
    // inline script. Prevents injection via crafted task names like
    // `foo'); require('child_process').exec(...); //`.
    let escaped_name = serde_json::to_string(name)
        .map_err(|e| format!("Failed to encode task name: {}", e))?;

    let script = format!(
        r#"
        import 'reflect-metadata';
        import {{ Ignitor }} from '@c9up/ream';
        const rc = (await import('./reamrc.ts')).default;
        const app = await new Ignitor(new URL('./', import.meta.url))
            .useRcFile(rc).setEnvironment('console').start();
        let scheduler;
        try {{
            scheduler = app.getApp().container.resolve('scheduler');
        }} catch {{
            console.error("No ScheduleProvider is registered. Add ScheduleProvider to the providers list in reamrc.ts.");
            await app.stop();
            process.exit(2);
        }}
        const taskName = {escaped_name};
        const result = await scheduler.runTaskNow(taskName);
        const messageOrDefault = (m) => (typeof m === 'string' && m.length > 0 ? m : 'unknown error');
        if (result.outcome === 'unknown') {{
            console.error(`Unknown task: ${{taskName}}. Run 'ream schedule:list' to see registered tasks.`);
            await app.stop();
            process.exit(2);
        }} else if (result.outcome === 'already-running') {{
            console.error(`Task ${{taskName}} is already running in this process — skipped. Try again after the current invocation completes.`);
            await app.stop();
            process.exit(3);
        }} else if (result.outcome === 'completed') {{
            console.log(`✓ ${{taskName}} completed in ${{Math.round(result.durationMs)}} ms`);
        }} else {{
            console.log(`✗ ${{taskName}} failed after ${{Math.round(result.durationMs)}} ms: ${{messageOrDefault(result.error?.message)}}`);
            await app.stop();
            process.exit(1);
        }}
        await app.stop();
    "#,
        escaped_name = escaped_name,
    );

    let status = inherited_status(
        "node",
        &["--import", "@swc-node/register/esm-register", "--input-type=module", "-e", &script],
    )?;

    // Preserve the child's exit code verbatim so scripts can
    // distinguish `1` (task failed) from `2` (unknown task). The
    // generic Err-path in main.rs would collapse both to `1`, so
    // short-circuit here and exit with the real code.
    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
    Ok(())
}

/// Show version and environment info.
pub fn info() -> Result<(), String> {
    println!("ream {}", env!("CARGO_PKG_VERSION"));
    println!();

    // Node.js version
    match Command::new("node").arg("--version").output() {
        Ok(output) if output.status.success() => {
            let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
            println!("  Node.js:  {}", version);
        }
        Ok(_) => println!("  Node.js:  error"),
        Err(_) => println!("  Node.js:  not found"),
    }

    // pnpm version
    match Command::new("pnpm").arg("--version").output() {
        Ok(output) if output.status.success() => {
            let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
            println!("  pnpm:     {}", version);
        }
        Ok(_) => println!("  pnpm:     error"),
        Err(_) => println!("  pnpm:     not found"),
    }

    // Rust version
    match Command::new("rustc").arg("--version").output() {
        Ok(output) if output.status.success() => {
            let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
            println!("  Rust:     {}", version);
        }
        Ok(_) => println!("  Rust:     error"),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dev_uses_swc_node_not_tsx() {
        let args = dev_args();
        // swc-node emits design:paramtypes (decorator metadata) → IoC DI works.
        assert!(
            args.contains(&"@swc-node/register/esm-register"),
            "ream dev must load swc-node so decorator metadata is emitted: {:?}",
            args
        );
        // tsx/esbuild can't emit decorator metadata → DI silently breaks.
        assert!(
            !args.iter().any(|a| a.contains("tsx")),
            "ream dev must NOT use tsx (esbuild cannot emit design:paramtypes): {:?}",
            args
        );
        // Native --watch drives the reload (replaces `tsx watch`).
        assert!(args.contains(&"--watch"), "ream dev must watch for changes: {:?}", args);
    }
}
