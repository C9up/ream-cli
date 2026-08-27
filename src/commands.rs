//! Commands — spawn Node.js processes and show info.

use std::process::{Command, ExitStatus, Stdio};

/// Refuse early when the TypeScript loader is absent.
///
/// Every command that boots the app spawns
/// `node --import @swc-node/register/esm-register`, resolved from the PROJECT's
/// node_modules: the CLI is a Rust binary and ships no JS dependencies. Without
/// the loader Node dies with a raw `ERR_MODULE_NOT_FOUND` naming a package the
/// user never asked for, from a path inside their app. Saying it here costs one
/// stat and tells them what to type.
pub fn require_ts_loader() -> Result<(), String> {
    require_ts_loader_at(std::path::Path::new("."))
}

/// The rooted form, so the guard can be tested without moving the process CWD —
/// which would race every other test in the binary.
pub fn require_ts_loader_at(root: &std::path::Path) -> Result<(), String> {
    if root.join("node_modules/@swc-node/register").exists() {
        return Ok(());
    }
    // Declared but absent means the tree is stale, and `pnpm install` is the
    // fix — telling the user to `add` a dependency they already declared sends
    // them to edit a manifest that is already right. Same split as `doctor`.
    let declared = std::fs::read_to_string(root.join("package.json"))
        .map(|c| c.contains("@swc-node/register"))
        .unwrap_or(false);
    let fix = if declared {
        "Run `pnpm install` — it is declared in package.json but not installed."
    } else {
        "Run `pnpm add -D @swc-node/register`."
    };
    Err(format!(
        "@swc-node/register is required by this command and is not installed.\n  \
         Ream runs your TypeScript through it.\n  {}",
        fix
    ))
}

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
        return Err(format!(
            "'{}' exited with code {}",
            cmd,
            status.code().unwrap_or(-1)
        ));
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

/// Read `assets` from the rc file, if the project has one.
///
/// The rc file is TypeScript, so it is read by Node rather than parsed here —
/// the same route `ream test` takes. A project without an rc file, or without
/// an `assets` key, simply has nothing to run alongside the server.
pub fn read_assets_config() -> Result<crate::dev::AssetsConfig, String> {
    if !std::path::Path::new("reamrc.ts").exists() {
        return Ok(crate::dev::AssetsConfig::default());
    }

    let output = Command::new("node")
        .args([
            "--import",
            "@swc-node/register/esm-register",
            "--input-type=module",
            "-e",
            "const rc = (await import('./reamrc.ts')).default; \
             process.stdout.write(JSON.stringify(rc?.assets ?? null));",
        ])
        .output()
        .map_err(|e| format!("Failed to read reamrc.ts: {e}"))?;

    if !output.status.success() {
        // A broken rc file must not silently drop the assets pipeline: say so
        // rather than starting a server whose stylesheet nobody rebuilds.
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "Could not read `assets` from reamrc.ts:\n{}",
            stderr.trim()
        ));
    }

    crate::dev::parse_assets(&String::from_utf8_lossy(&output.stdout))
}

/// `ream dev` — the server, plus whatever the rc file says builds the assets.
pub fn run_dev() -> Result<(), String> {
    if !std::path::Path::new("package.json").exists() {
        return Err("Not in a Ream project (no package.json found)".to_string());
    }
    require_ts_loader()?;

    let assets = read_assets_config()?;
    let Some(watcher) = assets.dev_server else {
        // Nothing to run alongside: keep the plain path, where the server owns
        // the terminal and its output is not piped through a prefix.
        return spawn_node("node", &dev_args());
    };

    let server = crate::dev::CommandSpec {
        command: "node".to_string(),
        args: dev_args().iter().map(|arg| (*arg).to_string()).collect(),
    };

    crate::dev::run_together(vec![
        crate::dev::Process {
            label: "server".to_string(),
            colour: crate::dev::COLOURS[0],
            spec: server,
        },
        crate::dev::Process {
            label: "assets".to_string(),
            colour: crate::dev::COLOURS[1],
            spec: watcher,
        },
    ])
}

/// `ream build` — the assets first, then TypeScript.
///
/// Assets first: a stylesheet the templates reference has to exist before the
/// build that copies it, and a failing asset build must stop the run rather
/// than ship a dist with a stale file in it.
pub fn run_build() -> Result<(), String> {
    if !std::path::Path::new("package.json").exists() {
        return Err("Not in a Ream project (no package.json found)".to_string());
    }
    require_ts_loader()?;

    if let Some(build) = read_assets_config()?.build {
        let args: Vec<&str> = build.args.iter().map(String::as_str).collect();
        let status = inherited_status(&build.command, &args)?;
        if !status.success() {
            return Err(format!(
                "assets build (`{}`) exited with code {}",
                build.command,
                status.code().unwrap_or(-1)
            ));
        }
    }

    spawn_node("npx", &["tsc"])
}

