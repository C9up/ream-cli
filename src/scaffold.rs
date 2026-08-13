//! Scaffold — create a new Ream project (pure Rust, no Node.js needed for generation).

use dialoguer::Select;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::Command;

pub fn run(name: &str) -> Result<(), String> {
    // Validate project name
    if !name
        .chars()
        .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
        return Err("Project name must be alphanumeric with hyphens/underscores only".to_string());
    }

    let target = Path::new(name);
    if target.exists() || target.is_symlink() {
        return Err(format!("'{}' already exists", name));
    }

    println!("\n  \x1b[1mCreating Ream project: {}\x1b[0m\n", name);

    // Template selection
    let templates = vec!["api", "web", "microservice", "slim"];
    let template_idx = Select::new()
        .with_prompt("Select a template")
        .items(&templates)
        .default(0)
        .interact()
        .map_err(|e| format!("Prompt failed: {}", e))?;
    let template = templates[template_idx];

    // Database selection
    let databases = vec!["postgres", "sqlite"];
    let db_idx = Select::new()
        .with_prompt("Select a database")
        .items(&databases)
        .default(0)
        .interact()
        .map_err(|e| format!("Prompt failed: {}", e))?;
    let database = databases[db_idx];

    println!(
        "\n  Scaffolding {} (template={}, database={})...\n",
        name, template, database
    );

    // Create project structure, then resolve the project root once against
    // the actual filesystem so every subsequent write_file() call is anchored
    // to the same canonical path — independent of the caller's CWD.
    //
    // Atomic create: `create_dir` (NOT `_all`) fails with `EEXIST` if
    // anything already exists at `target` — file, dir, OR symlink. Closes
    // the TOCTOU between the `target.is_symlink()` precheck above and the
    // `canonicalize` below; without it, a co-resident attacker could plant
    // a symlink in the prompt window and capture the entire scaffold.
    // `name` is single-component (alphanumeric+`-`+`_`), so non-recursive
    // create is sufficient.
    fs::create_dir(target).map_err(|e| format!("Failed to create '{}': {}", name, e))?;
    let root_canon = fs::canonicalize(target)
        .map_err(|e| format!("Failed to canonicalize project root '{}': {}", name, e))?;
    let root = root_canon.as_path();

    // Common files
    write_file(root, "package.json", &package_json(name, template))?;
    write_file(root, "tsconfig.json", &tsconfig())?;
    write_file(root, ".swcrc", &swcrc())?;
    write_file(root, ".env", &env_file(name, database))?;
    write_file(root, "env.ts", &env_typing(database))?;
    write_file(root, ".gitignore", GITIGNORE)?;
    write_file(root, "reamrc.ts", &reamrc(template))?;
    // Console entry. The microservice template runs a standalone bin/server.ts
    // with no Ignitor or rc file, so it has no console kernel to reach.
    if template != "microservice" {
        write_file(root, "bin/console.ts", CONSOLE_ENTRY)?;
    }

    // Template-specific files
    match template {
        "api" => write_api_template(root, name)?,
        "web" => write_web_template(root, name)?,
        "slim" => write_slim_template(root, name)?,
        "microservice" => write_microservice_template(root, name)?,
        _ => write_slim_template(root, name)?,
    }

    // Run pnpm install
    println!("  Installing dependencies...\n");
    let status = Command::new("pnpm")
        .arg("install")
        .current_dir(root)
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status();

    match status {
        Ok(s) if s.success() => {}
        _ => println!("  \x1b[33mWarning: pnpm install failed — run it manually\x1b[0m\n"),
    }

    println!("\n  \x1b[32mDone!\x1b[0m Next steps:\n");
    println!("    cd {}", name);
    println!("    ream dev\n");

    Ok(())
}

