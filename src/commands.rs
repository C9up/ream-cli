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
/// What a signal-terminated child means for the command that supervised it.
///
/// Ctrl-C is how a dev server is meant to be stopped, so INT and TERM are not
/// failures; anything else is the process dying, and saying which signal is the
/// difference between "it stopped" and "the OOM killer took it".
#[cfg(unix)]
fn signal_outcome(status: &std::process::ExitStatus) -> Result<(), String> {
    use std::os::unix::process::ExitStatusExt;
    match status.signal() {
        // SIGINT / SIGTERM: asked to stop, and it did.
        Some(2) | Some(15) | None => Ok(()),
        Some(9) => Err(
            "'node' was killed (SIGKILL) — usually the OOM killer, or an external `kill -9`"
                .to_string(),
        ),
        Some(11) => Err(
            "'node' crashed (SIGSEGV) — a native addon fault, not a JavaScript error".to_string(),
        ),
        Some(other) => Err(format!("'node' was terminated by signal {other}")),
    }
}

/// Windows has no signals: a process without an exit code has nothing to
/// report beyond the fact.
#[cfg(not(unix))]
fn signal_outcome(_status: &std::process::ExitStatus) -> Result<(), String> {
    Err("'node' ended without an exit code".to_string())
}

/// Run the built application, with V8's compile cache turned on.
///
/// `NODE_COMPILE_CACHE` makes V8 keep the bytecode it compiled the JavaScript
/// into, and reuse it on the next boot instead of parsing and compiling the
/// same files again. Set here rather than inside the framework because the
/// variable is read at process start: a call from library code would only ever
/// cover the modules imported after it, and miss everything that got the app
/// that far.
///
/// NAMED DEVIATION — AdonisJS does not do this; neither `@adonisjs/core` nor
/// `@adonisjs/assembler` mentions the compile cache. It is a plain win with no
/// semantic effect, and it belongs to `start` rather than to `dev` because a
/// development server is restarted by a file change, not by a deploy.
///
/// An existing value is left alone: an operator who pointed the cache somewhere
/// else, or emptied the variable to turn it off, meant it.
pub fn run_start() -> Result<(), String> {
    // Where the application's `.env*` files are, which after a build is not
    // where the application is: `start/env.js` resolves its root from its own
    // URL and lands inside `dist/`, so the files at the project root become
    // invisible and `start` dies on a `.env` it is standing next to.
    //
    // `ENV_PATH` is AdonisJS's own variable for this, and a directory as it is
    // there. A variable rather than `--env-file`: node applies those last-wins
    // while the framework's loader applies them most-specific-first, and a
    // second copy of that ordering rule here would be one to keep in step
    // forever.
    //
    // Only when the project actually has one of those files. Setting it
    // regardless would turn "this application has no env file" into an error,
    // because naming a directory that holds none is upstream's way of saying
    // the path is wrong.
    if std::env::var_os("ENV_PATH").is_none() && !env_files_in(std::path::Path::new(".")).is_empty()
    {
        if let Ok(root) = std::env::current_dir() {
            unsafe { std::env::set_var("ENV_PATH", root) };
        }
    }

    if std::env::var_os("NODE_COMPILE_CACHE").is_none() {
        // Under `node_modules` with the other build artefacts, so deleting that
        // directory clears the cache too and nothing has to know it exists.
        unsafe { std::env::set_var("NODE_COMPILE_CACHE", "node_modules/.cache/ream") };
    }
    if let Some(reason) = dependency_served_as_typescript() {
        // Refused rather than attempted: node would fail a second later with
        // ERR_MODULE_NOT_FOUND for a file that is exactly where it says it is,
        // and that trace says nothing about why.
        return Err(reason);
    }
    spawn_node("node", &["dist/bin/server.js"])
}

/// Whether a linked dependency answers with TypeScript rather than JavaScript.
///
/// `start` runs plain `node`, deliberately: a production install has no
/// TypeScript loader and should not need one. Inside this repository's own
/// workspace, though, `@c9up/*` packages point their exports at `./src/*.ts`
/// so development happens against the sources — `publishConfig` rewrites them
/// to `./dist/*.js` on the way to the registry. A built application there
/// therefore imports a `.ts` file and dies on `ERR_MODULE_NOT_FOUND` for a
/// module that is right where it says it is.
///
/// Saying so beats the stack trace: the failure is about how the dependency is
/// linked, and nothing in that trace mentions it.
fn dependency_served_as_typescript() -> Option<String> {
    let manifest = std::fs::read_to_string("node_modules/@c9up/ream/package.json").ok()?;
    let parsed: serde_json::Value = serde_json::from_str(&manifest).ok()?;
    let exports = parsed.get("exports")?.to_string();
    if !exports.contains(".ts\"") {
        return None;
    }
    Some(
        [
            "`@c9up/ream` resolves to TypeScript sources, which `ream start` cannot load:",
            "it runs plain node, because a production install has no TypeScript loader.",
            "A workspace checkout links the framework that way on purpose; a registry",
            "install resolves to compiled JavaScript and starts normally.",
            "Use `ream dev` here.",
        ]
        .join("\n  "),
    )
}

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

/// The `.env*` files sitting in `dir`.
///
/// Listed from the directory rather than rebuilt from `NODE_ENV`: which names
/// are loaded (and how `prod` becomes `production`) lives in `@c9up/ream`'s env
/// loader, and a second copy of that rule here would drift from it.
fn env_files_in(dir: &std::path::Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<String> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_str()?.to_string();
            if !name.starts_with(".env") {
                return None;
            }
            // A template is not configuration: restarting the server because
            // someone documented a variable would be a restart for nothing.
            if name.ends_with(".example") || name.ends_with(".sample") {
                return None;
            }
            entry.file_type().ok()?.is_file().then_some(name)
        })
        .collect();
    // `read_dir` has no defined order, and the argument list has to be stable.
    found.sort();
    found
}