/// Run a migration command via Node.js inline script.
/// Boots the app, resolves db from the container, creates a MigrationRunner, and delegates.
pub fn run_migration(action: &str) -> Result<(), String> {
    if !std::path::Path::new("package.json").exists() {
        return Err("Not in a Ream project (no package.json found)".to_string());
    }
    require_ts_loader()?;
    if !std::path::Path::new("database/migrations").exists() {
        return Err("database/migrations/ directory not found — run 'ream make:migration' to create your first migration".to_string());
    }

    let runner_action = match action {
        "migrate" => {
            r#"
            const executed = await runner.migrate();
            if (executed.length === 0) { console.log('  Nothing to migrate.'); }
            else { for (const n of executed) console.log('  migrated:', n); }
        "#
        }
        "migrate:rollback" => {
            r#"
            const rolled = await runner.rollback();
            if (rolled.length === 0) { console.log('  Nothing to rollback.'); }
            else { for (const n of rolled) console.log('  rolled back:', n); }
        "#
        }
        "migrate:status" => {
            r#"
            const statuses = await runner.status();
            if (statuses.length === 0) { console.log('  No migrations found.'); }
            else { for (const s of statuses) console.log(`  ${s.status === 'applied' ? '✓' : '○'} ${s.name}${s.batch ? ' (batch ' + s.batch + ')' : ''}`); }
        "#
        }
        _ => return Err(format!("Unknown migration action: {}", action)),
    };

    let script = format!(
        r#"
        import 'reflect-metadata';
        import {{ Ignitor }} from '@c9up/ream';
        import {{ MigrationRunner }} from '@c9up/atlas';
        // We drive migrations explicitly below — tell AtlasProvider NOT to also
        // auto-migrate on boot (it would double-apply, and re-apply right before
        // a rollback/status pass). Must be set before .start() boots providers.
        process.env.REAM_SKIP_BOOT_MIGRATE = '1';
        const rc = (await import('./reamrc.ts')).default;
        const app = await new Ignitor(new URL('./', import.meta.url))
            .useRcFile(rc).setEnvironment('console').start();
        // `container.resolve` is ASYNC (ream's container mirrors Adonis fold).
        // Without the await, `db` is a Promise: `db.dialect` reads undefined and
        // the runner fails with "db.execute is not a function".
        const db = await app.getApp().container.resolve('db');
        const runner = new MigrationRunner(db, {{ migrationsDir: 'database/migrations', dialect: db.dialect }});
        {}
        await app.stop();
        // Force-exit: app.stop() doesn't close app-owned handles (atlas pool,
        // an ioredis client built when config loads), so the event loop would
        // otherwise stay alive and the one-shot command would hang (exit 124).
        process.exit(0);
    "#,
        runner_action
    );

    let status = inherited_status(
        "node",
        &[
            "--import",
            "@swc-node/register/esm-register",
            "--input-type=module",
            "-e",
            &script,
        ],
    )?;

    if !status.success() {
        return Err(format!(
            "Migration failed with code {}",
            status.code().unwrap_or(-1)
        ));
    }

    Ok(())
}

/// `repl` — an interactive shell with the application booted.
///
/// Boots in console mode (providers + container, no HTTP server) and hands the
/// container to a Node REPL. `app`, `container` and `resolve(token)` are in
/// scope, so a service can be poked at without writing a throwaway script —
/// which is the habit this whole CLI exists to remove.
pub fn run_repl() -> Result<(), String> {
    if !std::path::Path::new("package.json").exists() {
        return Err("Not in a Ream project (no package.json found)".to_string());
    }
    require_ts_loader()?;
    if !std::path::Path::new("reamrc.ts").exists() {
        return Err("reamrc.ts not found — `ream repl` boots the app from the rc file".to_string());
    }

    let script = r#"
        import 'reflect-metadata';
        import repl from 'node:repl';
        import { Ignitor, prettyPrintError } from '@c9up/ream';

        const rc = (await import('./reamrc.ts')).default;
        const ignitor = await new Ignitor(new URL('./', import.meta.url))
            .useRcFile(rc)
            .setEnvironment('console')
            .start();
        const app = ignitor.getApp();

        process.stdout.write('\n  Ream REPL — `app`, `container`, `await resolve(token)`\n');
        process.stdout.write('  .exit or Ctrl-D to leave\n\n');

        const server = repl.start({ prompt: 'ream > ' });
        server.context.app = app;
        server.context.container = app.container;
        // `container.resolve` is async (Adonis fold parity) — without awaiting it
        // you get a Promise, and every property reads undefined. The inner
        // await is redundant for the caller (who awaits too) but keeps the
        // guard in `generated_scripts_await_every_container_resolve` honest.
        server.context.resolve = async (token) => await app.container.resolve(token);

        server.on('exit', async () => {
            try {
                await ignitor.stop();
            } catch (err) {
                prettyPrintError(err);
            }
            // App-owned handles (a DB pool, a redis client) keep the loop alive.
            process.exit(0);
        });
    "#;

    let status = inherited_status(
        "node",
        &[
            "--import",
            "@swc-node/register/esm-register",
            "--input-type=module",
            "-e",
            script,
        ],
    )?;

    if !status.success() {
        return Err(format!(
            "repl exited with code {}",
            status.code().unwrap_or(-1)
        ));
    }
    Ok(())
}

/// `generate:key` — write a fresh APP_KEY into `.env`.
///
/// A scaffolded project ships a placeholder; leaving it in place means cookies,
/// sessions and CSRF tokens are signed with a value that is public knowledge.
///
/// Generation is delegated to Node's `crypto` (as `nova:vapid:generate` does)
/// rather than pulled in as a Rust crypto dependency for one 32-byte draw. The
/// key is never printed: stdout ends up in shell history, scrollback and CI
/// logs — the `.env` write is the only sink.
pub fn run_generate_key(force: bool, show: bool) -> Result<(), String> {
    if !std::path::Path::new("package.json").exists() {
        return Err("Not in a Ream project (no package.json found)".to_string());
    }

    // `--show` prints the key and writes nothing — the way to obtain one for a
    // secrets manager without touching the local .env.
    if show {
        println!("{}", generate_app_key()?);
        return Ok(());
    }

    // Adonis guards production: rewriting APP_KEY there invalidates every
    // session and signed URL in circulation, and a deployed .env is usually not
    // the source of truth anyway.
    let in_production = std::env::var("NODE_ENV").is_ok_and(|env| env == "production");
    if in_production && !force {
        return Err(
            "Refusing to write .env in production — every existing session, cookie and \
             signed URL would be invalidated.\n  \
             Use --show to print a key for your secrets manager, or --force to write anyway."
                .to_string(),
        );
    }

    let env_path = std::path::Path::new(".env");
    let existing = if env_path.exists() {
        std::fs::read_to_string(env_path).map_err(|e| format!("Failed to read .env: {}", e))?
    } else {
        String::new()
    };

    // The scaffold's placeholder is not a real key, so it must not block.
    const PLACEHOLDER: &str = "change-me-to-a-unique-32+-byte-secret!!";
    if !force {
        if let Some(value) = crate::nova::read_env_value(&existing, "APP_KEY") {
            if !value.is_empty() && value != PLACEHOLDER {
                return Err("APP_KEY is already set in .env.\n  \
                     Re-run with --force to replace it — every existing cookie, \
                     session and signed URL becomes invalid."
                    .to_string());
            }
        }
    }

    let key = generate_app_key()?;
    let updated = crate::nova::upsert_env_var(&existing, "APP_KEY", &key);
    std::fs::write(env_path, updated).map_err(|e| format!("Failed to write .env: {}", e))?;

    println!();
    println!("  \x1b[32mGenerated APP_KEY\x1b[0m");
    println!(
        "  APP_KEY = [redacted — written to .env, {} chars]",
        key.chars().count()
    );
    println!();
    println!("  Move it to a secrets manager before deploying.");
    println!();
    Ok(())
}