fn write_file(root: &Path, path: &str, content: &str) -> Result<(), String> {
    // Reject paths that would escape `root` syntactically: `..` segments,
    // an absolute root (`/foo` on Unix, `\foo` on Windows), or a Windows
    // drive prefix (`C:\foo`). `Path::join` silently discards `root` when
    // the right-hand side is absolute, so this check is the only thing
    // standing between a future caller passing user-controlled `path`
    // and an arbitrary write — defense-in-depth, even though every
    // current call site uses a hardcoded literal.
    use std::path::Component;
    for component in Path::new(path).components() {
        if matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        ) {
            return Err(format!("Refusing to write outside project root: {}", path));
        }
    }
    let full = root.join(path);
    if let Some(parent) = full.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        // `root` is already canonical (the caller resolves it once at the
        // top of `run`); only `parent` needs canonicalization to detect a
        // symlinked subdirectory whose realpath escapes the project root.
        let canon_parent = fs::canonicalize(parent)
            .map_err(|e| format!("Failed to canonicalize {}: {}", parent.display(), e))?;
        if !canon_parent.starts_with(root) {
            return Err(format!(
                "Refusing to write outside project root (symlink): {}",
                path
            ));
        }
    }
    // Symlink-leaf check: `fs::symlink_metadata` does not follow symlinks,
    // so an existing leaf symlink is detected here regardless of whether
    // its target is inside or outside the project root. Kept for the
    // friendlier error message; the `create_new` open below is the
    // syscall-level guarantee against any pre-existing entry.
    if let Ok(meta) = fs::symlink_metadata(&full) {
        if meta.file_type().is_symlink() {
            return Err(format!(
                "Refusing to write through a leaf symlink: {}",
                path
            ));
        }
    }
    // O_EXCL via `create_new(true)`: the kernel atomically refuses any
    // pre-existing entry at `full` — including hardlinks, regular files
    // planted in a TOCTOU window, and (redundantly) symlinks. Closes the
    // race between the leaf check above and the actual write.
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&full)
        .map_err(|e| format!("Failed to create {}: {}", path, e))?;
    f.write_all(content.as_bytes())
        .map_err(|e| format!("Failed to write {}: {}", path, e))?;
    println!("  \x1b[32mcreated\x1b[0m {}/{}", root.display(), path);
    Ok(())
}

/// The console entry — Ream's `ace` equivalent.
///
/// `ream <command>` dispatches here, so a project has a working command line
/// from the first commit instead of growing throwaway `tsx bin/*.ts` scripts.
/// Commands live in `commands/` and are discovered automatically; generate one
/// with `ream make:command <name>`.
const CONSOLE_ENTRY: &str = r##"import 'reflect-metadata'
import { Ignitor, prettyPrintError } from '@c9up/ream'

const APP_ROOT = new URL('../', import.meta.url)

const IMPORTER = (filePath: string) =>
  filePath.startsWith('./') || filePath.startsWith('../')
    ? import(new URL(filePath, APP_ROOT).href)
    : import(filePath)

new Ignitor(APP_ROOT, { importer: IMPORTER })
  .useRcFile((await import('../reamrc.js')).default)
  .console()
  .handle(process.argv.slice(2))
  .catch((err) => {
    prettyPrintError(err)
    process.exit(1)
  })
"##;