/// Args for `ream dev`.
///
/// swc-node reads `.swcrc` (which extends `@c9up/ream/swcrc.app.json`,
/// `decoratorMetadata: true`) and EMITS `design:paramtypes` — required for IoC
/// constructor injection. `tsx` / esbuild can NOT emit it, which silently broke
/// DI in dev (every injected dependency resolved to `undefined`).
///
/// `@c9up/ream/hot` registers module hooks that track the import graph and swap
/// a changed module inside the running process. Node's own `--watch` is
/// deliberately ABSENT: it restarts on any change, which is exactly what hot
/// reloading exists to avoid, and having both means the restart always wins.
///
/// There is no second mode any more. Hot reloading used to depend on the
/// application having installed `hot-hook`, so a project created before that
/// convention quietly restarted on every save instead — and was told nothing,
/// because a restart looks like it is working. The loader is the framework's
/// own now, so it is simply always there; a project that declares no
/// boundaries gets a full reload for every change, which is the behaviour
/// `--watch` gave it, by a shorter route.
///
/// `resources/` is deliberately absent from what the loader watches: an asset
/// watcher owns it, and a stylesheet edit must not restart the server.
pub fn dev_args() -> Vec<String> {
    vec![
        "--import".to_string(),
        "@swc-node/register/esm-register".to_string(),
        // After the TypeScript loader, never before: this file is TypeScript
        // itself and nothing can load it until swc-node is registered.
        "--import".to_string(),
        "@c9up/ream/hot".to_string(),
        "bin/server.ts".to_string(),
    ]
}

/// Read `assets` from the rc file, if the project has one.
///
/// The rc file is TypeScript, so it is read by Node rather than parsed here —
/// the same route `ream test` takes. A project without an rc file, or without
/// an `assets` key, simply has nothing to run alongside the server.
pub struct RcSlice {
    pub assets: crate::dev::AssetsConfig,
    /// `metaFiles[].pattern`, verbatim. Expanded here, in Rust — see `glob_into`.
    pub meta_file_patterns: Vec<String>,
}