/// 32 random bytes, base64url — same shape AdonisJS generates.
fn generate_app_key() -> Result<String, String> {
    let output = Command::new("node")
        .args([
            "--input-type=module",
            "-e",
            "import { randomBytes } from 'node:crypto'; process.stdout.write(randomBytes(32).toString('base64url'));",
        ])
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("Failed to run node: {}", e))?;

    if !output.status.success() {
        return Err(format!(
            "Key generation failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    let key = String::from_utf8(output.stdout)
        .map_err(|e| format!("Key generation produced invalid output: {}", e))?;
    if key.trim().is_empty() {
        return Err("Key generation produced an empty key".to_string());
    }
    Ok(key.trim().to_string())
}

/// Does the application declare a command by this name?
///
/// Answered by scanning `commands/` for a `commandName` literal rather than by
/// booting Node: this runs before every native command, and paying a boot to
/// find out would make the whole CLI slow.
///
/// The trade-off is explicit: a command whose name is computed at runtime is
/// invisible here, and the native command wins. Declaring it as a literal —
/// which `ream make:command` always does — is what makes the override work.
pub fn app_declares_command(name: &str) -> bool {
    app_declares_command_in(std::path::Path::new("."), name)
}

/// Root-relative form — the whole of `app_declares_command`, with the project
/// directory passed in so it can be exercised without touching the process's
/// current directory (Rust runs tests in threads; changing it breaks the others).
pub fn app_declares_command_in(root: &std::path::Path, name: &str) -> bool {
    if !root.join("package.json").exists() {
        return false;
    }
    let needles = [
        format!("commandName = \"{name}\""),
        format!("commandName = '{name}'"),
    ];

    // `commands/` is the usual home, but its absence says nothing about
    // `reamrc.commands` — returning early here made rc-declared commands
    // undetectable in any project without that directory.
    let dir = root.join("commands");
    if dir.is_dir() && (scan_for(&dir, &needles) || scan_for_alias(&dir, name)) {
        return true;
    }

    // Commands can also be declared in `reamrc.commands`. Local entries point at
    // files we can read; entries resolving to a package cannot be inspected
    // without booting, and are the documented blind spot of this approach.
    rc_declared_commands(root, &needles) || rc_declared_alias(root, name)
}

/// Does any file under `dir` declare `name` in a `static aliases = [...]`?
///
/// A command answers to its aliases as much as to its name, so an app aliasing
/// `start` overrides the built-in exactly as a command named `start` would —
/// The console kernel resolves both through one registry.
fn scan_for_alias(dir: &std::path::Path, name: &str) -> bool {
    let quoted = [format!("\"{name}\""), format!("'{name}'")];
    scan_files(dir, &|text: &str| {
        list_after(text, "aliases")
            .is_some_and(|list| quoted.iter().any(|q| list.contains(q.as_str())))
    })
}

/// Does `reamrc.ts` map `name` through `commandsAliases`?
fn rc_declared_alias(root: &std::path::Path, name: &str) -> bool {
    let Ok(rc) = std::fs::read_to_string(root.join("reamrc.ts")) else {
        return false;
    };
    let Some(block) = block_after(&rc, "commandsAliases") else {
        return false;
    };
    // Keys may be bare, single- or double-quoted.
    [
        format!("{name}:"),
        format!("\"{name}\":"),
        format!("'{name}':"),
    ]
    .iter()
    .any(|key| block.contains(key.as_str()))
}

/// The `[ ... ]` following `marker`, if any.
fn list_after(text: &str, marker: &str) -> Option<String> {
    let start = text.find(marker)? + marker.len();
    let open = text[start..].find('[')? + start;
    let close = text[open..].find(']')? + open;
    Some(text[open..=close].to_string())
}

/// The `{ ... }` following `marker`, if any.
fn block_after(text: &str, marker: &str) -> Option<String> {
    let start = text.find(marker)? + marker.len();
    let open = text[start..].find('{')? + start;
    let close = text[open..].find('}')? + open;
    Some(text[open..=close].to_string())
}

/// Scan the files referenced by relative imports in `reamrc.commands`.
fn rc_declared_commands(root: &std::path::Path, needles: &[String]) -> bool {
    let Ok(rc) = std::fs::read_to_string(root.join("reamrc.ts")) else {
        return false;
    };

    for candidate in rc.split("import(").skip(1) {
        let Some(quote) = candidate.chars().find(|c| *c == '\'' || *c == '"') else {
            continue;
        };
        let Some(rest) = candidate.split_once(quote) else {
            continue;
        };
        let Some((path, _)) = rest.1.split_once(quote) else {
            continue;
        };
        if !path.starts_with("./") && !path.starts_with("../") {
            continue; // a package — not readable from here
        }
        // The rc file imports the built `.js`; the source next to it is `.ts`.
        for candidate_path in [path.to_string(), path.replace(".js", ".ts")] {
            let Ok(text) = std::fs::read_to_string(root.join(&candidate_path)) else {
                continue;
            };
            if needles.iter().any(|needle| text.contains(needle.as_str())) {
                return true;
            }
        }
    }
    false
}

/// Does any source file under `dir` satisfy `matches`?
///
/// Recursive, depth-capped: `commands/` is a flat convention, and an unbounded
/// walk would follow whatever happens to live under it.
fn scan_files(dir: &std::path::Path, matches: &dyn Fn(&str) -> bool) -> bool {
    fn walk(dir: &std::path::Path, matches: &dyn Fn(&str) -> bool, depth: usize) -> bool {
        if depth > 4 {
            return false;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return false;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if walk(&path, matches, depth + 1) {
                    return true;
                }
                continue;
            }
            let is_source = path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| matches!(ext, "ts" | "js" | "mts" | "mjs"));
            if !is_source {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            if matches(&text) {
                return true;
            }
        }
        false
    }
    walk(dir, matches, 0)
}

/// Does any source file under `dir` contain one of `needles`?
fn scan_for(dir: &std::path::Path, needles: &[String]) -> bool {
    scan_files(dir, &|text: &str| {
        needles.iter().any(|needle| text.contains(needle.as_str()))
    })
}

/// Dispatch a command to the app's console kernel — the app's own console kernel
/// equivalent, and the reason this binary accepts names it does not define.
///
/// Prefers the app's `bin/console.ts` entry when it exists: an app may wire a
/// custom importer, path aliases or preloads there, and re-implementing that
/// here would drift. Falls back to an inline boot for projects scaffolded
/// before the entry existed, so `ream <cmd>` works without touching the app.
pub fn run_console(argv: &[String]) -> Result<(), String> {
    if !std::path::Path::new("package.json").exists() {
        return Err("Not in a Ream project (no package.json found)".to_string());
    }
    require_ts_loader()?;

    let status = if std::path::Path::new("bin/console.ts").exists() {
        let mut args: Vec<&str> = vec![
            "--import",
            "@swc-node/register/esm-register",
            "bin/console.ts",
        ];
        args.extend(argv.iter().map(String::as_str));
        inherited_status("node", &args)?
    } else {
        if !std::path::Path::new("reamrc.ts").exists() {
            return Err(
                "No bin/console.ts and no reamrc.ts — cannot reach the app's console kernel.\n  \
                 Run 'ream new' for a project with a console entry, or add bin/console.ts."
                    .to_string(),
            );
        }
        let script = console_script(argv);
        inherited_status(
            "node",
            &[
                "--import",
                "@swc-node/register/esm-register",
                "--input-type=module",
                "-e",
                &script,
            ],
        )?
    };

    if !status.success() {
        // The kernel already reported what went wrong; propagate its code
        // rather than wrapping it in a second, less informative error.
        std::process::exit(status.code().unwrap_or(1));
    }

    Ok(())
}

/// The inline console boot, used when the app has no `bin/console.ts`.
///
/// argv goes through serde, not string concatenation: a command name comes from
/// the shell, and interpolating it raw would let it break out into executable
/// code. Same reasoning as `test_options`.
fn console_script(argv: &[String]) -> String {
    format!(
        r#"
        import 'reflect-metadata';
        import {{ Ignitor, prettyPrintError }} from '@c9up/ream';
        const rc = (await import('./reamrc.ts')).default;
        try {{
            await new Ignitor(new URL('./', import.meta.url))
                .useRcFile(rc)
                .console()
                .handle({});
        }} catch (err) {{
            prettyPrintError(err);
            process.exitCode = 1;
        }}
        // Force-exit: app-owned handles (a DB pool, a redis client built when
        // config loads) keep the event loop alive, and a one-shot command must
        // not hang. Same guard as run_migration.
        process.exit(process.exitCode ?? 0);
    "#,
        serde_json::json!(argv)
    )
}

/// One line of `ream list`.
///
/// `name` and `description` drive the grouped human listing; `metadata` is the
/// full console command contract, which is what `--json` prints. Both are carried
/// together so the two outputs cannot describe different sets of commands.
#[derive(Clone, Debug)]
pub struct ListEntry {
    pub name: String,
    pub description: String,
    pub metadata: serde_json::Value,
}

/// Report why the app's commands are missing from `ream list`, on stderr so the
/// list itself stays pipeable.
fn warn_app_commands(reason: &str) {
    eprintln!("warning: this project's own commands are not listed — {reason}");
}

/// `ream list` — one list covering this binary's commands and the app's own.
///
/// The console prints a single list; splitting "framework" from "app" would make the
/// user care about which side implements what. App commands are read as JSON so
/// they can be merged rather than appended.
pub fn run_list(
    framework: &[ListEntry],
    as_json: bool,
    namespaces: &[String],
) -> Result<(), String> {
    let app_entries = if std::path::Path::new("package.json").exists() {
        app_commands()
    } else {
        Vec::new()
    };

    // On a name collision the APP wins at run time, so the listing has to show
    // the app's entry — printing the built-in description for a command the app
    // actually handles is worse than not listing it at all. Marked, because a
    // shadowed built-in is worth knowing about.
    let app_names: std::collections::HashSet<String> =
        app_entries.iter().map(|entry| entry.name.clone()).collect();
    let framework_names: std::collections::HashSet<&str> =
        framework.iter().map(|entry| entry.name.as_str()).collect();

    let mut entries = merge_entries(app_entries, framework, namespaces)?;

    // The TS kernel answers `list --json`; the binary has to as well, or the
    // same command means different things depending on how it is reached. The
    // metadata is passed through untouched — including the override marker's
    // absence, which belongs to the human listing, not to a machine-read
    // description.
    if as_json {
        let payload: Vec<&serde_json::Value> =
            entries.iter().map(|entry| &entry.metadata).collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).map_err(|e| e.to_string())?
        );
        return Ok(());
    }

    for entry in entries.iter_mut() {
        if app_names.contains(&entry.name) && framework_names.contains(entry.name.as_str()) {
            entry.description = format!("{}  (overrides the built-in command)", entry.description);
        }
    }

    let width = entries
        .iter()
        .map(|entry| entry.name.len())
        .max()
        .unwrap_or(0);
    let mut current_group: Option<String> = None;

    println!();
    println!("Usage: ream <command> [options]");
    println!();

    for entry in &entries {
        let group = group_of(&entry.name);
        if current_group.as_ref() != Some(&group) {
            if current_group.is_some() {
                println!();
            }
            println!(
                "{}",
                if group.is_empty() {
                    "Available commands"
                } else {
                    group.as_str()
                }
            );
            current_group = Some(group);
        }
        println!(
            "  {:width$}  {}",
            entry.name,
            entry.description,
            width = width
        );
    }
    println!();

    Ok(())
}

