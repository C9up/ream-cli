//! Scaffold — create a new Ream project (pure Rust, no Node.js needed for generation).

use dialoguer::Select;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::Command;

/// What `ream new` was told on the command line.
///
/// A flag skips its prompt, and `--yes` takes the default for whatever is left,
/// so the command runs with no terminal at all — CI, a container, a script.
/// Without this the two `Select` prompts abort on a raw dialoguer I/O error.
#[derive(Default)]
pub struct NewOptions<'a> {
    pub template: Option<&'a str>,
    pub database: Option<&'a str>,
    pub yes: bool,
}

pub fn run(name: &str, options: &NewOptions<'_>) -> Result<(), String> {
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

    const TEMPLATES: [&str; 4] = ["api", "web", "microservice", "slim"];
    const DATABASES: [&str; 2] = ["postgres", "sqlite"];
    // Both flags are checked before either prompt: a bad value is an error in
    // the command line, and must be reported as one whether or not there is a
    // terminal to prompt on.
    validate("template", &TEMPLATES, options.template)?;
    validate("db", &DATABASES, options.database)?;

    let template = choose("template", &TEMPLATES, options.template, options.yes)?;
    let database = choose("db", &DATABASES, options.database, options.yes)?;

    let (template, database) = (template.as_str(), database.as_str());
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
    // The environment's TYPES come from `start/env.ts` — `Env.create` returns a
    // typed accessor, so `env.get('PORT')` is a number without a second
    // hand-maintained declaration to keep in step with the schema.
    //
    // The microservice template runs a standalone entry with no rc file and no
    // Ignitor, so nothing there would import it.
    if template != "microservice" {
        write_file(root, "start/env.ts", &start_env(database))?;
    }
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

/// The console entry — where the app's commands are booted from.
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
  .tap((app) => {
    // Validates the environment against start/env.ts before anything boots, so
    // a missing APP_KEY stops the app here rather than on the first response
    // that tries to sign a cookie.
    app.booting(async () => {
      await import('#start/env.js')
    })
  })
  .useRcFile((await import('../reamrc.js')).default)
  .console()
  .handle(process.argv.slice(2))
  .catch((err) => {
    prettyPrintError(err)
    process.exit(1)
  })
"##;

/// What a scaffolded app declares, one caret per package.
///
/// On 0.x a caret stays under the next MINOR, so a pin is not a floor that
/// keeps working — it is a ceiling. `^0.1.27` could not reach @c9up/ream once
/// it published 0.2.0, and `^0.2.0` could not reach @c9up/atlas once it
/// published 0.3.0: `ream new` went on scaffolding apps against the line before
/// the one being maintained, with none of its fixes, and nothing said so. The
/// comment here already named that failure; the table under it was left to walk
/// into it.
///
/// `scripts/check-scaffold-pins.sh` compares every entry against the registry
/// and fails the publish, because a list that has to be remembered is a list
/// that goes stale.
const BASE_DEPS: &[(&str, &str)] = &[
    ("@c9up/ream", "^0.2.14"),
    // bin/server.ts imports it directly; with pnpm's strict node_modules a
    // transitive copy (via @c9up/atlas) isn't resolvable, so declare it.
    ("reflect-metadata", "^0.2"),
];

/// Everything but `slim`, which stays at the framework alone.
const APP_DEPS: &[(&str, &str)] = &[
    ("@c9up/atlas", "^0.3.11"),
    ("@c9up/rune", "^0.1.13"),
    ("@c9up/warden", "^0.1.25"),
    ("@c9up/spectrum", "^0.1.12"),
];

/// The full web stack on top: HTML templating, events, middleware, signing, and
/// date/recurrence.
const WEB_DEPS: &[(&str, &str)] = &[
    ("@c9up/inker", "^0.1.15"),
    ("@c9up/echo", "^0.1.13"),
    ("@c9up/blackhole", "^0.1.17"),
    ("@c9up/sigil", "^0.1.13"),
    ("@c9up/chronos", "^0.1.12"),
];

/// Development dependencies every template gets.
///
/// `tsx` used to be here and is not any more: `ream dev` refuses it on purpose
/// — esbuild cannot emit `design:paramtypes`, so a project that ran its own
/// entry through tsx got a container where every injected dependency resolved
/// to `undefined`. Shipping the tool in the manifest invited exactly that.
///
/// The runner is helix, which is what `ream test` drives; `vitest` was here
/// while the two scripts said different things about what runs the tests.
const DEV_DEPS: &[(&str, &str)] = &[
    ("@c9up/helix", "^0.2.8"),
    ("@swc-node/register", "^1"),
    ("typescript", "^5.7"),
];

/// Plus the bridge, for a template whose app the suites boot.
const APP_DEV_DEPS: &[(&str, &str)] = &[("@c9up/helix-plugin-ream", "^0.1.6")];

fn package_json(name: &str, template: &str) -> String {
    let quoted = |(pkg, range): &(&str, &str)| format!(r#""{pkg}": "{range}""#);
    let mut deps: Vec<String> = BASE_DEPS.iter().map(quoted).collect();
    if template != "slim" {
        deps.extend(APP_DEPS.iter().map(quoted));
    }
    if template == "web" {
        deps.extend(WEB_DEPS.iter().map(quoted));
    }

    let mut dev_deps: Vec<String> = DEV_DEPS.iter().map(quoted).collect();
    if boots_its_app(template) {
        dev_deps.extend(APP_DEV_DEPS.iter().map(quoted));
    }
    dev_deps.sort();

    // `ream test` reads the suites out of the rc file; a template with no rc
    // file to read them from runs helix directly.
    let test_script = if boots_its_app(template) {
        "ream test"
    } else {
        "helix test"
    };

    let imports = "    \"#app/WILDCARD\": \"./app/WILDCARD\",\n    \"#middleware/WILDCARD\": \"./app/middleware/WILDCARD\",\n    \"#config/WILDCARD\": \"./config/WILDCARD\",\n    \"#providers/WILDCARD\": \"./providers/WILDCARD\",\n    \"#start/WILDCARD\": \"./start/WILDCARD\"".replace("WILDCARD", "*");

    format!("{{\n  \"name\": \"{}\",\n  \"version\": \"0.1.7\",\n  \"private\": true,\n  \"type\": \"module\",\n  \"imports\": {{\n{}\n  }},\n  \"scripts\": {{\n    \"dev\": \"ream dev\",\n    \"build\": \"ream build\",\n    \"start\": \"ream start\",\n    \"test\": \"{}\"\n  }},\n  \"dependencies\": {{\n    {}\n  }},\n  \"devDependencies\": {{\n    {}\n  }},\n  \"engines\": {{\n    \"node\": \">=22.0.0\"\n  }}\n}}", name, imports, test_script, deps.join(",\n    "), dev_deps.join(",\n    "))
}

/// Does this template start the application from the rc file?
///
/// `api` and `web` do. `slim` builds its own Ignitor inline and `microservice`
/// has none at all, so neither has an rc file a suite could boot from.
fn boots_its_app(template: &str) -> bool {
    matches!(template, "api" | "web")
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

/// Written only when a key cannot be generated. The framework's cookie signer
/// refuses it outright, so it never becomes a live signing key.
pub const PLACEHOLDER_APP_KEY: &str = "change-me-to-a-unique-32+-byte-secret!!";

fn env_file(name: &str, database: &str) -> String {
    let db_name = name.replace('-', "_");
    // APP_KEY signs cookies, sessions and CSRF tokens, so every app gets its OWN
    // random key at creation — the way `generate:key` mints one. A shared
    // placeholder would mean every scaffolded app signs with a value anybody
    // can read in this repository, which is not a secret at all.
    //
    // If node cannot be reached the placeholder is written instead: refusing to
    // create the project over it would be worse, and the cookie signer refuses
    // this exact value, so it cannot quietly reach production.
    let generated =
        crate::commands::generate_app_key().unwrap_or_else(|_| PLACEHOLDER_APP_KEY.to_string());
    let app_key = format!("APP_KEY={}\n", generated);
    let app_key = app_key.as_str();
    if database == "postgres" {
        format!("APP_NAME={}\n{}NODE_ENV=development\nPORT=3000\n\nDB_CONNECTION=postgres\nDB_HOST=localhost\nDB_PORT=5432\nDB_DATABASE={}\nDB_USER=postgres\nDB_PASSWORD=secret\n", name, app_key, db_name)
    } else {
        format!("APP_NAME={}\n{}NODE_ENV=development\nPORT=3000\n\nDB_CONNECTION=sqlite\nDB_FILENAME=./data/{}.sqlite\n", name, app_key, name)
    }
}

/// `start/env.ts` — the runtime validation of the environment, in the shape the
/// framework's `Env.create` expects.
///
/// This is what makes a missing `APP_KEY` a boot failure rather than a surprise
/// on the first response that tries to sign a cookie. Config files read their
/// values through it (`import env from '#start/env'`), which also loads the
/// `.env` files as an import side-effect, in every flow — server, console and
/// tests alike.
fn start_env(database: &str) -> String {
    let db_vars = if database == "postgres" {
        "  DB_CONNECTION: Env.schema.enum(['postgres', 'sqlite'] as const),\n  DB_HOST: Env.schema.string({ format: 'host' }),\n  DB_PORT: Env.schema.number(),\n  DB_DATABASE: Env.schema.string(),\n  DB_USER: Env.schema.string(),\n  DB_PASSWORD: Env.schema.string.optional(),"
    } else {
        "  DB_CONNECTION: Env.schema.enum(['postgres', 'sqlite'] as const),\n  DB_FILENAME: Env.schema.string(),"
    };
    format!(
        "import {{ Env }} from '@c9up/ream'\n\nexport default await Env.create(new URL('../', import.meta.url), {{\n  APP_NAME: Env.schema.string(),\n  // Signs cookies, sessions and CSRF tokens. Required: without it the app\n  // refuses to start, rather than serving unsigned values that read as signed.\n  APP_KEY: Env.schema.string(),\n  NODE_ENV: Env.schema.enum(['development', 'production', 'test'] as const),\n  PORT: Env.schema.number(),\n{}\n}})\n",
        db_vars
    )
}

/// The `tests` block `ream test` reads its suites out of.
///
/// Two suites, the split AdonisJS scaffolds: `unit` for what needs nothing
/// started, `functional` for what talks to a booted server. Only `functional`
/// pays for the server, because only its hook starts one.
const TESTS_BLOCK: &str = "  tests: {\n    suites: [\n      { name: 'unit', files: ['tests/unit/**/*.test.ts'], timeout: 2000 },\n      { name: 'functional', files: ['tests/functional/**/*.test.ts'], timeout: 30000 },\n    ],\n  },\n";

/// `tests/bootstrap.ts` — how the suites reach the application.
///
/// The server is started by the `functional` suite's hook rather than at plugin
/// time, so a unit test file boots nothing. `start()` hands back its own
/// teardown, which is why the hook is one line and there is no matching
/// `teardown` to forget.
const TEST_BOOTSTRAP: &str = r##"import { configure } from '@c9up/helix'
import { apiClient } from '@c9up/helix-plugin-ream'
import { Ignitor } from '@c9up/ream'
import { createHyperServerFactory } from '@c9up/ream/bootstrap'
import { createTestUtils } from '@c9up/ream/testing/utils'

const APP_ROOT = new URL('../', import.meta.url)

export const testUtils = createTestUtils(async (port) => {
  const ignitor = await new Ignitor(APP_ROOT, {
    port,
    serverFactory: createHyperServerFactory(),
  })
    .useRcFile((await import('../reamrc.js')).default)
    .httpServer()
    .start()

  // Port 0 asks the OS for a free one, so the caller has to be told which.
  return { port: await ignitor.port(), close: () => ignitor.stop() }
})

await configure({
  plugins: [apiClient({ testUtils })],
  configureSuite(suite) {
    if (suite.name === 'functional') {
      return suite.setup(() => testUtils.httpServer().start())
    }
  },
})
"##;

/// A first test, so `ream test` answers on a fresh project.
const FIRST_TEST: &str = r##"import { test } from '@c9up/helix'

test('the root route answers', async ({ client }) => {
  await client.get('/').assertOk()
})
"##;

fn reamrc(template: &str) -> String {
    // slim AND microservice get an empty reamrc: the microservice template only
    // writes a standalone bin/server.ts (EventBus, no Ignitor/reamrc) and never
    // creates #start/routes, #start/kernel, or providers/AppProvider — so the
    // non-slim preloads would fail to resolve for any reamrc-driven command
    // (audit 2026-06-13).
    if template == "microservice" {
        return "import { defineConfig } from '@c9up/ream'\n\nexport default defineConfig({\n  providers: [],\n  preloads: [],\n})\n".to_string();
    }
    if template == "slim" {
        return "import { defineConfig } from '@c9up/ream'\n\nexport default defineConfig({\n  providers: [],\n  preloads: [],\n})\n".to_string();
    }
    // The web template pre-wires the session/cookie auth kit: sigil (hashing),
    // warden (auth strategies), and blackhole (signed-CSRF + security headers)
    // providers, on top of the api set.
    if template == "web" {
        return format!(
            "import {{ defineConfig }} from '@c9up/ream'\n\nexport default defineConfig({{\n  providers: [\n    () => import('@c9up/sigil/provider'),\n    () => import('@c9up/warden/provider'),\n    () => import('@c9up/blackhole/provider'),\n    () => import('@c9up/ream/events/provider'),\n    () => import('#providers/AppProvider.js'),\n  ],\n  preloads: [\n    () => import('#start/routes.js'),\n    () => import('#start/kernel.js'),\n  ],\n{}}})\n",
            TESTS_BLOCK
        );
    }
    format!(
        "import {{ defineConfig }} from '@c9up/ream'\n\nexport default defineConfig({{\n  providers: [\n    () => import('@c9up/ream/events/provider'),\n    () => import('#providers/AppProvider.js'),\n  ],\n  preloads: [\n    () => import('#start/routes.js'),\n    () => import('#start/kernel.js'),\n  ],\n{}}})\n",
        TESTS_BLOCK
    )
}

/// Shared skeleton for the api + web templates: server entry, AppProvider, and
/// a root route. Each template adds its own `start/kernel.ts` on top (write_file
/// refuses to overwrite, so the kernel must NOT be written here).
fn write_app_base(root: &Path, name: &str) -> Result<(), String> {
    write_file(root, "tests/bootstrap.ts", TEST_BOOTSTRAP)?;
    write_file(root, "tests/functional/root.test.ts", FIRST_TEST)?;
    write_file(
        root,
        "bin/server.ts",
        "import 'reflect-metadata'\nimport { Ignitor, prettyPrintError } from '@c9up/ream'\nimport { createHyperServerFactory } from '@c9up/ream/bootstrap'\n\nconst APP_ROOT = new URL('../', import.meta.url)\n\nnew Ignitor(APP_ROOT, {\n  port: Number(process.env.PORT ?? 3000),\n  serverFactory: createHyperServerFactory(),\n})\n  .tap((app) => {\n    // Validates the environment against start/env.ts before anything boots,\n    // so a missing APP_KEY stops the app here rather than on the first\n    // response that tries to sign a cookie.\n    app.booting(async () => {\n      await import('#start/env.js')\n    })\n  })\n  .useRcFile((await import('../reamrc.js')).default)\n  .httpServer()\n  .start()\n  .then((app) => {\n    const port = Number(process.env.PORT ?? 3000)\n    console.log(`\\n  ➜ Ream ready on http://${app.host()}:${port}\\n`)\n  })\n  .catch((err) => {\n    prettyPrintError(err)\n    process.exit(1)\n  })\n",
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
        r#"import env from '#start/env'
import { blackholeMiddleware } from '@c9up/blackhole/middleware'
import { BodyParserMiddleware, SessionMiddleware } from '@c9up/ream'
import router from '@c9up/ream/services/router'

const bodyParser = new BodyParserMiddleware()

// Cookie session — the data lives in the encrypted cookie, signed with APP_KEY.
const session = new SessionMiddleware({
  driver: 'cookie',
  // Through the validated environment: start/env.ts is what guarantees the
  // key exists, and reading `process.env` around it returns `undefined` typed
  // as a string.
  secret: env.get('APP_KEY'),
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
        r#"import env from '#start/env'
import { defineConfig } from '@c9up/blackhole/config'

export default defineConfig({
  xss: true,
  csrf: { exceptRoutes: [] },
  // Through the validated environment, never `process.env` directly: the
  // schema in start/env.ts is what guarantees the key is there at all, and
  // reading around it gives back `string | undefined` typed as a string.
  secret: env.get('APP_KEY'),
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
        "import { Ignitor } from '@c9up/ream'\nimport { createHyperServerFactory } from '@c9up/ream/bootstrap'\n\nconst app = new Ignitor({\n  port: Number(process.env.PORT ?? 3000),\n  serverFactory: createHyperServerFactory(),\n})\n  .tap((app) => {\n    // Validates the environment against start/env.ts before anything boots,\n    // so a missing APP_KEY stops the app here rather than on the first\n    // response that tries to sign a cookie.\n    app.booting(async () => {\n      await import('#start/env.js')\n    })\n  })\n  .httpServer()\n  .routes((router) => {\n    router.get('/', async ({ response }) => {\n      response.status(200).send('Hello from Ream!')\n    })\n  })\n\nawait app.start()\nconsole.log(`\\n  ➜ Ream ready on http://${app.host()}:${process.env.PORT ?? 3000}\\n`)\n",
    )?;
    Ok(())
}

fn write_microservice_template(root: &Path, name: &str) -> Result<(), String> {
    write_file(root, "bin/server.ts", &format!("import {{ EventBus }} from '@c9up/ream/events'\nimport {{ Logger, ConsoleChannel }} from '@c9up/spectrum'\n\nconst bus = new EventBus()\nconst logger = new Logger({{\n  level: 'info',\n  channels: [new ConsoleChannel('pretty')],\n}})\n\nbus.subscribe('order.*', (eventJson) => {{\n  const event = JSON.parse(eventJson)\n  logger.info(`Received: ${{event.name}}`)\n}})\n\nlogger.info('{} microservice started')\n", name))?;
    Ok(())
}

const GITIGNORE: &str = "node_modules/\ndist/\n.env\n*.sqlite\ndata/\n";

/// Reject an unknown flag value, naming what was allowed — rather than
/// scaffolding something the user did not ask for.
pub fn validate(flag: &str, allowed: &[&str], given: Option<&str>) -> Result<(), String> {
    match given {
        Some(value) if !allowed.contains(&value) => Err(format!(
            "Unknown --{} '{}' — expected one of: {}",
            flag,
            value,
            allowed.join(", ")
        )),
        _ => Ok(()),
    }
}

/// Resolve one choice: the flag if given, the default under `--yes`, else a
/// prompt.
fn choose(flag: &str, allowed: &[&str], given: Option<&str>, yes: bool) -> Result<String, String> {
    if let Some(value) = given {
        return Ok(value.to_string());
    }
    if yes {
        return Ok(allowed[0].to_string());
    }
    let index = Select::new()
        .with_prompt(format!("Select a {}", flag))
        .items(allowed)
        .default(0)
        .interact()
        // dialoguer fails here when there is no terminal to read from, and its
        // own message says nothing about how to proceed.
        .map_err(|e| {
            format!(
                "Cannot prompt for --{} ({}). Pass --{} <{}> or --yes to take the default.",
                flag,
                e,
                flag,
                allowed.join("|")
            )
        })?;
    Ok(allowed[index].to_string())
}

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
    /// A scaffolded app tests with the runner `ream test` drives, and does not
    /// ship the loader the CLI refuses.
    ///
    /// `tsx` was a devDependency of every generated project while `ream dev`
    /// went out of its way not to use it — esbuild cannot emit
    /// `design:paramtypes`, so an entry run through it gets a container where
    /// every injected dependency is `undefined`. And `scripts.test` said
    /// `vitest run` while the framework's own command is `ream test`.
    #[test]
    fn a_scaffolded_app_tests_with_the_runner_the_framework_drives() {
        let manifest = package_json("demo", "api");
        assert!(manifest.contains("\"test\": \"ream test\""), "{manifest}");
        assert!(manifest.contains("@c9up/helix"), "{manifest}");
        assert!(manifest.contains("@c9up/helix-plugin-ream"), "{manifest}");
        assert!(
            !manifest.contains("tsx"),
            "tsx is still declared: {manifest}"
        );
        assert!(!manifest.contains("vitest"), "{manifest}");

        // No rc file to read suites from, so `ream test` has nothing to read.
        let slim = package_json("demo", "slim");
        assert!(slim.contains("\"test\": \"helix test\""), "{slim}");
        assert!(!slim.contains("@c9up/helix-plugin-ream"), "{slim}");
    }

    /// The suites are declared where `ream test` looks for them, and the
    /// bootstrap that boots the app for them is written.
    #[test]
    fn the_app_templates_declare_their_suites_and_a_bootstrap() {
        for template in ["api", "web"] {
            let rc = reamrc(template);
            assert!(rc.contains("tests: {"), "{template}: {rc}");
            assert!(rc.contains("name: 'unit'"), "{template}: {rc}");
            assert!(rc.contains("name: 'functional'"), "{template}: {rc}");
        }

        let root = unique_root("suites");
        write_api_template(&root, "demo").unwrap();
        let bootstrap = fs::read_to_string(root.join("tests/bootstrap.ts")).unwrap();
        // Only `functional` pays for a server: a unit test file boots nothing.
        assert!(bootstrap.contains("configureSuite"), "{bootstrap}");
        assert!(
            bootstrap.contains("testUtils.httpServer().start()"),
            "{bootstrap}"
        );
        assert!(
            root.join("tests/functional/root.test.ts").exists(),
            "a fresh project must have something for `ream test` to run"
        );
        let _ = fs::remove_dir_all(&root);
    }

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

    /// The environment is VALIDATED at boot, the way the framework's own
    /// `Env.create` is meant to be reached: `start/env.ts` declares the schema,
    /// and both entries import it from a `booting` hook.
    ///
    /// Without this, a missing `APP_KEY` is not a boot failure — it is a
    /// surprise on the first response that tries to sign a cookie.
    #[test]
    fn app_validates_its_environment_at_boot() {
        let root = unique_root("env-boot");
        write_api_template(&root, "demo").unwrap();
        let bin = fs::read_to_string(root.join("bin/server.ts")).unwrap();
        assert!(
            bin.contains("app.booting(") && bin.contains("#start/env.js"),
            "bin/server.ts must import #start/env from a booting hook — got:\n{}",
            bin
        );
        let _ = fs::remove_dir_all(&root);
    }

    /// `start/env.ts` requires APP_KEY, and there is no second hand-maintained
    /// type declaration to drift from it — the types come from `Env.create`.
    #[test]
    fn start_env_requires_the_app_key() {
        let env = start_env("sqlite");
        assert!(
            env.contains("Env.create") && env.contains("APP_KEY: Env.schema.string()"),
            "start/env.ts must require APP_KEY — got:\n{}",
            env
        );
        assert!(
            !env.contains("optional"),
            "APP_KEY must not be optional — got:\n{}",
            env
        );
    }

    /// Every scaffolded app gets its OWN key. A shared placeholder is a signing
    /// key anybody can read in this repository, which is not a secret at all.
    #[test]
    fn each_app_gets_its_own_app_key() {
        let first = env_file("one", "sqlite");
        let second = env_file("two", "sqlite");
        let key_of = |body: &str| {
            crate::envfile::read_env_value(body, "APP_KEY").expect("APP_KEY must be written")
        };
        let a = key_of(&first);
        let b = key_of(&second);
        assert_ne!(a, b, "two apps must not share a signing key");
        assert_ne!(a, PLACEHOLDER_APP_KEY, "the placeholder is not a key");
        assert!(a.len() >= 32, "key looks too short: {} chars", a.len());
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

    const TEMPLATES: [&str; 4] = ["api", "web", "microservice", "slim"];

    #[test]
    fn a_flag_value_is_taken_as_given() {
        assert_eq!(
            choose("template", &TEMPLATES, Some("slim"), false).unwrap(),
            "slim"
        );
    }

    #[test]
    fn yes_takes_the_first_option_without_a_terminal() {
        // The whole point of --yes: no prompt, so no TTY needed.
        assert_eq!(choose("template", &TEMPLATES, None, true).unwrap(), "api");
    }

    #[test]
    fn an_unknown_flag_value_names_what_was_allowed() {
        let error = validate("template", &TEMPLATES, Some("rails")).unwrap_err();
        assert!(error.contains("Unknown --template 'rails'"));
        assert!(error.contains("api, web, microservice, slim"));
    }

    #[test]
    fn a_valid_flag_value_passes_validation() {
        assert!(validate("template", &TEMPLATES, Some("web")).is_ok());
        assert!(validate("template", &TEMPLATES, None).is_ok());
    }
}