/// Read the parts of the rc file the CLI acts on, in ONE Node call.
///
/// Node is not a convenience here and it is not avoidable: `reamrc.ts` is
/// TypeScript, and `providers` is an array of `() => import(...)`. Nothing can
/// read it without executing it. What IS avoidable is doing it twice, which is
/// why `assets` and `metaFiles` come back together rather than one call each —
/// `ream build` needs both.
///
/// Only the patterns come back. Expanding them is a directory walk and a
/// string match, which the CLI does itself.
pub fn read_rc() -> Result<RcSlice, String> {
    if !std::path::Path::new("reamrc.ts").exists() {
        return Ok(RcSlice {
            assets: crate::dev::AssetsConfig::default(),
            meta_file_patterns: Vec::new(),
        });
    }

    let output = Command::new("node")
        .args([
            "--import",
            "@swc-node/register/esm-register",
            "--input-type=module",
            "-e",
            "const rc = (await import('./reamrc.ts')).default; \
             process.stdout.write(JSON.stringify({ \
               assets: rc?.assets ?? null, \
               metaFiles: rc?.metaFiles ?? [], \
             }));",
        ])
        .output()
        .map_err(|e| format!("Failed to read reamrc.ts: {e}"))?;

    if !output.status.success() {
        // A broken rc file must not silently drop the assets pipeline or ship a
        // `dist/` without its translations: say so rather than carrying on.
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("Could not read reamrc.ts:\n{}", stderr.trim()));
    }

    let raw = String::from_utf8_lossy(&output.stdout);
    let value: serde_json::Value = serde_json::from_str(raw.trim())
        .map_err(|e| format!("reamrc.ts did not yield readable config: {e}"))?;

    let assets = crate::dev::parse_assets(
        &value
            .get("assets")
            .map_or_else(|| "null".to_string(), std::string::ToString::to_string),
    )?;

    let meta_file_patterns = value
        .get("metaFiles")
        .and_then(serde_json::Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| {
                    entry
                        .get("pattern")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(RcSlice {
        assets,
        meta_file_patterns,
    })
}

/// The `assets` block alone, for the callers that need nothing else.
pub fn read_assets_config() -> Result<crate::dev::AssetsConfig, String> {
    Ok(read_rc()?.assets)
}

/// `ream dev` — the server, plus whatever the rc file says builds the assets.
pub fn run_dev(clear_screen: bool) -> Result<(), String> {
    // The child clears the terminal before it restarts, the way `ace serve`
    // does; `--no-clear` says not to. Passed as an environment variable
    // because the flag belongs to this process and the decision belongs to
    // the loader running inside node.
    if !clear_screen {
        // SAFETY: single-threaded here — nothing has spawned yet.
        unsafe { std::env::set_var("REAM_DEV_CLEAR_SCREEN", "false") };
    }
    // Tells the application it was started by `ream dev`, which is what makes
    // it print the ready sticker. Upstream's dev server prints that itself;
    // ours cannot, because the port is the child's to know.
    // SAFETY: same — nothing has spawned yet.
    unsafe { std::env::set_var("REAM_DEV", "true") };

    if !std::path::Path::new("package.json").exists() {
        return Err("Not in a Ream project (no package.json found)".to_string());
    }
    require_ts_loader()?;

    let assets = read_assets_config()?;
    let Some(watcher) = assets.dev_server else {
        // Nothing to run alongside: keep the plain path, where the server owns
        // the terminal and its output is not piped through a prefix.
        let args = dev_args();
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        // The server asks for its own restart by exiting on a
        // known code — see `EXIT_RESTART`. Without this loop that request is
        // just the dev server quitting, which is worse than no HMR at all.
        let recoverable = crate::dev::recoverable_paths(
            crate::dev::WATCH_DIRS,
            &env_files_in(std::path::Path::new(".")),
        );
        loop {
            let status = inherited_status("node", &refs)?;
            match status.code() {
                Some(crate::dev::EXIT_RESTART) => continue,
                // Exit 0 is a session that ENDED: it is how the graceful
                // shutdown leaves on Ctrl-C, and waiting for an edit there
                // would hang the terminal it just gave back.
                Some(0) => return Ok(()),
                // Anything else is a crash, and the watcher that would have
                // seen the fix died inside the process — so it is watched from
                // here instead. A syntax error used to end the session.
                Some(code) if !recoverable.is_empty() => {
                    eprintln!("{}", crate::dev::crash_notice(code));
                    crate::dev::wait_for_change(&recoverable);
                    continue;
                }
                Some(code) => return Err(format!("'node' exited with code {code}")),
                // No code means a SIGNAL, not a clean exit — `None` used to be
                // folded in with success, so a segfault in a native addon or a
                // kill by the OOM killer ended `ream dev` with status 0 and not
                // a word about why the server was gone.
                None => return signal_outcome(&status),
            }
        }
    };

    let server = crate::dev::CommandSpec {
        command: "node".to_string(),
        args: dev_args(),
    };

    crate::dev::run_together(
        vec![
            crate::dev::Process {
                label: "server".to_string(),
                colour: crate::dev::COLOURS[0],
                spec: server,
                // Only the server restarts itself — it asks for it by exiting
                // on `EXIT_RESTART`. An asset watcher that exits has genuinely
                // stopped.
                restartable: true,
            },
            crate::dev::Process {
                label: "assets".to_string(),
                colour: crate::dev::COLOURS[1],
                spec: watcher,
                restartable: false,
            },
        ],
        crate::dev::WATCH_DIRS,
    )
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

    // One read, both slices: the rc file is TypeScript and costs a Node process
    // to read at all, so `assets` and `metaFiles` come back together.
    let rc = read_rc()?;

    if let Some(build) = rc.assets.build {
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

    spawn_node("npx", &["tsc"])?;
    make_output_self_contained(&rc.meta_file_patterns)
}

/// Does `path` satisfy `pattern`?
///
/// The glob subset a `metaFiles` entry actually uses, and no more: `*` within a
/// segment, `**` across segments, and `{a,b}` alternation. Written out rather
/// than pulled in, because the whole surface is three rules and a crate would
/// bring a matcher for a syntax nothing here writes.
///
/// Segment-wise, which is what makes `**` mean "any depth" rather than "any
/// characters" — a character-wise matcher lets `resources/*.json` reach
/// `resources/lang/fr.json`, and the build then flattens a tree nobody asked it
/// to.
fn glob_matches(pattern: &str, path: &str) -> bool {
    fn segments(value: &str) -> Vec<&str> {
        value.split('/').filter(|part| !part.is_empty()).collect()
    }
    fn walk(pattern: &[&str], path: &[&str]) -> bool {
        match pattern.split_first() {
            None => path.is_empty(),
            Some((&"**", rest)) => {
                // Zero or more segments: try every split point.
                (0..=path.len()).any(|skip| walk(rest, &path[skip..]))
            }
            Some((head, rest)) => match path.split_first() {
                None => false,
                Some((name, tail)) => segment_matches(head, name) && walk(rest, tail),
            },
        }
    }
    walk(&segments(pattern), &segments(path))
}

/// One path segment against one pattern segment: `*` and `{a,b}`.
fn segment_matches(pattern: &str, name: &str) -> bool {
    if let Some(open) = pattern.find('{') {
        let Some(close) = pattern[open..].find('}').map(|at| at + open) else {
            return literal_matches(pattern, name);
        };
        return pattern[open + 1..close].split(',').any(|option| {
            segment_matches(
                &format!("{}{option}{}", &pattern[..open], &pattern[close + 1..]),
                name,
            )
        });
    }
    literal_matches(pattern, name)
}

/// `*` only, greedy with backtracking.
fn literal_matches(pattern: &str, name: &str) -> bool {
    match pattern.split_once('*') {
        None => pattern == name,
        Some((prefix, rest)) => {
            if !name.starts_with(prefix) {
                return false;
            }
            let remainder = &name[prefix.len()..];
            (0..=remainder.len()).any(|take| literal_matches(rest, &remainder[take..]))
        }
    }
}

/// Every file under `root` that any pattern matches, as root-relative paths.
///
/// Depth-capped for the same reason `scan_files` is: a pattern is a string in a
/// config file, and an unbounded walk follows whatever happens to be there.
/// `node_modules` and `dist` are skipped outright — a pattern that reached into
/// either would copy the build into itself.
fn meta_files_matching(root: &std::path::Path, patterns: &[String]) -> Vec<String> {
    fn walk(
        root: &std::path::Path,
        dir: &std::path::Path,
        patterns: &[String],
        depth: usize,
        found: &mut Vec<String>,
    ) {
        if depth > 12 {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if path.is_dir() {
                if name == "node_modules" || name == "dist" || name.starts_with('.') {
                    continue;
                }
                walk(root, &path, patterns, depth + 1, found);
                continue;
            }
            let Ok(relative) = path.strip_prefix(root) else {
                continue;
            };
            let relative = relative.to_string_lossy().replace('\\', "/");
            if patterns
                .iter()
                .any(|pattern| glob_matches(pattern, &relative))
            {
                found.push(relative);
            }
        }
    }

    let mut found = Vec::new();
    if !patterns.is_empty() {
        walk(root, root, patterns, 0, &mut found);
    }
    found.sort();
    found
}

/// Copy the files `metaFiles` names into the build output.
///
/// Paths keep their layout — `resources/lang/fr.json` lands at
/// `dist/resources/lang/fr.json` — which is what lets a loader configured with
/// `../resources/lang/` find them from `dist/`.
fn copy_meta_files(out: &std::path::Path, patterns: &[String]) -> Result<(), String> {
    let root =
        std::env::current_dir().map_err(|e| format!("Could not resolve the project root: {e}"))?;

    for relative in meta_files_matching(&root, patterns) {
        let source = root.join(&relative);
        let target = out.join(&relative);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Could not create {}: {e}", parent.display()))?;
        }
        std::fs::copy(&source, &target)
            .map_err(|e| format!("Could not copy {relative} into dist/: {e}"))?;
    }
    Ok(())
}

/// Files copied beside the compiled output, in the order a package manager is
/// likely to be in use. Only those that exist are copied.
const BUILD_META_FILES: &[&str] = &[
    "package.json",
    "pnpm-lock.yaml",
    "package-lock.json",
    "yarn.lock",
    "bun.lockb",
];

/// Copy the manifest and the lockfile into `dist/`.
///
/// Not a convenience: without the manifest the build does not run at all.
/// `dist/reamrc.js` imports `#providers/AppProvider.js`, and Node resolves a
/// `#` specifier against the package.json that governs the importing FILE —
/// walking up from `dist/`. With none there it reaches the project root, whose
/// `imports` map points at `./providers/*`: the TypeScript sources, which have
/// no `.js` to find. `ream start` then died on a module it had just compiled.
///
/// A copy inside `dist/` makes the same map resolve to `dist/providers/*`,
/// which is where the output is. The lockfile rides along for the same reason
/// upstream's bundler copies it: the folder is then something a deployment can
/// install dependencies into on its own.
///
/// This is AdonisJS's shape — its `Bundler` declares the manifest and lockfile
/// per package manager and copies them into the build output.
fn make_output_self_contained(meta_file_patterns: &[String]) -> Result<(), String> {
    let out = std::path::Path::new("dist");
    if !out.is_dir() {
        // `tsc` emitted nothing, which it reports itself; saying it twice helps
        // nobody.
        return Ok(());
    }
    for name in BUILD_META_FILES {
        let source = std::path::Path::new(name);
        if !source.is_file() {
            continue;
        }
        std::fs::copy(source, out.join(name))
            .map_err(|e| format!("Could not copy {name} into dist/: {e}"))?;
    }
    // And whatever the application declared as its own non-module files.
    copy_meta_files(out, meta_file_patterns)
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
            .setEnvironment('repl')
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

/// Whether this machine says it is production, under any of the spellings
/// people actually set.
///
/// NAMED DEVIATION — upstream's own `generate:key` compares the exact string
/// (`process.env.NODE_ENV !== "production"`), and this does not.
///
/// `NODE_ENV=prod` is ordinary in a Dockerfile or a platform dashboard, and
/// read exactly it answers "not production": the guard steps aside and the
/// command rewrites `APP_KEY` on a live box, invalidating every session,
/// signed cookie and signed URL in circulation. That is not a failure mode
/// worth reproducing for the sake of matching a string comparison — and the
/// framework normalises these same aliases everywhere else, so the strict
/// reading here was the odd one out.
fn in_production_env() -> bool {
    std::env::var("NODE_ENV")
        .is_ok_and(|env| matches!(env.to_lowercase().as_str(), "prod" | "production"))
}

/// `generate:key` — write a fresh APP_KEY into `.env`.
///
/// A scaffolded project ships a placeholder; leaving it in place means cookies,
/// sessions and CSRF tokens are signed with a value that is public knowledge.
///
/// Generation is delegated to Node's `crypto` (as the Web Push key generator does)
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
    let in_production = in_production_env();
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
        if let Some(value) = crate::envfile::read_env_value(&existing, "APP_KEY") {
            if !value.is_empty() && value != PLACEHOLDER {
                return Err("APP_KEY is already set in .env.\n  \
                     Re-run with --force to replace it — every existing cookie, \
                     session and signed URL becomes invalid."
                    .to_string());
            }
        }
    }

    let key = generate_app_key()?;
    let updated = crate::envfile::upsert_env_var(&existing, "APP_KEY", &key);
    std::fs::write(env_path, updated).map_err(|e| format!("Failed to write .env: {}", e))?;

    println!();
    let lines = vec![
        crate::ui::paint("Generated APP_KEY", crate::ui::GREEN),
        format!(
            "APP_KEY = [redacted — written to .env, {} chars]",
            key.chars().count()
        ),
        String::new(),
        crate::ui::paint(
            "Move it to a secrets manager before deploying.",
            crate::ui::DIM,
        ),
    ];
    for line in crate::ui::sticker(&lines, |character| {
        crate::ui::paint(character, crate::ui::DIM)
    }) {
        println!("{line}");
    }
    println!();
    Ok(())
}