/// The single list `ream list` prints: the app's commands, then the binary's,
/// deduplicated, optionally narrowed to some namespaces, and grouped.
fn merge_entries(
    app: Vec<ListEntry>,
    framework: &[ListEntry],
    namespaces: &[String],
) -> Result<Vec<ListEntry>, String> {
    // App entries first: the dedup keeps the first of each name, and the app is
    // what runs.
    let mut entries = app;

    // Except `list`: the console kernel registers its own (the console kernel does too, so
    // that `bin/console.ts list` works), but here it is the SAME command this
    // binary is already running — keeping the app's copy would mark the
    // built-in as shadowed, which is not what happens at dispatch.
    entries.retain(|entry| entry.name != "list");
    entries.extend(framework.iter().cloned());

    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    entries.retain(|entry| seen.insert(entry.name.clone()));

    if !namespaces.is_empty() {
        entries.retain(|entry| namespaces.contains(&group_of(&entry.name)));
        // A namespace nobody matches is almost always a typo, and an empty list
        // reads as "this namespace is empty" instead of "no such namespace".
        if entries.is_empty() {
            return Err(format!(
                "No command in namespace \"{}\".",
                namespaces.join("\", \"")
            ));
        }
    }

    // Sort by (namespace, name) — sorting on the name alone interleaves the
    // groups, so a heading would be reprinted every time the alphabet crosses
    // back out of a namespace.
    entries.sort_by(|a, b| {
        group_of(&a.name)
            .cmp(&group_of(&b.name))
            .then(a.name.cmp(&b.name))
    });

    Ok(entries)
}

