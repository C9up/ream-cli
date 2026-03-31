//! Scaffold — create a new Ream project (pure Rust, no Node.js needed for generation).

use dialoguer::Select;
use std::fs;
use std::path::Path;
use std::process::Command;

pub fn run(name: &str) -> Result<(), String> {
    // Validate project name
    if !name.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_') {
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

    println!("\n  Scaffolding {} (template={}, database={})...\n", name, template, database);

    // Create project structure
    let root = Path::new(name);
    fs::create_dir_all(root).map_err(|e| e.to_string())?;

    // Common files
    write_file(root, "package.json", &package_json(name, template))?;
    write_file(root, "tsconfig.json", &tsconfig())?;
    write_file(root, ".env", &env_file(name, database))?;
    write_file(root, "env.ts", &env_typing(database))?;
    write_file(root, ".gitignore", GITIGNORE)?;
    write_file(root, "reamrc.ts", &reamrc(template))?;

    // Template-specific files
    match template {
        "api" | "web" => write_api_template(root, name)?,
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
    let full = root.join(path);
    if let Some(parent) = full.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    fs::write(&full, content).map_err(|e| format!("Failed to write {}: {}", path, e))?;
    println!("  \x1b[32mcreated\x1b[0m {}/{}", root.display(), path);
    Ok(())
}

fn package_json(name: &str, template: &str) -> String {
    let mut deps = vec![r#""@c9up/ream": "^0.1.0""#.to_string()];
    if template != "slim" {
        deps.extend([
            r#""@c9up/pulsar": "^0.1.0""#.to_string(),
            r#""@c9up/atlas": "^0.1.0""#.to_string(),
            r#""@c9up/rune": "^0.1.0""#.to_string(),
            r#""@c9up/warden": "^0.1.0""#.to_string(),
            r#""@c9up/spectrum": "^0.1.0""#.to_string(),
        ]);
    }

    let imports = "    \"#modules/WILDCARD\": \"./app/modules/WILDCARD\",\n    \"#config/WILDCARD\": \"./config/WILDCARD\",\n    \"#providers/WILDCARD\": \"./providers/WILDCARD\",\n    \"#start/WILDCARD\": \"./start/WILDCARD\"".replace("WILDCARD", "*");

    format!("{{\n  \"name\": \"{}\",\n  \"version\": \"0.1.0\",\n  \"private\": true,\n  \"type\": \"module\",\n  \"imports\": {{\n{}\n  }},\n  \"scripts\": {{\n    \"dev\": \"ream dev\",\n    \"build\": \"ream build\",\n    \"start\": \"ream start\",\n    \"test\": \"vitest run\"\n  }},\n  \"dependencies\": {{\n    {}\n  }},\n  \"devDependencies\": {{\n    \"tsx\": \"^4\",\n    \"typescript\": \"^5.7\",\n    \"vitest\": \"^3\"\n  }},\n  \"engines\": {{\n    \"node\": \">=22.0.0\"\n  }}\n}}", name, imports, deps.join(",\n    "))
}

fn tsconfig() -> String {
    let paths = "      \"#modules/WILDCARD\": [\"./app/modules/WILDCARD\"],\n      \"#config/WILDCARD\": [\"./config/WILDCARD\"],\n      \"#providers/WILDCARD\": [\"./providers/WILDCARD\"],\n      \"#start/WILDCARD\": [\"./start/WILDCARD\"]".replace("WILDCARD", "*");
    format!("{{\n  \"compilerOptions\": {{\n    \"strict\": true,\n    \"target\": \"ES2022\",\n    \"module\": \"NodeNext\",\n    \"moduleResolution\": \"NodeNext\",\n    \"outDir\": \"dist\",\n    \"rootDir\": \".\",\n    \"declaration\": true,\n    \"skipLibCheck\": true,\n    \"esModuleInterop\": true,\n    \"experimentalDecorators\": true,\n    \"emitDecoratorMetadata\": true,\n    \"paths\": {{\n{}\n    }}\n  }},\n  \"include\": [\"app\", \"bin\", \"config\", \"providers\", \"start\", \"tests\", \"reamrc.ts\"]\n}}", paths)
}

fn env_file(name: &str, database: &str) -> String {
    let db_name = name.replace('-', "_");
    if database == "postgres" {
        format!("APP_NAME={}\nNODE_ENV=development\nPORT=3000\n\nDB_CONNECTION=postgres\nDB_HOST=localhost\nDB_PORT=5432\nDB_DATABASE={}\nDB_USER=postgres\nDB_PASSWORD=secret\n", name, db_name)
    } else {
        format!("APP_NAME={}\nNODE_ENV=development\nPORT=3000\n\nDB_CONNECTION=sqlite\nDB_FILENAME=./data/{}.sqlite\n", name, name)
    }
}

fn env_typing(database: &str) -> String {
    let db_vars = if database == "postgres" {
        "  DB_CONNECTION: 'postgres' | 'sqlite'\n  DB_HOST: string\n  DB_PORT: string\n  DB_DATABASE: string\n  DB_USER: string\n  DB_PASSWORD: string"
    } else {
        "  DB_CONNECTION: 'postgres' | 'sqlite'\n  DB_FILENAME: string"
    };
    format!("export interface Env {{\n  APP_NAME: string\n  NODE_ENV: 'development' | 'production' | 'test'\n  PORT: string\n{}\n}}\n\ndeclare global {{\n  namespace NodeJS {{\n    interface ProcessEnv extends Env {{}}\n  }}\n}}\n", db_vars)
}

fn reamrc(template: &str) -> String {
    if template == "slim" {
        return "import { defineConfig } from '@c9up/ream'\n\nexport default defineConfig({\n  providers: [],\n  preloads: [],\n})\n".to_string();
    }
    "import { defineConfig } from '@c9up/ream'\n\nexport default defineConfig({\n  providers: [\n    () => import('#providers/AppProvider.js'),\n  ],\n  preloads: [\n    () => import('#start/routes.js'),\n    () => import('#start/kernel.js'),\n  ],\n})\n".to_string()
}

fn write_api_template(root: &Path, name: &str) -> Result<(), String> {
    write_file(root, "bin/server.ts", &format!("import {{ Ignitor }} from '@c9up/ream'\n\nconst app = new Ignitor({{ port: Number(process.env.PORT ?? 3000) }})\n  .httpServer()\n\nawait app.start()\n"))?;
    write_file(root, "providers/AppProvider.ts", "import { Provider } from '@c9up/ream'\n\nexport default class AppProvider extends Provider {\n  register() {}\n  async boot() {}\n  async start() {}\n  async ready() {}\n  async shutdown() {}\n}\n")?;
    write_file(root, "start/routes.ts", &format!("import type {{ Router }} from '@c9up/ream'\n\nexport default function (router: Router) {{\n  router.get('/', async (ctx) => {{\n    ctx.response!.headers['content-type'] = 'application/json'\n    ctx.response!.body = JSON.stringify({{ name: '{}', status: 'running' }})\n  }})\n}}\n", name))?;
    write_file(root, "start/kernel.ts", "import type { MiddlewareRegistry } from '@c9up/ream'\n\nexport default function (middleware: MiddlewareRegistry) {\n  middleware.use(async (ctx, next) => {\n    const start = Date.now()\n    await next()\n    ctx.response!.headers['x-response-time'] = `${Date.now() - start}ms`\n  })\n}\n")?;
    Ok(())
}

fn write_slim_template(root: &Path, _name: &str) -> Result<(), String> {
    write_file(root, "app.ts", "import { Ignitor } from '@c9up/ream'\n\nconst app = new Ignitor({ port: Number(process.env.PORT ?? 3000) })\n  .httpServer()\n  .routes((router) => {\n    router.get('/', async (ctx) => {\n      ctx.response!.body = 'Hello from Ream!'\n    })\n  })\n\nawait app.start()\n")?;
    Ok(())
}

fn write_microservice_template(root: &Path, name: &str) -> Result<(), String> {
    write_file(root, "bin/server.ts", &format!("import {{ PulsarBus }} from '@c9up/pulsar'\nimport {{ Logger, ConsoleChannel }} from '@c9up/spectrum'\n\nconst bus = new PulsarBus()\nconst logger = new Logger({{\n  level: 'info',\n  channels: [new ConsoleChannel('pretty')],\n}})\n\nbus.subscribe('order.*', (eventJson) => {{\n  const event = JSON.parse(eventJson)\n  logger.info(`Received: ${{event.name}}`)\n}})\n\nlogger.info('{} microservice started')\n", name))?;
    Ok(())
}

const GITIGNORE: &str = "node_modules/\ndist/\n.env\n*.sqlite\ndata/\n";