fn package_json(name: &str, template: &str) -> String {
    // Current framework baseline. `^0.1.0` would misrepresent the released
    // version AND never pull a future 0.2.0 (caret stays under the next minor on
    // 0.x). Bump this one line when the @c9up packages move to a new minor.
    const FW: &str = "^0.1.4";
    let mut deps = vec![
        format!(r#""@c9up/ream": "{FW}""#),
        // bin/server.ts imports it directly; with pnpm's strict node_modules a
        // transitive copy (via @c9up/atlas) isn't resolvable, so declare it.
        r#""reflect-metadata": "^0.2""#.to_string(),
    ];
    if template != "slim" {
        deps.extend([
            format!(r#""@c9up/atlas": "{FW}""#),
            format!(r#""@c9up/rune": "{FW}""#),
            format!(r#""@c9up/warden": "{FW}""#),
            format!(r#""@c9up/spectrum": "{FW}""#),
        ]);
    }
    if template == "web" {
        // Full web stack on top of the api set: HTML templating, events,
        // middleware, signing, and date/recurrence.
        deps.extend([
            format!(r#""@c9up/inker": "{FW}""#),
            format!(r#""@c9up/echo": "{FW}""#),
            format!(r#""@c9up/blackhole": "{FW}""#),
            format!(r#""@c9up/sigil": "{FW}""#),
            format!(r#""@c9up/chronos": "{FW}""#),
        ]);
    }

    let imports = "    \"#app/WILDCARD\": \"./app/WILDCARD\",\n    \"#middleware/WILDCARD\": \"./app/middleware/WILDCARD\",\n    \"#config/WILDCARD\": \"./config/WILDCARD\",\n    \"#providers/WILDCARD\": \"./providers/WILDCARD\",\n    \"#start/WILDCARD\": \"./start/WILDCARD\"".replace("WILDCARD", "*");

    format!("{{\n  \"name\": \"{}\",\n  \"version\": \"0.1.7\",\n  \"private\": true,\n  \"type\": \"module\",\n  \"imports\": {{\n{}\n  }},\n  \"scripts\": {{\n    \"dev\": \"ream dev\",\n    \"build\": \"ream build\",\n    \"start\": \"ream start\",\n    \"test\": \"vitest run\"\n  }},\n  \"dependencies\": {{\n    {}\n  }},\n  \"devDependencies\": {{\n    \"@swc-node/register\": \"^1\",\n    \"tsx\": \"^4\",\n    \"typescript\": \"^5.7\",\n    \"vitest\": \"^3\"\n  }},\n  \"engines\": {{\n    \"node\": \">=22.0.0\"\n  }}\n}}", name, imports, deps.join(",\n    "))
}

fn tsconfig() -> String {
    // Extend the framework-shipped base — apps only declare `paths` + their
    // own `include`. Strict / target / decorators / etc. all come from the
    // base so a Ream bump can adjust them across the ecosystem at once.
    let paths = "      \"#app/WILDCARD\": [\"./app/WILDCARD\"],\n      \"#middleware/WILDCARD\": [\"./app/middleware/WILDCARD\"],\n      \"#config/WILDCARD\": [\"./config/WILDCARD\"],\n      \"#providers/WILDCARD\": [\"./providers/WILDCARD\"],\n      \"#start/WILDCARD\": [\"./start/WILDCARD\"]".replace("WILDCARD", "*");
    format!("{{\n  \"extends\": \"@c9up/ream/tsconfig.app.json\",\n  \"compilerOptions\": {{\n    \"paths\": {{\n{}\n    }}\n  }},\n  \"include\": [\"app\", \"bin\", \"config\", \"providers\", \"start\", \"tests\", \"reamrc.ts\"]\n}}", paths)
}

/// `.swcrc` for swc-node + unplugin-swc — extends the framework-shipped
/// base via SWC's native `extends` field. Decorator metadata is mandatory
/// for IoC ctor-injection; without this file every `@inject()` controller
/// resolves `undefined` for its dependencies.
fn swcrc() -> String {
    "{\n  \"extends\": \"@c9up/ream/swcrc.app.json\"\n}\n".to_string()
}

fn env_file(name: &str, database: &str) -> String {
    let db_name = name.replace('-', "_");
    // APP_KEY signs cookies, sessions, and CSRF tokens. Placeholder here — change
    // it to a unique 32+ byte secret per app/environment before going to prod.
    let app_key = "APP_KEY=change-me-to-a-unique-32+-byte-secret!!\n";
    if database == "postgres" {
        format!("APP_NAME={}\n{}NODE_ENV=development\nPORT=3000\n\nDB_CONNECTION=postgres\nDB_HOST=localhost\nDB_PORT=5432\nDB_DATABASE={}\nDB_USER=postgres\nDB_PASSWORD=secret\n", name, app_key, db_name)
    } else {
        format!("APP_NAME={}\n{}NODE_ENV=development\nPORT=3000\n\nDB_CONNECTION=sqlite\nDB_FILENAME=./data/{}.sqlite\n", name, app_key, name)
    }
}

fn env_typing(database: &str) -> String {
    let db_vars = if database == "postgres" {
        "  DB_CONNECTION: 'postgres' | 'sqlite'\n  DB_HOST: string\n  DB_PORT: string\n  DB_DATABASE: string\n  DB_USER: string\n  DB_PASSWORD: string"
    } else {
        "  DB_CONNECTION: 'postgres' | 'sqlite'\n  DB_FILENAME: string"
    };
    format!("export interface Env {{\n  APP_NAME: string\n  APP_KEY: string\n  NODE_ENV: 'development' | 'production' | 'test'\n  PORT: string\n{}\n}}\n\ndeclare global {{\n  namespace NodeJS {{\n    interface ProcessEnv extends Env {{}}\n  }}\n}}\n", db_vars)
}

fn reamrc(template: &str) -> String {
    // slim AND microservice get an empty reamrc: the microservice template only
    // writes a standalone bin/server.ts (EventBus, no Ignitor/reamrc) and never
    // creates #start/routes, #start/kernel, or providers/AppProvider — so the
    // non-slim preloads would fail to resolve for any reamrc-driven command
    // (audit 2026-06-13).
    if template == "slim" || template == "microservice" {
        return "import { defineConfig } from '@c9up/ream'\n\nexport default defineConfig({\n  providers: [],\n  preloads: [],\n})\n".to_string();
    }
    // The web template pre-wires the session/cookie auth kit: sigil (hashing),
    // warden (auth strategies), and blackhole (signed-CSRF + security headers)
    // providers, on top of the api set.
    if template == "web" {
        return "import { defineConfig } from '@c9up/ream'\n\nexport default defineConfig({\n  providers: [\n    () => import('@c9up/sigil/provider'),\n    () => import('@c9up/warden/provider'),\n    () => import('@c9up/blackhole/provider'),\n    () => import('@c9up/ream/events/provider'),\n    () => import('#providers/AppProvider.js'),\n  ],\n  preloads: [\n    () => import('#start/routes.js'),\n    () => import('#start/kernel.js'),\n  ],\n})\n".to_string();
    }
    "import { defineConfig } from '@c9up/ream'\n\nexport default defineConfig({\n  providers: [\n    () => import('@c9up/ream/events/provider'),\n    () => import('#providers/AppProvider.js'),\n  ],\n  preloads: [\n    () => import('#start/routes.js'),\n    () => import('#start/kernel.js'),\n  ],\n})\n".to_string()
}

/// Shared skeleton for the api + web templates: server entry, AppProvider, and
/// a root route. Each template adds its own `start/kernel.ts` on top (write_file
/// refuses to overwrite, so the kernel must NOT be written here).
fn write_app_base(root: &Path, name: &str) -> Result<(), String> {
    write_file(
        root,
        "bin/server.ts",
        "import 'reflect-metadata'\nimport { Ignitor, prettyPrintError } from '@c9up/ream'\nimport { createHyperServerFactory } from '@c9up/ream/bootstrap'\n\nconst APP_ROOT = new URL('../', import.meta.url)\n\nnew Ignitor(APP_ROOT, {\n  port: Number(process.env.PORT ?? 3000),\n  serverFactory: createHyperServerFactory(),\n})\n  .useRcFile((await import('../reamrc.js')).default)\n  .httpServer()\n  .start()\n  .then(() => {\n    const port = Number(process.env.PORT ?? 3000)\n    console.log(`\\n  ➜ Ream ready on http://localhost:${port}\\n`)\n  })\n  .catch((err) => {\n    prettyPrintError(err)\n    process.exit(1)\n  })\n",
    )?;
    write_file(root, "providers/AppProvider.ts", "import { Provider } from '@c9up/ream'\n\nexport default class AppProvider extends Provider {\n  register() {}\n  async boot() {}\n  async start() {}\n  async ready() {}\n  async shutdown() {}\n}\n")?;
    write_file(root, "start/routes.ts", &format!("import router from '@c9up/ream/services/router'\n\nrouter.get('/', async ({{ response }}) => {{\n  response.status(200).json({{ name: '{}', status: 'running' }})\n}})\n", name))?;
    Ok(())
}

fn write_api_template(root: &Path, name: &str) -> Result<(), String> {
    write_app_base(root, name)?;
    write_file(root, "start/kernel.ts", "import server from '@c9up/ream/services/server'\n\nserver.use([\n  async (ctx, next) => {\n    const start = Date.now()\n    await next()\n    ctx.response.header('x-response-time', `${Date.now() - start}ms`)\n  },\n])\n")?;
    Ok(())
}

/// Web template — the api skeleton plus the session/cookie auth kit:
/// a security-aware kernel (blackhole signed-CSRF + cookie session), the
/// session auth middleware, and auth + blackhole config. Mirrors the proven
/// kitchen-sink wiring so `ream new web` boots a cookie-authed app out of the box.
fn write_web_template(root: &Path, name: &str) -> Result<(), String> {
    write_app_base(root, name)?;

    // Kernel: blackhole (signed CSRF + headers) → body parser → cookie session
    // → auth middleware. Session runs BEFORE auth so `ctx.session` is populated
    // when `@Guard('session')` resolves the user.
    write_file(
        root,
        "start/kernel.ts",
        r#"import { blackholeMiddleware } from '@c9up/blackhole/middleware'
import { BodyParserMiddleware, SessionMiddleware } from '@c9up/ream'
import router from '@c9up/ream/services/router'

const bodyParser = new BodyParserMiddleware()

// Cookie session (stateless — data lives in the encrypted cookie). Secret is
// APP_KEY; the dev fallback keeps `ream dev` working before you set one.
const session = new SessionMiddleware({
  driver: 'cookie',
  secret: process.env.APP_KEY ?? 'change-me-to-a-unique-32+-byte-secret!!',
})

router.use([
  blackholeMiddleware,
  (ctx, next) => bodyParser.handle(ctx, next),
  (ctx, next) => session.handle(ctx, next),
  () => import('#middleware/auth_middleware.js'),
])
"#,
    )?;

    // Session-first auth (AdonisJS web-kit shape). Add a `jwt` block and select
    // it per-route with `@Guard('jwt')` if you also expose an API.
    write_file(
        root,
        "config/auth.ts",
        r#"import type { UserPayload } from '@c9up/warden'

export default {
  defaultStrategy: 'session',
  session: {
    // TODO: resolve your authenticated user from the session-stored id.
    async findUser(_id: string | number): Promise<UserPayload | null> {
      return null
    },
  },
}
"#,
    )?;

    // Signed double-submit CSRF (HMAC over APP_KEY). Cookie/session routes are
    // protected; add Bearer-only API prefixes to `exceptRoutes` (Bearer is
    // CSRF-immune, so it needs no token).
    write_file(
        root,
        "config/blackhole.ts",
        r#"import { defineConfig } from '@c9up/blackhole/config'

export default defineConfig({
  xss: true,
  csrf: { exceptRoutes: [] },
  secret: process.env.APP_KEY ?? 'change-me-to-a-unique-32+-byte-secret!!',
})
"#,
    )?;

    // Auth middleware: populate `ctx.auth` from the cookie session. The guard
    // enforcer (`@Guard('session')`) then asserts authentication per route.
    write_file(
        root,
        "app/middleware/auth_middleware.ts",
        r#"import type { HttpContext } from '@c9up/ream'
import type { AuthResult } from '@c9up/warden'
import auth from '@c9up/warden/services/main'

interface StrategyWithContext {
  verifyWithContext(token: string, ctx: { session?: unknown }): Promise<AuthResult>
}

function hasVerifyWithContext(value: unknown): value is StrategyWithContext {
  return (
    typeof value === 'object' &&
    value !== null &&
    'verifyWithContext' in value &&
    typeof value.verifyWithContext === 'function'
  )
}

export default class AuthMiddleware {
  async handle(ctx: HttpContext, next: () => Promise<void>) {
    if (ctx.session && auth.getStrategyNames().includes('session')) {
      const strategy = auth.getStrategy('session')
      if (hasVerifyWithContext(strategy)) {
        const result = await strategy.verifyWithContext('', { session: ctx.session })
        if (result.authenticated && result.user) {
          ctx.auth = {
            authenticated: true,
            user: result.user,
            roles: result.user.roles ?? [],
            permissions: result.user.permissions ?? [],
          }
        }
      }
    }
    await next()
  }
}
"#,
    )?;

    Ok(())
}

fn write_slim_template(root: &Path, _name: &str) -> Result<(), String> {
    write_file(
        root,
        "bin/server.ts",
        "import { Ignitor } from '@c9up/ream'\nimport { createHyperServerFactory } from '@c9up/ream/bootstrap'\n\nconst app = new Ignitor({\n  port: Number(process.env.PORT ?? 3000),\n  serverFactory: createHyperServerFactory(),\n})\n  .httpServer()\n  .routes((router) => {\n    router.get('/', async ({ response }) => {\n      response.status(200).send('Hello from Ream!')\n    })\n  })\n\nawait app.start()\nconsole.log(`\\n  ➜ Ream ready on http://localhost:${process.env.PORT ?? 3000}\\n`)\n",
    )?;
    Ok(())
}

fn write_microservice_template(root: &Path, name: &str) -> Result<(), String> {
    write_file(root, "bin/server.ts", &format!("import {{ EventBus }} from '@c9up/ream/events'\nimport {{ Logger, ConsoleChannel }} from '@c9up/spectrum'\n\nconst bus = new EventBus()\nconst logger = new Logger({{\n  level: 'info',\n  channels: [new ConsoleChannel('pretty')],\n}})\n\nbus.subscribe('order.*', (eventJson) => {{\n  const event = JSON.parse(eventJson)\n  logger.info(`Received: ${{event.name}}`)\n}})\n\nlogger.info('{} microservice started')\n", name))?;
    Ok(())
}

const GITIGNORE: &str = "node_modules/\ndist/\n.env\n*.sqlite\ndata/\n";

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    fn unique_root(label: &str) -> std::path::PathBuf {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "ream-cli-scaffold-{}-{}-{}",
            label,
            std::process::id(),
            n
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::canonicalize(&root).unwrap()
    }

    /// Sub-dir invocation: write_file uses the canonical root passed by the
    /// caller (mirrors `run()`'s top-of-function canonicalization), so the
    /// write succeeds and lands at the expected path regardless of the
    /// process's current working directory.
    #[test]
    fn microservice_reamrc_has_no_phantom_preloads() {
        // The microservice template only writes bin/server.ts, so its reamrc must
        // not preload #start/routes, #start/kernel, or providers/AppProvider —
        // those files never exist (audit 2026-06-13).
        let rc = reamrc("microservice");
        assert!(rc.contains("preloads: []"));
        assert!(!rc.contains("#start/routes"));
        assert!(!rc.contains("AppProvider"));
    }

    #[test]
    fn write_file_anchors_to_the_canonical_root_passed_by_the_caller() {
        let root = unique_root("anchor");
        write_file(&root, "config/app.ts", "export const x = 1\n").unwrap();
        let written = root.join("config/app.ts");
        assert!(written.exists(), "expected file to be written under root");
        let bytes = fs::read_to_string(&written).unwrap();
        assert_eq!(bytes, "export const x = 1\n");
        let _ = fs::remove_dir_all(&root);
    }

    /// Symlink rejection: a pre-existing leaf symlink at the target path is
    /// refused regardless of where the symlink points (defense-in-depth: the
    /// canonicalize-parent check would still pass if the symlink stays inside
    /// the project tree).
    #[cfg(unix)]
    #[test]
    fn write_file_refuses_to_write_through_a_leaf_symlink() {
        use std::os::unix::fs::symlink;
        let root = unique_root("symlink-leaf");
        let real_target = root.join("real.ts");
        fs::write(&real_target, "// real").unwrap();
        symlink(&real_target, root.join("alias.ts")).unwrap();
        let err = write_file(&root, "alias.ts", "// new").expect_err(
            "writing through an existing leaf symlink must error, not silently overwrite",
        );
        assert!(
            err.contains("leaf symlink"),
            "unexpected error message: {}",
            err
        );
        // The original target still has its pre-symlink content.
        let preserved = fs::read_to_string(&real_target).unwrap();
        assert_eq!(preserved, "// real");
        let _ = fs::remove_dir_all(&root);
    }

    /// Component-level escape: an absolute path on the right-hand side of
    /// `Path::join` discards `root` entirely. The component walk must
    /// reject `RootDir` (and `Prefix` on Windows) so the function refuses
    /// before it computes a path that would escape.
    #[test]
    fn write_file_rejects_absolute_path() {
        let root = unique_root("absolute-reject");
        let err = write_file(&root, "/etc/passwd-spoof", "// nope")
            .expect_err("absolute path must be rejected at the component check");
        assert!(
            err.contains("outside project root"),
            "unexpected error message: {}",
            err
        );
        // No file was created anywhere we can observe.
        let _ = fs::remove_dir_all(&root);
    }

    /// O_EXCL: a pre-existing regular file (no symlink, no hardlink, just
    /// a plain file planted in a TOCTOU window) is refused atomically by
    /// the kernel — closes the race between the leaf-symlink check and
    /// the write.
    #[test]
    fn write_file_refuses_to_overwrite_existing_regular_file() {
        let root = unique_root("o-excl");
        let target = root.join("collision.ts");
        fs::write(&target, "// original").unwrap();
        let err = write_file(&root, "collision.ts", "// new")
            .expect_err("writing onto an existing regular file must error (O_EXCL guarantee)");
        // create_new returns ErrorKind::AlreadyExists → "File exists".
        assert!(
            err.contains("Failed to create") || err.contains("exists"),
            "unexpected error message: {}",
            err
        );
        // Original content untouched.
        let preserved = fs::read_to_string(&target).unwrap();
        assert_eq!(preserved, "// original");
        let _ = fs::remove_dir_all(&root);
    }

    /// `tsconfig.json` extends the framework-shipped base so future Ream
    /// strictness bumps reach every scaffolded app at once. Asserts the
    /// extends pointer is present and the app-side surface stays minimal
    /// (paths + include only).
    #[test]
    fn tsconfig_extends_the_framework_base() {
        let content = tsconfig();
        assert!(
            content.contains("\"extends\": \"@c9up/ream/tsconfig.app.json\""),
            "scaffolded tsconfig.json must extend @c9up/ream/tsconfig.app.json — got:\n{}",
            content
        );
        // The base owns strict/target/decorators — the app config must NOT
        // re-declare them (otherwise future Ream bumps stop propagating).
        assert!(!content.contains("\"strict\""), "app tsconfig leaks strict");
        assert!(
            !content.contains("\"emitDecoratorMetadata\""),
            "app tsconfig leaks emitDecoratorMetadata"
        );
    }

    /// `.swcrc` extends `@c9up/ream/swcrc.app.json` — decorator metadata is
    /// mandatory for `@inject()` IoC ctor resolution. Without this file,
    /// every scaffolded app boots with broken DI.
    #[test]
    fn swcrc_extends_the_framework_base() {
        let content = swcrc();
        assert!(
            content.contains("\"extends\": \"@c9up/ream/swcrc.app.json\""),
            "scaffolded .swcrc must extend @c9up/ream/swcrc.app.json — got:\n{}",
            content
        );
    }

    /// `bin/server.ts` for api/web template uses `createHyperServerFactory()`
    /// from `@c9up/ream/bootstrap` rather than hand-rolling the platform
    /// suffix table and napi require — eliminates ~25 lines of per-app
    /// duplication.
    #[test]
    fn api_template_uses_bootstrap_factory() {
        let root = unique_root("api-template");
        write_api_template(&root, "demo").unwrap();
        let bin = fs::read_to_string(root.join("bin/server.ts")).unwrap();
        assert!(
            bin.contains("createHyperServerFactory"),
            "bin/server.ts must call createHyperServerFactory() — got:\n{}",
            bin
        );
        assert!(
            !bin.contains("createRequire"),
            "bin/server.ts must not hand-roll napi loading anymore"
        );
        let _ = fs::remove_dir_all(&root);
    }

    /// The web template must pre-wire the session/cookie auth kit: a kernel that
    /// registers blackhole + a cookie SessionMiddleware before the auth
    /// middleware, a session-first `config/auth.ts`, signed-CSRF
    /// `config/blackhole.ts`, and the session auth middleware.
    #[test]
    fn web_template_prewires_session_auth() {
        let root = unique_root("web-template");
        write_web_template(&root, "demo").unwrap();

        let kernel = fs::read_to_string(root.join("start/kernel.ts")).unwrap();
        assert!(
            kernel.contains("SessionMiddleware") && kernel.contains("blackholeMiddleware"),
            "web kernel must wire SessionMiddleware + blackhole — got:\n{}",
            kernel
        );

        let auth = fs::read_to_string(root.join("config/auth.ts")).unwrap();
        assert!(
            auth.contains("defaultStrategy: 'session'") && auth.contains("findUser"),
            "config/auth.ts must default to the session strategy — got:\n{}",
            auth
        );

        let bh = fs::read_to_string(root.join("config/blackhole.ts")).unwrap();
        assert!(
            bh.contains("csrf") && bh.contains("secret"),
            "config/blackhole.ts must enable signed CSRF with a secret — got:\n{}",
            bh
        );

        assert!(
            root.join("app/middleware/auth_middleware.ts").exists(),
            "web template must write the session auth middleware"
        );
        let _ = fs::remove_dir_all(&root);
    }

    /// Audit 2026-06-13: the slim template wrote its entry at `app.ts`, but the
    /// shared scripts (`ream dev` → `tsx watch bin/server.ts`, `ream start` →
    /// `node dist/bin/server.js`) target bin/server.ts — so a fresh slim project
    /// could not boot. The entry must live at bin/server.ts.
    #[test]
    fn slim_template_entry_is_bin_server() {
        let root = unique_root("slim-template");
        write_slim_template(&root, "demo").unwrap();
        let bin = fs::read_to_string(root.join("bin/server.ts")).unwrap();
        assert!(
            bin.contains("createHyperServerFactory"),
            "slim bin/server.ts must bootstrap the server — got:\n{}",
            bin
        );
        assert!(
            !root.join("app.ts").exists(),
            "slim must not write app.ts (ream dev/start target bin/server.ts)"
        );
        let _ = fs::remove_dir_all(&root);
    }
}