/// 32 random bytes, base64url — same shape AdonisJS generates.
pub fn generate_app_key() -> Result<String, String> {
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
    crate::ui::warning(&format!(
        "this project's own commands are not listed — {reason}"
    ));
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

    // One width for the WHOLE listing, not one per group: the descriptions of
    // every section then line up in a single column, which is what makes a
    // long list scannable. Measured on the visible width — the names are
    // coloured below, and escape codes occupy no column.
    let width = entries
        .iter()
        .map(|entry| crate::ui::display_width(&entry.name))
        .max()
        .unwrap_or(0);
    let mut current_group: Option<String> = None;

    println!();
    println!(
        "{} ream <command> {}",
        crate::ui::heading("Usage:"),
        crate::ui::paint("[options]", crate::ui::DIM)
    );
    println!();

    for entry in &entries {
        let group = group_of(&entry.name);
        if current_group.as_ref() != Some(&group) {
            if current_group.is_some() {
                println!();
            }
            println!(
                "{}",
                crate::ui::heading(if group.is_empty() {
                    "Available commands"
                } else {
                    group.as_str()
                })
            );
            current_group = Some(group);
        }
        println!(
            "  {}  {}",
            crate::ui::pad_end(&crate::ui::paint(&entry.name, crate::ui::GREEN), width),
            crate::ui::paint(&entry.description, crate::ui::DIM)
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

/// Inspect: list registered routes, providers, and container bindings.
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
            .useRcFile(rc).setEnvironment('console').warmUp();
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

        const bindings = app.getApp().container.bindings ?? [];
        console.log('\nContainer bindings:');
        if (bindings.length === 0) {
            console.log('  (none)');
        } else {
            for (const b of bindings) {
                const via = b.target ? ` -> ${b.target}` : '';
                console.log(`  ${b.token}${via}  (${b.scope})`);
            }
        }

        console.log(`\nTotal: ${routes.length} routes, ${providers.length} providers, ${bindings.length} bindings.`);
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

/// Show version and environment info.
pub fn info() -> Result<(), String> {
    println!(
        "{} {}",
        crate::ui::paint("ream", crate::ui::BOLD),
        env!("CARGO_PKG_VERSION")
    );
    println!();

    // Node.js version
    match Command::new("node").arg("--version").output() {
        Ok(output) if output.status.success() => {
            let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
            println!(
                "  {}  {}",
                crate::ui::paint("Node.js:", crate::ui::DIM),
                version
            );
        }
        Ok(_) => println!(
            "  {}  {}",
            crate::ui::paint("Node.js:", crate::ui::DIM),
            crate::ui::paint("error", crate::ui::YELLOW)
        ),
        Err(_) => println!(
            "  {}  {}",
            crate::ui::paint("Node.js:", crate::ui::DIM),
            crate::ui::paint("not found", crate::ui::YELLOW)
        ),
    }

    // pnpm version
    match Command::new("pnpm").arg("--version").output() {
        Ok(output) if output.status.success() => {
            let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
            println!(
                "  {}     {}",
                crate::ui::paint("pnpm:", crate::ui::DIM),
                version
            );
        }
        Ok(_) => println!(
            "  {}     {}",
            crate::ui::paint("pnpm:", crate::ui::DIM),
            crate::ui::paint("error", crate::ui::YELLOW)
        ),
        Err(_) => println!(
            "  {}     {}",
            crate::ui::paint("pnpm:", crate::ui::DIM),
            crate::ui::paint("not found", crate::ui::YELLOW)
        ),
    }

    // Rust version
    match Command::new("rustc").arg("--version").output() {
        Ok(output) if output.status.success() => {
            let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
            println!(
                "  {}     {}",
                crate::ui::paint("Rust:", crate::ui::DIM),
                version
            );
        }
        Ok(_) => println!(
            "  {}     {}",
            crate::ui::paint("Rust:", crate::ui::DIM),
            crate::ui::paint("error", crate::ui::YELLOW)
        ),
        Err(_) => println!(
            "  {}     {}",
            crate::ui::paint("Rust:", crate::ui::DIM),
            crate::ui::paint("not found", crate::ui::YELLOW)
        ),
    }

    // Check if in a Ream project
    if std::path::Path::new("reamrc.ts").exists() {
        println!();
        println!(
            "  {}  reamrc.ts found (framework mode)",
            crate::ui::paint("Project:", crate::ui::DIM)
        );
    } else if std::path::Path::new("package.json").exists() {
        println!();
        println!(
            "  {}  package.json found (toolkit mode)",
            crate::ui::paint("Project:", crate::ui::DIM)
        );
    }

    Ok(())
}

/// Run the test suites declared in the rc file's `tests` block.
///
/// The AdonisJS stratification: the framework reads its rc file and hands the
/// suites to the runner. All of that lives in TypeScript (`@c9up/helix-plugin-ream/runner`),
/// so this stays a thin spawn — the same split as `run_migration`.
/// The coverage half of `ream test`, spelled the way helix spells it.
///
/// Coverage belongs to the runner, not to ream: the CLI only names the flags
/// and hands the values across, the same way it hands `--threads` across.
pub struct CoverageFlags<'a> {
    pub enabled: bool,
    pub reporters: Option<&'a str>,
    pub dir: Option<&'a str>,
    pub thresholds: Option<&'a str>,
    pub include: Option<&'a str>,
    pub exclude: Option<&'a str>,
}

/// What `--coverage` measures when the run names nothing.
///
/// The runner's own default is `src/**`, which a package has and an application
/// does not — an app's code sits in `app/`, `start/`, `config/`. Left alone,
/// every `ream test --coverage` in an application would print a report over
/// zero files. `src/**` stays in the list so a package driven by `ream test`
/// keeps working.
const APP_COVERAGE_INCLUDE: &[&str] = &[
    "app/**/*.{ts,tsx}",
    "start/**/*.{ts,tsx}",
    "config/**/*.{ts,tsx}",
    "commands/**/*.{ts,tsx}",
    "database/**/*.{ts,tsx}",
    "src/**/*.{ts,tsx,js,mjs,cjs}",
];

pub fn run_tests(
    suites: &[String],
    threads: Option<usize>,
    reporters: Option<&str>,
    bail: bool,
    coverage: CoverageFlags<'_>,
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

    let options = test_options(suites, threads, reporters, bail, &coverage)?;

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
    coverage: &CoverageFlags<'_>,
) -> Result<serde_json::Value, String> {
    Ok(serde_json::json!({
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
        // Here the process IS the run, so a summary followed by an unexplained
        // hang is worse than an exit naming what stayed open. The runner leaves
        // this off by default, because it is a library and the caller owns the
        // process.
        "drainGuard": true,
        "coverage": coverage_options(coverage)?,
    }))
}

/// A comma-separated flag value, trimmed, with the empty entries dropped.
fn split_list(value: &str) -> Vec<&str> {
    value
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .collect()
}

/// The globs of a `--coverage-include` / `--coverage-exclude` value.
fn globs(value: &str) -> Result<Vec<&str>, String> {
    let patterns = split_list(value);
    if patterns.is_empty() {
        return Err("a coverage glob flag was given no pattern".to_string());
    }
    Ok(patterns)
}

/// Translate the coverage flags into helix's `CoverageOptions`, or `null` when
/// the run collects nothing.
///
/// The keys the user did not name are LEFT OUT rather than sent as `null`: the
/// runner reads `reporters`/`outputDir` as "unset means the default", and an
/// explicit `null` would have to be defended against on the other side.
fn coverage_options(flags: &CoverageFlags<'_>) -> Result<serde_json::Value, String> {
    // Naming a coverage detail without asking for coverage is a typo worth
    // saying out loud — silently collecting nothing is the failure mode that
    // wastes an afternoon.
    let detailed = flags.reporters.is_some()
        || flags.dir.is_some()
        || flags.thresholds.is_some()
        || flags.include.is_some()
        || flags.exclude.is_some();
    if !flags.enabled {
        if detailed {
            return Err(
                "--coverage-* was given without --coverage, so nothing would be collected"
                    .to_string(),
            );
        }
        return Ok(serde_json::Value::Null);
    }

    let mut options = serde_json::Map::new();
    options.insert("enabled".to_string(), serde_json::Value::Bool(true));
    if let Some(reporters) = flags.reporters {
        let names = split_list(reporters);
        if names.is_empty() {
            return Err("--coverage-reporters names no reporter".to_string());
        }
        options.insert("reporters".to_string(), serde_json::json!(names));
    }
    match flags.include {
        Some(include) => options.insert("include".to_string(), serde_json::json!(globs(include)?)),
        None => options.insert(
            "include".to_string(),
            serde_json::json!(APP_COVERAGE_INCLUDE),
        ),
    };
    if let Some(exclude) = flags.exclude {
        options.insert("exclude".to_string(), serde_json::json!(globs(exclude)?));
    }
    if let Some(dir) = flags.dir {
        options.insert("outputDir".to_string(), serde_json::json!(dir));
    }
    if let Some(thresholds) = flags.thresholds {
        // Parsed HERE so a malformed value is named before a full test run is
        // spent on it, and so it reaches the script as JSON rather than as text
        // spliced into a program.
        let parsed: serde_json::Value = serde_json::from_str(thresholds)
            .map_err(|err| format!("--coverage-thresholds is not valid JSON: {err}"))?;
        if !parsed.is_object() {
            return Err(
                "--coverage-thresholds expects a JSON object, e.g. {\"lines\":80}".to_string(),
            );
        }
        options.insert("thresholds".to_string(), parsed);
    }
    Ok(serde_json::Value::Object(options))
}

#[cfg(test)]
mod tests {
    use super::{coverage_options, CoverageFlags};

    fn flags<'a>() -> CoverageFlags<'a> {
        CoverageFlags {
            enabled: false,
            reporters: None,
            dir: None,
            thresholds: None,
            include: None,
            exclude: None,
        }
    }

    /// Without `--coverage` the runner is handed nothing at all — not an
    /// object with `enabled: false`, which it would still have to reason about.
    #[test]
    fn coverage_is_absent_until_asked_for() {
        assert!(coverage_options(&flags()).unwrap().is_null());
    }

    /// The application layout, not the runner's `src/**`: an app keeps its code
    /// in `app/`, and a report over zero files reads as "nothing to cover".
    #[test]
    fn enabling_coverage_measures_the_app_layout() {
        let options = coverage_options(&CoverageFlags {
            enabled: true,
            ..flags()
        })
        .unwrap();
        let include = options["include"].as_array().expect("include");
        assert!(include.iter().any(|glob| glob == "app/**/*.{ts,tsx}"));
        assert!(include.iter().any(|glob| glob == "start/**/*.{ts,tsx}"));
        assert!(options.get("exclude").is_none());
    }

    /// A named `--coverage-include` replaces the layout rather than adding to
    /// it, so a run can be narrowed to one directory.
    #[test]
    fn an_explicit_include_replaces_the_layout() {
        let options = coverage_options(&CoverageFlags {
            enabled: true,
            include: Some("app/models/**/*.ts, app/services/**/*.ts"),
            ..flags()
        })
        .unwrap();
        assert_eq!(
            options["include"],
            serde_json::json!(["app/models/**/*.ts", "app/services/**/*.ts"])
        );
    }

    /// Detailing a collection that was never enabled collects nothing at all,
    /// which is worth a word rather than a green run with an empty report.
    #[test]
    fn detailing_coverage_without_enabling_it_is_refused() {
        let err = coverage_options(&CoverageFlags {
            reporters: Some("lcov"),
            ..flags()
        })
        .unwrap_err();
        assert!(err.contains("--coverage"), "{err}");
    }

    /// Thresholds reach the runner as JSON, so a malformed value is named here
    /// rather than after a full run.
    #[test]
    fn thresholds_are_parsed_before_the_run() {
        let options = coverage_options(&CoverageFlags {
            enabled: true,
            thresholds: Some(r#"{"lines":80}"#),
            ..flags()
        })
        .unwrap();
        assert_eq!(options["thresholds"]["lines"], 80);

        let err = coverage_options(&CoverageFlags {
            enabled: true,
            thresholds: Some("80"),
            ..flags()
        })
        .unwrap_err();
        assert!(err.contains("JSON object"), "{err}");
    }

    /// `generate:key` refuses on a production machine whatever spelling it
    /// uses. Reading the exact string only let `NODE_ENV=prod` through, and
    /// rewriting the key there invalidates every session in circulation.
    ///
    /// One test rather than two: `NODE_ENV` is process-global and cargo runs
    /// tests in parallel, so two of them setting it race each other.
    #[test]
    fn production_is_recognised_under_every_spelling() {
        let restore = std::env::var("NODE_ENV").ok();

        for value in ["production", "prod", "PROD", "Production"] {
            unsafe { std::env::set_var("NODE_ENV", value) };
            assert!(super::in_production_env(), "`{}` is production", value);
        }
        for value in ["development", "dev", "test", "staging", "stage", ""] {
            unsafe { std::env::set_var("NODE_ENV", value) };
            assert!(!super::in_production_env(), "`{}` is not production", value);
        }
        unsafe { std::env::remove_var("NODE_ENV") };
        assert!(!super::in_production_env(), "absent is not production");

        match restore {
            Some(v) => unsafe { std::env::set_var("NODE_ENV", v) },
            None => unsafe { std::env::remove_var("NODE_ENV") },
        }
    }

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
    fn only_real_env_files_are_listed() {
        let dir =
            std::env::temp_dir().join(format!("ream-env-list-{}-{}", std::process::id(), line!()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".env.d")).expect("a directory named like one");
        std::fs::write(dir.join(".env"), "").expect(".env");
        std::fs::write(dir.join(".env.production"), "").expect(".env.production");
        std::fs::write(dir.join(".env.example"), "").expect(".env.example");
        std::fs::write(dir.join("env.ts"), "").expect("env.ts");

        let found = env_files_in(&dir);

        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            found,
            vec![".env".to_string(), ".env.production".to_string()]
        );
    }

    /// Node's own watcher stays off, and no `--watch-path` is handed over.
    ///
    /// Both used to be there for the mode that had no hot reloading. The loader
    /// watches the project itself now, from inside the process that holds the
    /// import graph — and `--watch` restarts on any change, which would win
    /// over every swap.
    #[test]
    fn dev_leaves_nodes_own_watcher_off() {
        let args = dev_args();
        assert!(
            !args
                .iter()
                .any(|a| a == "--watch" || a.starts_with("--watch-path")),
            "node's own watcher must be off: {:?}",
            args
        );
    }

    /// HMR mode is not "watch plus a flag": Node's `--watch` restarts on any
    /// change, so leaving it in means the restart always beats the hot swap and
    /// nothing is ever hot-reloaded.
    /// A signal is not a clean exit.
    ///
    /// `status.code()` answers `None` when a child was killed rather than
    /// returning — SIGKILL from the OOM killer, SIGSEGV from a native addon.
    /// Folded in with `Some(0)`, `ream dev` ended with status 0 and said
    /// nothing about why the server had gone.
    #[cfg(unix)]
    #[test]
    fn a_signal_is_reported_unless_it_is_the_one_the_user_sent() {
        use std::os::unix::process::ExitStatusExt;
        let killed = std::process::ExitStatus::from_raw(9);
        let segfault = std::process::ExitStatus::from_raw(11);
        let interrupted = std::process::ExitStatus::from_raw(2);
        let terminated = std::process::ExitStatus::from_raw(15);

        assert!(signal_outcome(&killed).unwrap_err().contains("OOM"));
        assert!(signal_outcome(&segfault).unwrap_err().contains("SIGSEGV"));
        // Ctrl-C is how a dev server is meant to be stopped.
        assert!(signal_outcome(&interrupted).is_ok());
        assert!(signal_outcome(&terminated).is_ok());
    }

    #[test]
    fn dev_loads_the_hot_entry_after_the_typescript_loader() {
        let args = dev_args();

        assert!(
            !args
                .iter()
                .any(|a| a == "--watch" || a.starts_with("--watch-path")),
            "node's own watcher must be off in HMR mode: {:?}",
            args
        );
        assert!(args.iter().any(|a| a == "@c9up/ream/hot"), "{:?}", args);
        // Order is load-bearing: `@c9up/ream/hot` is TypeScript, so nothing can
        // load it until swc-node is registered.
        let swc = args
            .iter()
            .position(|a| a == "@swc-node/register/esm-register")
            .expect("swc-node");
        let hot = args
            .iter()
            .position(|a| a == "@c9up/ream/hot")
            .expect("hot");
        assert!(swc < hot, "swc-node must come first: {:?}", args);
        assert_eq!(args.last().map(String::as_str), Some("bin/server.ts"));
    }

    #[test]
    fn dev_uses_swc_node_not_tsx() {
        let owned = dev_args();
        let args: Vec<&str> = owned.iter().map(String::as_str).collect();
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
        // The loader drives the reload, from inside the process that holds the
        // import graph — so node's own watcher must NOT be on as well.
        assert!(
            !args.contains(&"--watch"),
            "ream dev must not restart on every change: {:?}",
            args
        );
    }

    #[test]
    fn a_suite_name_cannot_break_out_of_the_generated_script() {
        // The name reaches an inline `node -e` script inside a JSON string
        // literal. What could terminate that literal is a double quote or a
        // newline; interpolated raw, this name would close it and run code.
        let hostile = "a\", process.exit(42); //\nb".to_string();
        let options =
            test_options(std::slice::from_ref(&hostile), None, None, false, &flags()).unwrap();

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
        let options = test_options(&[], None, Some("spec, json ,,"), false, &flags()).unwrap();
        assert_eq!(options["reporters"], serde_json::json!(["spec", "json"]));
    }

    #[test]
    fn absent_options_stay_null_so_the_script_deletes_them() {
        // `runTests` fills its own defaults; a `null` would override them.
        let options = test_options(&[], None, None, false, &flags()).unwrap();
        assert!(options["threads"].is_null());
        assert!(options["reporters"].is_null());
    }

    #[test]
    fn workers_are_spawned_with_the_swc_loader_not_input_type() {
        let options = test_options(&[], None, None, false, &flags()).unwrap();
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
            calls, 6,
            "expected the 6 app-booting commands to guard on the TypeScript loader; \
             if you added or removed one, update this count deliberately"
        );
    }
}

#[cfg(test)]
mod meta_file_tests {
    use super::{copy_meta_files, glob_matches, meta_files_matching};

    #[test]
    fn matches_a_literal_path() {
        assert!(glob_matches("config/tokens.json", "config/tokens.json"));
        assert!(!glob_matches("config/tokens.json", "config/other.json"));
    }

    #[test]
    fn a_star_stays_inside_one_segment() {
        assert!(glob_matches("resources/*.json", "resources/en.json"));
        // The trap: a character-wise matcher lets this through and the build
        // then flattens a tree nobody asked it to.
        assert!(!glob_matches("resources/*.json", "resources/lang/en.json"));
    }

    #[test]
    fn a_double_star_crosses_any_number_of_segments() {
        assert!(glob_matches("resources/**/*.json", "resources/en.json"));
        assert!(glob_matches(
            "resources/**/*.json",
            "resources/lang/en.json"
        ));
        assert!(glob_matches(
            "resources/**/*.json",
            "resources/lang/fr/deep/en.json"
        ));
        assert!(!glob_matches("resources/**/*.json", "public/en.json"));
    }

    #[test]
    fn braces_offer_alternatives() {
        let pattern = "resources/lang/**/*.{json,yaml,yml}";
        assert!(glob_matches(pattern, "resources/lang/en.json"));
        assert!(glob_matches(pattern, "resources/lang/fr.yaml"));
        assert!(glob_matches(pattern, "resources/lang/it.yml"));
        assert!(!glob_matches(pattern, "resources/lang/en.txt"));
    }

    #[test]
    fn an_unclosed_brace_is_read_literally_rather_than_panicking() {
        // A config file can hold anything; a malformed pattern must simply
        // match nothing rather than take the build down.
        assert!(!glob_matches("resources/{json", "resources/en.json"));
    }

    #[test]
    fn walks_the_tree_and_reports_root_relative_paths() {
        let dir = tempdir();
        write(&dir, "resources/lang/en.json", "{}");
        write(&dir, "resources/lang/fr/deep.json", "{}");
        write(&dir, "resources/views/home.edge", "x");
        write(&dir, "src/main.ts", "x");

        let found = meta_files_matching(&dir, &["resources/lang/**/*.json".to_string()]);
        assert_eq!(
            found,
            vec![
                "resources/lang/en.json".to_string(),
                "resources/lang/fr/deep.json".to_string(),
            ]
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn never_walks_into_node_modules_or_dist() {
        let dir = tempdir();
        write(&dir, "node_modules/pkg/resources/lang/en.json", "{}");
        write(&dir, "dist/resources/lang/en.json", "{}");
        write(&dir, "resources/lang/en.json", "{}");

        // A pattern reaching into `dist` would copy the build into itself.
        let found = meta_files_matching(&dir, &["**/*.json".to_string()]);
        assert_eq!(found, vec!["resources/lang/en.json".to_string()]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn copies_into_the_output_keeping_the_layout() {
        let dir = tempdir();
        write(&dir, "resources/lang/en.json", "{\"a\":1}");
        let out = dir.join("dist");
        std::fs::create_dir_all(&out).unwrap();

        let previous = std::env::current_dir().unwrap();
        std::env::set_current_dir(&dir).unwrap();
        let result = copy_meta_files(
            std::path::Path::new("dist"),
            &["resources/lang/**/*.json".to_string()],
        );
        std::env::set_current_dir(previous).unwrap();

        assert!(result.is_ok(), "{result:?}");
        // The layout is the point: a loader configured with `../resources/lang/`
        // finds it from `dist/` only if the path survived the copy.
        let copied = out.join("resources/lang/en.json");
        assert!(copied.is_file(), "expected {}", copied.display());
        assert_eq!(std::fs::read_to_string(copied).unwrap(), "{\"a\":1}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn no_patterns_means_no_walk_and_no_copy() {
        let dir = tempdir();
        write(&dir, "resources/lang/en.json", "{}");
        assert!(meta_files_matching(&dir, &[]).is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    fn tempdir() -> std::path::PathBuf {
        let base = std::env::temp_dir().join(format!(
            "ream-meta-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&base).unwrap();
        base.canonicalize().unwrap()
    }

    fn write(root: &std::path::Path, relative: &str, contents: &str) {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }
}