/// The namespace of a command name: `make:entity` → `make`, `dev` → `` .
/// Ungrouped commands sort first because the empty string precedes everything.
fn group_of(name: &str) -> String {
    name.split_once(':')
        .map(|(prefix, _)| prefix.to_string())
        .unwrap_or_default()
}

/// Ask the app's console kernel for its commands.
///
/// The framework commands must still be listed when the app cannot answer, so a
/// failure here is not fatal — but it IS reported. Swallowing it silently means
/// a command missing because of a broken import looks like a command that was
/// never written.
fn app_commands() -> Vec<ListEntry> {
    let output = if std::path::Path::new("bin/console.ts").exists() {
        Command::new("node")
            .args([
                "--import",
                "@swc-node/register/esm-register",
                "bin/console.ts",
                "list",
                "--json",
            ])
            .stderr(Stdio::piped())
            .output()
    } else if std::path::Path::new("reamrc.ts").exists() {
        Command::new("node")
            .args([
                "--import",
                "@swc-node/register/esm-register",
                "--input-type=module",
                "-e",
                &console_script(&["list".to_string(), "--json".to_string()]),
            ])
            .stderr(Stdio::piped())
            .output()
    } else {
        return Vec::new();
    };

    let output = match output {
        Ok(output) => output,
        Err(err) => {
            warn_app_commands(&format!("could not run the console entry: {err}"));
            return Vec::new();
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = stderr.trim();
        warn_app_commands(if detail.is_empty() {
            "the console entry exited with an error"
        } else {
            detail
        });
        return Vec::new();
    }

    let Ok(text) = String::from_utf8(output.stdout) else {
        warn_app_commands("the console entry produced non-UTF-8 output");
        return Vec::new();
    };
    match parse_command_list(&text) {
        Ok(entries) => entries,
        Err(err) => {
            warn_app_commands(&err);
            Vec::new()
        }
    }
}

/// Read the kernel's `list --json` payload.
///
/// Kept apart from the process plumbing so the field names stay under test: the
/// kernel publishes the console's metadata contract, whose key is `commandName`, and a
/// silent mismatch here drops every one of the app's commands from the listing.
fn parse_command_list(text: &str) -> Result<Vec<ListEntry>, String> {
    let parsed = serde_json::from_str::<Vec<serde_json::Value>>(text.trim())
        .map_err(|err| format!("could not read the command list ({err})"))?;

    Ok(parsed
        .into_iter()
        .filter_map(|metadata| {
            let name = metadata.get("commandName")?.as_str()?.to_string();
            let description = metadata
                .get("description")
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .to_string();
            Some(ListEntry {
                name,
                description,
                metadata,
            })
        })
        .collect())
}

/// Inspect: list registered routes, providers, and decorated services.
/// Boots the app in console mode and dumps an introspection summary to stdout.
pub fn run_inspect() -> Result<(), String> {
    if !std::path::Path::new("package.json").exists() {
        return Err("Not in a Ream project (no package.json found)".to_string());
    }
    require_ts_loader()?;
    if !std::path::Path::new("reamrc.ts").exists() {
        return Err("reamrc.ts not found — inspect requires a Ream framework project".to_string());
    }

    let script = r#"
        import 'reflect-metadata';
        import { Ignitor } from '@c9up/ream';
        const rc = (await import('./reamrc.ts')).default;
        const app = await new Ignitor(new URL('./', import.meta.url))
            .useRcFile(rc).setEnvironment('console').start();
        const router = await app.getApp().container.resolve('router');

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

    let status = inherited_status(
        "node",
        &[
            "--import",
            "@swc-node/register/esm-register",
            "--input-type=module",
            "-e",
            script,
        ],
    )?;

    if !status.success() {
        return Err(format!(
            "Inspect failed with code {}",
            status.code().unwrap_or(-1)
        ));
    }

    Ok(())
}

/// List registered scheduled tasks (Story 28.5).
pub fn run_schedule_list() -> Result<(), String> {
    if !std::path::Path::new("package.json").exists() {
        return Err("Not in a Ream project (no package.json found)".to_string());
    }
    require_ts_loader()?;
    if !std::path::Path::new("reamrc.ts").exists() {
        return Err(
            "reamrc.ts not found — schedule:list requires a Ream framework project".to_string(),
        );
    }

    let script = r#"
        import 'reflect-metadata';
        import { Ignitor } from '@c9up/ream';
        const rc = (await import('./reamrc.ts')).default;
        const app = await new Ignitor(new URL('./', import.meta.url))
            .useRcFile(rc).setEnvironment('console').start();
        let scheduler;
        try {
            scheduler = await app.getApp().container.resolve('scheduler');
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
        &[
            "--import",
            "@swc-node/register/esm-register",
            "--input-type=module",
            "-e",
            script,
        ],
    )?;

    if !status.success() {
        return Err(format!(
            "schedule:list failed with code {}",
            status.code().unwrap_or(-1)
        ));
    }
    Ok(())
}

/// Run a single registered scheduled task once, bypassing the cron schedule
/// AND the distributed lock backend (admin override). See Story 28.5 AC 4.
pub fn run_schedule_run(name: &str) -> Result<(), String> {
    if !std::path::Path::new("package.json").exists() {
        return Err("Not in a Ream project (no package.json found)".to_string());
    }
    require_ts_loader()?;
    if !std::path::Path::new("reamrc.ts").exists() {
        return Err(
            "reamrc.ts not found — schedule:run requires a Ream framework project".to_string(),
        );
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
    let escaped_name =
        serde_json::to_string(name).map_err(|e| format!("Failed to encode task name: {}", e))?;

    let script = format!(
        r#"
        import 'reflect-metadata';
        import {{ Ignitor }} from '@c9up/ream';
        const rc = (await import('./reamrc.ts')).default;
        const app = await new Ignitor(new URL('./', import.meta.url))
            .useRcFile(rc).setEnvironment('console').start();
        let scheduler;
        try {{
            scheduler = await app.getApp().container.resolve('scheduler');
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
        &[
            "--import",
            "@swc-node/register/esm-register",
            "--input-type=module",
            "-e",
            &script,
        ],
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

/// Run the test suites declared in the rc file's `tests` block.
///
/// The AdonisJS stratification: the framework reads its rc file and hands the
/// suites to the runner. All of that lives in TypeScript (`@c9up/helix-plugin-ream/runner`),
/// so this stays a thin spawn — the same split as `run_migration`.
pub fn run_tests(
    suites: &[String],
    threads: Option<usize>,
    reporters: Option<&str>,
    bail: bool,
) -> Result<(), String> {
    if !std::path::Path::new("package.json").exists() {
        return Err("Not in a Ream project (no package.json found)".to_string());
    }
    require_ts_loader()?;
    if !std::path::Path::new("reamrc.ts").exists() {
        return Err(
            "reamrc.ts not found — `ream test` reads its suites from the rc file".to_string(),
        );
    }

    let options = test_options(suites, threads, reporters, bail);

    let script = format!(
        r#"
        import 'reflect-metadata';
        const options = {};
        for (const key of Object.keys(options)) {{
            if (options[key] === null) delete options[key];
        }}
        let runTestsFromRcFile;
        try {{
            ({{ runTestsFromRcFile }} = await import('@c9up/helix-plugin-ream/runner'));
        }} catch (err) {{
            // The runner is opt-in: a project testing with vitest never installs
            // it. Saying so beats a bare ERR_MODULE_NOT_FOUND on a package the
            // user never named.
            if (String(err && err.message).includes('@c9up/helix-plugin-ream')) {{
                process.stderr.write(
                    'ream: `ream test` runs the suites through helix, which this project does not have.\n' +
                    '  pnpm add -D @c9up/helix @c9up/helix-plugin-ream\n'
                );
                process.exit(1);
            }}
            throw err;
        }}
        try {{
            process.exitCode = await runTestsFromRcFile('./reamrc.ts', options);
        }} catch (err) {{
            // A misspelled suite name is a user error, not a crash — print what
            // is wrong and what exists, without a stack trace.
            process.stderr.write('ream: ' + (err instanceof Error ? err.message : String(err)) + '\n');
            process.exitCode = 1;
        }}
    "#,
        options
    );

    let status = inherited_status(
        "node",
        &[
            "--import",
            "@swc-node/register/esm-register",
            "--input-type=module",
            "-e",
            &script,
        ],
    )?;

    if !status.success() {
        return Err(format!(
            "Tests failed with code {}",
            status.code().unwrap_or(-1)
        ));
    }

    Ok(())
}

/// The options object handed to `runTestsFromRcFile`.
///
/// Built through serde rather than string concatenation: a suite name comes
/// from the command line, and interpolating it raw into the script would let it
/// break out into executable code.
fn test_options(
    suites: &[String],
    threads: Option<usize>,
    reporters: Option<&str>,
    bail: bool,
) -> serde_json::Value {
    serde_json::json!({
        "suites": suites,
        "threads": threads,
        "reporters": reporters.map(|r| {
            r.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
        }),
        "bail": bail,
        // Explicit rather than inherited: this process also carries
        // `--input-type=module`, which a worker spawned with a file entry
        // must not receive.
        "nodeArgs": ["--import", "@swc-node/register/esm-register"],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, description: &str) -> ListEntry {
        ListEntry {
            name: name.to_string(),
            description: description.to_string(),
            metadata: serde_json::json!({ "commandName": name, "description": description }),
        }
    }

    #[test]
    fn reads_the_kernel_metadata_key_not_a_summary_field() {
        // The kernel publishes the console's contract, keyed on `commandName`. Reading
        // `name` here silently dropped every one of the app's commands.
        let entries = parse_command_list(
            r#"[{ "commandName": "provision", "description": "Create the owner", "flags": [] }]"#,
        )
        .expect("the payload is valid JSON");

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "provision");
        assert_eq!(entries[0].description, "Create the owner");
        // The metadata is passed through whole: `--json` prints it verbatim.
        assert!(entries[0].metadata.get("flags").is_some());
    }

    #[test]
    fn reports_a_payload_it_cannot_read() {
        assert!(parse_command_list("not json").is_err());
    }

    #[test]
    fn app_entries_shadow_the_built_in_of_the_same_name() {
        let merged = merge_entries(
            vec![entry("start", "The app's own start")],
            &[
                entry("start", "Built-in start"),
                entry("dev", "Run the dev server"),
            ],
            &[],
        )
        .expect("no namespace filter");

        assert_eq!(merged.len(), 2);
        let start = merged
            .iter()
            .find(|e| e.name == "start")
            .expect("start is listed");
        assert_eq!(start.description, "The app's own start");
    }

    #[test]
    fn the_kernels_own_list_does_not_shadow_the_built_in() {
        let merged = merge_entries(
            vec![entry("list", "List all the available commands")],
            &[
                entry("list", "List every command available here"),
                entry("dev", "Dev server"),
            ],
            &[],
        )
        .expect("no namespace filter");

        let listed: Vec<&str> = merged.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(listed, vec!["dev", "list"]);
        // The binary's own description survives — the app's kernel exposes the
        // same command, it does not override it.
        let list = merged
            .iter()
            .find(|e| e.name == "list")
            .expect("list is listed");
        assert_eq!(list.description, "List every command available here");
    }

    #[test]
    fn narrows_the_listing_to_the_requested_namespaces() {
        let merged = merge_entries(
            Vec::new(),
            &[entry("make:entity", "Entity"), entry("dev", "Dev server")],
            &["make".to_string()],
        )
        .expect("make matches");

        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].name, "make:entity");
    }

    #[test]
    fn rejects_a_namespace_nothing_matches() {
        let error = merge_entries(
            Vec::new(),
            &[entry("dev", "Dev server")],
            &["mak".to_string()],
        )
        .expect_err("no command lives in \"mak\"");
        assert!(
            error.contains("mak"),
            "the message must name the namespace: {error}"
        );
    }

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
        assert!(
            args.contains(&"--watch"),
            "ream dev must watch for changes: {:?}",
            args
        );
    }

    #[test]
    fn a_suite_name_cannot_break_out_of_the_generated_script() {
        // The name reaches an inline `node -e` script inside a JSON string
        // literal. What could terminate that literal is a double quote or a
        // newline; interpolated raw, this name would close it and run code.
        let hostile = "a\", process.exit(42); //\nb".to_string();
        let options = test_options(std::slice::from_ref(&hostile), None, None, false);

        let rendered = options.to_string();
        // Round-trips as data...
        assert_eq!(options["suites"][0], serde_json::Value::String(hostile));
        // ...and every literal-terminating character is escaped on the way out.
        assert!(
            rendered.contains("\\\""),
            "the quote is escaped: {}",
            rendered
        );
        assert!(
            rendered.contains("\\n"),
            "the newline is escaped: {}",
            rendered
        );
        assert!(!rendered.contains('\n'), "no raw newline survives");
    }

    #[test]
    fn reporters_are_split_and_emptied_entries_dropped() {
        let options = test_options(&[], None, Some("spec, json ,,"), false);
        assert_eq!(options["reporters"], serde_json::json!(["spec", "json"]));
    }

    #[test]
    fn absent_options_stay_null_so_the_script_deletes_them() {
        // `runTests` fills its own defaults; a `null` would override them.
        let options = test_options(&[], None, None, false);
        assert!(options["threads"].is_null());
        assert!(options["reporters"].is_null());
    }

    #[test]
    fn workers_are_spawned_with_the_swc_loader_not_input_type() {
        let options = test_options(&[], None, None, false);
        let args = options["nodeArgs"].as_array().expect("nodeArgs is a list");
        assert_eq!(
            args,
            &serde_json::json!(["--import", "@swc-node/register/esm-register"])
                .as_array()
                .unwrap()
                .clone()
        );
        // `--input-type=module` belongs to the `-e` parent only: a worker gets a
        // FILE, and Node rejects the flag there.
        assert!(!options["nodeArgs"].to_string().contains("input-type"));
    }

    /// An alias must override a built-in exactly as a command name does — the
    /// resolves both through one registry, so `ream start` has to reach an app
    /// command aliased to `start`, not the binary's own.
    #[test]
    fn aliases_count_as_a_declaration() {
        let dir = std::env::temp_dir().join(format!("ream-alias-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("commands")).unwrap();
        std::fs::write(dir.join("package.json"), "{}").unwrap();
        std::fs::write(
            dir.join("commands/app_start.ts"),
            "export default class AppStart { static commandName = 'app:start'\n static aliases = ['start', 'up'] }\n",
        )
        .unwrap();

        assert!(
            app_declares_command_in(&dir, "start"),
            "static aliases must count"
        );
        assert!(app_declares_command_in(&dir, "up"), "every alias counts");
        assert!(
            app_declares_command_in(&dir, "app:start"),
            "the name still counts"
        );
        // A word appearing in prose must not be mistaken for a declaration.
        assert!(!app_declares_command_in(&dir, "build"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Same for `commandsAliases` in the rc file.
    #[test]
    fn rc_command_aliases_count_as_a_declaration() {
        let dir = std::env::temp_dir().join(format!("ream-rc-alias-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("package.json"), "{}").unwrap();
        std::fs::write(
            dir.join("reamrc.ts"),
            "export default defineConfig({\n  commandsAliases: { start: 'app:start' },\n})\n",
        )
        .unwrap();

        assert!(app_declares_command_in(&dir, "start"));
        assert!(!app_declares_command_in(&dir, "test"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A command declared in `reamrc.commands` must be detectable even when the
    /// project has no `commands/` directory at all.
    ///
    /// It did not: the lookup returned early on a missing directory, so an app
    /// whose commands live only in the rc file could never override a built-in.
    #[test]
    fn rc_declared_commands_are_found_without_a_commands_directory() {
        let dir = std::env::temp_dir().join(format!("ream-rc-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("app/console")).unwrap();
        std::fs::write(dir.join("package.json"), "{}").unwrap();
        std::fs::write(
            dir.join("reamrc.ts"),
            "export default defineConfig({\n  commands: [() => import('./app/console/deploy.js')],\n})\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("app/console/deploy.ts"),
            "export default class Deploy { static commandName = 'start' }\n",
        )
        .unwrap();

        let found = app_declares_command_in(&dir, "start");
        let absent = app_declares_command_in(&dir, "nope");
        let _ = std::fs::remove_dir_all(&dir);

        assert!(
            found,
            "a command declared in reamrc.commands must be detected"
        );
        assert!(
            !absent,
            "an undeclared name must not be reported as declared"
        );
    }

    /// Every `container.resolve(...)` inside the inline JS scripts must be
    /// awaited.
    ///
    /// ream's container is asynchronous (Adonis fold parity), so a missing
    /// `await` yields a Promise that fails LATE and misleadingly: `ream migrate`
    /// reported "db.execute is not a function", and `ream routes` silently
    /// printed no routes at all because `router.getRoutes` read as undefined on
    /// a Promise. Nothing executes these scripts in CI, so this guards them at
    /// the source level.
    #[test]
    fn generated_scripts_await_every_container_resolve() {
        // Stop at the test module: its own string literals mention the call.
        let file = include_str!("commands.rs");
        let source = file.split("#[cfg(test)]").next().unwrap_or(file);
        // The await sits ahead of the receiver (`await app.getApp().container…`),
        // so the whole statement is what has to carry it.
        for line in source.lines() {
            if line.contains("container.resolve(") {
                assert!(
                    line.contains("await "),
                    "unawaited container.resolve() in generated script: {}",
                    line.trim()
                );
            }
        }
    }

    /// The migration script must not double-apply migrations: AtlasProvider
    /// auto-migrates on boot unless this flag is set before `.start()`.
    #[test]
    fn migration_script_skips_the_boot_migration() {
        let source = include_str!("commands.rs");
        assert!(source.contains("REAM_SKIP_BOOT_MIGRATE"));
    }

    /// A unique directory under the system temp dir, as `doctor`'s tests use.
    fn loader_fixture(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("ream-loader-{}-{}", std::process::id(), name));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("fixture dir");
        dir
    }

    #[test]
    fn ts_loader_guard_lets_an_installed_project_through() {
        let dir = loader_fixture("installed");
        std::fs::create_dir_all(dir.join("node_modules/@swc-node/register")).unwrap();
        assert!(require_ts_loader_at(&dir).is_ok());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn ts_loader_guard_says_what_to_install_instead_of_err_module_not_found() {
        let dir = loader_fixture("absent");
        std::fs::write(dir.join("package.json"), r#"{"name":"app"}"#).unwrap();

        let err = require_ts_loader_at(&dir).unwrap_err();
        // The point of the guard: Node's own message names a package the user
        // never asked for, from a path inside their app.
        assert!(err.contains("@swc-node/register is required by this command"));
        assert!(err.contains("pnpm add -D @swc-node/register"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn ts_loader_guard_tells_a_stale_tree_to_install_not_to_add() {
        let dir = loader_fixture("declared");
        std::fs::write(
            dir.join("package.json"),
            r#"{"devDependencies":{"@swc-node/register":"^1"}}"#,
        )
        .unwrap();

        let err = require_ts_loader_at(&dir).unwrap_err();
        assert!(err.contains("pnpm install"));
        assert!(!err.contains("pnpm add -D"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The guard is worth nothing on a command that never calls it. This pins
    /// the count so a new app-booting command cannot quietly skip it.
    #[test]
    fn every_app_booting_command_calls_the_loader_guard() {
        let source = include_str!("commands.rs");
        // The pattern carries its leading indentation and trailing semicolon so
        // this assertion does not count the literal on this very line — the file
        // is read back through include_str!.
        let calls = source.matches("\n    require_ts_loader()?;").count();
        assert_eq!(
            calls, 9,
            "expected the 9 app-booting commands to guard on the TypeScript loader; \
             if you added or removed one, update this count deliberately"
        );
    }
}
